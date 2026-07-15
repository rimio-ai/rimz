use super::super::*;
use super::*;

use crate::cli::render;
use rimz::agents::transcript::TranscriptCursor;
use rimz::harness::run_wake::{self, ExpectedRunFrame, SocketGuard};
use std::io::IsTerminal as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::cli::agents_cmd) enum RunPlacement {
    Split,
    LoopZone,
    Tab,
}

/// A supervised `-p` run hosts its agent pane in a split of the current tab so
/// focus stays with the caller; it opens a new tab only when forced or when
/// there is no ambient pane to split.
pub(in crate::cli::agents_cmd) fn run_placement(
    force_new_tab: bool,
    has_ambient_pane: bool,
    loop_zone: bool,
) -> RunPlacement {
    if loop_zone && !force_new_tab {
        RunPlacement::LoopZone
    } else if force_new_tab || !has_ambient_pane {
        RunPlacement::Tab
    } else {
        RunPlacement::Split
    }
}

pub(in crate::cli::agents_cmd) fn run_print(
    args: AgentsArgs,
    globals: &GlobalFlags,
) -> Result<Option<RunRecord>> {
    let output_format = args.output_format.unwrap_or_default();
    let record = run_supervised(args, globals)?;
    let Some(record_ref) = record.as_ref() else {
        return Ok(record);
    };
    match output_format {
        OutputFormat::Text => {
            let mut stdout = render::out();
            let mut stderr = render::err();
            supervised::output::print_run_output(record_ref, &mut stdout, &mut stderr)?
        }
        OutputFormat::Json => supervised::output::print_json(record_ref)?,
        // stream-json already emitted its events as the run progressed.
        OutputFormat::StreamJson => {}
    }
    Ok(record)
}

pub(in crate::cli::agents_cmd) fn validate_supervised_output(
    args: &AgentsArgs,
    output_format: OutputFormat,
) -> Result<()> {
    if args.bg && output_format == OutputFormat::StreamJson {
        bail!("--output-format stream-json cannot be combined with --bg");
    }
    if args.retries.unwrap_or(0) > 0 && output_format == OutputFormat::StreamJson {
        bail!("--retries cannot be combined with --output-format stream-json; choose text or json");
    }
    if args.verify.is_some() && output_format == OutputFormat::StreamJson {
        bail!("--verify cannot be combined with --output-format stream-json; choose text or json");
    }
    if args.max_attempts == Some(0) {
        bail!("--max-attempts must be at least 1");
    }
    if args.max_attempts.is_some() && args.verify.is_none() {
        bail!("--max-attempts requires --verify");
    }
    Ok(())
}

struct PreparedRun {
    workspace: rimz::ResolvedWorkspace,
    machine_config: Arc<rimz::config::MachineConfig>,
    mode: PermissionMode,
    layout: LayoutSpec,
    adapter: &'static dyn AgentAdapter,
    launch: rimz::worktree::LaunchCheckout,
    store: rimz::Store,
    kind: AgentKind,
    room_channel: Option<String>,
    prompt: String,
    output_format: OutputFormat,
}

struct RunWaiter {
    sock: std::os::unix::net::UnixDatagram,
    expected: ExpectedRunFrame,
    interrupt: Arc<AtomicBool>,
    _socket_guard: SocketGuard,
    stream_cursor: Option<TranscriptCursor>,
}

