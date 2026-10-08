use super::*;
mod duty;
mod supervisor;
use crate::cli::{require_existing_store, worktree};
use duty::Duty;
pub(super) use duty::{SuperviseDutyRequest, run as run_supervise_duty};
use rimz::mux::winsize::WinsizeRepair;
use rimz::store::snapshot::find_agent;
use std::cell::RefCell;
use std::io::IsTerminal;
use std::os::fd::AsFd;
use supervisor::ExecOutcome;

const PARK_STRAND_POLL: Duration = Duration::from_secs(5);
const PARENT_RECEIPT_POLL: Duration = Duration::from_secs(1);
const HOOK_DRAIN_WAIT: Duration = Duration::from_secs(30);
const AGENT_ENDED_EVENT: &str = "rimz.agent-ended";
const AGENT_RESUMED_EVENT: &str = "rimz.agent-resumed";

pub(super) fn run_exec(args: ExecArgs, globals: &GlobalFlags) -> Result<()> {
    #[cfg(unix)]
    let handoff = args.supervise.clone();
    let result = run_exec_inner(args, globals);
    #[cfg(unix)]
    if let Err(error) = &result
        && let Some(path) = handoff
    {
        supervisor::fail_handoff(&path, error);
    }
    result
}

fn run_exec_inner(args: ExecArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())
        .context("resolving the agent launch workspace")?;
    let envelope = rimz::harness::launch::decode_exec_envelope(
        &args.kind,
        args.worktree_path.as_deref(),
        &args.request,
    )
    .context("decoding hidden agent exec request")?;
    let cwd = envelope
        .request()
        .worktree_path
        .as_deref()
        .map(absolute_lexical_path)
        .unwrap_or_else(|| std::env::current_dir().context("reading the agent pane cwd"))?;
    #[cfg(unix)]
    if let Some(path) = args.supervise.as_deref() {
        let exit = match supervisor::run(path, &workspace) {
            Ok(exit) => exit,
            Err(error) => {
                let _ = writeln!(crate::cli::render::err(), "rimz: {error:#}");
                install_cleanup_signal_handlers()?;
                install_interrupt_signal_handler()?;
                rimz::child_process::CleanupSignalMask::unblock()?;
                let status = supervisor::terminate_retained();
                let request = envelope.request().clone();
                let isolation = request
                    .isolation_default
                    .unwrap_or(crate::cli::machine_config().agents.isolation);
                let keep = request
                    .run_id
                    .as_ref()
                    .and_then(|id| {
                        let paths =
                            rimz::StatePaths::for_project_root(&workspace.project_root).ok()?;
                        rimz::harness::run::load(&paths, id)
                            .ok()
                            .map(|record| record.keep)
                    })
                    .unwrap_or(false);
                ParkExit {
                    identity: exec_launch_identity(&request)?,
                    entered_worktree: request.worktree_path.as_ref().map(|_| cwd.clone()),
                    request,
                    cwd,
                    isolation,
                    keep,
                    outcome: ExecOutcome {
                        status,
                        abrupt: true,
                        parent_ended: false,
                        parent_watchdog: None,
                    },
                    terminal_grace: RUN_EXIT_TERMINAL_GRACE,
                    startup_deaths: None,
                }
            }
        };
        return settle_park_exit(exit, &workspace, globals);
    }
    #[cfg(not(unix))]
    if args.supervise.is_some() {
        bail!("park image handoff is unavailable on this platform");
    }
    let mut invocation = ExecInvocationContext::new(&workspace, cwd);
    invocation.store()?;
    let run_context = run_exec_context(envelope.request(), &invocation)?;
    let mut provisional_identity = None;
    let launched = exec_launch_identity(envelope.request()).and_then(|identity| {
        provisional_identity = identity;
        launch_and_supervise(
            envelope,
            globals,
            &mut invocation,
            run_context.as_ref(),
            provisional_identity.as_ref(),
        )
    });
    // The one place a wrapper error settles its launch and its run: every exit
    // below the run context returns here, so none can forget either.
    if let Err(err) = &launched {
        mark_launch_failed_if_provisional(&invocation, provisional_identity.as_ref());
        if let Some(context) = run_context.as_ref() {
            fail_run_if_nonterminal(context, &crate::cli::render::error_line(err));
        }
    }
    launched
}

