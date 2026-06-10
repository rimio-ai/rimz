//! `rimz run` — supervised one-shot agent runs.

use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use super::{GlobalFlags, RoomTarget};
use rimz::agents::{AgentAdapter, hook_trust_fix};
use rimz::bridge::{self, ExpectedRunFrame, RunWakeOutcome, SocketGuard};
use rimz::ids::{AgentKind, PaneId};
use rimz::mux::{LayoutPanes, PaneCmd, PaneListOptions, SessionOptions, TabOptions};
use rimz::run::{PermissionMode, RunLiveStatus, RunRecord, RunStatus};
use rimz::workspace::WorkspaceResolver;

const STOP_BACKSTOP_GRACE: Duration = Duration::from_secs(3);
const STOP_BACKSTOP_POLL: Duration = Duration::from_millis(250);

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
    let record = if args.stream {
        stream_blocking_run(sock, expected, &ledger, &run_id, adapter, args.timeout)?
    } else {
        let outcome = wait_for_run(sock, expected, args.timeout)?;
        terminal_record_after_wait(ledger.paths(), &run_id, outcome)?
    };

    if !args.keep {
        close_run_pane(backend.as_ref(), &ledger, &workspace.session_name, &record);
    }
    match blocking_run_output(args.json, args.stream) {
        BlockingRunOutput::Json => print_json(&record)?,
        BlockingRunOutput::FinalMessage => print_run_output(&record)?,
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
        print_json(&RunStatusReport { record, live })?;
    } else {
        #[expect(clippy::print_stdout, reason = "command result is the run status")]
        {
            println!("{}", human_status_line(&record, live.as_ref()));
        }
    }
    Ok(())
}

