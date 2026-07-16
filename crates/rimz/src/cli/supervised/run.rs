use super::*;
use crate::cli::supervised;

use crate::cli::render;
use rimz::agents::transcript::TranscriptCursor;
use rimz::harness::plan::{LaunchFinalizeOptions, launch_identity_requests};
use rimz::harness::run::{
    PermissionMode, RunRecord, RunStatus, SupervisedRunOutcome, SupervisedRunRequest,
};
use rimz::harness::run_wake::{self, ExpectedRunFrame};
use rimz::harness::spec::LayoutSpec;
use rimz::ids::AgentKind;
use rimz::mux::{LayoutColumn, LayoutPanes, SplitPaneOptions, TabOptions, own_pane_id};
use rimz::store::{AgentLaunchBatch, AgentLaunchName, AgentLaunchScope};
use std::io::{IsTerminal as _, Write as _};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::cli) enum RunPlacement {
    Split,
    LoopZone,
    Tab,
}

/// A supervised `-p` run hosts its agent pane in a split of the current tab so
/// focus stays with the caller; it opens a new tab only when forced or when
/// there is no ambient pane to split.
pub(in crate::cli) fn run_placement(
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

/// Resolve and finalize the one-cell layout for a command-neutral supervised request.
pub(in crate::cli) fn prepare_supervised_launch_layout(
    request: &SupervisedRunRequest,
    workspace: &rimz::ResolvedWorkspace,
    machine_config: &rimz::config::MachineConfig,
) -> Result<LayoutSpec> {
    let effective = rimz::config::effective::load(
        &machine_config.agents,
        &workspace.project_root,
        &rimz::store::paths::config_home(),
    )?;
    let mut resolved = rimz::harness::plan::resolve_launch(
        &effective,
        &machine_config.agents.commands,
        Some(&request.spec),
    )?;
    rimz::harness::plan::reject_prompt_that_looks_like_spec(
        Some(&request.spec),
        Some(&request.prompt),
        &effective.profiles,
        &machine_config.agents.commands,
        &effective.teams,
    )?;
    rimz::harness::plan::validate_profile_prompt_files(&resolved.layout)?;
    let preset = rimz::agents::LaunchPreset {
        model: rimz::harness::plan::normalized_preset_value(request.model.as_deref()),
        effort: rimz::harness::plan::normalized_preset_value(request.effort.as_deref()),
        system_prompt_file: request.system_prompt_file.clone(),
        append_system_prompt_file: request.append_system_prompt_file.clone(),
    };
    let warnings = rimz::harness::plan::finalize_launch_layout(
        &mut resolved.layout,
        LaunchFinalizeOptions {
            permission_mode: Some(request.permission_mode),
            preset: &preset,
            passthrough: &request.passthrough,
            budget: request.budget,
            max_turns: request.max_turns,
        },
    )
    .inspect_err(|err| {
        for warning in err.warnings() {
            let _ = writeln!(std::io::stderr(), "{warning}");
        }
    })?;
    for warning in warnings {
        writeln!(std::io::stderr(), "{warning}")?;
    }
    Ok(resolved.layout)
}

pub(in crate::cli) fn run_print(
    request: SupervisedRunRequest,
    presentation: SupervisedPresentation,
    globals: &GlobalFlags,
) -> Result<Option<RunRecord>> {
    let output_format = presentation.output_format;
    let record = match run_supervised(request, presentation, globals)? {
        SupervisedRunOutcome::Record(record) => Some(*record),
        SupervisedRunOutcome::Background => None,
        SupervisedRunOutcome::BudgetExceeded { reason } => {
            render::report(&anyhow::anyhow!(reason));
            std::process::exit(RunStatus::BudgetExceeded.exit_code());
        }
    };
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
    stream_text: bool,
    provider_account_binding: Option<rimz::agents::ProviderAccountBinding>,
}

struct PresentationWaiter {
    waiter: run_wake::RunWaiter,
    stream_cursor: Option<TranscriptCursor>,
}

impl PresentationWaiter {
    /// Block until the run reaches a terminal record, streaming transcript
    /// output when the run was started with a stream cursor.
    fn await_terminal(
        &mut self,
        prepared: &PreparedRun,
        room: &rimz::room::RoomContext,
        request: &SupervisedRunRequest,
    ) -> Result<RunRecord> {
        let record = if prepared.output_format == OutputFormat::StreamJson {
            let mut stdout = std::io::stdout().lock();
            let mut sink = supervised::output::StreamSink::ndjson(&mut stdout);
            supervised::stream::stream_blocking_run(
                &self.waiter,
                &prepared.store,
                prepared.adapter,
                request.timeout,
                (
                    self.stream_cursor
                        .as_mut()
                        .context("stream run lost its transcript cursor")?,
                    &mut sink,
                ),
            )?
        } else if prepared.stream_text {
            let mut stdout = render::out();
            let mut gutter = render::GutterWriter::new(&mut stdout);
            let mut stderr = render::err();
            let mut sink = supervised::output::StreamSink::text(&mut gutter, &mut stderr);
            supervised::stream::stream_blocking_run(
                &self.waiter,
                &prepared.store,
                prepared.adapter,
                request.timeout,
                (
                    self.stream_cursor
                        .as_mut()
                        .context("stream run lost its transcript cursor")?,
                    &mut sink,
                ),
            )?
        } else {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .enable_time()
                .build()
                .context("creating run wait runtime")?;
            runtime.block_on(
                self.waiter
                    .wait_terminal(&prepared.store, request.timeout, None),
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
    waiter: PresentationWaiter,
}

fn open_attempt_pane(
    prepared: &PreparedRun,
    room: &rimz::room::RoomContext,
    request: &SupervisedRunRequest,
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
    let open_result =
        match run_placement(request.force_new_tab, target.is_some(), request.loop_zone) {
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
                        request.worktree.is_none() && request.from_pr.is_none(),
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
                    request.worktree.is_none() && request.from_pr.is_none(),
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

fn prepare_supervised(
    request: &SupervisedRunRequest,
    presentation: &SupervisedPresentation,
    globals: &GlobalFlags,
) -> Result<PreparedRun> {
    let workspace = supervised::resolve_run_workspace(globals)?;
    let machine_config = crate::cli::machine_config();
    let mode = request.permission_mode;
    let layout = prepare_supervised_launch_layout(request, &workspace, &machine_config)?;
    let agent_cells = layout.agent_cells().collect::<Vec<_>>();
    if agent_cells.len() != 1 {
        bail!("--print requires a layout with exactly one agent cell");
    }
    if layout_cell_count(&layout) != 1 {
        bail!("--print requires a single-cell agent layout");
    }
    let agent_cell = agent_cells[0];
    let adapter = rimz::agents::find_adapter(&agent_cell.kind)
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", agent_cell.kind))?;
    let launch = rimz::worktree::resolve_launch_checkout(
        &workspace,
        &machine_config.agents.worktree,
        request.worktree.as_deref(),
        request.from_pr.as_ref(),
    )?;
    let mut preflight_launch = agent_cell.launch.clone();
    preflight_launch.channel.clone_from(&request.channel);
    let launch_invocation = rimz::harness::launch::ExecInvocation {
        kind: agent_cell.kind.as_str(),
        action: rimz::harness::launch::ExecAction::Launch {
            prompt: Some(&request.prompt),
            extra_args: &agent_cell.args,
        },
        provider_account_binding: None,
        provider_account_binding_finalized: false,
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: false,
        exit_on_run_completion: false,
        identity: rimz::harness::launch::ExecIdentity {
            params: Some(&preflight_launch),
            ..rimz::harness::launch::ExecIdentity::default()
        },
    };
    let process = rimz::harness::launch::compile_agent_process(
        &workspace.project_root,
        machine_config.harness.rtk,
        &launch_invocation,
        &launch.cwd,
    )?;
    let provider_account_binding = if request.managed_provider_binding_resolved {
        request.managed_provider_binding.clone()
    } else {
        adapter.resolve_managed_launch(
            &launch.cwd,
            &rimz::harness::launch::effective_launch_env(&process.env),
            agent_cell.launch.model.as_deref(),
            &process.provider_argv,
        )
    };
    supervised::preflight_agent(adapter)?;
    supervised::preflight_program(&process)?;
    let store = crate::cli::open_store(&workspace)?;
    let kind = AgentKind::new_unchecked(adapter.descriptor().kind);
    if let Some(channel) = request.channel.as_deref() {
        crate::cli::channel::ensure_named_channel_available(&workspace, channel)?;
        rimz::channel::register(store.paths(), channel)?;
    }
    let room_channel = rimz::harness::target::resolve_room_channel(
        &workspace.project_root,
        &launch.cwd,
        None,
        request.channel.as_deref(),
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
        prompt: request.prompt.clone(),
        output_format: presentation.output_format,
        stream_text: presentation.stream_text,
        provider_account_binding,
    })
}

fn execute_attempt(
    prepared: &PreparedRun,
    room: &rimz::room::RoomContext,
    request: &SupervisedRunRequest,
    prompt: &str,
    retry_of: Option<&rimz::RunId>,
    attempt: u32,
    retries: u32,
) -> Result<Option<BlockingAttempt>> {
    let agent_cell = prepared
        .layout
        .agent_cells()
        .next()
        .expect("prepared supervised layout has one agent cell");
    let permission_mode = agent_cell.launch.mode.unwrap_or(prepared.mode);
    let mut record = RunRecord::new(
        prepared.workspace.workspace_id.clone(),
        AgentKind::new_unchecked(prepared.adapter.descriptor().kind),
        permission_mode,
        prompt.to_owned(),
        prepared.launch.cwd.clone(),
    );
    record.budget.clone_from(&agent_cell.launch.budget);
    record.retry_of = retry_of.cloned();
    record.loop_task.clone_from(&request.loop_task);
    let run_id = record.run_id.clone();
    let mut launch_requests = launch_identity_requests(
        &prepared.layout,
        request.name.as_deref(),
        prepared.launch.generated_name(),
        None,
        None,
        prepared.room_channel.as_deref(),
        Some((prompt, 0)),
        None,
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
            description: request.description.clone(),
        },
    )?;
    let launch_identity = launch_batch.single_identity()?;
    record.agent_name = Some(launch_identity.name.clone());
    let pane = supervised::run_pane_cmd(supervised::RunPaneCmdArgs {
        adapter: prepared.adapter,
        run_id: &run_id,
        agent_name: Some(&launch_identity.name),
        agent_name_explicit: launch_identity.name_explicit,
        launch: &launch_identity.launch,
        launch_id: Some(&launch_identity.agent_id),
        cwd: &prepared.launch.cwd,
        prompt,
        cleanup_worktree: (request.worktree.is_some() || request.from_pr.is_some()) && retries == 0,
        permission_args: &agent_cell.args,
        self_cleanup_on_completion: request.background && !request.keep,
        provider_account_binding: prepared.provider_account_binding.as_ref(),
    })?;
    let waiter = if request.background {
        None
    } else {
        let cancellation = supervised::install_run_interrupt_flag()?;
        Some(
            run_wake::RunWaiter::bind(
                prepared.store.runtime_paths(),
                ExpectedRunFrame {
                    workspace_id: prepared.workspace.workspace_id.clone(),
                    run_id: run_id.clone(),
                },
                cancellation,
            )
            .context("binding run socket")?,
        )
    };
    rimz::harness::run::create(prepared.store.paths(), &record).context("recording run")?;
    open_attempt_pane(prepared, room, request, &run_id, &launch_batch, &pane)?;
    if request.background {
        #[expect(clippy::print_stdout, reason = "command result is the agent name")]
        {
            println!("{}", launch_identity.name);
        }
        return Ok(None);
    }
    let Some(waiter) = waiter else {
        bail!("blocking run did not bind its completion waiter");
    };
    let mut waiter = PresentationWaiter {
        waiter,
        stream_cursor: (prepared.output_format == OutputFormat::StreamJson || prepared.stream_text)
            .then(|| TranscriptCursor::new(true)),
    };
    let record = waiter.await_terminal(prepared, room, request)?;
    Ok(Some(BlockingAttempt { record, waiter }))
}

fn verify_phase(
    prepared: &PreparedRun,
    room: &rimz::room::RoomContext,
    request: &SupervisedRunRequest,
    blocking: BlockingAttempt,
) -> Result<(RunRecord, Option<anyhow::Error>, PresentationWaiter)> {
    let BlockingAttempt {
        mut record,
        mut waiter,
    } = blocking;
    let Some(cmd) = request.verify.as_deref() else {
        return Ok((record, None, waiter));
    };
    if record.status != RunStatus::Completed {
        return Ok((record, None, waiter));
    }
    let max_attempts = request.max_attempts.unwrap_or(3);
    let verify_timeout = request
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
        if waiter.waiter.cancellation().is_requested() {
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
        record = waiter.await_terminal(prepared, room, request)?;
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

pub(in crate::cli) fn run_supervised(
    request: SupervisedRunRequest,
    presentation: SupervisedPresentation,
    globals: &GlobalFlags,
) -> Result<SupervisedRunOutcome> {
    let prepared = prepare_supervised(&request, &presentation, globals)?;
    if let Some(binding) = prepared.provider_account_binding.as_ref()
        && let Some(reason) = rimz::agents::provider_budget_gate(
            prepared.store.runtime_paths(),
            prepared.kind.as_str(),
            binding,
            jiff::Timestamp::now(),
        )
    {
        return Ok(SupervisedRunOutcome::BudgetExceeded { reason });
    }
    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    let mut room = rimz::room::RoomContext::from_resolved(
        &prepared.workspace,
        prepared.machine_config.clone(),
        mux,
        rimz::room::RoomSizing::Birth,
    )?;
    render::room::present_birth_outcome(
        room.birth(rimz::room::RoomBirth::Supervised(
            rimz::room::SupervisedBirth {
                cwd: prepared.launch.cwd.clone(),
                recovery: if std::io::stdin().is_terminal() {
                    rimz::room::AttendedRecovery::Reset
                } else {
                    rimz::room::AttendedRecovery::RequireExplicitReset
                },
            },
        )),
        room.session_name(),
    )?;
    let retries = request.retries;
    let owns_worktree = request.worktree.is_some() || request.from_pr.is_some();
    let base_prompt = prepared.prompt.clone();
    let mut prompt = prepared.prompt.clone();
    let mut retry_of = None;
    let mut attempt = 0;
    loop {
        if let Some(binding) = prepared.provider_account_binding.as_ref()
            && let Some(reason) = rimz::agents::provider_budget_gate(
                prepared.store.runtime_paths(),
                prepared.kind.as_str(),
                binding,
                jiff::Timestamp::now(),
            )
        {
            return Ok(SupervisedRunOutcome::BudgetExceeded { reason });
        }
        if let Some(reason) = rimz::harness::budget::scope_gate(
            prepared.store.runtime_paths(),
            &prepared.kind,
            &prepared.machine_config,
            jiff::Timestamp::now(),
        ) {
            return Ok(SupervisedRunOutcome::BudgetExceeded { reason });
        }
        let Some(blocking) = execute_attempt(
            &prepared,
            &room,
            &request,
            &prompt,
            retry_of.as_ref(),
            attempt,
            retries,
        )?
        else {
            return Ok(SupervisedRunOutcome::Background);
        };
        let (record, verify_error, waiter) = verify_phase(&prepared, &room, &request, blocking)?;
        if !request.keep {
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
            return Ok(SupervisedRunOutcome::Record(Box::new(record)));
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

fn layout_cell_count(layout: &LayoutSpec) -> usize {
    layout.columns.iter().map(|column| column.rows.len()).sum()
}