impl RunWaiter {
    /// Block until the run reaches a terminal record, streaming transcript
    /// output when the run was started with a stream cursor.
    fn await_terminal(
        &mut self,
        prepared: &PreparedRun,
        room: &rimz::room::RoomContext,
        args: &AgentsArgs,
    ) -> Result<RunRecord> {
        let record = if prepared.output_format == OutputFormat::StreamJson {
            let mut stdout = std::io::stdout().lock();
            let mut sink = supervised::output::StreamSink::ndjson(&mut stdout);
            let record = run_wake::wait_until_terminal(
                self.sock.try_clone().context("cloning run stream socket")?,
                self.expected.clone(),
                prepared.store.paths(),
                args.timeout,
                &self.interrupt,
                |record| {
                    supervised::stream::emit_stream_updates(
                        &prepared.store,
                        prepared.adapter,
                        self.stream_cursor
                            .as_mut()
                            .context("stream run lost its transcript cursor")?,
                        &mut sink,
                        record,
                    )
                },
            )?;
            sink.end_status(record.status, record.last_message.as_deref())?;
            record
        } else if args.stream_text {
            let mut stdout = render::out();
            let mut gutter = render::GutterWriter::new(&mut stdout);
            let mut stderr = render::err();
            let mut sink = supervised::output::StreamSink::text(&mut gutter, &mut stderr);
            let record = run_wake::wait_until_terminal(
                self.sock.try_clone().context("cloning run stream socket")?,
                self.expected.clone(),
                prepared.store.paths(),
                args.timeout,
                &self.interrupt,
                |record| {
                    supervised::stream::emit_stream_updates(
                        &prepared.store,
                        prepared.adapter,
                        self.stream_cursor
                            .as_mut()
                            .context("stream run lost its transcript cursor")?,
                        &mut sink,
                        record,
                    )
                },
            )?;
            sink.end_status(record.status, record.last_message.as_deref())?;
            record
        } else {
            run_wake::wait_until_terminal(
                self.sock.try_clone().context("cloning run wait socket")?,
                self.expected.clone(),
                prepared.store.paths(),
                args.timeout,
                &self.interrupt,
                |_| Ok::<(), std::io::Error>(()),
            )?
        };
        Ok(record_failure_tail_before_cleanup(
            room.backend(),
            &prepared.store,
            &prepared.workspace.session_name,
            record,
        ))
    }
}

struct BlockingAttempt {
    record: RunRecord,
    waiter: RunWaiter,
}

fn open_attempt_pane(
    prepared: &PreparedRun,
    room: &rimz::room::RoomContext,
    args: &AgentsArgs,
    run_id: &rimz::RunId,
    launch_batch: &AgentLaunchBatch,
    pane: &PaneCmd,
) -> Result<()> {
    let target = own_pane_id(room.mux_name());
    let direction = rimz::mux::detect_terminal_size()
        .map(|(cols, rows)| rimz::mux::split_along_longer_edge(cols, rows))
        .unwrap_or_default();
    let tab = || -> Result<()> {
        let sidebar = room.sidebar_options(&prepared.launch.cwd, Vec::new(), None);
        room.backend()
            .open_tab(&TabOptions {
                session_name: prepared.workspace.session_name.clone(),
                title: format!("run {}", prepared.adapter.descriptor().kind),
                cwd: prepared.launch.cwd.clone(),
                panes: LayoutPanes {
                    columns: vec![LayoutColumn {
                        panes: vec![pane.clone()],
                        stacked: false,
                    }],
                },
                focus: false,
                dock_sidebar: true,
                sidebar,
            })
            .map_err(anyhow::Error::from)
    };
    let open_result = match run_placement(args.new_tab, target.is_some(), args.loop_zone) {
        RunPlacement::Split => room
            .backend()
            .split_pane(SplitPaneOptions {
                session_name: None,
                target_view_id: None,
                target_pane_id: target,
                cwd: Some(prepared.launch.cwd.to_string_lossy().into_owned()),
                command: Some(pane.argv.clone()),
                title: None,
                env: rimz::room::pane_identity_env(
                    &prepared.workspace,
                    prepared.room_channel.as_deref(),
                    args.worktree.is_none() && args.from_pr.is_none(),
                ),
                stacked: false,
                direction,
                focus: false,
            })
            .map_err(anyhow::Error::from),
        RunPlacement::LoopZone => {
            let env = rimz::room::pane_identity_env(
                &prepared.workspace,
                prepared.room_channel.as_deref(),
                args.worktree.is_none() && args.from_pr.is_none(),
            );
            match supervised::pane::split_into_loop_zone(
                room.backend(),
                &prepared.workspace,
                &prepared.launch.cwd,
                env,
                pane,
            )? {
                true => Ok(()),
                false => tab(),
            }
        }
        RunPlacement::Tab => tab(),
    };
    if let Err(err) = open_result {
        let _ = rimz::harness::run::fail(prepared.store.paths(), run_id);
        let _ = prepared.store.fail_agent_launch_batch(launch_batch);
        return Err(err).context("opening run pane");
    }
    Ok(())
}