fn list(json: bool, globals: &GlobalFlags) -> Result<()> {
    let workspace = resolve_run_workspace(globals)?;
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

fn stop(run_id: rimz::RunId, globals: &GlobalFlags) -> Result<()> {
    let workspace = resolve_run_workspace(globals)?;
    let ledger = super::open_ledger(&workspace)?;
    let (record, wrote) = rimz::run::cancel(ledger.paths(), &run_id)?;
    if !wrote {
        writeln!(
            std::io::stderr().lock(),
            "rimz: run {} is already {}",
            record.run_id,
            status_label(record.status)
        )?;
        return Ok(());
    }
    rimz::ledger::wakeup::wake_run(ledger.runtime_paths(), &record).context("waking run waiter")?;
    if let Ok(backend) = backend_for_workspace_session(&workspace, globals) {
        close_stopped_run_pane_after_grace(
            backend.as_ref(),
            &ledger,
            &workspace.session_name,
            &record,
            STOP_BACKSTOP_GRACE,
        );
    }
    Ok(())
}

fn send(run_id: rimz::RunId, enter: bool, mut text: String, globals: &GlobalFlags) -> Result<()> {
    let workspace = resolve_run_workspace(globals)?;
    let ledger = super::open_ledger(&workspace)?;
    let record = rimz::run::load(ledger.paths(), &run_id)?;
    ensure_sendable(&record)?;
    let pane = resolve_run_pane(&ledger, &workspace.session_name, &record).with_context(|| {
        format!(
            "run {} has no resolvable pane yet; retry in a moment",
            run_id
        )
    })?;
    if enter {
        text.push('\r');
    }
    let backend = backend_for_workspace_session(&workspace, globals)?;
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
    match stream_attached_run(&ledger, &run_id, adapter, from_start, timeout)? {
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

fn stream_blocking_run(
    sock: std::os::unix::net::UnixDatagram,
    expected: ExpectedRunFrame,
    ledger: &rimz::Ledger,
    run_id: &rimz::RunId,
    adapter: &dyn AgentAdapter,
    timeout: Option<Duration>,
) -> Result<RunRecord> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .context("creating run stream runtime")?;
    runtime.block_on(async {
        let sock = bridge::adopt(sock).context("adopting run socket")?;
        let deadline = timeout.map(|duration| Instant::now() + duration);
        let mut cursor = TranscriptCursor::new(true);
        let mut last_live = None;
        loop {
            let record = rimz::run::load(ledger.paths(), run_id)?;
            emit_stream_updates(ledger, adapter, &mut cursor, &mut last_live, &record)?;
            if record.status.is_terminal() {
                emit_stream_end(&record)?;
                return Ok(record);
            }
            let Some(wait) = next_stream_wait(deadline) else {
                let timed_out = rimz::run::timeout(ledger.paths(), run_id)?;
                emit_stream_updates(ledger, adapter, &mut cursor, &mut last_live, &timed_out)?;
                emit_stream_end(&timed_out)?;
                return Ok(timed_out);
            };
            match bridge::wait_for_run_completion(&sock, &expected, Some(wait))
                .await
                .context("waiting for run stream tick")?
            {
                RunWakeOutcome::Completed(_status) => {
                    let record = rimz::run::load(ledger.paths(), run_id)?;
                    emit_stream_updates(ledger, adapter, &mut cursor, &mut last_live, &record)?;
                    emit_stream_end(&record)?;
                    return Ok(record);
                }
                RunWakeOutcome::Neutral => {}
            }
        }
    })
}

fn stream_attached_run(
    ledger: &rimz::Ledger,
    run_id: &rimz::RunId,
    adapter: &dyn AgentAdapter,
    from_start: bool,
    timeout: Option<Duration>,
) -> Result<Option<RunRecord>> {
    let mut cursor = TranscriptCursor::new(from_start);
    let mut last_live = None;
    let deadline = timeout.map(|duration| Instant::now() + duration);
    loop {
        let record = rimz::run::load(ledger.paths(), run_id)?;
        emit_stream_updates(ledger, adapter, &mut cursor, &mut last_live, &record)?;
        if record.status.is_terminal() {
            emit_stream_end(&record)?;
            return Ok(Some(record));
        }
        if reached_deadline(deadline) {
            return Ok(None);
        }
        std::thread::sleep(next_attached_stream_sleep(deadline));
    }
}

fn reached_deadline(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| Instant::now() >= deadline)
}

fn next_attached_stream_sleep(deadline: Option<Instant>) -> Duration {
    const ATTACHED_STREAM_TICK: Duration = Duration::from_millis(500);
    let Some(deadline) = deadline else {
        return ATTACHED_STREAM_TICK;
    };
    let now = Instant::now();
    if now >= deadline {
        Duration::ZERO
    } else {
        (deadline - now).min(ATTACHED_STREAM_TICK)
    }
}

fn next_stream_wait(deadline: Option<Instant>) -> Option<Duration> {
    const STREAM_TICK: Duration = Duration::from_secs(1);
    let deadline = match deadline {
        Some(deadline) => deadline,
        None => return Some(STREAM_TICK),
    };
    let now = Instant::now();
    if now >= deadline {
        return None;
    }
    Some((deadline - now).min(STREAM_TICK))
}

#[derive(Debug)]
struct TranscriptCursor {
    path: Option<String>,
    offset: u64,
    skip_existing_on_first_path: bool,
}

impl TranscriptCursor {
    fn new(from_start: bool) -> Self {
        Self {
            path: None,
            offset: 0,
            skip_existing_on_first_path: !from_start,
        }
    }

    fn messages(&mut self, record: &RunRecord, adapter: &dyn AgentAdapter) -> Vec<String> {
        let Some(path) = record.transcript_path.as_deref() else {
            return Vec::new();
        };
        if self.path.as_deref() != Some(path) {
            self.offset = if self.skip_existing_on_first_path {
                std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
            } else {
                0
            };
            self.path = Some(path.to_owned());
            self.skip_existing_on_first_path = false;
        }
        if std::fs::metadata(path)
            .map(|meta| meta.len() < self.offset)
            .unwrap_or(false)
        {
            self.offset = 0;
        }
        let Some((bytes, next)) = rimz::agents::read_transcript_lines(Path::new(path), self.offset)
        else {
            return Vec::new();
        };
        self.offset = next;
        let text = String::from_utf8_lossy(&bytes);
        adapter.stream_assistant_messages(&text)
    }
}

fn emit_stream_updates(
    ledger: &rimz::Ledger,
    adapter: &dyn AgentAdapter,
    cursor: &mut TranscriptCursor,
    last_live: &mut Option<RunLiveStatus>,
    record: &RunRecord,
) -> Result<()> {
    for text in cursor.messages(record, adapter) {
        emit_ndjson(&RunStreamEvent::Message { text })?;
    }
    if let Some(live) = ledger
        .snapshot_cached()
        .ok()
        .and_then(|snapshot| rimz::run::live_status(record, &snapshot))
        && last_live.as_ref() != Some(&live)
    {
        emit_ndjson(&RunStreamEvent::Status { live: live.clone() })?;
        *last_live = Some(live);
    }
    Ok(())
}

fn emit_stream_end(record: &RunRecord) -> Result<()> {
    emit_ndjson(&RunStreamEvent::End {
        status: record.status,
        last_message: record.last_message.clone(),
    })
}

fn emit_ndjson(value: &impl serde::Serialize) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, value)?;
    writeln!(stdout)?;
    stdout.flush()?;
    Ok(())
}