fn launch_and_supervise(
    envelope: rimz::harness::launch::ExecEnvelope,
    globals: &GlobalFlags,
    invocation: &mut ExecInvocationContext<'_>,
    run_context: Option<&RunExecContext>,
    provisional_identity: Option<&LaunchIdentity>,
) -> Result<()> {
    let workspace = invocation.workspace;
    let mut launch_warnings = Vec::new();
    let mut warn = |warning: String| {
        let _ = writeln!(crate::cli::render::err(), "rimz: {warning}");
        launch_warnings.push(warning);
    };
    let machine_config = crate::cli::machine_config();
    let effective = rimz::config::effective::load_with_roots(
        &machine_config,
        &workspace.project_root,
        &rimz::disk::paths::rimz_home(),
    );
    if let Err(err) = &effective {
        warn(err.to_string());
    }
    let kind = envelope.request().kind.clone();
    let fresh_launch = matches!(
        envelope.request().action,
        rimz::harness::launch::ExecAction::Launch { .. }
    );
    let relaunch_cap = machine_config.agents.startup_relaunches;
    let relaunch_wait = if fresh_launch {
        machine_config.agents.startup_relaunch_wait()?
    } else {
        Duration::ZERO
    };
    let room_agents = || {
        invocation
            .store()
            .and_then(|store| Ok(store.snapshot_cached()?))
            .map(|snapshot| snapshot.agents.clone())
            .map_err(|err| format!("{err:#}"))
    };
    let prepared = rimz::harness::launch_plan::prepare_exec(
        envelope,
        &invocation.cwd,
        &workspace.project_root,
        &rimz::proc::rimz_exe(),
        &machine_config,
        effective.as_ref().ok(),
        &room_agents,
        // Before the plan compiles: it resolves the skill root through these links.
        |request, login| {
            let launching: Vec<AgentSessionId> = request
                .identity
                .launch_id
                .as_deref()
                .map(AgentSessionId::from)
                .into_iter()
                .chain(exec_attach_target(request).into_iter().map(|(_, id)| id))
                .collect();
            let shared = rimz::agents::account_links::reconcile(
                login,
                &rimz::agents::ambient_env(),
                &|| rimz::room::other_live_agents_on(&login.key(), &launching).ok(),
            )?;
            Ok(shared.iter().flat_map(|shared| shared.warnings()).collect())
        },
    );
    let diag = match rimz::StatePaths::for_project_root(&workspace.project_root) {
        Ok(state) => rimz::diag::DiagSink::under(
            state.root,
            workspace.workspace_id.clone(),
            workspace.session_name.clone(),
            None,
        ),
        Err(error) => {
            tracing::debug!(%error, "diagnostic sink unavailable");
            rimz::diag::DiagSink::disabled()
        }
    };
    if let Some(failure) = prepared.model_refresh_failure {
        diag.emit(rimz::diag::record::DiagEvent::ModelCatalogRefreshFailed {
            agent_kind: kind.clone(),
            login: failure.login,
            alias: failure.alias,
            rung: match failure.rung {
                rimz::agents::capabilities::ModelAliasRung::Baked => {
                    rimz::diag::record::ModelCatalogFallback::Baked
                }
                // A fresh rung carries no refresh failure, so every other
                // failed refresh resolved on the cached catalog.
                _ => rimz::diag::record::ModelCatalogFallback::CachedCatalog,
            },
            reason: failure.reason,
        });
    }
    for warning in prepared.link_warnings {
        warn(warning);
    }
    for warning in prepared.model_warnings {
        warn(warning);
    }
    if let Some((movement, login)) = prepared.model_move {
        let _ = writeln!(
            crate::cli::render::err(),
            "rimz: {kind} alias {} now resolves to {} (was {})",
            movement.alias,
            movement.to,
            movement.from
        );
        if let Err(error) =
            rimz::harness::assist_log::try_append(&rimz::harness::assist_log::AssistRecord {
                at: jiff::Timestamp::now(),
                assist: rimz::harness::assist_log::Assist::ModelAlias {
                    kind,
                    login,
                    alias: movement.alias,
                    from: movement.from,
                    to: movement.to,
                },
            })
        {
            warn(format!("could not record model alias move: {error}"));
        }
    }
    let (plan, isolation) = prepared.outcome?;
    invocation.effective_isolation = Some(isolation);
    let request = &plan.request;
    let attach_target = exec_attach_target(request);
    let mut launch_identity = provisional_identity.cloned();
    if let Some(identity) = &mut launch_identity {
        identity
            .launch
            .model
            .clone_from(&request.identity.params.model);
    }
    for warning in &plan.warnings {
        warn(warning.to_string());
    }
    if let Some(links) = &plan.skill_links {
        for line in links.to_string().lines() {
            let _ = writeln!(crate::cli::render::err(), "rimz: {line}");
        }
    }
    if let Some(sandbox) = &plan.sandbox {
        for skipped in &sandbox.skipped {
            warn(skipped.to_string());
        }
    }
    let skill_links = rimz::harness::launch_plan::apply(&plan)?;
    if let Some((links, outcome)) = plan.skill_links.as_ref().zip(skill_links)
        && let Some(report) = links.shadowed_report(&outcome.shadowed)
    {
        warn(report);
    }
    let entered_worktree = request
        .worktree_path
        .as_deref()
        .map(enter_worktree)
        .transpose()?;
    let process = plan.process();
    if let Some(adapter) = rimz::agents::find_definition(request.kind.as_str())
        && adapter.min_version().is_some()
        && let Ok(Some(path)) = process.resolve_program_after_shell_rc()
    {
        rimz::agents::version::check_launch_version_floor(adapter, &path)?;
    }
    if let Err(error) = rimz::lsp::registry::register_lease(
        &invocation.cwd,
        request.identity.launch_id.as_deref(),
        std::process::id(),
    ) {
        tracing::debug!(%error, "language-server lease registration failed");
    }
    repair_own_launch_winsize(workspace, &diag);
    if let rimz::harness::launch::AgentProcessStage::LoginShellReentry { argv, .. } = &plan.stage {
        let (program, rest) = argv
            .split_first()
            .ok_or_else(|| anyhow::anyhow!("finalized Qwen launch produced an empty command"))?;
        exec_agent_command(program, rest, &process.env, &process.unset)?;
        return Ok(());
    }
    // A supervising wrapper owes the store an end from its first binding on,
    // so a hangup after that binding must reach the settle path. A direct exec
    // keeps the default dispositions its provider would see.
    let exec_directly = isolation != rimz::config::Isolation::Sandbox
        && should_exec_agent_directly(request, relaunch_cap);
    if !exec_directly {
        reset_cleanup_signal_flag();
        install_cleanup_signal_handlers().context("installing cleanup signal handlers")?;
        install_interrupt_signal_handler().context("installing interrupt signal handler")?;
    }
    if let Some(context) = run_context {
        record_own_run_pane(context);
    }
    if let Some(identity) = launch_identity.as_ref() {
        record_own_launch_pane(invocation, identity);
        if attach_target.is_none() {
            attach_own_launch_pane(invocation, identity);
        }
    }
    if let Some(target) = attach_target.as_ref() {
        record_own_resume_pane(
            invocation,
            target,
            request
                .identity
                .launch_id
                .as_deref()
                .map(AgentSessionId::from),
            rimz::store::runtime::current_process_owner(
                rimz::pane::RuntimeOwnerKind::Agent,
                target.1.as_str(),
            ),
            &request.identity.params,
        );
    }
    let warning_target = attach_target
        .as_ref()
        .map(|(kind, id)| (kind, id))
        .or_else(|| {
            launch_identity
                .as_ref()
                .map(|identity| (&identity.kind, &identity.agent_id))
        });
    if let Some((kind, agent_id)) = warning_target
        && let Err(error) = invocation.store().and_then(|store| {
            Ok(store.record_launch_warnings(
                kind,
                agent_id,
                request
                    .identity
                    .launch_id
                    .as_deref()
                    .map(AgentSessionId::from)
                    .as_ref(),
                &workspace.session_name,
                launch_warnings,
            )?)
        })
    {
        tracing::debug!(%error, "could not record agent launch warnings");
    }
    let (program, rest) = process.argv.split_first().ok_or_else(|| {
        anyhow::anyhow!("agent `{}` produced an empty launch command", request.kind)
    })?;
    let resume_since = jiff::Timestamp::now();
    // Stamp the resumed card started, reviving an ended one so delivery reaches it, before its
    // provider starts: a provider that forks on resume then registers its new session after the
    // stamp, and that registration on the card's pane and process ends the stamped card.
    if let Some(target) = attach_target.as_ref() {
        append_agent_lifecycle_trace(
            invocation,
            target.0.clone(),
            target.1.clone(),
            rimz::agents::LifecycleSignal::Registered,
            AGENT_RESUMED_EVENT,
            "agent resume start stamp",
        );
    }
    if exec_directly {
        if let Some(target) = attach_target.as_ref() {
            rewake_resumed(invocation, target, globals.mux, resume_since);
        }
        return exec_agent_command(program, rest, &process.env, &process.unset);
    }
    let mut command = Command::new(program);
    command.args(rest);
    command.envs(&process.env);
    for key in &process.unset {
        command.env_remove(key);
    }
    if let Some(path) = entered_worktree.as_deref() {
        command.current_dir(path);
    }
    let awaiting_reopen = run_context
        .as_ref()
        .filter(|_| request.subagent && attach_target.is_some())
        .map(|context| rimz::harness::run::load(context.store.paths(), &context.run_id))
        .transpose()
        .context("reading resumed run before spawning provider")?
        .and_then(|record| resumed_run_follow_ups(request, &record));
    let _drainer_lease = supervisor::drainer_lease(workspace, isolation)?;
    let provider_terminal = ProviderTerminal::capture();
    let spawned_at = jiff::Timestamp::now();
    let child = command
        .spawn()
        .with_context(|| format!("running {program}"))?;
    if let Some(target) = attach_target.as_ref() {
        record_own_resume_pane(
            invocation,
            target,
            request
                .identity
                .launch_id
                .as_deref()
                .map(AgentSessionId::from),
            rimz::store::runtime::process_owner(
                rimz::pane::RuntimeOwnerKind::Agent,
                target.1.as_str(),
                child.id(),
            ),
            &request.identity.params,
        );
    }
    if let Some(target) = attach_target.as_ref() {
        rewake_resumed(invocation, target, globals.mux, resume_since);
    }
    if let Some(context) = run_context {
        record_provider_process(context, child.id());
    }
    let (keep, survives_parent) = run_context
        .as_ref()
        .and_then(|context| {
            rimz::harness::run::load(context.store.paths(), &context.run_id)
                .inspect_err(|err| {
                    tracing::debug!(
                        run_id = %context.run_id,
                        error = %err,
                        "could not load supervised run policy",
                    );
                })
                .ok()
        })
        .map(|record| (record.keep, record.survives_parent()))
        .unwrap_or_default();
    let watchdog = subagent_parent_watchdog(
        request,
        run_context,
        launch_identity.as_ref(),
        survives_parent,
    );
    let exit = supervisor::park(
        ParkStartup {
            request,
            identity: launch_identity.as_ref(),
            command: &command,
            provider_pid: child.id(),
            spawned_at,
            relaunch_cap,
            relaunch_wait,
            fresh_launch,
            terminal: &provider_terminal,
            keep,
            survives_parent,
            awaiting_reopen,
            watchdog,
            entered_worktree: entered_worktree.as_deref(),
            cwd: &invocation.cwd,
            isolation,
        },
        &plan.park_state_file(std::process::id()),
        child,
        workspace,
    )?;
    settle_after_exit(
        &exit.request,
        globals,
        invocation,
        RunExitContext {
            run: run_context,
            keep: exit.keep,
            checkout: &invocation.cwd,
            terminal_grace: exit.terminal_grace,
            startup_deaths: exit.startup_deaths,
        },
        exit.identity.as_ref(),
        exit.entered_worktree.as_deref(),
        exit.outcome,
    )
}

