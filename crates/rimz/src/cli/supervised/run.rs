use super::*;
use crate::cli::supervised;

use crate::cli::render;
use rimz::agents::PermissionMode;
use rimz::agents::transcript::TranscriptCursor;
use rimz::harness::plan::launch_identity_requests;
use rimz::harness::run::{SupervisedRunOutcome, SupervisedRunRequest, VerifyStep};
use rimz::harness::run_wake::{self, ExpectedRunFrame};
use rimz::harness::spec::LayoutSpec;
use rimz::ids::AgentKind;
use rimz::mux::{
    LayoutColumn, LayoutPanes, SplitPaneOptions, SplitPlacement, SplitTarget, TabOptions,
    own_pane_id,
};
use rimz::store::run::{RunRecord, RunStatus};
use rimz::store::{
    writer::AgentLaunchBatch, writer::AgentLaunchName, writer::AgentLaunchScope,
    writer::LaunchLogin,
};
use std::borrow::Cow;
use std::io::{IsTerminal as _, Write as _};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RunPlacement {
    Split,
    SubagentZone,
    Tab,
}

/// A supervised `-p` run normally splits the current tab so focus stays with
/// the caller. Subagents use their dedicated zone; forced and out-of-pane
/// launches open a new tab.
pub(super) fn run_placement(
    force_new_tab: bool,
    has_ambient_pane: bool,
    subagent: bool,
) -> RunPlacement {
    if force_new_tab || !has_ambient_pane {
        RunPlacement::Tab
    } else if subagent {
        RunPlacement::SubagentZone
    } else {
        RunPlacement::Split
    }
}

pub(super) fn supervised_prompt<'a>(
    request: &'a SupervisedRunRequest,
    adapter: &rimz::agents::AgentDefinition,
) -> Cow<'a, str> {
    if request.subagent && adapter.append_system_text_channel().is_none() {
        Cow::Owned(format!(
            "{}\n\n{}",
            request.prompt,
            rimz::harness::launch_reminders::subagent_reminder()
        ))
    } else {
        Cow::Borrowed(&request.prompt)
    }
}

pub(super) fn check_supervised_subagent_allowed(
    request: &SupervisedRunRequest,
    caller: Option<&rimz::agents::AgentState>,
    profiles: &rimz::config::ProfilesConfig,
) -> Result<()> {
    if !request.subagent {
        return Ok(());
    }
    let Some(caller) = caller else {
        return Ok(());
    };
    rimz::harness::subagent_policy::check_allowed(
        caller,
        profiles,
        &request.spec,
        request.agent.as_deref(),
    )?;
    Ok(())
}

pub(in crate::cli) fn run_print(
    request: SupervisedRunRequest,
    presentation: SupervisedPresentation,
    globals: &GlobalFlags,
) -> Result<Option<RunRecord>> {
    let output_format = presentation.output_format;
    let report_to = request.report_to;
    let Some(outcome) = run_supervised(request, presentation, globals)? else {
        return Ok(None);
    };
    let record = match outcome {
        SupervisedRunOutcome::Record(record) => Some(*record),
        SupervisedRunOutcome::Background {
            agent_name,
            response_path,
            ..
        } => {
            writeln!(render::out(), "{agent_name}")?;
            supervised::output::write_background_receipt(
                &mut render::err(),
                &[agent_name.as_str()],
                response_path.as_deref(),
                false,
                report_to,
            )?;
            None
        }
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
            supervised::output::print_run_output(
                record_ref,
                &mut stdout,
                &mut stderr,
                render::prose::Prose::for_stdout(),
                render::prose::prose_width(0),
            )?
        }
        OutputFormat::Json => crate::cli::render::json_pretty(record_ref)?,
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
    adapter: &'static AgentDefinition,
    launch: rimz::worktree::LaunchCheckout,
    store: rimz::Store,
    kind: AgentKind,
    login: rimz::ids::LoginName,
    room_channel: Option<String>,
    prompt: String,
    output_format: OutputFormat,
    stream_text: bool,
    managed_launch: rimz::agents::ManagedLaunchState,
    ancestry: Option<rimz::harness::ancestry::LaunchAncestry>,
    /// An agent launched the run, so its receipt names the response path.
    agent_launched: bool,
    /// The launching agent's handle, whose `out/` directory receives the response.
    reader: Option<String>,
}