#[derive(Debug, PartialEq, serde::Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum RunStreamEvent {
    Message {
        text: String,
    },
    Status {
        #[serde(flatten)]
        live: RunLiveStatus,
    },
    End {
        status: RunStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        last_message: Option<String>,
    },
}

fn backend_for_workspace_session(
    workspace: &rimz::ResolvedWorkspace,
    globals: &GlobalFlags,
) -> Result<Box<dyn rimz::mux::MuxBackend>> {
    let mux = super::pick_mux_for_session(
        &workspace.session_name,
        globals.mux,
        super::MissingSessionReport::Silent,
    )?;
    Ok(rimz::mux::backend_for(mux))
}

pub(super) fn close_run_pane(
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
    let Some(pane) = resolve_run_pane_from_snapshot(ledger, session_name, record) else {
        return;
    };
    if let Err(err) = backend.close_pane(&pane.session_name, &pane.pane_id) {
        tracing::debug!(
            run_id = %record.run_id,
            pane = %pane.pane_id,
            error = %err,
            "run cleanup could not close the agent pane",
        );
    }
}

fn close_stopped_run_pane_after_grace(
    backend: &dyn rimz::mux::MuxBackend,
    ledger: &rimz::Ledger,
    session_name: &str,
    record: &RunRecord,
    grace: Duration,
) {
    let deadline = Instant::now() + grace;
    loop {
        let Some((latest, pane)) = latest_resolved_run_pane(ledger, session_name, record) else {
            if Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(STOP_BACKSTOP_POLL);
            continue;
        };
        match backend.list_panes(PaneListOptions {
            session_name: Some(pane.session_name.clone()),
            command_timeout: Some(STOP_BACKSTOP_POLL),
            ..Default::default()
        }) {
            Ok(panes)
                if panes
                    .iter()
                    .any(|candidate| candidate.pane_id == pane.pane_id) =>
            {
                if Instant::now() >= deadline {
                    close_run_pane(backend, ledger, session_name, &latest);
                    return;
                }
            }
            Ok(_) => return,
            Err(err) => {
                tracing::debug!(
                    run_id = %record.run_id,
                    error = %err,
                    "run stop backstop skipped; pane list unavailable",
                );
                return;
            }
        }
        std::thread::sleep(STOP_BACKSTOP_POLL);
    }
}

fn latest_resolved_run_pane(
    ledger: &rimz::Ledger,
    session_name: &str,
    fallback: &RunRecord,
) -> Option<(RunRecord, ResolvedRunPane)> {
    let latest = latest_run_record(ledger, fallback);
    let pane = resolve_run_pane(ledger, session_name, &latest)?;
    Some((latest, pane))
}

