//! `rimz run` — supervised one-shot agent runs.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use super::{GlobalFlags, RoomTarget};
use rimz::agents::{AgentAdapter, hook_trust_fix};
use rimz::bridge::{self, ExpectedRunFrame, RunWakeOutcome, SocketGuard};
use rimz::ids::AgentKind;
use rimz::mux::{LayoutPanes, PaneCmd, SessionOptions, TabOptions};
use rimz::run::{PermissionMode, RunRecord, RunStatus};
use rimz::workspace::{self, WorkspaceResolver};

#[derive(Debug, Args)]
pub struct RunArgs {
    #[command(subcommand)]
    command: Option<RunSubcmd>,
    /// Prompt to run in the selected agent.
    #[arg(value_name = "PROMPT")]
    prompt: Option<String>,
    /// Agent kind to launch.
    #[arg(long)]
    agent: Option<String>,
    /// Use a Rimz-owned worktree. Bare flag creates one fresh worktree; NAME reuses or creates it.
    #[arg(long, value_name = "NAME", num_args = 0..=1, default_missing_value = "")]
    worktree: Option<String>,
    /// Let the agent ask before tool use instead of accepting edits automatically.
    #[arg(long)]
    ask: bool,
    /// Skip provider permission prompts where the adapter supports it.
    #[arg(long)]
    yolo: bool,
    /// Wait cap (`30s`, `5m`, `1h`, `1d`). Omit for unbounded.
    #[arg(long, value_parser = parse_timeout)]
    timeout: Option<Duration>,
    /// Leave the agent pane open after the run finishes.
    #[arg(long)]
    keep: bool,
    /// Launch the run and print only its run id.
    #[arg(long)]
    detach: bool,
}

#[derive(Debug, Subcommand)]
enum RunSubcmd {
    /// Show one run in the current workspace.
    Status {
        run_id: rimz::RunId,
        #[arg(long)]
        json: bool,
    },
    /// List runs in the current workspace, newest first.
    List {
        #[arg(long)]
        json: bool,
    },
}

pub fn run(args: RunArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        Some(RunSubcmd::Status { run_id, json }) => status(run_id, json, globals),
        Some(RunSubcmd::List { json }) => list(json, globals),
        None => run_prompt(args, globals),
    }
}

fn run_prompt(args: RunArgs, globals: &GlobalFlags) -> Result<()> {
    let mode = permission_mode(&args)?;
    let prompt = args
        .prompt
        .filter(|prompt| !prompt.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("expected a prompt, or `rimz run status|list`"))?;
    let workspace = WorkspaceResolver::resolve(".", globals.root.clone())
        .context("resolving current workspace")?;
    guard_workspace_pin(&workspace, globals.root.as_deref())?;
    let (adapter, permission_args) = resolve_run_adapter(args.agent.as_deref(), mode, &prompt)?;

    let machine_config = super::machine_config();
    let launch = super::tab::resolve_cwd(
        &workspace,
        &machine_config.worktree,
        args.worktree.as_deref(),
    )?;

    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    let backend = rimz::mux::backend_for(mux);
    let mux_config = rimz::config::MultiplexerConfig::from(&machine_config);
    let width = rimz::mux::SidebarWidth::from_config(&machine_config.sidebar);
    let detected_size = rimz::mux::detect_terminal_size();
    let ledger = super::open_ledger(&workspace)?;
    backend.ensure_session(&SessionOptions {
        session_name: workspace.session_name.clone(),
        workspace_id: workspace.workspace_id.clone(),
        project_root: workspace.project_root.clone(),
        cwd: launch.cwd.clone(),
        config: mux_config.clone(),
        detected_size,
    })?;
    let room = RoomTarget {
        workspace_id: &workspace.workspace_id,
        project_root: &workspace.project_root,
        session_name: &workspace.session_name,
        cwd: &launch.cwd,
        mux_config: &mux_config,
        width,
        detected_size,
        refresh_ms: None,
    };
    super::launch_sidebar_for_workspace(backend.as_ref(), &room, None, &[]);
    super::gate_room_before_attach(backend.as_ref(), &room, None, &[])?;
    super::ensure_presence_plugin(
        backend.as_ref(),
        &workspace.session_name,
        &workspace.workspace_id,
    );

    let record = RunRecord::new(
        workspace.workspace_id.clone(),
        AgentKind::new_unchecked(adapter.descriptor().kind),
        mode,
        prompt.clone(),
        launch.cwd.clone(),
    );
    let run_id = record.run_id.clone();
    let pane = run_pane_cmd(
        adapter,
        &run_id,
        &launch.cwd,
        &prompt,
        args.worktree.is_some(),
        &permission_args,
        args.detach && !args.keep,
    )?;
    let bound = if args.detach {
        None
    } else {
        Some(bridge::bind_run(ledger.runtime_paths(), &run_id).context("binding run socket")?)
    };
    let socket_guard = bound
        .as_ref()
        .map(|(_sock, sock_path)| SocketGuard::new(sock_path.clone()));
    rimz::run::create(ledger.paths(), &record).context("recording run")?;

    let open_result = backend.open_tab(&TabOptions {
        session_name: workspace.session_name.clone(),
        title: format!("run: {}", adapter.descriptor().kind),
        cwd: launch.cwd.clone(),
        panes: LayoutPanes {
            columns: vec![vec![pane]],
        },
        focus: false,
        sidebar: super::build_sidebar_opts(&room, Vec::new())?,
    });
    if let Err(err) = open_result {
        let _ = rimz::run::fail(ledger.paths(), &run_id);
        return Err(err).context("opening run tab");
    }

    if args.detach {
        #[expect(clippy::print_stdout, reason = "command result is the run id")]
        {
            println!("{run_id}");
        }
        return Ok(());
    }

    let Some((sock, _sock_path)) = bound else {
        bail!("blocking run did not bind its completion socket");
    };
    let expected = ExpectedRunFrame {
        workspace_id: workspace.workspace_id.clone(),
        run_id: run_id.clone(),
    };
    let outcome = wait_for_run(sock, expected, args.timeout)?;
    let record = terminal_record_after_wait(ledger.paths(), &run_id, outcome)?;

    if !args.keep {
        close_run_pane(backend.as_ref(), &ledger, &workspace.session_name, &record);
    }
    print_run_output(&record)?;
    drop(socket_guard);
    std::process::exit(record.status.exit_code());
}