fn prepare_supervised(args: &AgentsArgs, globals: &GlobalFlags) -> Result<PreparedRun> {
    if args.json {
        bail!("on `-p`, choose output with `--output-format json` (`--json` is for `list`)");
    }
    let output_format = args.output_format.unwrap_or_default();
    let input_format = args.input_format.unwrap_or_default();
    validate_supervised_output(args, output_format)?;
    let prompt = resolve_print_prompt(args, input_format)?;
    let workspace = supervised::resolve_run_workspace(globals)?;
    let machine_config = crate::cli::machine_config();
    let mode = supervised_permission_mode_from_flags(args.ask, args.yolo)?;
    let effective = rimz::config::effective::load(
        &machine_config.agents,
        &workspace.project_root,
        &rimz::store::paths::config_home(),
    )?;
    let mut resolved = rimz::harness::plan::resolve_launch(
        &effective,
        &machine_config.agents.commands,
        args.spec.as_deref(),
    )?;
    let preset = validate_resolved_launch_inputs(
        args,
        &effective,
        &machine_config.agents.commands,
        &resolved.layout,
    )?;
    let warnings = rimz::harness::plan::finalize_launch_layout(
        &mut resolved.layout,
        LaunchFinalizeOptions {
            permission_mode: Some(mode),
            preset: &preset,
            passthrough: &args.passthrough,
            budget: args.budget,
            max_turns: args.max_turns,
        },
    )
    .inspect_err(|err| {
        for warning in err.warnings() {
            let _ = writeln!(std::io::stderr(), "{warning}");
        }
    })?;
    for warning in &warnings {
        writeln!(std::io::stderr(), "{warning}")?;
    }
    let layout = resolved.layout;
    let agent_cells = agent_cells(&layout);
    if agent_cells.len() != 1 {
        bail!("--print requires a layout with exactly one agent cell");
    }
    if layout_cell_count(&layout) != 1 {
        bail!("--print requires a single-cell agent layout");
    }
    let agent_cell = agent_cells[0];
    let adapter = rimz::agents::find_adapter(agent_cell.kind)
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", agent_cell.kind))?;
    let launch_invocation = rimz::harness::launch::ExecInvocation {
        kind: agent_cell.kind,
        action: rimz::harness::launch::ExecAction::Launch {
            prompt: Some(&prompt),
            extra_args: &[],
        },
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: false,
        exit_on_run_completion: false,
        identity: rimz::harness::launch::ExecIdentity {
            profile: agent_cell.profile,
            mode: agent_cell.mode,
            role: agent_cell.role,
            channel: args.channel.as_deref(),
            model: agent_cell.model,
            effort: agent_cell.effort,
            budget: agent_cell.budget,
            ..rimz::harness::launch::ExecIdentity::default()
        },
    };
    let process = rimz::harness::launch::compile_agent_process(
        &workspace.project_root,
        machine_config.harness.rtk,
        &launch_invocation,
        &workspace.worktree_root,
    )?;
    supervised::preflight_agent(adapter)?;
    supervised::preflight_program(&process)?;
    let launch = rimz::worktree::resolve_launch_checkout(
        &workspace,
        &machine_config.agents.worktree,
        args.worktree.as_deref(),
        args.from_pr.as_ref(),
    )?;
    let store = crate::cli::open_store(&workspace)?;
    let kind = AgentKind::new_unchecked(adapter.descriptor().kind);
    if let Some(channel) = args.channel.as_deref() {
        crate::cli::channel::ensure_named_channel_available(&workspace, channel)?;
        rimz::channel::register(store.paths(), channel)?;
    }
    let room_channel = rimz::harness::target::resolve_room_channel(
        &workspace.project_root,
        &launch.cwd,
        None,
        args.channel.as_deref(),
    );
    Ok(PreparedRun {
        workspace,
        machine_config,
        mode,
        layout,
        adapter,
        launch,
        store,
        kind,
        room_channel,
        prompt,
        output_format,
    })
}