struct PresentationWaiter {
    waiter: run_wake::RunWaiter,
    stream_cursor: Option<TranscriptCursor>,
}

enum AttemptOutcome {
    Background {
        agent_name: String,
        run_id: rimz::RunId,
        response_path: Option<std::path::PathBuf>,
    },
    Blocking(Box<BlockingAttempt>),
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
            let mut sink = supervised::output::StreamSink::text(
                &mut gutter,
                &mut stderr,
                render::prose::Prose::for_stdout(),
                render::prose::prose_width(4),
            );
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
    let env = rimz::room::pane_identity_env(
        &prepared.workspace,
        &prepared.launch.cwd,
        prepared.room_channel.as_deref(),
        request.worktree.is_none() && request.from_pr.is_none() && request.loop_task.is_none(),
    );
    let tab_anchor = target.clone();
    let launch_identity = launch_batch.single_identity()?;
    let direction = rimz::mux::detect_terminal_size()
        .map(|(cols, rows)| rimz::mux::split_along_longer_edge(cols, rows))
        .unwrap_or_default();
    let tab = |title: String| -> Result<()> {
        let sidebar = room.sidebar_options(&prepared.launch.cwd);
        room.backend()
            .open_tab(&TabOptions {
                env: env.clone(),
                title: request
                    .loop_task
                    .as_ref()
                    .map_or(title, |task| format!("loop {task}")),
                panes: LayoutPanes {
                    columns: vec![LayoutColumn {
                        panes: vec![pane.clone()],
                        stacked: false,
                    }],
                    focused_pane: 0,
                },
                focus: false,
                dock_sidebar: true,
                after: tab_anchor.clone(),
                sidebar,
            })
            .map_err(anyhow::Error::from)
    };
    let mut subagent_zone_guard = None;
    let open_result = match run_placement(request.force_new_tab, target.is_some(), request.subagent)
    {
        RunPlacement::Split => room
            .backend()
            .split_pane(SplitPaneOptions {
                target: target.map_or(SplitTarget::Ambient, SplitTarget::Pane),
                cwd: Some(prepared.launch.cwd.to_string_lossy().into_owned()),
                command: Some(pane.argv.clone()),
                title: pane.name.clone(),
                close_on_exit: false,
                env: env.clone(),
                placement: SplitPlacement::Directional(direction),
                focus: false,
            })
            .map(|_| ())
            .map_err(anyhow::Error::from),
        RunPlacement::SubagentZone => match supervised::pane::lock_subagent_zone(&prepared.store) {
            Ok(guard) => {
                subagent_zone_guard = Some(guard);
                let sidebar = room.sidebar_options(&prepared.launch.cwd);
                match supervised::pane::split_into_subagent_zone(
                    room.backend(),
                    &prepared.store,
                    &prepared.workspace,
                    &prepared.launch.cwd,
                    env.clone(),
                    sidebar,
                    pane,
                    &launch_identity.name,
                ) {
                    supervised::pane::SubagentZoneOpen::Opened => Ok(()),
                    supervised::pane::SubagentZoneOpen::Failed(err) => Err(err.into()),
                    supervised::pane::SubagentZoneOpen::CompanionTab => {
                        let companion = supervised::pane::subagent_companion_title(&prepared.store);
                        // A failed response may follow a successful tab birth.
                        // Do not execute the same durable run in a second tab.
                        tab(companion)
                    }
                    supervised::pane::SubagentZoneOpen::RunTab => {
                        tab(format!("run {}", prepared.adapter.spec().kind))
                    }
                }
            }
            Err(err) => Err(anyhow::Error::from(err)),
        },
        RunPlacement::Tab => tab(format!("run {}", prepared.adapter.spec().kind)),
    };
    if let Err(err) = open_result {
        let err = err.context("opening run pane");
        let _ = rimz::harness::run::fail_if_nonterminal(
            prepared.store.paths(),
            run_id,
            &render::error_line(&err),
        );
        let _ = prepared.store.fail_agent_launch_batch(launch_batch);
        return Err(err);
    }
    if request.subagent {
        supervised::pane::wait_for_subagent_pane_bind(
            &prepared.store,
            &launch_identity.kind,
            &launch_identity.agent_id,
        );
    }
    drop(subagent_zone_guard);
    Ok(())
}