fn guard_workspace_pin(
    workspace: &rimz::ResolvedWorkspace,
    root_override: Option<&Path>,
) -> Result<()> {
    if root_override.is_some() || std::env::var_os(workspace::ENV_WORKSPACE_ID).is_none() {
        return Ok(());
    }
    let pinned = WorkspaceResolver::resolve_participant(".", None)
        .context("checking the current Rimz workspace pin")?;
    if pinned.workspace_id == workspace.workspace_id {
        return Ok(());
    }
    bail!(
        "`rimz run` would launch in workspace {} from cwd {}, but this process is pinned to Rimz workspace {}; run it outside that room, cd into the pinned workspace, or pass --root",
        workspace.workspace_id,
        workspace.project_root.display(),
        pinned.workspace_id
    )
}

fn status(run_id: rimz::RunId, json: bool, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve(".", globals.root.clone())
        .context("resolving current workspace")?;
    let ledger = super::open_ledger(&workspace)?;
    let record = rimz::run::load(ledger.paths(), &run_id)?;
    if json {
        print_json(&record)?;
    } else {
        #[expect(clippy::print_stdout, reason = "command result is the run status")]
        {
            println!(
                "{} {} {}",
                record.run_id,
                status_label(record.status),
                record.kind
            );
        }
    }
    Ok(())
}

fn list(json: bool, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve(".", globals.root.clone())
        .context("resolving current workspace")?;
    let ledger = super::open_ledger(&workspace)?;
    let records = rimz::run::list(ledger.paths())?;
    if json {
        print_json(&records)?;
    } else {
        #[expect(clippy::print_stdout, reason = "command result is the run list")]
        {
            for record in records {
                println!(
                    "{} {} {} {}",
                    record.run_id,
                    status_label(record.status),
                    record.kind,
                    record.prompt
                );
            }
        }
    }
    Ok(())
}

fn permission_mode(args: &RunArgs) -> Result<PermissionMode> {
    if args.ask && args.yolo {
        bail!("choose at most one of --ask and --yolo");
    }
    Ok(if args.yolo {
        PermissionMode::Yolo
    } else if args.ask {
        PermissionMode::Ask
    } else {
        PermissionMode::Auto
    })
}