struct ParkStartup<'a> {
    request: &'a rimz::harness::launch::ExecRequest,
    identity: Option<&'a LaunchIdentity>,
    command: &'a Command,
    provider_pid: u32,
    spawned_at: jiff::Timestamp,
    relaunch_cap: u8,
    relaunch_wait: Duration,
    fresh_launch: bool,
    terminal: &'a ProviderTerminal,
    keep: bool,
    survives_parent: bool,
    awaiting_reopen: Option<u32>,
    watchdog: Option<rimz::harness::parent_watch::WatchdogSeed>,
    entered_worktree: Option<&'a Path>,
    cwd: &'a Path,
    isolation: rimz::config::Isolation,
}

struct ParkExit {
    request: rimz::harness::launch::ExecRequest,
    identity: Option<LaunchIdentity>,
    cwd: PathBuf,
    isolation: rimz::config::Isolation,
    entered_worktree: Option<PathBuf>,
    keep: bool,
    outcome: ExecOutcome,
    terminal_grace: Duration,
    startup_deaths: Option<u32>,
}

#[cfg(unix)]
fn settle_park_exit(
    exit: ParkExit,
    workspace: &rimz::ResolvedWorkspace,
    globals: &GlobalFlags,
) -> Result<()> {
    let mut invocation = ExecInvocationContext::new(workspace, exit.cwd);
    invocation.effective_isolation = Some(exit.isolation);
    let run_context = match run_exec_context(&exit.request, &invocation) {
        Ok(context) => context,
        Err(error) => {
            supervisor::fail_run(&exit.request, &workspace.project_root, &error);
            supervisor::terminate_retained();
            return Err(error);
        }
    };
    settle_after_exit(
        &exit.request,
        globals,
        &invocation,
        RunExitContext {
            run: run_context.as_ref(),
            keep: exit.keep,
            checkout: &invocation.cwd,
            terminal_grace: exit.terminal_grace,
            startup_deaths: exit.startup_deaths,
        },
        exit.identity.as_ref(),
        exit.entered_worktree.as_deref(),
        exit.outcome,
    )
}

struct RunExitContext<'a> {
    run: Option<&'a RunExecContext>,
    keep: bool,
    checkout: &'a Path,
    /// What is left of `RUN_EXIT_TERMINAL_GRACE` for a late terminal hook.
    terminal_grace: Duration,
    /// How many provider processes died at startup, once every relaunch is spent.
    startup_deaths: Option<u32>,
}

fn settle_after_exit(
    request: &rimz::harness::launch::ExecRequest,
    globals: &GlobalFlags,
    invocation: &ExecInvocationContext<'_>,
    run_exit: RunExitContext<'_>,
    launch_identity: Option<&LaunchIdentity>,
    entered_worktree: Option<&Path>,
    outcome: ExecOutcome,
) -> ! {
    let RunExitContext {
        run,
        keep,
        checkout,
        terminal_grace,
        startup_deaths,
    } = run_exit;
    if let Err(error) = rimz::lsp::registry::release_lease(
        checkout,
        request.identity.launch_id.as_deref(),
        std::process::id(),
    ) {
        tracing::debug!(%error, "language-server lease release failed");
    }
    let ExecOutcome {
        status,
        abrupt: child_exit_abrupt,
        parent_ended,
        parent_watchdog,
    } = outcome;
    if let Some(context) = run {
        fail_run_if_child_exited_first(context, globals, terminal_grace);
    }
    if let Some(context) = run
        && !parent_ended
    {
        report_settled_child_or_log(context);
    }
    // After the failed run's pane tail is captured: the parent's reason stays
    // the provider's own last line.
    if let Some(deaths) = startup_deaths {
        let _ = writeln!(
            crate::cli::render::err(),
            "rimz: {} died at startup {deaths} times; not relaunching it again",
            request.kind
        );
    }
    let startup_failure =
        !status.success() && mark_launch_failed_if_provisional(invocation, launch_identity);

    let session_name = run
        .map(|context| context.session_name.as_str())
        .unwrap_or(&invocation.workspace.session_name);
    let abrupt = child_exit_abrupt || cleanup_signal_received();
    let linger = should_linger_subagent(request, keep, parent_ended);
    let (deliberate, ended_session) =
        stamp_own_end_if_deliberate(invocation, request, abrupt, linger, || {
            session_accepts_agent_close(globals, session_name)
        });
    if let Some((kind, agent_id)) = &ended_session {
        // The stamp above is a durable end no hook will ever report; its
        // subscriptions die with it here rather than waiting for gc. A wrapper
        // whose session has moved to another pane stamps nothing and so reaches
        // none of this (`resolve_own_agent_end_trace`).
        if let Err(err) = rimz::harness::schedule::arm::retire_session(
            &invocation.workspace.project_root,
            kind,
            agent_id,
            rimz::harness::schedule::arm::RetireScope::Session,
        ) {
            tracing::warn!(error = %err, "could not retire the exiting session's deliveries");
        }
    }
    if should_drop_to_shell(request, abrupt) {
        // The trace above stamps the agent ended; gc reclaims any worktree later.
        drop_to_shell_after_agent_exit(request, &status, startup_failure, ended_session.as_ref());
    }
    if let Some(path) = entered_worktree
        && deliberate
        && let Err(err) = cleanup_worktree_via_ondisk(path, globals, !abrupt, abrupt)
    {
        let _ = writeln!(
            std::io::stderr().lock(),
            "rimz: worktree cleanup did not complete: {err}"
        );
    }
    if linger {
        if let Some(identity) = launch_identity {
            // Provider hooks temporarily own the runtime row while the turn
            // runs. The wrapper becomes the long-lived owner once the
            // provider exits, so dead-provider reaping must not end the pane.
            attach_own_launch_pane(invocation, identity);
        }
        linger_subagent(globals, session_name, run, status, parent_watchdog);
    }
    if parent_ended || request.close_pane_on_exit {
        close_own_pane(globals, session_name);
    }
    std::process::exit(status.code().unwrap_or(1));
}