fn latest_run_record(ledger: &rimz::Ledger, fallback: &RunRecord) -> RunRecord {
    rimz::run::load(ledger.paths(), &fallback.run_id).unwrap_or_else(|err| {
        tracing::debug!(
            run_id = %fallback.run_id,
            error = %err,
            "run stop backstop using stale record; latest record unavailable",
        );
        fallback.clone()
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResolvedRunPane {
    pane_id: PaneId,
    session_name: String,
}

fn resolve_run_pane(
    ledger: &rimz::Ledger,
    session_name: &str,
    record: &RunRecord,
) -> Option<ResolvedRunPane> {
    record
        .pane_id
        .as_ref()
        .map(|pane_id| ResolvedRunPane {
            pane_id: pane_id.clone(),
            session_name: session_name.to_owned(),
        })
        .or_else(|| resolve_run_pane_from_snapshot(ledger, session_name, record))
}

fn resolve_run_pane_from_snapshot(
    ledger: &rimz::Ledger,
    session_name: &str,
    record: &RunRecord,
) -> Option<ResolvedRunPane> {
    let snapshot = match ledger.snapshot_cached() {
        Ok(snapshot) => snapshot,
        Err(err) => {
            tracing::debug!(run_id = %record.run_id, error = %err, "run pane resolution skipped; snapshot unavailable");
            return None;
        }
    };
    resolve_run_pane_in_snapshot(&snapshot, session_name, record)
}

fn resolve_run_pane_in_snapshot(
    snapshot: &rimz::SidebarSnapshot,
    session_name: &str,
    record: &RunRecord,
) -> Option<ResolvedRunPane> {
    let agent_id = record.agent_id.as_ref()?;
    let pane = snapshot
        .agents
        .iter()
        .find(|agent| agent.kind == record.kind && agent.agent_id == *agent_id)
        .and_then(|agent| agent.pane.as_ref())?;
    Some(ResolvedRunPane {
        pane_id: pane.pane_id.clone(),
        session_name: if pane.session_name.is_empty() {
            session_name.to_owned()
        } else {
            pane.session_name.clone()
        },
    })
}

fn ensure_sendable(record: &RunRecord) -> Result<()> {
    if record.status.is_terminal() {
        bail!(
            "run {} is {}; nothing to send",
            record.run_id,
            status_label(record.status)
        );
    }
    Ok(())
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockingRunOutput {
    Json,
    FinalMessage,
    StreamAlreadyEmitted,
}

fn blocking_run_output(json: bool, stream: bool) -> BlockingRunOutput {
    if stream {
        BlockingRunOutput::StreamAlreadyEmitted
    } else if json {
        BlockingRunOutput::Json
    } else {
        BlockingRunOutput::FinalMessage
    }
}

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, value)?;
    writeln!(stdout)?;
    Ok(())
}

#[derive(serde::Serialize)]
struct RunStatusReport {
    #[serde(flatten)]
    record: RunRecord,
    #[serde(skip_serializing_if = "Option::is_none")]
    live: Option<RunLiveStatus>,
}

fn human_status_line(record: &RunRecord, live: Option<&RunLiveStatus>) -> String {
    let mut line = format!(
        "{} {} {}",
        record.run_id,
        status_label(record.status),
        record.kind
    );
    if let Some(live) = live {
        line.push_str(" (live: ");
        line.push_str(live_status_label(live).as_str());
        line.push(')');
    }
    line
}

fn live_status_label(live: &RunLiveStatus) -> String {
    if let Some(ask) = live.pending_ask.as_ref() {
        return format!(
            "{} - ask {} on {}",
            live.agent_status.as_str(),
            ask.request_id,
            ask.surface
        );
    }
    if live.phase != rimz::agents::TurnPhase::Idle {
        format!(
            "{} - {}",
            live.agent_status.as_str(),
            phase_label(live.phase)
        )
    } else {
        live.agent_status.as_str().to_owned()
    }
}

fn phase_label(phase: rimz::agents::TurnPhase) -> &'static str {
    match phase {
        rimz::agents::TurnPhase::Idle => "idle",
        rimz::agents::TurnPhase::Reasoning => "reasoning",
        rimz::agents::TurnPhase::Acting => "acting",
        rimz::agents::TurnPhase::Parked => "parked",
    }
}

fn status_label(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Pending => "pending",
        RunStatus::Running => "running",
        RunStatus::Completed => "completed",
        RunStatus::Failed => "failed",
        RunStatus::TimedOut => "timed_out",
        RunStatus::Canceled => "canceled",
    }
}