fn execute_attempt(
    prepared: &PreparedRun,
    room: &rimz::room::RoomContext,
    args: &AgentsArgs,
    prompt: &str,
    retry_of: Option<&rimz::RunId>,
    attempt: u32,
    retries: u32,
) -> Result<Option<BlockingAttempt>> {
    let agent_cell = agent_cells(&prepared.layout)[0];
    let permission_mode = agent_cell.mode.unwrap_or(prepared.mode);
    let mut record = RunRecord::new(
        prepared.workspace.workspace_id.clone(),
        AgentKind::new_unchecked(prepared.adapter.descriptor().kind),
        permission_mode,
        prompt.to_owned(),
        prepared.launch.cwd.clone(),
    );
    record.budget = agent_cell.budget.map(ToOwned::to_owned);
    record.retry_of = retry_of.cloned();
    record.loop_task.clone_from(&args.loop_task);
    let run_id = record.run_id.clone();
    let mut launch_requests = launch_identity_requests(
        &prepared.layout,
        args.name.as_deref(),
        prepared.launch.generated_name(),
        None,
        None,
        prepared.room_channel.as_deref(),
        Some((prompt, 0)),
    )?;
    for request in &mut launch_requests {
        if attempt > 0
            && let AgentLaunchName::Explicit(name) = &request.name
        {
            request.name = AgentLaunchName::Soft(name.clone());
        }
        request.run_id = Some(run_id.clone());
    }
    let launch_batch = prepared.store.begin_agent_launch_batch(
        &launch_requests,
        AgentLaunchScope {
            session_name: prepared.workspace.session_name.clone(),
            cwd: prepared.launch.cwd.clone(),
            worktree_name: prepared.launch.worktree_name.clone(),
            channel: prepared.room_channel.clone(),
            description: args.description.clone(),
        },
    )?;
    let launch_identity = launch_batch.single_identity()?;
    record.agent_name = Some(launch_identity.name.clone());
    let pane = supervised::run_pane_cmd(supervised::RunPaneCmdArgs {
        adapter: prepared.adapter,
        run_id: &run_id,
        agent_name: Some(&launch_identity.name),
        agent_name_explicit: launch_identity.name_explicit,
        agent_profile: agent_cell.profile,
        agent_mode: agent_cell.mode,
        agent_role: agent_cell.role,
        agent_channel: prepared.room_channel.as_deref(),
        agent_model: agent_cell.model,
        agent_effort: agent_cell.effort,
        agent_budget: agent_cell.budget,
        launch_id: Some(&launch_identity.agent_id),
        cwd: &prepared.launch.cwd,
        prompt,
        cleanup_worktree: (args.worktree.is_some() || args.from_pr.is_some()) && retries == 0,
        permission_args: agent_cell.args,
        self_cleanup_on_completion: args.bg && !args.keep,
    })?;
    let bound = if args.bg {
        None
    } else {
        Some(
            run_wake::bind_run(prepared.store.runtime_paths(), &run_id)
                .context("binding run socket")?,
        )
    };
    let interrupt = if args.bg {
        None
    } else {
        Some(supervised::install_run_interrupt_flag()?)
    };
    let socket_guard = bound
        .as_ref()
        .map(|(_sock, sock_path)| SocketGuard::new(sock_path.clone()));
    rimz::harness::run::create(prepared.store.paths(), &record).context("recording run")?;
    open_attempt_pane(prepared, room, args, &run_id, &launch_batch, &pane)?;
    if args.bg {
        #[expect(clippy::print_stdout, reason = "command result is the agent name")]
        {
            println!("{}", launch_identity.name);
        }
        return Ok(None);
    }
    let Some((sock, _sock_path)) = bound else {
        bail!("blocking run did not bind its completion socket");
    };
    let Some(interrupt) = interrupt else {
        bail!("blocking run did not install its interrupt flag");
    };
    let Some(socket_guard) = socket_guard else {
        bail!("blocking run did not guard its completion socket");
    };
    let mut waiter = RunWaiter {
        sock,
        expected: ExpectedRunFrame {
            workspace_id: prepared.workspace.workspace_id.clone(),
            run_id: run_id.clone(),
        },
        interrupt,
        _socket_guard: socket_guard,
        stream_cursor: (prepared.output_format == OutputFormat::StreamJson || args.stream_text)
            .then(|| TranscriptCursor::new(true)),
    };
    let record = waiter.await_terminal(prepared, room, args)?;
    Ok(Some(BlockingAttempt { record, waiter }))
}