fn report_settled_child_or_log(context: &RunExecContext) {
    let run = match rimz::harness::run::load(context.store.paths(), &context.run_id) {
        Ok(run) => run,
        Err(err) => {
            tracing::warn!(
                run_id = %context.run_id,
                error = %err,
                "could not load settled subagent run for parent report",
            );
            return;
        }
    };
    match super::subagent_report::report_settled_child(&context.workspace, &context.store, &run) {
        Ok(outcome) => tracing::debug!(
            run_id = %context.run_id,
            ?outcome,
            "evaluated settled subagent parent report",
        ),
        Err(err) => tracing::warn!(
            run_id = %context.run_id,
            error = %err,
            "could not queue settled subagent parent report",
        ),
    }
}

/// Whether this exit closes the agent deliberately, and the session it then
/// stamped ended. A lingering subagent or a request that records no end
/// stamps nothing.
fn stamp_own_end_if_deliberate(
    invocation: &ExecInvocationContext<'_>,
    request: &rimz::harness::launch::ExecRequest,
    abrupt: bool,
    linger: bool,
    session_accepts_close: impl FnOnce() -> bool,
) -> (bool, Option<(AgentKind, AgentSessionId)>) {
    let deliberate = close_is_deliberate(abrupt, session_accepts_close, || {
        own_agent_pending_recovery(invocation, request)
    });
    let ended_session = (deliberate && !linger && should_record_end_trace(request))
        .then(|| record_own_agent_end_trace(invocation, request))
        .flatten();
    (deliberate, ended_session)
}

fn should_linger_subagent(
    request: &rimz::harness::launch::ExecRequest,
    keep: bool,
    parent_ended: bool,
) -> bool {
    request.subagent && keep && !parent_ended && !cleanup_signal_received()
}

fn linger_subagent(
    globals: &GlobalFlags,
    session_name: &str,
    run_context: Option<&RunExecContext>,
    status: ExitStatus,
    parent_watchdog: Option<rimz::harness::parent_watch::ParentWatch>,
) -> ! {
    let run_status = run_context
        .and_then(|context| {
            rimz::harness::run::load(context.store.paths(), &context.run_id)
                .ok()
                .map(|record| record.status.as_str())
        })
        .unwrap_or("done");
    let _ = writeln!(
        std::io::stderr().lock(),
        "rimz: subagent {run_status}; --keep holds the pane (`rimz subagents stop` closes it)"
    );
    loop {
        if cleanup_signal_received() {
            std::process::exit(status.code().unwrap_or(1));
        }
        if parent_watchdog
            .as_ref()
            .is_some_and(|watchdog| watchdog.parent_ended())
        {
            close_own_pane(globals, session_name);
            std::process::exit(status.code().unwrap_or(1));
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn announce_startup_relaunch(
    request: &rimz::harness::launch::ExecRequest,
    exit_code: Option<i32>,
    wait: Duration,
    relaunches: u8,
    cap: u8,
) {
    let _ = writeln!(
        crate::cli::render::err(),
        "rimz: {} exited {} before its session opened; relaunching in {} ({} of {cap}, Ctrl-C cancels)",
        request.kind,
        exit_code.map_or_else(
            || "on a signal".to_owned(),
            |code| format!("with code {code}")
        ),
        rimz::utils::time::format_duration_compact(wait),
        relaunches + 1,
    );
}

/// The terminal modes the provider started from, when stdin is a terminal.
struct ProviderTerminal {
    #[cfg(unix)]
    saved: Option<nix::sys::termios::Termios>,
}

impl ProviderTerminal {
    #[cfg(unix)]
    fn capture() -> Self {
        use std::io::IsTerminal as _;

        let stdin = std::io::stdin();
        let saved = stdin
            .is_terminal()
            .then(|| nix::sys::termios::tcgetattr(&stdin))
            .transpose()
            .inspect_err(|err| tracing::debug!(error = %err, "provider terminal snapshot failed"))
            .ok()
            .flatten();
        Self { saved }
    }

    #[cfg(not(unix))]
    fn capture() -> Self {
        Self {}
    }

    #[cfg(unix)]
    fn restore(&self) {
        let Some(saved) = &self.saved else {
            return;
        };
        if let Err(err) = nix::sys::termios::tcsetattr(
            std::io::stdin(),
            nix::sys::termios::SetArg::TCSANOW,
            saved,
        ) {
            tracing::debug!(error = %err, "provider terminal restore failed");
        }
    }

    #[cfg(not(unix))]
    fn restore(&self) {}
}

fn should_exec_agent_directly(
    request: &rimz::harness::launch::ExecRequest,
    relaunch_cap: u8,
) -> bool {
    let relaunchable = relaunch_cap > 0
        && matches!(
            request.action,
            rimz::harness::launch::ExecAction::Launch { .. }
        );
    cfg!(unix)
        && !relaunchable
        && request.run_id.is_none()
        && request.worktree_path.is_none()
        && !request.exit_on_run_completion
        && !request.close_pane_on_exit
}

fn should_record_end_trace(request: &rimz::harness::launch::ExecRequest) -> bool {
    !request.exit_on_run_completion || request.subagent
}

fn should_drop_to_shell(request: &rimz::harness::launch::ExecRequest, abrupt: bool) -> bool {
    (request.close_pane_on_exit || request.worktree_path.is_some())
        && request.run_id.is_none()
        && !abrupt
}

fn relaunch_command(request: &rimz::harness::launch::ExecRequest) -> String {
    format!(
        "rimz agents {}",
        rimz::harness::resume::relaunch_spec(
            request.identity.params.team.as_deref(),
            request.identity.params.role.as_deref(),
            request.identity.params.profile.as_deref(),
            request.kind.as_str(),
        )
    )
}

/// Whether the pane's ended session is one `--resume` can redeem: a real
/// provider session id whose adapter compiles a resume command for this
/// directory. The hint then teaches resume; anything else relaunches fresh.
fn exited_session_resumable(ended: Option<&(AgentKind, AgentSessionId)>, cwd: &Path) -> bool {
    ended.is_some_and(|(kind, agent_id)| {
        !agent_id.is_provisional()
            && rimz::agents::find_definition(kind.as_str())
                .is_some_and(|adapter| adapter.resume_command(agent_id, cwd).is_some())
    })
}

fn exit_hint(
    kind: &str,
    status: &ExitStatus,
    startup_failure: bool,
    relaunch: &str,
    resumable: bool,
    worktree: Option<&Path>,
) -> String {
    let mut hint = if startup_failure {
        format!("rimz: agent `{kind}` failed to start ({status}); relaunch with `{relaunch}`\r\n")
    } else if resumable {
        format!("rimz: agent `{kind}` exited ({status}); resume with `{relaunch} --resume`\r\n")
    } else {
        format!("rimz: agent `{kind}` exited ({status}); relaunch with `{relaunch}`\r\n")
    };
    if let Some(path) = worktree {
        let path = crate::cli::render::home_relative_path(path);
        hint.push_str(&format!(
            "rimz: worktree {path} kept; `rimz worktree sweep` reclaims it once its work lands\r\n"
        ));
    }
    hint
}

#[cfg(unix)]
fn drop_to_shell_after_agent_exit(
    request: &rimz::harness::launch::ExecRequest,
    status: &ExitStatus,
    startup_failure: bool,
    ended: Option<&(AgentKind, AgentSessionId)>,
) {
    use std::os::unix::process::CommandExt;

    let resumable = std::env::current_dir().is_ok_and(|cwd| exited_session_resumable(ended, &cwd));
    let hint = exit_hint(
        request.kind.as_str(),
        status,
        startup_failure,
        &relaunch_command(request),
        resumable,
        request.worktree_path.as_deref(),
    );
    let _ = write!(std::io::stderr().lock(), "{hint}");
    let shell = rimz::proc::user_shell_program();
    let err = Command::new(&shell).exec();
    tracing::debug!(shell = %shell, error = %err, "could not exec idle shell after agent exit");
}

#[cfg(not(unix))]
fn drop_to_shell_after_agent_exit(
    _request: &rimz::harness::launch::ExecRequest,
    _status: &ExitStatus,
    _startup_failure: bool,
    _ended: Option<&(AgentKind, AgentSessionId)>,
) {
}

/// Non-abrupt exits are deliberate. Abrupt exits are deliberate only while the
/// mux session still accepts live pane closes; if the mux is gone or wedged,
/// skip cleanup so the prior live-roster snapshot can recover the agent.
/// An agent a rebirth parked for recovery is not deliberately closed either:
/// the listed session is then the reborn room. The park precedes the reborn
/// session, so the record is read only after the listing.
fn close_is_deliberate(
    abrupt: bool,
    session_accepts_close: impl FnOnce() -> bool,
    agent_pending_recovery: impl FnOnce() -> bool,
) -> bool {
    !abrupt || (session_accepts_close() && !agent_pending_recovery())
}

fn enter_worktree(path: &Path) -> Result<PathBuf> {
    let path = absolute_lexical_path(path).context("resolving worktree checkout path")?;
    let marker = rimz::worktree::read_marker_for_worktree(&path)
        .with_context(|| format!("reading worktree marker for {}", path.display()))?;
    if marker.is_none() {
        bail!(
            "worktree checkout {} is gone or no longer a RimZ worktree (removed by a concurrent cleanup?); refusing to launch the agent in the project root",
            path.display()
        );
    }
    std::env::set_current_dir(&path).with_context(|| {
        format!(
            "worktree checkout {} is gone (removed by a concurrent cleanup?); refusing to launch the agent in the project root",
            path.display()
        )
    })?;
    Ok(path)
}

fn absolute_lexical_path(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("reading current directory")?
            .join(path)
    };
    Ok(rimz::utils::path::normalize_path_lexical(&path))
}

#[cfg(unix)]
fn exec_agent_command(
    program: &str,
    rest: &[String],
    env: &std::collections::BTreeMap<String, String>,
    unset: &std::collections::BTreeSet<String>,
) -> Result<()> {
    use std::os::unix::process::CommandExt;

    let mut command = Command::new(program);
    command.args(rest);
    command.envs(env);
    for key in unset {
        command.env_remove(key);
    }
    let err = command.exec();
    Err(err).with_context(|| format!("running {program}"))
}

#[cfg(not(unix))]
fn exec_agent_command(
    _program: &str,
    _rest: &[String],
    _env: &std::collections::BTreeMap<String, String>,
    _unset: &std::collections::BTreeSet<String>,
) -> Result<()> {
    anyhow::bail!("direct agent exec is disabled on non-Unix platforms")
}

fn cleanup_worktree_via_ondisk(
    path: &Path,
    globals: &GlobalFlags,
    interactive: bool,
    detached: bool,
) -> Result<()> {
    let cleanup_path = cleanup_target_path(path);
    let path = cleanup_path.as_path();
    leave_worktree_before_cleanup(path);
    let Some(bin) = rimz::reload::current_reexec_target() else {
        return worktree::cleanup_worktree(path, globals, interactive);
    };

    let mut command = Command::new(&bin);
    command.args(["worktree", "cleanup"]).arg(path);
    if !interactive {
        command.arg("--non-interactive");
    }
    if let Some(mux) = globals.mux {
        command.args(["--mux", mux.as_str()]);
    }

    if detached {
        return spawn_detached_worktree_cleanup(command);
    }

    match command.status() {
        Ok(status) => {
            if !status.success() {
                tracing::debug!(
                    status = %status,
                    "on-disk worktree cleanup exited non-zero",
                );
            }
            Ok(())
        }
        Err(err) => {
            tracing::debug!(
                binary = %bin.display(),
                error = %err,
                "could not spawn on-disk worktree cleanup; falling back in-process",
            );
            worktree::cleanup_worktree(path, globals, interactive)
        }
    }
}

#[cfg(unix)]
fn spawn_detached_worktree_cleanup(mut command: Command) -> Result<()> {
    use std::os::unix::process::CommandExt;

    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        // Keep cleanup outside the pane's foreground process group; null stdio
        // removes the remaining terminal dependency.
        .process_group(0);
    rimz::child_process::spawn_detached_reaped(&mut command, "worktree-cleanup-detached")
        .map(|_| ())
        .context("spawning detached worktree cleanup")
}

#[cfg(not(unix))]
fn spawn_detached_worktree_cleanup(mut command: Command) -> Result<()> {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("spawning detached worktree cleanup")?;
    Ok(())
}

fn cleanup_target_path(path: &Path) -> std::path::PathBuf {
    path.canonicalize().unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(path))
                .unwrap_or_else(|_| path.to_path_buf())
        }
    })
}

