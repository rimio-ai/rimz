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
use rimz::workspace::WorkspaceResolver;

mod output;
mod pane;
mod stream;

#[cfg(test)]
use output::RunStreamEvent;
use output::{BlockingRunOutput, RunStatusReport, blocking_run_output};
#[cfg(test)]
use pane::{ensure_sendable, latest_resolved_run_pane, resolve_run_pane_in_snapshot};
#[cfg(test)]
use stream::{TranscriptCursor, stream_attached_run, stream_blocking_run};

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
    /// Print the terminal run record as JSON instead of the final assistant message.
    #[arg(long, conflicts_with = "detach")]
    json: bool,
    /// Stream run progress as NDJSON.
    #[arg(long, conflicts_with_all = ["detach", "json"])]
    stream: bool,
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
    /// Cancel a run and wake any blocked waiter.
    Stop { run_id: rimz::RunId },
    /// Send text to the run's pane.
    Send {
        run_id: rimz::RunId,
        #[arg(long)]
        enter: bool,
        #[arg(last = true)]
        text: String,
    },
    /// Stream one run as NDJSON until it reaches a terminal status.
    Stream {
        run_id: rimz::RunId,
        #[arg(long)]
        from_start: bool,
        /// Stop watching after this duration without changing the run record.
        #[arg(long, value_parser = parse_timeout)]
        timeout: Option<Duration>,
    },
}

pub fn run(args: RunArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        Some(RunSubcmd::Status { run_id, json }) => status(run_id, json, globals),
        Some(RunSubcmd::List { json }) => list(json, globals),
        Some(RunSubcmd::Stop { run_id }) => stop(run_id, globals),
        Some(RunSubcmd::Send {
            run_id,
            enter,
            text,
        }) => send(run_id, enter, text, globals),
        Some(RunSubcmd::Stream {
            run_id,
            from_start,
            timeout,
        }) => stream_existing(run_id, from_start, timeout, globals),
        None => run_prompt(args, globals),
    }
}

fn run_prompt(args: RunArgs, globals: &GlobalFlags) -> Result<()> {
    let mode = permission_mode(&args)?;
    let prompt = args
        .prompt
        .filter(|prompt| !prompt.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("expected a prompt, or `rimz run status|list`"))?;
    let workspace = resolve_run_workspace(globals)?;
    let (adapter, permission_args) = resolve_run_adapter(
        &workspace.project_root,
        args.agent.as_deref(),
        mode,
        &prompt,
    )?;

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
    let record = if args.stream {
        stream::stream_blocking_run(sock, expected, &ledger, &run_id, adapter, args.timeout)?
    } else {
        let outcome = wait_for_run(sock, expected, args.timeout)?;
        terminal_record_after_wait(ledger.paths(), &run_id, outcome)?
    };

    if !args.keep {
        pane::close_run_pane(backend.as_ref(), &ledger, &workspace.session_name, &record);
    }
    match blocking_run_output(args.json, args.stream) {
        BlockingRunOutput::Json => output::print_json(&record)?,
        BlockingRunOutput::FinalMessage => output::print_run_output(&record)?,
        BlockingRunOutput::StreamAlreadyEmitted => {}
    }
    drop(socket_guard);
    std::process::exit(record.status.exit_code());
}

fn resolve_run_workspace(globals: &GlobalFlags) -> Result<rimz::ResolvedWorkspace> {
    WorkspaceResolver::resolve_participant(".", globals.root.clone())
        .context("resolving current workspace")
}

fn status(run_id: rimz::RunId, json: bool, globals: &GlobalFlags) -> Result<()> {
    let workspace = resolve_run_workspace(globals)?;
    let ledger = super::open_ledger(&workspace)?;
    let record = rimz::run::load(ledger.paths(), &run_id)?;
    let live = if record.status.is_terminal() {
        None
    } else {
        ledger
            .snapshot_cached()
            .ok()
            .and_then(|snapshot| rimz::run::live_status(&record, &snapshot))
    };
    if json {
        output::print_json(&RunStatusReport { record, live })?;
    } else {
        #[expect(clippy::print_stdout, reason = "command result is the run status")]
        {
            println!("{}", output::human_status_line(&record, live.as_ref()));
        }
    }
    Ok(())
}

fn list(json: bool, globals: &GlobalFlags) -> Result<()> {
    let workspace = resolve_run_workspace(globals)?;
    let ledger = super::open_ledger(&workspace)?;
    let records = rimz::run::list(ledger.paths())?;
    if json {
        output::print_json(&records)?;
    } else {
        #[expect(clippy::print_stdout, reason = "command result is the run list")]
        {
            for record in records {
                println!(
                    "{} {} {} {}",
                    record.run_id,
                    output::status_label(record.status),
                    record.kind,
                    record.prompt
                );
            }
        }
    }
    Ok(())
}