/// The account a supervised launch of `kind` runs under: the request's pin,
/// else a subagent's same-kind parent's account, else the room default.
pub(super) fn launch_login(
    request: &SupervisedRunRequest,
    caller: Option<&rimz::agents::AgentState>,
    kind: &AgentKind,
) -> LaunchLogin {
    if request.login != LaunchLogin::RoomDefault {
        return request.login.clone();
    }
    match caller.filter(|caller| request.subagent && &caller.kind == kind) {
        Some(caller) => LaunchLogin::Pinned(caller.login.clone().unwrap_or_default()),
        None => LaunchLogin::RoomDefault,
    }
}

fn prepare_supervised(
    request: &SupervisedRunRequest,
    presentation: &SupervisedPresentation,
    globals: &GlobalFlags,
) -> Result<Option<PreparedRun>> {
    // Loop-owned launches have no agent caller, from either env or ancestry.
    let loop_owned = request.loop_task.is_some();
    if !loop_owned {
        crate::cli::check_launch_room(globals)?;
    }
    let workspace = supervised::resolve_run_workspace(globals)?;
    let machine_config = crate::cli::machine_config();
    machine_config.agents.startup_relaunch_wait()?;
    let worktree_launch = request.worktree.is_some() || request.from_pr.is_some();
    if worktree_launch {
        crate::cli::require_worktree_config(&machine_config)?;
    }
    let mode = request.permission_mode.unwrap_or(PermissionMode::Auto);
    let store = crate::cli::open_store(&workspace)?;
    let cwd = crate::cli::resolve_launch_cwd(request.cwd.as_deref(), store.paths())?;
    // Inside a team's lane, a bare role names that team's role, exactly as it
    // does for an interactive launch: in `#forge`, `reviewer` means
    // `forge.reviewer`.
    let effective = rimz::config::effective::load(&machine_config, &workspace.project_root)?;
    let projection = store.runtime_projection(rimz::RuntimeScope::Audit)?;
    let caller_identity = if loop_owned {
        None
    } else {
        rimz::harness::ancestry::resolve_caller(&projection.agents)
    };
    let caller = caller_identity
        .as_ref()
        .map(|identity| {
            rimz::harness::ancestry::resolve_launch_caller(&projection.agents, identity)
        })
        .transpose()?;
    let ancestry = rimz::harness::ancestry::resolve_launch_ancestry(
        caller,
        request.subagent,
        machine_config.agents.max_chain_length,
    )?;
    let agent_launched = caller.is_some();
    let reader = caller.and_then(|caller| caller.name.clone());
    // The room pin keeps the store and effective config on the same project root.
    let workspace = supervised::anchor_subagent_workspace(workspace, request, caller, globals)?;
    check_supervised_subagent_allowed(request, caller, &effective.profiles)?;
    crate::cli::admit_launch_worktree_name(
        &workspace,
        &machine_config.agents.worktree,
        &projection.agents,
        request.worktree.as_deref(),
    )?;
    let lane = request.channel.clone().or_else(|| {
        if loop_owned {
            crate::cli::directory_channel(
                &workspace,
                cwd.as_deref().unwrap_or(&workspace.worktree_root),
            )
        } else {
            crate::cli::current_channel(&workspace, Some(&store)).into_name()
        }
    });
    let snapshot = lane
        .is_some()
        .then(|| store.snapshot_cached())
        .transpose()
        .context("reading agent snapshot")?;
    let lane = lane
        .as_deref()
        .zip(snapshot.as_ref())
        .and_then(|(channel, snapshot)| {
            rimz::address::channel_team(&snapshot.agents, channel).map(|team| (channel, team))
        });
    let mut warnings = Vec::new();
    let planned = rimz::harness::plan::plan_supervised_launch(
        request,
        caller,
        lane,
        rimz::config::Isolation::ambient(&rimz::agents::ambient_env()),
        &workspace,
        &machine_config,
        effective,
        |kind| launch_login(request, caller, kind),
        &mut warnings,
    );
    let printed = warnings
        .iter()
        .try_for_each(|warning| writeln!(std::io::stderr(), "{warning}"));
    let rimz::harness::plan::SupervisedLaunch {
        layout,
        team_name,
        inferred_lane,
        clamped,
    } = planned?;
    printed?;
    // The plan guarantees exactly one cell, an agent cell.
    let agent_cell = layout
        .agent_cells()
        .next()
        .expect("a supervised launch plan holds exactly one agent cell");
    if let Some(source) = &clamped {
        supervised::note_isolation_clamp(
            agent_cell
                .launch
                .profile
                .as_deref()
                .unwrap_or(&agent_cell.kind),
            source,
        )?;
    }
    let adapter = rimz::agents::find_definition(&agent_cell.kind)
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", agent_cell.kind))?;
    let isolation = rimz::config::Isolation::resolve(
        agent_cell.launch.isolation,
        agent_cell.isolation_default,
        machine_config.agents.isolation,
    );
    rimz::sandbox::preflight_launch(
        isolation,
        &agent_cell.kind,
        agent_cell.skills.is_some(),
        adapter.manual_skill(),
    )?;
    let prompt = supervised_prompt(request, adapter);
    if worktree_launch && !crate::cli::confirm_cross_repo_worktree(&workspace)? {
        return Ok(None);
    }
    let Some(launch) = crate::cli::resolve_launch_checkout(
        &workspace,
        &machine_config,
        request.worktree.as_deref(),
        request.from_pr.as_ref(),
        cwd.as_deref(),
    )?
    else {
        return Ok(None);
    };
    let mut preflight_launch = agent_cell.launch.clone();
    preflight_launch.channel.clone_from(&request.channel);
    let pins = rimz::agents::room_accounts(&store.paths().workspace_record, &machine_config)?
        .pinned_names();
    let logins =
        rimz::agents::resolve_room_accounts(&pins, &workspace.project_root, &machine_config);
    let login = launch_login(request, caller, &agent_cell.kind).resolve(
        &agent_cell.kind,
        &logins,
        &machine_config.accounts,
    )?;
    preflight_launch.login = (!login.is_default()).then(|| login.name().clone());
    login.preflight(&rimz::agents::ambient_env())?;
    let launch_invocation = rimz::harness::launch::ExecRequest {
        action: rimz::harness::launch::ExecAction::Launch {
            prompt: Some(prompt.to_string()),
            extra_args: agent_cell.args.clone(),
        },
        subagent: request.subagent,
        ..rimz::harness::launch::ExecRequest::fresh(
            agent_cell,
            rimz::harness::launch::ExecIdentity {
                params: preflight_launch,
                ..Default::default()
            },
            None,
            false,
        )
    };
    let (process, managed_launch) = rimz::harness::launch::compile_managed_agent_process(
        &workspace.project_root,
        &launch_invocation,
        &launch.cwd,
        &request.managed_launch,
        (isolation == rimz::config::Isolation::Host).then_some(store.runtime_paths()),
    )?;
    // Judge the agent's hooks in the account home it will run under before
    // probing the program or touching the multiplexer.
    supervised::preflight_agent(adapter, &launch, &login)?;
    supervised::preflight_program(adapter, &process)?;
    if !request.subagent {
        crate::cli::lsp_admission::admit(&launch.cwd, &machine_config)?;
    }
    let kind = adapter.spec().kind_id();
    if let Some(channel) = request.channel.as_deref() {
        rimz::channel::admit_launch(&workspace, channel)?;
    }
    // An inferred lane joins the exact channel it was inferred from, rather than
    // one recomputed from the caller's cwd.
    let room_channel = rimz::harness::spec::resolve_room_channel(
        &workspace.project_root,
        &launch.cwd,
        team_name.as_deref(),
        request
            .channel
            .as_deref()
            .or(inferred_lane.as_deref().filter(|_| cwd.is_none())),
    );
    Ok(Some(PreparedRun {
        workspace,
        machine_config,
        mode,
        layout,
        adapter,
        launch,
        store,
        kind,
        login: login.name().clone(),
        room_channel,
        prompt: prompt.into_owned(),
        output_format: presentation.output_format,
        stream_text: presentation.stream_text,
        managed_launch,
        ancestry,
        agent_launched,
        reader,
    }))
}