fn leave_worktree_before_cleanup(path: &Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    if let Err(err) = std::env::set_current_dir(parent) {
        tracing::debug!(
            path = %parent.display(),
            error = %err,
            "could not leave worktree before delegated cleanup",
        );
    }
}

struct ExecInvocationContext<'a> {
    workspace: &'a rimz::ResolvedWorkspace,
    cwd: PathBuf,
    store: RefCell<Option<rimz::Store>>,
    effective_isolation: Option<rimz::config::Isolation>,
}

impl<'a> ExecInvocationContext<'a> {
    fn new(workspace: &'a rimz::ResolvedWorkspace, cwd: PathBuf) -> Self {
        Self {
            workspace,
            cwd,
            store: RefCell::new(None),
            effective_isolation: None,
        }
    }

    fn store(&self) -> Result<rimz::Store> {
        if let Some(store) = self.store.borrow().as_ref() {
            return Ok(store.clone());
        }
        let store = require_existing_store(self.workspace)?;
        *self.store.borrow_mut() = Some(store.clone());
        Ok(store)
    }
}

#[derive(Clone, Debug)]
struct RunExecContext {
    run_id: rimz::RunId,
    store: rimz::Store,
    session_name: String,
    /// What the fleet digest reporter needs when a parked run repairs its own
    /// lost digest.
    workspace: rimz::ResolvedWorkspace,
}

impl RunExecContext {
    fn is_terminal(&self) -> bool {
        self.load_record()
            .is_some_and(|record| record.status.is_terminal())
    }

    fn parent_received_and_rested(
        &self,
        record: &rimz::store::run::RunRecord,
    ) -> std::result::Result<bool, rimz::store::StoreErr> {
        if record.owes_report() {
            return Ok(false);
        }
        // Pane send leaves a prompt Sent. The lifecycle hook records TurnStarted
        // before confirming Delivered; queue-before-snapshot preserves that order.
        let messages = self.store.list_messages()?;
        if record.joined_at.is_none() {
            // A queued report is not yet delivered; history is read only once it leaves the queue.
            let Some(report) = record.report_message_id.as_ref() else {
                return Ok(false);
            };
            if messages.iter().any(|message| &message.message_id == report)
                || !self.store.list_message_history()?.iter().any(|message| {
                    &message.message_id == report
                        && message.status == rimz::store::message::MessageStatus::Delivered
                })
            {
                return Ok(false);
            }
        }
        let snapshot = self.store.snapshot_cached()?;
        let Some(child) = record
            .agent_id
            .as_ref()
            .and_then(|id| find_agent(&snapshot.agents, &record.kind, id))
        else {
            return Ok(false);
        };
        if child.holds_open_turn()
            || messages
                .iter()
                .any(|message| !message.status.is_terminal() && message.same_agent_card(child))
        {
            return Ok(false);
        }
        Ok(!rimz::address::launched_parent(&snapshot.agents, child)
            .is_some_and(|parent| parent.ended_at.is_none() && parent.holds_open_turn()))
    }