fn stop(run_id: rimz::RunId, globals: &GlobalFlags) -> Result<()> {
    let workspace = resolve_run_workspace(globals)?;
    let ledger = super::open_ledger(&workspace)?;
    let (record, wrote) = rimz::run::cancel(ledger.paths(), &run_id)?;
    if !wrote {
        writeln!(
            std::io::stderr().lock(),
            "rimz: run {} is already {}",
            record.run_id,
            output::status_label(record.status)
        )?;
        return Ok(());
    }
    rimz::ledger::wakeup::wake_run(ledger.runtime_paths(), &record).context("waking run waiter")?;
    if let Ok(backend) = pane::backend_for_workspace_session(&workspace, globals) {
        pane::close_stopped_run_pane_after_grace(
            backend.as_ref(),
            &ledger,
            &workspace.session_name,
            &record,
            pane::STOP_BACKSTOP_GRACE,
        );
    }
    Ok(())
}

fn send(run_id: rimz::RunId, enter: bool, mut text: String, globals: &GlobalFlags) -> Result<()> {
    let workspace = resolve_run_workspace(globals)?;
    let ledger = super::open_ledger(&workspace)?;
    let record = rimz::run::load(ledger.paths(), &run_id)?;
    pane::ensure_sendable(&record)?;
    let pane =
        pane::resolve_run_pane(&ledger, &workspace.session_name, &record).with_context(|| {
            format!(
                "run {} has no resolvable pane yet; retry in a moment",
                run_id
            )
        })?;
    if enter {
        text.push('\r');
    }
    let backend = pane::backend_for_workspace_session(&workspace, globals)?;
    super::pane::send_text(backend.as_ref(), &pane.pane_id, &text)
}

fn stream_existing(
    run_id: rimz::RunId,
    from_start: bool,
    timeout: Option<Duration>,
    globals: &GlobalFlags,
) -> Result<()> {
    let workspace = resolve_run_workspace(globals)?;
    let ledger = super::open_ledger(&workspace)?;
    let record = rimz::run::load(ledger.paths(), &run_id)?;
    let adapter = rimz::agents::find_adapter(record.kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", record.kind))?;
    match stream::stream_attached_run(&ledger, &run_id, adapter, from_start, timeout)? {
        Some(record) => std::process::exit(record.status.exit_code()),
        None => std::process::exit(RunStatus::TimedOut.exit_code()),
    }
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
    project_root: &Path,
    requested: Option<&str>,
    mode: PermissionMode,
    prompt: &str,
) -> Result<(&'static dyn AgentAdapter, Vec<String>)> {
    if let Some(kind) = requested {
        let adapter = rimz::agents::find_adapter(kind)
            .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{kind}`"))?;
        let launch_env = super::agents_cmd::full_agent_launch_env(project_root, adapter, None)?;
        preflight_agent(adapter)?;
        let permission_args = adapter.permission_args(mode);
        preflight_program(adapter, &permission_args, prompt, &launch_env)?;
        return Ok((adapter, permission_args));
    }

    for adapter in rimz::agents::ADAPTERS {
        let permission_args = adapter.permission_args(mode);
        if default_run_adapter_ready(*adapter, project_root, &permission_args, prompt) {
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
    project_root: &Path,
    permission_args: &[String],
    prompt: &str,
) -> bool {
    if !adapter.hooks_installed() || !adapter.untrusted_installed_hooks().is_empty() {
        return false;
    }
    let Ok(launch_env) = super::agents_cmd::full_agent_launch_env(project_root, adapter, None)
    else {
        return false;
    };
    preflight_program(adapter, permission_args, prompt, &launch_env).is_ok()
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
    launch_env: &std::collections::BTreeMap<String, String>,
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
    let resolves = rimz::launch::program_resolves_after_shell_rc(launch_env, program)
        .with_context(|| format!("checking `{program}` after shell startup"))?;
    if !resolves {
        bail!("finding `{program}` on PATH after shell startup");
    }
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

fn parse_timeout(raw: &str) -> std::result::Result<Duration, String> {
    super::parse::parse_duration_units(raw, &[("s", 1), ("m", 60), ("h", 3600), ("d", 86_400)])
}

#[cfg(test)]
#[path = "run/tests.rs"]
mod tests;