fn verify_phase(
    prepared: &PreparedRun,
    room: &rimz::room::RoomContext,
    args: &AgentsArgs,
    blocking: BlockingAttempt,
) -> Result<(RunRecord, Option<anyhow::Error>, RunWaiter)> {
    let BlockingAttempt {
        mut record,
        mut waiter,
    } = blocking;
    let Some(cmd) = args.verify.as_deref() else {
        return Ok((record, None, waiter));
    };
    if record.status != RunStatus::Completed {
        return Ok((record, None, waiter));
    }
    let max_attempts = args.max_attempts.unwrap_or(3);
    let verify_timeout = args
        .timeout
        .unwrap_or(rimz::harness::schedule::runner::CHECK_DEFAULT_TIMEOUT);
    let mut verify_attempt = 1;
    let mut verify_error = None;
    while record.status == RunStatus::Completed {
        let outcome =
            match supervised::verify::run_verify(&prepared.launch.cwd, cmd, verify_timeout) {
                Ok(outcome) => outcome,
                Err(err) => {
                    verify_error = Some(err);
                    break;
                }
            };
        let detail = rimz::harness::schedule::runner::check_record(&outcome);
        let output = if outcome.passed() {
            record
                .verify
                .as_ref()
                .filter(|verify| !verify.passed)
                .map(|verify| verify.output.clone())
                .unwrap_or_default()
        } else {
            detail.output.clone()
        };
        let verify = rimz::harness::run::RunVerify {
            cmd: cmd.to_owned(),
            attempts: verify_attempt,
            passed: outcome.passed(),
            code: detail.code,
            timed_out: detail.timed_out,
            output,
        };
        if waiter.interrupt.load(Ordering::SeqCst) {
            let _reopened = rimz::harness::run::reopen_for_verify(
                prepared.store.paths(),
                &record.run_id,
                verify,
            )?;
            let (canceled, _wrote) =
                rimz::harness::run::cancel(prepared.store.paths(), &record.run_id)?;
            record = canceled;
            break;
        }
        if outcome.passed() {
            record =
                rimz::harness::run::verify_passed(prepared.store.paths(), &record.run_id, verify)?;
            break;
        }
        if verify_attempt == max_attempts {
            record =
                rimz::harness::run::verify_failed(prepared.store.paths(), &record.run_id, verify)?;
            break;
        }
        let status = supervised::output::verify_status_label(&verify);
        writeln!(
            render::err(),
            "rimz: verify `{cmd}` exited {status}; re-prompting (attempt {} of {max_attempts})",
            verify_attempt + 1,
        )?;
        let reprompt = rimz::harness::run::verify_reprompt(cmd, &status, &verify.output);
        record =
            rimz::harness::run::reopen_for_verify(prepared.store.paths(), &record.run_id, verify)?;
        if let Err(err) = supervised::verify::deliver_reprompt(
            &prepared.workspace,
            &prepared.store,
            &record,
            reprompt,
        ) {
            if let Some(failed) =
                rimz::harness::run::fail_if_nonterminal(prepared.store.paths(), &record.run_id)?
            {
                record = failed;
            }
            verify_error = Some(err);
            break;
        }
        record = waiter.await_terminal(prepared, room, args)?;
        verify_attempt += 1;
    }
    Ok((record, verify_error, waiter))
}