    fn load_record(&self) -> Option<rimz::store::run::RunRecord> {
        match rimz::harness::run::load(self.store.paths(), &self.run_id) {
            Ok(record) => Some(record),
            Err(err) => {
                tracing::debug!(
                    run_id = %self.run_id,
                    error = %err,
                    "could not read supervised run record while monitoring pane",
                );
                None
            }
        }
    }
}

fn resumed_run_follow_ups(
    request: &rimz::harness::launch::ExecRequest,
    record: &rimz::store::run::RunRecord,
) -> Option<u32> {
    (request.subagent && exec_attach_target(request).is_some() && record.status.is_terminal())
        .then_some(record.follow_ups)
}

fn run_exec_context(
    request: &rimz::harness::launch::ExecRequest,
    invocation: &ExecInvocationContext<'_>,
) -> Result<Option<RunExecContext>> {
    let Some(run_id) = request.run_id.clone() else {
        return Ok(None);
    };
    let store = invocation.store().context("opening supervised run store")?;
    Ok(Some(RunExecContext {
        run_id,
        store,
        session_name: invocation.workspace.session_name.clone(),
        workspace: invocation.workspace.clone(),
    }))
}

fn exec_launch_identity(
    request: &rimz::harness::launch::ExecRequest,
) -> Result<Option<LaunchIdentity>> {
    match (
        request.identity.launch_id.as_deref(),
        request.identity.name.as_deref(),
    ) {
        (None, None) => Ok(None),
        (Some(_), None)
            if matches!(
                request.action,
                rimz::harness::launch::ExecAction::Resume { .. }
            ) =>
        {
            Ok(None)
        }
        (Some(_), None) => bail!("--launch-id requires --agent-name"),
        (None, Some(_)) => Ok(None),
        (Some(launch_id), Some(name)) => {
            validate_agent_name(name)?;
            Ok(Some(LaunchIdentity {
                kind: request.kind.clone(),
                agent_id: AgentSessionId::from(launch_id),
                name: name.to_owned(),
                name_explicit: request.identity.name_explicit,
                launch: request.identity.params.clone(),
                run_id: request.run_id.clone(),
                prompt: match &request.action {
                    rimz::harness::launch::ExecAction::Launch { prompt, .. } => prompt.clone(),
                    rimz::harness::launch::ExecAction::Resume { .. }
                    | rimz::harness::launch::ExecAction::Fork { .. } => None,
                },
            }))
        }
    }
}

/// The exact durable card an exec wrapper can attach before provider startup.
/// A fork abstains because its provider-assigned session id is not known yet.
fn exec_attach_target(
    request: &rimz::harness::launch::ExecRequest,
) -> Option<(AgentKind, AgentSessionId)> {
    match &request.action {
        rimz::harness::launch::ExecAction::Resume { session_id, .. } => Some((
            request.kind.clone(),
            AgentSessionId::from(session_id.as_str()),
        )),
        rimz::harness::launch::ExecAction::Launch { .. }
        | rimz::harness::launch::ExecAction::Fork { .. } => None,
    }
}

fn record_own_run_pane(context: &RunExecContext) {
    let Some(pane_id) = rimz::mux::ambient_pane_id() else {
        return;
    };
    if let Err(err) =
        rimz::harness::run::record_pane(context.store.paths(), &context.run_id, pane_id.clone())
    {
        tracing::debug!(
            run_id = %context.run_id,
            pane = %pane_id,
            error = %err,
            "could not persist supervised run pane id",
        );
    }
}

fn record_provider_process(context: &RunExecContext, pid: u32) {
    let process_start = rimz::proc::process_start_token(pid);
    if let Err(err) = rimz::harness::run::record_provider_process(
        context.store.paths(),
        &context.run_id,
        pid,
        process_start,
    ) {
        tracing::debug!(
            run_id = %context.run_id,
            pid,
            error = %err,
            "could not persist supervised provider process identity",
        );
    }
}

fn repair_own_launch_winsize(workspace: &rimz::ResolvedWorkspace, diag: &rimz::diag::DiagSink) {
    let stdin = std::io::stdin();
    let Some(pane) = rimz::mux::ambient_pane_id() else {
        return;
    };
    if !stdin.is_terminal() {
        return;
    }
    let backend = rimz::mux::backend_for(pane.mux());
    match rimz::mux::winsize::repair_zero_winsize(
        backend.as_ref(),
        &pane,
        &workspace.session_name,
        stdin.as_fd(),
        Duration::from_secs(2),
    ) {
        WinsizeRepair::Sized => {}
        WinsizeRepair::Repaired { rows, cols } => {
            tracing::debug!(%pane, rows, cols, "repaired unsized provider tty");
            diag.emit(rimz::diag::record::DiagEvent::PaneWinsizeRepaired { pane, rows, cols });
        }
        WinsizeRepair::Unavailable(reason) => {
            tracing::debug!(%pane, %reason, "provider tty size repair unavailable");
        }
    }
}

fn record_own_launch_pane(invocation: &ExecInvocationContext<'_>, identity: &LaunchIdentity) {
    let Some(pane_id) = rimz::mux::ambient_pane_id() else {
        return;
    };
    let workspace = invocation.workspace;
    match invocation.store().and_then(|store| {
        store.bind_agent_launch(identity, &workspace.session_name, &invocation.cwd, &pane_id)?;
        Ok(())
    }) {
        Ok(()) => {}
        Err(err) => tracing::debug!(
            agent_name = %identity.name,
            pane = %pane_id,
            error = %err,
            "could not persist provisional agent pane id",
        ),
    }
}

fn attach_own_launch_pane(invocation: &ExecInvocationContext<'_>, identity: &LaunchIdentity) {
    let Some(pane_id) = rimz::mux::ambient_pane_id() else {
        return;
    };
    let workspace = invocation.workspace;
    let attached = invocation.store().and_then(|store| {
        let projection = store.runtime_projection(rimz::RuntimeScope::Audit)?;
        let current =
            rimz::address::launch_row(&projection.agents, &identity.kind, &identity.agent_id)
                .context("resolving current agent row by launch id")?;
        store.attach_agent_pane(
            &current.kind,
            &current.agent_id,
            Some(&identity.agent_id),
            &current.login.clone().unwrap_or_default(),
            &workspace.session_name,
            &pane_id,
            rimz::store::runtime::current_process_owner(
                rimz::pane::RuntimeOwnerKind::Agent,
                current.agent_id.as_str(),
            ),
            None,
            invocation.effective_isolation,
            None,
        )?;
        Ok(())
    });
    if let Err(err) = attached {
        tracing::debug!(
            agent_name = %identity.name,
            launch_id = %identity.agent_id,
            pane = %pane_id,
            error = %err,
            "could not attach launch pane ownership to its wrapper",
        );
    }
}