fn resolve_run_adapter(
    requested: Option<&str>,
    mode: PermissionMode,
    prompt: &str,
) -> Result<(&'static dyn AgentAdapter, Vec<String>)> {
    if let Some(kind) = requested {
        let adapter = rimz::agents::find_adapter(kind)
            .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{kind}`"))?;
        preflight_agent(adapter)?;
        let permission_args = adapter.permission_args(mode);
        preflight_program(adapter, &permission_args, prompt)?;
        return Ok((adapter, permission_args));
    }

    for adapter in rimz::agents::ADAPTERS {
        let permission_args = adapter.permission_args(mode);
        if default_run_adapter_ready(*adapter, &permission_args, prompt) {
            return Ok((*adapter, permission_args));
        }
    }

    let kinds = rimz::agents::known_kinds().collect::<Vec<_>>().join(", ");
    bail!(
        "no launchable agent is ready for `rimz run`; install and trust hooks for one of ({kinds}) and ensure its binary is on PATH, or pass `--agent <kind>`"
    )
}

fn default_run_adapter_ready(
    adapter: &'static dyn AgentAdapter,
    permission_args: &[String],
    prompt: &str,
) -> bool {
    if !adapter.hooks_installed() || !adapter.untrusted_installed_hooks().is_empty() {
        return false;
    }
    let Some(argv) = adapter.launch_command(permission_args, Some(prompt)) else {
        return false;
    };
    argv.first()
        .is_some_and(|program| which::which(program).is_ok())
}

fn preflight_agent(adapter: &dyn AgentAdapter) -> Result<()> {
    let kind = adapter.descriptor().kind;
    if !adapter.hooks_installed() {
        bail!(
            "`rimz run` requires {kind} hooks so the supervised turn can report completion; run `rimz hooks install {kind}`"
        );
    }
    let untrusted = adapter.untrusted_installed_hooks();
    if !untrusted.is_empty() {
        bail!(
            "{kind} hooks are installed but not trusted ({}); {}",
            untrusted.join(", "),
            hook_trust_fix(kind)
        );
    }
    Ok(())
}

fn preflight_program(
    adapter: &dyn AgentAdapter,
    permission_args: &[String],
    prompt: &str,
) -> Result<()> {
    let argv = adapter
        .launch_command(permission_args, Some(prompt))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "agent `{}` has no launch command",
                adapter.descriptor().kind
            )
        })?;
    let Some(program) = argv.first() else {
        bail!(
            "agent `{}` produced an empty launch command",
            adapter.descriptor().kind
        );
    };
    which::which(program).with_context(|| format!("finding `{program}` on PATH"))?;
    Ok(())
}

fn run_pane_cmd(
    adapter: &dyn AgentAdapter,
    run_id: &rimz::RunId,
    cwd: &Path,
    prompt: &str,
    cleanup_worktree: bool,
    permission_args: &[String],
    self_cleanup_on_completion: bool,
) -> Result<PaneCmd> {
    let rimz_bin = std::env::current_exe().context("locating the rimz executable")?;
    let mut argv = vec![
        rimz_bin.to_string_lossy().into_owned(),
        "agents".to_owned(),
        "exec".to_owned(),
        adapter.descriptor().kind.to_owned(),
        "--run-id".to_owned(),
        run_id.to_string(),
    ];
    if self_cleanup_on_completion {
        argv.extend([
            "--exit-on-run-completion".to_owned(),
            "--close-pane-on-exit".to_owned(),
        ]);
    }
    if cleanup_worktree {
        argv.extend([
            "--worktree-path".to_owned(),
            cwd.to_string_lossy().into_owned(),
        ]);
    }
    argv.extend(["--prompt".to_owned(), prompt.to_owned()]);
    if !permission_args.is_empty() {
        argv.push("--".to_owned());
        argv.extend(permission_args.iter().cloned());
    }
    Ok(PaneCmd { argv })
}

fn wait_for_run(
    sock: std::os::unix::net::UnixDatagram,
    expected: ExpectedRunFrame,
    timeout: Option<Duration>,
) -> Result<RunWakeOutcome> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .context("creating run wait runtime")?;
    runtime
        .block_on(bridge::wait_for_run_completion_owning(
            sock, expected, timeout,
        ))
        .context("waiting for run completion")
}

fn terminal_record_after_wait(
    paths: &rimz::StatePaths,
    run_id: &rimz::RunId,
    outcome: RunWakeOutcome,
) -> Result<RunRecord> {
    match outcome {
        RunWakeOutcome::Completed(_status) => Ok(rimz::run::load(paths, run_id)?),
        RunWakeOutcome::Neutral => {
            let current = rimz::run::load(paths, run_id)?;
            if current.status.is_terminal() {
                Ok(current)
            } else {
                Ok(rimz::run::timeout(paths, run_id)?)
            }
        }
    }
}

fn close_run_pane(
    backend: &dyn rimz::mux::MuxBackend,
    ledger: &rimz::Ledger,
    session_name: &str,
    record: &RunRecord,
) {
    if let Some(pane_id) = record.pane_id.as_ref() {
        match backend.close_pane(session_name, pane_id) {
            Ok(()) => return,
            Err(err) => tracing::debug!(
                run_id = %record.run_id,
                pane = %pane_id,
                error = %err,
                "run cleanup could not close the recorded pane",
            ),
        }
    }
    let Some(agent_id) = record.agent_id.as_ref() else {
        return;
    };
    let snapshot = match ledger.snapshot_cached() {
        Ok(snapshot) => snapshot,
        Err(err) => {
            tracing::debug!(run_id = %record.run_id, error = %err, "run cleanup skipped; snapshot unavailable");
            return;
        }
    };
    let Some(pane) = snapshot
        .agents
        .into_iter()
        .find(|agent| agent.kind == record.kind && agent.agent_id == *agent_id)
        .and_then(|agent| agent.pane)
    else {
        return;
    };
    if let Err(err) = backend.close_pane(session_name, &pane.pane_id) {
        tracing::debug!(
            run_id = %record.run_id,
            pane = %pane.pane_id,
            error = %err,
            "run cleanup could not close the agent pane",
        );
    }
}

fn print_run_output(record: &RunRecord) -> Result<()> {
    if let Some(message) = record
        .last_message
        .as_deref()
        .map(str::trim)
        .filter(|message| !message.is_empty())
    {
        #[expect(clippy::print_stdout, reason = "command result is the run output")]
        {
            println!("{message}");
        }
    } else if record.status == RunStatus::Completed {
        writeln!(
            std::io::stderr().lock(),
            "rimz: run completed but no final assistant message was extracted"
        )?;
    }
    Ok(())
}

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, value)?;
    writeln!(stdout)?;
    Ok(())
}

fn status_label(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Pending => "pending",
        RunStatus::Running => "running",
        RunStatus::Completed => "completed",
        RunStatus::Failed => "failed",
        RunStatus::TimedOut => "timed_out",
    }
}

fn parse_timeout(raw: &str) -> std::result::Result<Duration, String> {
    super::parse::parse_duration_units(raw, &[("s", 1), ("m", 60), ("h", 3600), ("d", 86_400)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::bridge::{ExpectedRunFrame, WakeupFrame};
    use rimz::ids::WorkspaceId;
    use rimz::ledger::{RuntimePaths, StatePaths};
    use tokio::net::UnixDatagram;

    #[test]
    fn permission_mode_rejects_conflicting_flags() {
        let args = RunArgs {
            command: None,
            prompt: Some("hi".to_owned()),
            agent: Some("claude".to_owned()),
            worktree: None,
            ask: true,
            yolo: true,
            timeout: None,
            keep: false,
            detach: false,
        };
        assert!(permission_mode(&args).is_err());
    }

    #[test]
    fn parse_timeout_accepts_duration_units() {
        assert_eq!(parse_timeout("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_timeout("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_timeout("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_timeout("1d").unwrap(), Duration::from_secs(86_400));
    }

    #[test]
    fn completed_run_wakeup_reloads_terminal_record() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        runtime.ensure_dirs().unwrap();
        let mut record = RunRecord::new(
            workspace_id.clone(),
            AgentKind::new_unchecked("codex"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        let run_id = record.run_id.clone();
        rimz::run::create(&paths, &record).unwrap();
        let (sock, sock_path) = bridge::bind_run(&runtime, &run_id).unwrap();

        record.status = RunStatus::Completed;
        record.last_message = Some("done".to_owned());
        rimz::ledger::run_store::write(&paths.runs_dir, &record).unwrap();
        let frame = WakeupFrame::RunCompleted {
            workspace_id: workspace_id.clone(),
            run_id: run_id.clone(),
            status: RunStatus::Completed,
        };
        send_run_frame(&sock_path, &frame);

        let outcome = wait_for_run(
            sock,
            ExpectedRunFrame {
                workspace_id,
                run_id: run_id.clone(),
            },
            Some(Duration::from_secs(1)),
        )
        .unwrap();
        let loaded = terminal_record_after_wait(&paths, &run_id, outcome).unwrap();

        assert_eq!(loaded.status, RunStatus::Completed);
        assert_eq!(loaded.last_message.as_deref(), Some("done"));
        assert_eq!(loaded.status.exit_code(), 0);
    }

    #[test]
    fn neutral_run_wait_marks_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        runtime.ensure_dirs().unwrap();
        let record = RunRecord::new(
            workspace_id.clone(),
            AgentKind::new_unchecked("codex"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        let run_id = record.run_id.clone();
        rimz::run::create(&paths, &record).unwrap();
        let (sock, _sock_path) = bridge::bind_run(&runtime, &run_id).unwrap();

        let outcome = wait_for_run(
            sock,
            ExpectedRunFrame {
                workspace_id,
                run_id: run_id.clone(),
            },
            Some(Duration::from_millis(10)),
        )
        .unwrap();
        let timed_out = terminal_record_after_wait(&paths, &run_id, outcome).unwrap();

        assert_eq!(timed_out.status, RunStatus::TimedOut);
        assert_eq!(timed_out.status.exit_code(), 124);
    }

    fn send_run_frame(path: &Path, frame: &WakeupFrame) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .unwrap();
        runtime.block_on(async {
            let sender = UnixDatagram::unbound().unwrap();
            let bytes = serde_json::to_vec(frame).unwrap();
            sender.send_to(&bytes, path).await.unwrap();
        });
    }
}