fn execute_attempt(
    prepared: &PreparedRun,
    room: &rimz::room::RoomContext,
    request: &SupervisedRunRequest,
    prompt: &str,
    retry_of: Option<&rimz::RunId>,
    attempt: u32,
    retries: u32,
) -> Result<AttemptOutcome> {
    let agent_cell = prepared
        .layout
        .agent_cells()
        .next()
        .expect("prepared supervised layout has one agent cell");
    rimz::harness::launch::compile_provider_argv(
        prepared.adapter,
        prepared.kind.as_str(),
        &rimz::harness::launch::ExecAction::Launch {
            prompt: Some(prompt.to_owned()),
            extra_args: agent_cell.args.clone(),
        },
        &prepared.launch.cwd,
    )?;
    let permission_mode = agent_cell.launch.mode.unwrap_or(prepared.mode);
    let mut record = RunRecord::new(
        prepared.workspace.workspace_id.clone(),
        prepared.adapter.spec().kind_id(),
        permission_mode,
        prompt.to_owned(),
        prepared.launch.cwd.clone(),
    );
    record.keep = request.keep;
    record.report_to = request.report_to;
    record.subagent = request.subagent;
    record.budget.clone_from(&agent_cell.launch.budget);
    record.deadline_at = request
        .timeout
        .map(|timeout| record.started_at.checked_add(timeout))
        .transpose()
        .context("computing supervised run deadline")?;
    if request.subagent {
        record.timeout = request.timeout;
        record.warn.clone_from(&request.warn);
        record.grace = request.grace.filter(|grace| !grace.is_zero());
    }
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
        prepared.ancestry.as_ref(),
    )?;
    for request in &mut launch_requests {
        request.login = LaunchLogin::Pinned(prepared.login.clone());
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
            branch: prepared.launch.branch.clone(),
            description: request.description.clone(),
        },
    )?;
    let launch_identity = launch_batch.single_identity()?;
    record.agent_name = Some(launch_identity.name.clone());
    record.reader.clone_from(&prepared.reader);
    let (close_pane_on_exit, exit_on_run_completion) =
        supervised::run_exit_policy(request.self_cleanup_on_completion && !request.keep);
    let worktree_path = (prepared.launch.owns_checkout_lifecycle() && retries == 0)
        .then(|| prepared.launch.cwd.clone());
    let exec_request = rimz::harness::launch::ExecRequest {
        kind: prepared.kind.clone(),
        action: rimz::harness::launch::ExecAction::Launch {
            prompt: Some(prompt.to_owned()),
            extra_args: agent_cell.args.clone(),
        },
        provider_account: prepared.managed_launch.binding().map_or(
            rimz::harness::launch::ProviderAccountState::Unbound,
            |binding| rimz::harness::launch::ProviderAccountState::Pending {
                binding: binding.clone(),
            },
        ),
        run_id: Some(run_id.clone()),
        exit_on_run_completion,
        subagent: request.subagent,
        loop_reminder: request.loop_reminder.clone(),
        ..rimz::harness::launch::ExecRequest::fresh(
            agent_cell,
            rimz::harness::launch::ExecIdentity {
                resume_model_override: false,
                name: Some(launch_identity.name.clone()),
                name_explicit: launch_identity.name_explicit,
                launch_id: Some(launch_identity.agent_id.to_string()),
                params: launch_identity.launch.clone(),
            },
            worktree_path,
            close_pane_on_exit,
        )
    };
    let pane = supervised::run_pane_cmd(prepared.store.runtime_paths(), &exec_request)
        .inspect_err(|_| {
            let _ = prepared.store.fail_agent_launch_batch(&launch_batch);
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
    if let Some(turn) = &request.throttle_turn {
        turn.report_launch(&prepared.workspace.workspace_id, &launch_identity.agent_id);
    }
    open_attempt_pane(prepared, room, request, &run_id, &launch_batch, &pane)?;
    rimz::harness::assist_log::record_tier_fallbacks(launch_batch.identities());
    if request.background {
        return Ok(AttemptOutcome::Background {
            response_path: prepared
                .agent_launched
                .then(|| rimz::harness::run::response_path(prepared.store.paths(), &record))
                .flatten(),
            agent_name: launch_identity.name.clone(),
            run_id,
        });
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
    Ok(AttemptOutcome::Blocking(Box::new(BlockingAttempt {
        record,
        waiter,
    })))
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
    let max_attempts = request
        .max_attempts
        .unwrap_or(rimz::harness::run::VERIFY_MAX_ATTEMPTS_DEFAULT);
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
        let verify = rimz::store::run::RunVerify {
            cmd: cmd.to_owned(),
            attempts: verify_attempt,
            passed: outcome.passed(),
            code: detail.code,
            timed_out: detail.timed_out,
            output,
        };
        let status = supervised::output::verify_status_label(&verify);
        let reprompt = rimz::harness::prompt_compose::verify_reprompt(cmd, &status, &verify.output);
        record = match rimz::harness::run::settle_verify(
            prepared.store.paths(),
            &record.run_id,
            verify,
            waiter.waiter.cancellation().is_requested(),
            max_attempts,
        )? {
            VerifyStep::Settled(settled) => {
                record = settled;
                break;
            }
            VerifyStep::Reprompt(reopened) => reopened,
        };
        writeln!(
            render::err(),
            "rimz: verify `{cmd}` exited {status}; re-prompting (attempt {} of {max_attempts})",
            verify_attempt + 1,
        )?;
        if let Err(err) = supervised::verify::deliver_reprompt(
            &prepared.workspace,
            &prepared.store,
            &record,
            reprompt,
        ) {
            if let Some(failed) = rimz::harness::run::fail_if_nonterminal(
                prepared.store.paths(),
                &record.run_id,
                &render::error_line(&err),
            )? {
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
    let closed = if record.status == RunStatus::Canceled {
        supervised::pane::close_stopped_run_pane_after_grace(
            room.backend(),
            &prepared.store,
            &prepared.workspace.session_name,
            record,
            supervised::pane::STOP_BACKSTOP_GRACE,
        )
        .map_err(|open| open.to_string())
    } else {
        supervised::pane::close_run_pane(
            room.backend(),
            &prepared.store,
            &prepared.workspace.session_name,
            record,
        )
        .map_err(|err| err.to_string())
    };
    if let Err(error) = closed {
        tracing::debug!(run_id = %record.run_id, %error, "run cleanup left the pane open");
    }
}

pub(in crate::cli) fn run_supervised(
    request: SupervisedRunRequest,
    presentation: SupervisedPresentation,
    globals: &GlobalFlags,
) -> Result<Option<SupervisedRunOutcome>> {
    let Some(mut prepared) = prepare_supervised(&request, &presentation, globals)? else {
        return Ok(None);
    };
    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    let mut room = rimz::room::RoomContext::from_resolved(
        &prepared.workspace,
        prepared.machine_config.clone(),
        mux,
        rimz::room::RoomSizing::Birth,
    )?;
    let login_key = rimz::ids::LoginKey {
        kind: prepared.kind.clone(),
        name: prepared.login.clone(),
    };
    let provider_budget_gate = || {
        rimz::agents::provider_budget_gate(
            prepared.store.runtime_paths(),
            &login_key,
            prepared.managed_launch.binding()?,
            jiff::Timestamp::now(),
        )
    };
    if let Some(reason) = provider_budget_gate() {
        return Ok(Some(SupervisedRunOutcome::BudgetExceeded { reason }));
    }
    render::room::present_birth_outcome(
        room.birth(rimz::room::RoomBirth::Supervised {
            cwd: prepared.launch.cwd.clone(),
            recovery: if std::io::stdin().is_terminal() {
                rimz::room::AttendedRecovery::Reset
            } else {
                rimz::room::AttendedRecovery::RequireExplicitReset
            },
        }),
        room.session_name(),
    )?;
    prepared.workspace.session_name = room.session_name().to_owned();
    let retries = request.retries;
    let base_prompt = prepared.prompt.clone();
    let mut prompt = prepared.prompt.clone();
    let mut retry_of = None;
    let mut attempt = 0;
    loop {
        if let Some(reason) = provider_budget_gate() {
            return Ok(Some(SupervisedRunOutcome::BudgetExceeded { reason }));
        }
        if let Some(reason) = rimz::harness::budget::scope_gate(
            prepared.store.runtime_paths(),
            prepared.store.paths(),
            Some(&login_key),
            &prepared.machine_config,
            jiff::Timestamp::now(),
        ) {
            return Ok(Some(SupervisedRunOutcome::BudgetExceeded { reason }));
        }
        let attempt_outcome = execute_attempt(
            &prepared,
            &room,
            &request,
            &prompt,
            retry_of.as_ref(),
            attempt,
            retries,
        )?;
        let blocking = match attempt_outcome {
            AttemptOutcome::Background {
                agent_name,
                run_id,
                response_path,
            } => {
                return Ok(Some(SupervisedRunOutcome::Background {
                    agent_name,
                    run_id,
                    response_path,
                }));
            }
            AttemptOutcome::Blocking(blocking) => *blocking,
        };
        let (record, verify_error, waiter) = verify_phase(&prepared, &room, &request, blocking)?;
        join_presented_attempt(&prepared.store, &prepared.workspace.session_name, &record);
        if !request.keep {
            close_attempt_pane(&prepared, &room, &record);
        }
        drop(waiter);
        if let Some(err) = verify_error {
            return Err(err);
        }
        if !record.status.is_retryable() || attempt == retries {
            if retries > 0
                && prepared.launch.owns_checkout_lifecycle()
                && let Err(err) =
                    crate::cli::worktree::cleanup_worktree(&prepared.launch.cwd, globals, false)
            {
                let _ = writeln!(
                    render::err(),
                    "rimz: worktree cleanup did not complete: {err}"
                );
            }
            return Ok(Some(SupervisedRunOutcome::Record(Box::new(record))));
        }
        let mut stderr = render::err();
        supervised::output::print_run_forensics(&record, &mut stderr)?;
        writeln!(
            stderr,
            "rimz: retrying (attempt {} of {})",
            u64::from(attempt) + 2,
            u64::from(retries) + 1,
        )?;
        prompt = rimz::harness::prompt_compose::retry_prompt(
            &base_prompt,
            record.failure_tail.as_deref(),
        );
        retry_of = Some(record.run_id.clone());
        attempt += 1;
    }
}

/// A blocking attempt's result reaches its caller inline, so it must not also
/// wake a launching agent with a fleet report. Stamp it before the pane closes,
/// which is what lets the in-pane wrapper evaluate the report; a caller killed
/// before this point leaves the run to the report.
pub(super) fn join_presented_attempt(store: &rimz::Store, session_name: &str, record: &RunRecord) {
    if !record.status.is_terminal() {
        return;
    }
    if let Err(err) = rimz::harness::run::report::join_and_settle_digest(
        store,
        session_name,
        &record.run_id,
        Some(record.follow_ups + 1),
        "joined inline",
    ) {
        tracing::warn!(
            run_id = %record.run_id,
            error = %err,
            "could not mark the blocking run joined",
        );
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
    let Some(tail) =
        supervised::pane::capture_failure_tail(backend, &pane.pane_id, &pane.session_name)
    else {
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