fn record_own_resume_pane(
    invocation: &ExecInvocationContext<'_>,
    target: &(AgentKind, AgentSessionId),
    launch_id: Option<AgentSessionId>,
    runtime_owner: rimz::pane::RuntimeOwner,
    params: &rimz::agents::LaunchParams,
) {
    let isolation = params.isolation;
    let Some(pane_id) = rimz::mux::ambient_pane_id() else {
        return;
    };
    let workspace = invocation.workspace;
    if let Err(err) = invocation.store().and_then(|store| {
        store.attach_agent_pane(
            &target.0,
            &target.1,
            launch_id.as_ref(),
            &params.login.clone().unwrap_or_default(),
            &workspace.session_name,
            &pane_id,
            runtime_owner,
            isolation,
            invocation.effective_isolation,
            Some(params),
        )?;
        Ok(())
    }) {
        if let Some(isolation) = isolation {
            let _ = writeln!(
                crate::cli::render::err(),
                "rimz: could not record isolation {isolation} for resumed {} {}: {err}",
                target.0,
                target.1,
            );
            return;
        }
        tracing::debug!(
            kind = %target.0,
            agent_id = %target.1,
            pane = %pane_id,
            error = %err,
            "could not persist resumed agent pane attach",
        );
    }
}

fn record_launch_failed(invocation: &ExecInvocationContext<'_>, identity: &LaunchIdentity) {
    let workspace = invocation.workspace;
    if let Err(err) = invocation.store().and_then(|store| {
        store.fail_agent_launch(identity, &workspace.session_name, &invocation.cwd)?;
        Ok(())
    }) {
        tracing::debug!(
            agent_name = %identity.name,
            error = %err,
            "could not mark provisional agent launch failed",
        );
    }
}

fn mark_launch_failed_if_provisional(
    invocation: &ExecInvocationContext<'_>,
    identity: Option<&LaunchIdentity>,
) -> bool {
    let Some(identity) = identity else {
        return false;
    };
    // A card the store cannot show is treated as still provisional.
    if !launch_card_provisional(invocation, identity).unwrap_or(true) {
        return false;
    }
    record_launch_failed(invocation, identity);
    true
}

fn record_own_agent_end_trace(
    invocation: &ExecInvocationContext<'_>,
    request: &rimz::harness::launch::ExecRequest,
) -> Option<(AgentKind, AgentSessionId)> {
    match resolve_own_agent_end_trace(invocation, request) {
        Ok(Some((kind, agent_id))) => {
            append_agent_lifecycle_trace(
                invocation,
                kind.clone(),
                agent_id.clone(),
                rimz::agents::LifecycleSignal::Ended,
                AGENT_ENDED_EVENT,
                "agent exit end stamp",
            );
            Some((kind, agent_id))
        }
        Ok(None) => {
            tracing::debug!("agent exit produced no pane binding to stamp ended");
            None
        }
        Err(err) => {
            tracing::debug!(
                error = %err,
                "could not resolve agent exit end stamp",
            );
            None
        }
    }
}

fn own_agent_pending_recovery(
    invocation: &ExecInvocationContext<'_>,
    request: &rimz::harness::launch::ExecRequest,
) -> bool {
    let Ok(Some((kind, agent_id))) = resolve_own_agent_end_trace(invocation, request) else {
        return false;
    };
    invocation
        .store()
        .is_ok_and(|store| store.is_pending_recovery(&kind, &agent_id))
}

fn resolve_own_agent_end_trace(
    invocation: &ExecInvocationContext<'_>,
    request: &rimz::harness::launch::ExecRequest,
) -> Result<Option<(AgentKind, AgentSessionId)>> {
    let own_pane = rimz::mux::ambient_pane_id();
    let attach_target = exec_attach_target(request);
    if own_pane.is_none() && attach_target.is_none() {
        return Ok(None);
    }
    let store = invocation
        .store()
        .context("opening store for agent exit end stamp")?;
    let mut projection = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .context("reading audit projection for agent exit end stamp")?;
    if let Some(pane_id) = own_pane.clone() {
        rimz::store::agent_context::attach_rest_certificates(
            store.runtime_paths(),
            &mut projection.agents,
        );
        let pane = rimz::pane::PaneRef::from_id(pane_id);
        let owner = rimz::store::snapshot::stamped_agent_for_pane(&pane, &projection.agents);
        if let Some(launch_id) = request.identity.launch_id.as_deref() {
            let launch_id = AgentSessionId::from(launch_id);
            if let Some(agent) = owner
                && agent.kind == request.kind
                && agent.launch_id.as_ref() == Some(&launch_id)
                && !agent.agent_id.is_empty()
            {
                return Ok(Some((agent.kind.clone(), agent.agent_id.clone())));
            }
        }
        if let Some(agent) = owner
            && !agent.agent_id.is_empty()
        {
            return Ok(Some((agent.kind.clone(), agent.agent_id.clone())));
        }
    }
    // A resumed pane owns the resumed session and can safely fall back to the
    // same argv identity used by its pre-start attach — unless the store now
    // binds that session to a different pane. A same-session `agents restart`
    // is a continuation: the replacement stamps its own pane binding
    // (`record_own_resume_pane`) before it spawns its provider, so a wrapper
    // that sees the session bound elsewhere has been superseded, and ending it
    // would mark a live agent dead and retire the rows it is still listening on.
    let Some((kind, session)) = attach_target else {
        return Ok(None);
    };
    if session_bound_to_another_pane(&projection.agents, &kind, &session, own_pane.as_ref()) {
        return Ok(None);
    }
    Ok(Some((kind, session)))
}

/// Whether `agents` binds `(kind, session)` to a pane that is not `own_pane`.
/// A wrapper with no ambient pane of its own cannot claim any binding as its
/// own, so any binding at all supersedes it.
fn session_bound_to_another_pane(
    agents: &[rimz::agents::AgentState],
    kind: &AgentKind,
    session: &AgentSessionId,
    own_pane: Option<&rimz::ids::PaneId>,
) -> bool {
    agents.iter().any(|agent| {
        &agent.kind == kind
            && &agent.agent_id == session
            && agent
                .pane
                .as_ref()
                .is_some_and(|bound| Some(&bound.pane_id) != own_pane)
    })
}

/// Re-wake a stamped resumed card when it owns its team's open stage. `since` precedes the
/// provider's start, so a Stage notice it already took counts.
fn rewake_resumed(
    invocation: &ExecInvocationContext<'_>,
    target: &(AgentKind, AgentSessionId),
    mux: Option<rimz::ids::MuxName>,
    since: jiff::Timestamp,
) {
    let rewoken = invocation.store().and_then(|store| {
        Ok(rimz::harness::team_stage::rewake_resumed(
            invocation.workspace,
            &store,
            &target.0,
            &target.1,
            mux,
            jiff::Timestamp::now(),
            since,
        )?)
    });
    match rewoken {
        Ok(Some(receipt)) => {
            tracing::debug!(delivery = ?receipt.delivery, "resume: re-woke team stage owner");
        }
        Ok(None) => tracing::debug!("resume: no team stage to re-wake"),
        Err(err) => tracing::warn!(
            kind = %target.0,
            agent_id = %target.1,
            error = %err,
            "could not re-wake the resumed team stage owner",
        ),
    }
}

fn append_agent_lifecycle_trace(
    invocation: &ExecInvocationContext<'_>,
    kind: AgentKind,
    agent_id: AgentSessionId,
    signal: rimz::agents::LifecycleSignal,
    event_name: &'static str,
    label: &'static str,
) {
    let workspace = invocation.workspace;
    let appended = (|| -> Result<()> {
        let store = invocation
            .store()
            .context("opening store for agent lifecycle trace")?;
        let observation =
            rimz::agents::AgentLifecycleObservation::new(Some(agent_id.clone()), signal);
        let event = rimz::EventEnvelope::agent_lifecycle(
            workspace.workspace_id.clone(),
            &workspace.session_name,
            kind.as_str(),
            event_name,
            &observation,
        );
        store.append_event(&event)?;
        Ok(())
    })();
    if let Err(err) = appended {
        tracing::warn!(
            kind = %kind,
            agent_id = %agent_id,
            error = %err,
            "could not record {label}",
        );
    }
}