fn parse_timeout(raw: &str) -> std::result::Result<Duration, String> {
    super::parse::parse_duration_units(raw, &[("s", 1), ("m", 60), ("h", 3600), ("d", 86_400)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use rimz::bridge::{ExpectedRunFrame, WakeupFrame};
    use rimz::feed::{AgentState, AgentStatus, PaneRef};
    use rimz::ids::{AgentSessionId, MuxName, WorkspaceId};
    use rimz::ledger::{RuntimePaths, StatePaths};
    use tokio::net::UnixDatagram;

    #[derive(Debug, Parser)]
    struct RunHarness {
        #[command(flatten)]
        args: RunArgs,
    }

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
            json: false,
            stream: false,
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
    fn send_subcommand_requires_separator_and_parses_enter() {
        let run_id = rimz::RunId::new();
        let parsed = RunHarness::try_parse_from([
            "run",
            "send",
            run_id.as_str(),
            "--enter",
            "--",
            "continue",
        ])
        .expect("parse send");
        let Some(RunSubcmd::Send {
            run_id: parsed_id,
            enter,
            text,
        }) = parsed.args.command
        else {
            panic!("expected send subcommand");
        };
        assert_eq!(parsed_id, run_id);
        assert!(enter);
        assert_eq!(text, "continue");

        assert!(
            RunHarness::try_parse_from(["run", "send", run_id.as_str(), "continue"]).is_err(),
            "the free text must live after --"
        );
    }

    #[test]
    fn terminal_run_is_not_sendable() {
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let mut record = RunRecord::new(
            workspace_id,
            AgentKind::new_unchecked("codex"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        record.status = RunStatus::Canceled;

        let err = ensure_sendable(&record).expect_err("terminal run rejects sends");
        assert!(err.to_string().contains("nothing to send"));
    }

    #[test]
    fn stream_output_mode_suppresses_final_message_print() {
        assert_eq!(
            blocking_run_output(false, true),
            BlockingRunOutput::StreamAlreadyEmitted
        );
        assert_eq!(blocking_run_output(true, false), BlockingRunOutput::Json);
        assert_eq!(
            blocking_run_output(false, false),
            BlockingRunOutput::FinalMessage
        );
    }

    #[test]
    fn pane_resolution_uses_snapshot_when_record_has_no_pane() {
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let mut record = RunRecord::new(
            workspace_id.clone(),
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        record.agent_id = Some(AgentSessionId::from("sess-1"));
        let pane_id = PaneId::from_parts(MuxName::Tmux, "%9");
        let mut pane = PaneRef::from_id(pane_id.clone());
        pane.session_name = "live-session".to_owned();
        let mut agent = agent_state("claude", "sess-1", AgentStatus::Running);
        agent.pane = Some(pane);
        let snapshot = rimz::SidebarSnapshot::build_with_agents(
            workspace_id,
            Vec::new(),
            vec![agent],
            jiff::Timestamp::UNIX_EPOCH,
        );

        let resolved =
            resolve_run_pane_in_snapshot(&snapshot, "fallback-session", &record).unwrap();
        assert_eq!(resolved.pane_id, pane_id);
        assert_eq!(resolved.session_name, "live-session");
    }

    #[test]
    fn stop_backstop_uses_late_recorded_pane_id() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
        let ledger = rimz::Ledger::open(paths.clone(), runtime).unwrap();
        let mut stale = RunRecord::new(
            workspace_id,
            AgentKind::new_unchecked("codex"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        stale.status = RunStatus::Canceled;
        rimz::run::create(ledger.paths(), &stale).unwrap();
        let pane_id = PaneId::from_parts(MuxName::Tmux, "%8");
        rimz::run::record_pane(ledger.paths(), &stale.run_id, pane_id.clone()).unwrap();

        let (latest, resolved) = latest_resolved_run_pane(&ledger, "rimz-test", &stale).unwrap();
        assert_eq!(latest.pane_id.as_ref(), Some(&pane_id));
        assert_eq!(resolved.pane_id, pane_id);
        assert_eq!(resolved.session_name, "rimz-test");
    }

    #[test]
    fn stream_event_shapes_are_ndjson_ready() {
        let value = serde_json::to_value(RunStreamEvent::End {
            status: RunStatus::Canceled,
            last_message: Some("bye".to_owned()),
        })
        .unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "event": "end",
                "status": "canceled",
                "last_message": "bye"
            })
        );
    }

    #[test]
    fn blocking_stream_wakeup_reloads_terminal_record() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        runtime.ensure_dirs().unwrap();
        let ledger = rimz::Ledger::open(paths.clone(), runtime.clone()).unwrap();
        let mut record = RunRecord::new(
            workspace_id.clone(),
            AgentKind::new_unchecked("codex"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        record.status = RunStatus::Running;
        let run_id = record.run_id.clone();
        rimz::run::create(&paths, &record).unwrap();
        let (sock, sock_path) = bridge::bind_run(&runtime, &run_id).unwrap();

        record.status = RunStatus::Completed;
        record.last_message = Some("done".to_owned());
        rimz::ledger::run_store::write(&paths.runs_dir, &record).unwrap();
        send_run_frame(
            &sock_path,
            &WakeupFrame::RunCompleted {
                workspace_id: workspace_id.clone(),
                run_id: run_id.clone(),
                status: RunStatus::Completed,
            },
        );

        let loaded = stream_blocking_run(
            sock,
            ExpectedRunFrame {
                workspace_id,
                run_id: run_id.clone(),
            },
            &ledger,
            &run_id,
            &rimz::agents::CodexAdapter,
            Some(Duration::from_secs(1)),
        )
        .unwrap();

        assert_eq!(loaded.status, RunStatus::Completed);
        assert_eq!(loaded.last_message.as_deref(), Some("done"));
    }

    #[test]
    fn blocking_stream_timeout_marks_run_timed_out() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        runtime.ensure_dirs().unwrap();
        let ledger = rimz::Ledger::open(paths.clone(), runtime.clone()).unwrap();
        let mut record = RunRecord::new(
            workspace_id.clone(),
            AgentKind::new_unchecked("codex"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        record.status = RunStatus::Running;
        let run_id = record.run_id.clone();
        rimz::run::create(&paths, &record).unwrap();
        let (sock, _sock_path) = bridge::bind_run(&runtime, &run_id).unwrap();

        let timed_out = stream_blocking_run(
            sock,
            ExpectedRunFrame {
                workspace_id,
                run_id: run_id.clone(),
            },
            &ledger,
            &run_id,
            &rimz::agents::CodexAdapter,
            Some(Duration::ZERO),
        )
        .unwrap();

        assert_eq!(timed_out.status, RunStatus::TimedOut);
        assert_eq!(
            rimz::run::load(&paths, &run_id).unwrap().status,
            RunStatus::TimedOut
        );
    }

    #[test]
    fn attached_stream_timeout_does_not_mark_run_timed_out() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        runtime.ensure_dirs().unwrap();
        let ledger = rimz::Ledger::open(paths.clone(), runtime).unwrap();
        let mut record = RunRecord::new(
            workspace_id,
            AgentKind::new_unchecked("codex"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        record.status = RunStatus::Running;
        let run_id = record.run_id.clone();
        rimz::run::create(&paths, &record).unwrap();

        let outcome = stream_attached_run(
            &ledger,
            &run_id,
            &rimz::agents::CodexAdapter,
            false,
            Some(Duration::ZERO),
        )
        .unwrap();

        assert_eq!(outcome, None);
        assert_eq!(
            rimz::run::load(&paths, &run_id).unwrap().status,
            RunStatus::Running
        );
    }

    #[test]
    fn transcript_cursor_skips_existing_attach_bytes_and_resets_on_path_change() {
        use std::io::Write as _;

        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.jsonl");
        std::fs::write(
            &first,
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"old\"}}\n",
        )
        .unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let mut record = RunRecord::new(
            workspace_id,
            AgentKind::new_unchecked("codex"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        record.transcript_path = Some(first.to_string_lossy().into_owned());
        let mut cursor = TranscriptCursor::new(false);

        assert!(
            cursor
                .messages(&record, &rimz::agents::CodexAdapter)
                .is_empty(),
            "default attach starts at the current end"
        );

        std::fs::OpenOptions::new()
            .append(true)
            .open(&first)
            .unwrap()
            .write_all(
                b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"new\"}}\n",
            )
            .unwrap();
        assert_eq!(
            cursor.messages(&record, &rimz::agents::CodexAdapter),
            vec!["new"]
        );

        let second = dir.path().join("second.jsonl");
        std::fs::write(
            &second,
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"fresh\"}}\n",
        )
        .unwrap();
        record.transcript_path = Some(second.to_string_lossy().into_owned());
        assert_eq!(
            cursor.messages(&record, &rimz::agents::CodexAdapter),
            vec!["fresh"],
            "a new transcript path starts at byte zero"
        );
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

    fn agent_state(kind: &str, id: &str, status: AgentStatus) -> AgentState {
        AgentState {
            agent_id: AgentSessionId::from(id),
            kind: AgentKind::new_unchecked(kind),
            status,
            phase: rimz::agents::TurnPhase::Idle,
            pane: None,
            agent_pid: None,
            agent_process_start: None,
            runtime_owner: None,
            parent_agent_id: None,
            worktree_path: None,
            worktree_branch: None,
            task: None,
            prompt: None,
            transcript_path: None,
            recent_prompts: Vec::new(),
            model: None,
            effort: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            cache_read_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            todo_done: None,
            todo_total: None,
            context: None,
            subagent_description: None,
            subagent_started_at: None,
            turn_started_at: None,
            compacting_since: None,
            compaction_count: 0,
            last_seen: jiff::Timestamp::UNIX_EPOCH,
            last_activity: jiff::Timestamp::UNIX_EPOCH,
            registered_at: Some(jiff::Timestamp::UNIX_EPOCH),
        }
    }
}