fn close_attempt_pane(prepared: &PreparedRun, room: &rimz::room::RoomContext, record: &RunRecord) {
    if record.status == RunStatus::Canceled {
        supervised::pane::close_stopped_run_pane_after_grace(
            room.backend(),
            &prepared.store,
            &prepared.workspace.session_name,
            record,
            supervised::pane::STOP_BACKSTOP_GRACE,
        );
    } else {
        supervised::pane::close_run_pane(
            room.backend(),
            &prepared.store,
            &prepared.workspace.session_name,
            record,
        );
    }
}

pub(in crate::cli::agents_cmd) fn run_supervised(
    args: AgentsArgs,
    globals: &GlobalFlags,
) -> Result<Option<RunRecord>> {
    let prepared = prepare_supervised(&args, globals)?;
    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    let mut room = rimz::room::RoomContext::from_resolved(
        &prepared.workspace,
        prepared.machine_config.clone(),
        mux,
        rimz::room::RoomSizing::Birth,
    )?;
    let outcome = match room.birth(rimz::room::RoomBirth::Supervised(
        rimz::room::SupervisedBirth {
            cwd: prepared.launch.cwd.clone(),
            recovery: if std::io::stdin().is_terminal() {
                rimz::room::AttendedRecovery::Reset
            } else {
                rimz::room::AttendedRecovery::RequireExplicitReset
            },
        },
    )) {
        Ok(outcome) => outcome,
        Err(err) => {
            if let Some(reset) = err.downcast_ref::<rimz::room::ResetRecoveryError>() {
                render::room::print_automatic_reset(room.session_name(), &reset.report)?;
            }
            return Err(err);
        }
    };
    if let Some(reset) = outcome.reset.as_ref() {
        render::room::print_automatic_reset(room.session_name(), reset)?;
    }
    let retries = args.retries.unwrap_or(0);
    let owns_worktree = args.worktree.is_some() || args.from_pr.is_some();
    let base_prompt = prepared.prompt.clone();
    let mut prompt = prepared.prompt.clone();
    let mut retry_of = None;
    let mut attempt = 0;
    loop {
        if let Some(reason) = rimz::harness::budget::scope_gate(
            prepared.store.runtime_paths(),
            &prepared.kind,
            &prepared.machine_config,
            jiff::Timestamp::now(),
        ) {
            crate::cli::render::report(&anyhow::anyhow!(reason));
            std::process::exit(RunStatus::BudgetExceeded.exit_code());
        }
        let Some(blocking) = execute_attempt(
            &prepared,
            &room,
            &args,
            &prompt,
            retry_of.as_ref(),
            attempt,
            retries,
        )?
        else {
            return Ok(None);
        };
        let (record, verify_error, waiter) = verify_phase(&prepared, &room, &args, blocking)?;
        if !args.keep {
            close_attempt_pane(&prepared, &room, &record);
        }
        drop(waiter);
        if let Some(err) = verify_error {
            return Err(err);
        }
        if !record.status.is_retryable() || attempt == retries {
            if retries > 0
                && owns_worktree
                && let Err(err) =
                    crate::cli::worktree::cleanup_worktree(&prepared.launch.cwd, globals, false)
            {
                let _ = writeln!(
                    render::err(),
                    "rimz: worktree cleanup did not complete: {err}"
                );
            }
            return Ok(Some(record));
        }
        let mut stderr = render::err();
        supervised::output::print_run_forensics(&record, &mut stderr)?;
        writeln!(
            stderr,
            "rimz: retrying (attempt {} of {})",
            u64::from(attempt) + 2,
            u64::from(retries) + 1,
        )?;
        prompt = rimz::harness::run::retry_prompt(&base_prompt, record.failure_tail.as_deref());
        retry_of = Some(record.run_id.clone());
        attempt += 1;
    }
}