/// Whether the launch's card is still the provisional one, keyed by the launch
/// id under the launch name; `None` when the store cannot say.
fn launch_card_provisional(
    invocation: &ExecInvocationContext<'_>,
    identity: &LaunchIdentity,
) -> Option<bool> {
    match invocation.store().and_then(|store| {
        duty::run_card_evidence(&store, &identity.kind, &identity.agent_id, &identity.name)
    }) {
        Ok(provisional) => Some(provisional),
        Err(err) => {
            tracing::debug!(
                agent_name = %identity.name,
                error = %err,
                "could not inspect launch card",
            );
            None
        }
    }
}

fn fail_run_if_child_exited_first(
    context: &RunExecContext,
    globals: &GlobalFlags,
    terminal_grace: Duration,
) {
    if let Err(error) =
        rimz::harness::hook_drain::drain_through(&context.store, 0, None, HOOK_DRAIN_WAIT)
    {
        tracing::debug!(%error, "could not drain terminal hooks before child-exit settlement");
    }
    if wait_for_terminal_run(context, terminal_grace) {
        if let Ok(record) = rimz::harness::run::load(context.store.paths(), &context.run_id)
            && record.status.is_terminal()
            && record.status != rimz::store::run::RunStatus::Completed
            && record.failure_tail.is_none()
        {
            // The provider exited independently; self-close must not race the waiter's capture.
            record_own_run_failure_tail(context, globals);
        }
        return;
    }
    record_own_run_failure_tail(context, globals);
    // No reason: the evidence is the pane tail, which the waiter still
    // captures when the read above found none.
    fail_run_if_nonterminal(context, "");
}

fn fail_run_if_nonterminal(context: &RunExecContext, reason: &str) {
    match rimz::harness::run::fail_if_nonterminal(context.store.paths(), &context.run_id, reason) {
        Ok(Some(record)) => rimz::store::run::wake_run(context.store.runtime_paths(), &record),
        Ok(None) => {}
        Err(err) => tracing::debug!(
            run_id = %context.run_id,
            error = %err,
            "could not mark supervised run failed",
        ),
    }
}

fn record_own_run_failure_tail(context: &RunExecContext, globals: &GlobalFlags) {
    let Ok(mux) = rimz::mux::auto_detect_backend(globals.mux) else {
        return;
    };
    let Some(own) = own_pane_id(mux) else {
        return;
    };
    let backend = rimz::mux::backend_for(mux);
    let Some(tail) =
        supervised::pane::capture_failure_tail(backend.as_ref(), &own, &context.session_name)
    else {
        return;
    };
    if let Err(err) =
        rimz::harness::run::record_failure_tail(context.store.paths(), &context.run_id, &tail)
    {
        tracing::debug!(
            run_id = %context.run_id,
            pane = %own,
            error = %err,
            "could not record supervised run failure pane tail",
        );
    }
}

fn wait_for_terminal_run(context: &RunExecContext, cap: Duration) -> bool {
    let deadline = Instant::now() + cap;
    loop {
        if context.is_terminal() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(CHILD_WAIT_POLL);
    }
}

fn subagent_parent_watchdog(
    request: &rimz::harness::launch::ExecRequest,
    run_context: Option<&RunExecContext>,
    launch_identity: Option<&LaunchIdentity>,
    survives_parent: bool,
) -> Option<rimz::harness::parent_watch::WatchdogSeed> {
    if !request.subagent {
        return None;
    }
    let Some(context) = run_context else {
        tracing::debug!("subagent parent watchdog has no supervised run context");
        return None;
    };
    if survives_parent {
        return None;
    }
    let Some(identity) = launch_identity else {
        tracing::debug!("subagent parent watchdog has no launch identity");
        return None;
    };
    let paths = context.store.paths();
    let cursor = duty::parent_cursor(paths).unwrap_or_else(|error| {
        tracing::debug!(%error, "could not capture parent watchdog cursor");
        rimz::store::event_log::LogExtent {
            generation: 0,
            offset: 0,
        }
    });
    let child_pane = rimz::mux::ambient_pane_id();
    let seed = match context.store.runtime_projection(rimz::RuntimeScope::Audit) {
        Ok(projection) => rimz::harness::parent_watch::seed(
            &projection.agents,
            identity.kind.clone(),
            identity.agent_id.clone(),
            child_pane.clone(),
            context.session_name.clone(),
            cursor,
        ),
        Err(error) => {
            tracing::debug!(%error, "could not seed parent watchdog projection");
            None
        }
    };
    Some(
        seed.unwrap_or_else(|| rimz::harness::parent_watch::WatchdogSeed {
            child_kind: identity.kind.clone(),
            child_launch_id: identity.agent_id.clone(),
            parent_kind: identity.kind.clone(),
            parent_refs: Vec::new(),
            members: Default::default(),
            parent_pane: None,
            child_pane,
            session_name: context.session_name.clone(),
            cursor,
        }),
    )
}

fn reset_cleanup_signal_flag() {
    cleanup_signal_flag().store(false, Ordering::SeqCst);
}

fn cleanup_signal_received() -> bool {
    cleanup_signal_flag().load(Ordering::SeqCst)
}

fn cleanup_signal_flag() -> &'static Arc<AtomicBool> {
    CLEANUP_SIGNAL_RECEIVED.get_or_init(|| Arc::new(AtomicBool::new(false)))
}

fn interrupt_signal_flag() -> &'static Arc<AtomicBool> {
    INTERRUPT_SIGNAL_RECEIVED.get_or_init(|| Arc::new(AtomicBool::new(false)))
}

#[cfg(unix)]
fn install_cleanup_signal_handlers() -> Result<()> {
    use signal_hook::consts::signal::{SIGHUP, SIGTERM};

    for signal in [SIGHUP, SIGTERM] {
        signal_hook::flag::register(signal, cleanup_signal_flag().clone())?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn install_cleanup_signal_handlers() -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn install_interrupt_signal_handler() -> Result<()> {
    use signal_hook::consts::signal::SIGINT;

    // Registering a handler keeps the wrapper alive when the agent handles
    // Ctrl-C, so the wrapper can record the exit trace and drop to a shell.
    signal_hook::flag::register(SIGINT, interrupt_signal_flag().clone())?;
    Ok(())
}

#[cfg(not(unix))]
fn install_interrupt_signal_handler() -> Result<()> {
    Ok(())
}

fn session_accepts_agent_close(globals: &GlobalFlags, session_name: &str) -> bool {
    let Ok(mux) = rimz::mux::auto_detect_backend(globals.mux) else {
        return false;
    };
    let backend = rimz::mux::backend_for(mux);
    backend.session_accepts_agent_close(session_name)
}

fn close_own_pane(globals: &GlobalFlags, session_name: &str) {
    let Ok(mux) = rimz::mux::auto_detect_backend(globals.mux) else {
        return;
    };
    let Some(own) = own_pane_id(mux) else {
        return;
    };
    let backend = rimz::mux::backend_for(mux);
    if let Err(err) = backend.close_pane(session_name, &own) {
        tracing::debug!(
            pane = %own,
            error = %err,
            "supervised run wrapper could not close its pane",
        );
    }
}

#[cfg(test)]
mod tests;