fn record_failure_tail_before_cleanup(
    backend: &dyn rimz::mux::MuxBackend,
    store: &rimz::Store,
    session_name: &str,
    record: RunRecord,
) -> RunRecord {
    if record.status == RunStatus::Completed || record.failure_tail.is_some() {
        return record;
    }
    let Some(pane) = supervised::pane::resolve_run_pane(store, session_name, &record) else {
        return record;
    };
    let Some(tail) = supervised::pane::capture_failure_tail(backend, &pane.pane_id) else {
        return record;
    };
    match rimz::harness::run::record_failure_tail(store.paths(), &record.run_id, &tail) {
        Ok(record) => record,
        Err(err) => {
            tracing::debug!(
                run_id = %record.run_id,
                pane = %pane.pane_id,
                error = %err,
                "could not record supervised run failure pane tail",
            );
            record
        }
    }
}

/// Resolve the supervised prompt from text input or, for `--input-format
/// stream-json`, from stream-json user messages on stdin.
fn resolve_print_prompt(args: &AgentsArgs, input_format: InputFormat) -> Result<String> {
    match input_format {
        InputFormat::Text => {
            let piped = if args.stdin {
                crate::cli::send::read_stdin_prompt()?
            } else {
                crate::cli::send::warn_ignored_stdin();
                None
            };
            crate::cli::send::combine_text_prompt(args.prompt.as_deref(), piped.as_deref())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "expected a prompt for `rimz agents <spec> -p` (positional PROMPT or `--stdin`)"
                    )
                })
        }
        InputFormat::StreamJson => {
            if args.stdin {
                bail!("--input-format stream-json already reads stdin; drop --stdin");
            }
            if args.prompt.as_deref().is_some_and(|p| !p.trim().is_empty()) {
                bail!(
                    "--input-format stream-json reads the prompt from stdin; drop the positional PROMPT"
                );
            }
            let prompt = supervised::read_stream_json_prompt(std::io::stdin().lock())
                .context("reading stream-json prompt from stdin")?;
            if prompt.trim().is_empty() {
                bail!("--input-format stream-json received no user message text on stdin");
            }
            Ok(prompt)
        }
    }
}

#[derive(Clone, Copy)]
struct AgentCell<'a> {
    kind: &'a str,
    args: &'a [String],
    mode: Option<PermissionMode>,
    profile: Option<&'a str>,
    role: Option<&'a str>,
    model: Option<&'a str>,
    effort: Option<&'a str>,
    budget: Option<&'a str>,
}

fn agent_cells(layout: &LayoutSpec) -> Vec<AgentCell<'_>> {
    layout
        .agent_cells()
        .filter_map(|cell| match cell {
            Cell::Agent {
                kind,
                args,
                mode,
                profile,
                role,
                model,
                effort,
                budget,
                ..
            } => Some(AgentCell {
                kind: kind.as_str(),
                args: args.as_slice(),
                mode: *mode,
                profile: profile.as_deref(),
                role: role.as_deref(),
                model: model.as_deref(),
                effort: effort.as_deref(),
                budget: budget.as_deref(),
            }),
            Cell::Command { .. } => None,
        })
        .collect()
}

fn layout_cell_count(layout: &LayoutSpec) -> usize {
    layout.columns.iter().map(|column| column.rows.len()).sum()
}
