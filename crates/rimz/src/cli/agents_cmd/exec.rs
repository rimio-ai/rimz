use super::*;
use crate::cli::{open_store, worktree};
use std::cell::RefCell;
use std::sync::mpsc;

const PARK_STRAND_POLL: Duration = Duration::from_secs(5);
const PARENT_RECEIPT_POLL: Duration = Duration::from_secs(1);

pub(super) fn run_exec(args: ExecArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())
        .context("resolving the agent launch workspace")?;
    let envelope = rimz::harness::launch::decode_exec_envelope(
        &args.kind,
        args.worktree_path.as_deref(),
        &args.request,
    )
    .context("decoding hidden agent exec request")?;
    let mut invocation = ExecInvocationContext::new(&workspace);
    let run_context = run_exec_context(envelope.request(), &invocation)?;
    let launch_identity = exec_launch_identity(envelope.request())?;
    let machine_config = crate::cli::machine_config();
    let effective = rimz::config::effective::load_with_roots(
        &machine_config,
        &workspace.project_root,
        &rimz::disk::paths::rimz_home(),
    );
    if let Err(err) = &effective {
        let _ = writeln!(crate::cli::render::err(), "rimz: {err}");
    }
    if let Some(detail) =
        exec_definition_failure(envelope.request(), &machine_config, effective.as_ref().ok())
    {
        mark_launch_failed_if_provisional(&invocation, launch_identity.as_ref());
        if let Some(context) = run_context.as_ref()
            && let Err(err) = rimz::harness::run::record_failure_tail(
                context.store.paths(),
                &context.run_id,
                &detail,
            )
        {
            tracing::debug!(
                run_id = %context.run_id,
                error = %err,
                "could not record supervised run definition failure",
            );
        }
        fail_run_on_exec_precondition(run_context.as_ref());
        anyhow::bail!(detail);
    }
    let isolation = rimz::config::Isolation::resolve(
        envelope.request().identity.params.isolation,
        envelope.request().isolation_default,
        machine_config.agents.isolation,
    );
    let adapter = rimz::agents::find_definition(envelope.request().kind.as_str());
    let bwrap = rimz::sandbox::preflight_skills(
        isolation,
        &envelope.request().kind,
        envelope.request().skills.is_some(),
        adapter.map_or(rimz::agents::ManualSkill::Unsupported, |adapter| {
            adapter.manual_skill()
        }),
    )
    .and_then(|()| rimz::sandbox::preflight(isolation))
    .inspect_err(|_| {
        mark_launch_failed_if_provisional(&invocation, launch_identity.as_ref());
        fail_run_on_exec_precondition(run_context.as_ref());
    })?;
    invocation.effective_isolation = Some(isolation);
    let request = match envelope.materialize() {
        Ok(request) => request,
        Err(err) => {
            mark_launch_failed_if_provisional(&invocation, launch_identity.as_ref());
            fail_run_on_exec_precondition(run_context.as_ref());
            return Err(err).context("materializing launch prompt");
        }
    };
    let attach_target = exec_attach_target(&request);
    let (runtime, state) = rimz::StatePaths::for_project_root(&workspace.project_root)
        .and_then(|state| rimz::RuntimePaths::for_state(&state).map(|runtime| (runtime, state)))
        .inspect_err(|_| {
            mark_launch_failed_if_provisional(&invocation, launch_identity.as_ref());
            fail_run_on_exec_precondition(run_context.as_ref());
        })?;
    let provider_cwd = request
        .worktree_path
        .as_deref()
        .map(absolute_lexical_path)
        .unwrap_or_else(|| match &request.action {
            rimz::harness::launch::ExecAction::Launch { .. } => Ok(workspace.worktree_root.clone()),
            _ => std::env::current_dir().context("reading the agent pane cwd"),
        })
        .inspect_err(|_| {
            mark_launch_failed_if_provisional(&invocation, launch_identity.as_ref());
            fail_run_on_exec_precondition(run_context.as_ref());
        })?;
    let ambient_env = rimz::agents::ambient_env();
    let plan = rimz::harness::launch_plan::compile(rimz::harness::launch_plan::LaunchPlanInputs {
        request: &request,
        cwd: &provider_cwd,
        project_root: &workspace.project_root,
        rimz_bin: &rimz::proc::rimz_exe(),
        runtime: &runtime,
        state: &state,
        effective: effective.as_ref().ok(),
        commands: &machine_config.agents.commands,
        accounts: &machine_config.accounts,
        bwrap: bwrap.as_deref(),
        ambient_env: &ambient_env,
    })
    .inspect_err(|_| {
        mark_launch_failed_if_provisional(&invocation, launch_identity.as_ref());
        fail_run_on_exec_precondition(run_context.as_ref());
    })?;
    for warning in &plan.warnings {
        let _ = writeln!(crate::cli::render::err(), "rimz: {warning}");
    }
    if let Some(links) = &plan.skill_links {
        for line in links.to_string().lines() {
            let _ = writeln!(crate::cli::render::err(), "rimz: {line}");
        }
    }
    if let Some(sandbox) = &plan.sandbox {
        for skipped in &sandbox.skipped {
            let _ = writeln!(crate::cli::render::err(), "rimz: {skipped}");
        }
    }
    let skill_links = rimz::harness::launch_plan::apply(&plan).inspect_err(|_| {
        mark_launch_failed_if_provisional(&invocation, launch_identity.as_ref());
        fail_run_on_exec_precondition(run_context.as_ref());
    })?;
    if let Some((links, outcome)) = plan.skill_links.as_ref().zip(skill_links)
        && let Some(report) = links.shadowed_report(&outcome.shadowed)
    {
        let _ = writeln!(crate::cli::render::err(), "rimz: {report}");
    }
    let entered_worktree = request
        .worktree_path
        .as_deref()
        .map(enter_worktree)
        .transpose()
        .inspect_err(|_| {
            mark_launch_failed_if_provisional(&invocation, launch_identity.as_ref());
            fail_run_on_exec_precondition(run_context.as_ref());
        })?;
    let process = plan.process();
    if let Some(launch_id) = request.identity.launch_id.as_deref()
        && let Err(error) = rimz::lsp::lease::register(&provider_cwd, launch_id, std::process::id())
    {
        tracing::debug!(%error, "language-server lease registration failed");
    }
    if let rimz::harness::launch::AgentProcessStage::LoginShellReentry { argv, .. } = &plan.stage {
        let (program, rest) = argv
            .split_first()
            .ok_or_else(|| anyhow::anyhow!("finalized Qwen launch produced an empty command"))?;
        exec_agent_command(program, rest, &process.env, &process.unset).inspect_err(|_| {
            mark_launch_failed_if_provisional(&invocation, launch_identity.as_ref());
            fail_run_on_exec_precondition(run_context.as_ref());
        })?;
        return Ok(());
    }
    if let Some(context) = run_context.as_ref() {
        record_own_run_pane(context);
    }
    if let Some(identity) = launch_identity.as_ref() {
        record_own_launch_pane(&invocation, identity);
        if attach_target.is_none() {
            attach_own_launch_pane(&invocation, identity);
        }
    }
    if let Some(target) = attach_target.as_ref() {
        record_own_resume_pane(
            &invocation,
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
            request.identity.params.isolation,
        );
    }
    let (program, rest) = process.argv.split_first().ok_or_else(|| {
        anyhow::anyhow!("agent `{}` produced an empty launch command", request.kind)
    })?;
    if should_exec_agent_directly(&request) {
        match exec_agent_command(program, rest, &process.env, &process.unset) {
            Ok(()) => return Ok(()),
            Err(err) => {
                mark_launch_failed_if_provisional(&invocation, launch_identity.as_ref());
                return Err(err);
            }
        }
    }
    reset_cleanup_signal_flag();
    install_cleanup_signal_handlers().context("installing cleanup signal handlers")?;
    install_interrupt_signal_handler().context("installing interrupt signal handler")?;
    let mut command = Command::new(program);
    command.args(rest);
    command.envs(&process.env);
    for key in &process.unset {
        command.env_remove(key);
    }
    if let Some(path) = entered_worktree.as_deref() {
        command.current_dir(path);
    }
    let child = command
        .spawn()
        .with_context(|| format!("running {program}"))?;
    if let Some(target) = attach_target.as_ref() {
        record_own_resume_pane(
            &invocation,
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
            request.identity.params.isolation,
        );
    }
    if let Some(context) = run_context.as_ref() {
        record_provider_process(context, child.id());
    }
    let keep = run_context
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
        .is_some_and(|record| record.keep);
    let parent_watchdog = subagent_parent_watchdog(
        &request,
        run_context.as_ref(),
        launch_identity.as_ref(),
        keep,
    );
    let outcome = supervise_child(
        child,
        run_context.as_ref(),
        request.exit_on_run_completion,
        if request.subagent {
            StopPolicy::ParentReceived
        } else {
            StopPolicy::RunTerminal
        },
        parent_watchdog,
    )
    .context("supervising agent process")?;
    settle_after_exit(
        &request,
        globals,
        &invocation,
        RunExitContext {
            run: run_context.as_ref(),
            keep,
            checkout: &provider_cwd,
        },
        launch_identity.as_ref(),
        entered_worktree.as_deref(),
        outcome,
    )
}

struct RunExitContext<'a> {
    run: Option<&'a RunExecContext>,
    keep: bool,
    checkout: &'a Path,
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
    } = run_exit;
    if let Some(launch_id) = request.identity.launch_id.as_deref()
        && let Err(error) = rimz::lsp::lease::release(checkout, launch_id, std::process::id())
    {
        tracing::debug!(%error, "language-server lease release failed");
    }
    let ExecOutcome {
        status,
        abrupt: child_exit_abrupt,
        parent_ended,
        parent_watchdog,
    } = outcome;
    if let Some(context) = run {
        fail_run_if_child_exited_first(context, globals, RUN_EXIT_TERMINAL_GRACE);
    }
    if let Some(context) = run
        && !parent_ended
    {
        report_settled_child_or_log(context);
    }
    let startup_failure =
        !status.success() && mark_launch_failed_if_provisional(invocation, launch_identity);

    let session_name = run
        .map(|context| context.session_name.as_str())
        .unwrap_or(&invocation.workspace.session_name);
    let abrupt = child_exit_abrupt || cleanup_signal_received();
    let session_accepts_close = !abrupt || session_accepts_agent_close(globals, session_name);
    let deliberate = close_is_deliberate(abrupt, session_accepts_close);
    let linger = should_linger_subagent(request, keep, parent_ended);
    let ended_session = if deliberate && !linger && should_record_end_trace(request) {
        record_own_agent_end_trace(invocation, request)
    } else {
        None
    };
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

/// The host-side refusal for a fresh launch of a profile whose definition failed to load. Resume
/// and fork are left alone: lane resume degrades to a bare resume by design, and restart/fork
/// refuse at the CLI. A trusted project profile that shadows the failed name launches normally.
pub(super) fn exec_definition_failure(
    request: &rimz::harness::launch::ExecRequest,
    machine: &rimz::config::MachineConfig,
    effective: Option<&rimz::config::effective::LaunchAgents>,
) -> Option<String> {
    if !matches!(
        request.action,
        rimz::harness::launch::ExecAction::Launch { .. }
    ) {
        return None;
    }
    let profile = request.identity.params.profile.as_deref()?;
    let shadowed = effective.is_some_and(|effective| {
        effective.profiles.0.contains_key(profile)
            || effective.subagent_profiles.0.contains_key(profile)
    });
    if shadowed {
        return None;
    }
    machine.definition_failure_for(profile)
}

pub(super) fn should_exec_agent_directly(request: &rimz::harness::launch::ExecRequest) -> bool {
    cfg!(unix)
        && request.run_id.is_none()
        && request.worktree_path.is_none()
        && !request.exit_on_run_completion
        && !request.close_pane_on_exit
}

pub(super) fn should_record_end_trace(request: &rimz::harness::launch::ExecRequest) -> bool {
    !request.exit_on_run_completion || request.subagent
}

pub(super) fn should_drop_to_shell(
    request: &rimz::harness::launch::ExecRequest,
    abrupt: bool,
) -> bool {
    (request.close_pane_on_exit || request.worktree_path.is_some())
        && request.run_id.is_none()
        && !abrupt
}

pub(super) fn relaunch_command(request: &rimz::harness::launch::ExecRequest) -> String {
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
pub(super) fn exited_session_resumable(
    ended: Option<&(AgentKind, AgentSessionId)>,
    cwd: &Path,
) -> bool {
    ended.is_some_and(|(kind, agent_id)| {
        !agent_id.is_provisional()
            && rimz::agents::find_definition(kind.as_str())
                .is_some_and(|adapter| adapter.resume_command(agent_id, cwd).is_some())
    })
}

pub(super) fn exit_hint(
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
pub(super) fn close_is_deliberate(abrupt: bool, session_accepts_close: bool) -> bool {
    !abrupt || session_accepts_close
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
    store: RefCell<Option<rimz::Store>>,
    effective_isolation: Option<rimz::config::Isolation>,
}

impl<'a> ExecInvocationContext<'a> {
    fn new(workspace: &'a rimz::ResolvedWorkspace) -> Self {
        Self {
            workspace,
            store: RefCell::new(None),
            effective_isolation: None,
        }
    }

    fn store(&self) -> Result<rimz::Store> {
        if let Some(store) = self.store.borrow().as_ref() {
            return Ok(store.clone());
        }
        let store = open_store(self.workspace)?;
        *self.store.borrow_mut() = Some(store.clone());
        Ok(store)
    }
}

#[derive(Clone, Debug)]
pub(super) struct RunExecContext {
    pub(super) run_id: rimz::RunId,
    pub(super) store: rimz::Store,
    pub(super) session_name: String,
    /// What the fleet digest reporter needs when a parked run repairs its own
    /// lost digest.
    pub(super) workspace: rimz::ResolvedWorkspace,
}

impl RunExecContext {
    fn ready_for_self_cleanup(&self, record: &rimz::store::run::RunRecord) -> bool {
        record.status.is_terminal()
            && match rimz::store::run::run_waiter_is_live(self.store.runtime_paths(), &self.run_id)
            {
                Ok(live) => !live,
                Err(error) => {
                    tracing::debug!(run_id = %self.run_id, %error, "could not probe supervised run waiter");
                    false
                }
            }
    }

    fn is_terminal(&self) -> bool {
        self.load_record()
            .is_some_and(|record| record.status.is_terminal())
    }

    fn parent_received_and_rested(
        &self,
        record: &rimz::store::run::RunRecord,
    ) -> std::result::Result<bool, rimz::store::StoreErr> {
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
        let Some(child) = snapshot.agents.iter().find(|agent| {
            agent.kind == record.kind && Some(&agent.agent_id) == record.agent_id.as_ref()
        }) else {
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

#[derive(Clone, Copy)]
enum StopPolicy {
    RunTerminal,
    ParentReceived,
}

struct RunMonitor {
    self_cleanup: bool,
    stop_policy: StopPolicy,
    next_receipt_check: Instant,
    reported_revision: Option<jiff::Timestamp>,
    next_park_check: Instant,
    previous: Option<jiff::Timestamp>,
}

impl RunMonitor {
    fn poll(&mut self, context: &RunExecContext, now: Instant) -> bool {
        let Some(record) = context.load_record() else {
            self.previous = None;
            return false;
        };
        if record.parked_at.is_none() || record.status.is_terminal() {
            self.previous = None;
        } else if now >= self.next_park_check {
            self.next_park_check = now + PARK_STRAND_POLL;
            self.repair_digest(context, &record);
            self.previous = match rimz::harness::run::settle_stranded_park(
                &context.store,
                &record,
                self.previous,
            ) {
                Ok(rimz::harness::run::ParkCheck::Stranded(at)) => Some(at),
                Ok(_) => None,
                Err(error) => {
                    tracing::debug!(run_id = %context.run_id, %error, "could not settle parked run");
                    None
                }
            };
        }
        if !self.self_cleanup {
            return false;
        }
        if matches!(self.stop_policy, StopPolicy::RunTerminal) {
            return context.ready_for_self_cleanup(&record);
        }
        if !record.status.is_terminal() || now < self.next_receipt_check {
            return false;
        }
        self.next_receipt_check = now + PARENT_RECEIPT_POLL;
        // Once per record revision: a fleet with a running sibling stamps nothing,
        // and the last sibling to settle reports for the whole fleet.
        if record.report_message_id.is_none()
            && record.joined_at.is_none()
            && self.reported_revision != Some(record.updated_at)
        {
            self.reported_revision = Some(record.updated_at);
            report_settled_child_or_log(context);
        }
        if !context.ready_for_self_cleanup(&record) {
            return false;
        }
        match context.parent_received_and_rested(&record) {
            Ok(ready) => ready,
            Err(error) => {
                tracing::debug!(run_id = %context.run_id, %error, "could not read subagent output receipt");
                false
            }
        }
    }

    /// The strand settle exits a park that is owed nothing; a settled fleet
    /// whose digest was lost is owed something no one else will deliver, since
    /// the child's reporter does not retry and `orphan_sweep`'s backstop needs
    /// a live sidebar producer. So the parked run repairs its own fleet before
    /// each strand check. The repair is idempotent and its stamp CAS tolerates
    /// a concurrent reporter, so every outcome and error is logged and ignored:
    /// it never gates the check that follows it. A park that launched nobody
    /// costs one projection read, since the reporter answers a launcher with no
    /// members without listing the runs.
    fn repair_digest(&self, context: &RunExecContext, record: &rimz::store::run::RunRecord) {
        let Some(agent_id) = record.agent_id.as_ref() else {
            return;
        };
        match super::subagent_report::report_fleet(&context.workspace, &context.store, agent_id) {
            Ok(outcome) => {
                tracing::debug!(run_id = %context.run_id, ?outcome, "repaired a parked run's fleet digest");
            }
            Err(error) => {
                tracing::debug!(run_id = %context.run_id, %error, "could not repair a parked run's fleet digest");
            }
        }
    }
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

pub(super) fn exec_launch_identity(
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
pub(super) fn exec_attach_target(
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

fn record_own_launch_pane(invocation: &ExecInvocationContext<'_>, identity: &LaunchIdentity) {
    let Some(pane_id) = rimz::mux::ambient_pane_id() else {
        return;
    };
    let workspace = invocation.workspace;
    let cwd = std::env::current_dir().unwrap_or_else(|_| workspace.worktree_root.clone());
    match invocation.store().and_then(|store| {
        store.bind_agent_launch(identity, &workspace.session_name, &cwd, &pane_id)?;
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
            &workspace.session_name,
            &pane_id,
            rimz::store::runtime::current_process_owner(
                rimz::pane::RuntimeOwnerKind::Agent,
                current.agent_id.as_str(),
            ),
            None,
            invocation.effective_isolation,
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
    isolation: Option<rimz::config::Isolation>,
) {
    let Some(pane_id) = rimz::mux::ambient_pane_id() else {
        return;
    };
    let workspace = invocation.workspace;
    if let Err(err) = invocation.store().and_then(|store| {
        store.attach_agent_pane(
            &target.0,
            &target.1,
            launch_id.as_ref(),
            &workspace.session_name,
            &pane_id,
            runtime_owner,
            isolation,
            invocation.effective_isolation,
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
    let cwd = std::env::current_dir().unwrap_or_else(|_| workspace.worktree_root.clone());
    if let Err(err) = invocation.store().and_then(|store| {
        store.fail_agent_launch(identity, &workspace.session_name, &cwd)?;
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
    if !launch_is_still_provisional(invocation, identity) {
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
                "rimz.agent-ended",
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
        for agent in &mut projection.agents {
            if agent.ended_at.is_some()
                || agent.is_provider_subagent()
                || !matches!(
                    agent.status,
                    rimz::agents::AgentStatus::Running | rimz::agents::AgentStatus::Waiting
                )
            {
                continue;
            }
            agent.context = rimz::store::agent_context::read_one(
                store.runtime_paths(),
                agent.kind.as_str(),
                agent.agent_id.as_str(),
            )
            .map(|record| record.context);
        }
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

fn launch_is_still_provisional(
    invocation: &ExecInvocationContext<'_>,
    identity: &LaunchIdentity,
) -> bool {
    match invocation
        .store()
        .and_then(|store| store.snapshot_cached().map_err(Into::into))
    {
        Ok(snapshot) => snapshot.agents.iter().any(|agent| {
            agent.kind == identity.kind
                && agent.agent_id == identity.agent_id
                && agent.name.as_deref() == Some(identity.name.as_str())
        }),
        Err(err) => {
            tracing::debug!(
                agent_name = %identity.name,
                error = %err,
                "could not inspect launch card before marking failure",
            );
            true
        }
    }
}

pub(super) fn fail_run_if_child_exited_first(
    context: &RunExecContext,
    globals: &GlobalFlags,
    terminal_grace: Duration,
) {
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
    fail_run_if_nonterminal(
        context,
        "agent process exited before supervised run reached a terminal state",
    );
}

fn fail_run_on_exec_precondition(context: Option<&RunExecContext>) {
    let Some(context) = context else {
        return;
    };
    fail_run_if_nonterminal(context, "agent exec precondition failed");
}

fn fail_run_if_nonterminal(context: &RunExecContext, reason: &'static str) {
    match rimz::harness::run::fail_if_nonterminal(context.store.paths(), &context.run_id) {
        Ok(Some(record)) => rimz::store::run::wake_run(context.store.runtime_paths(), &record),
        Ok(None) => {}
        Err(err) => tracing::debug!(
            run_id = %context.run_id,
            error = %err,
            reason,
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
    let Some(tail) = supervised::pane::capture_failure_tail(backend.as_ref(), &own) else {
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

struct ExecOutcome {
    status: ExitStatus,
    abrupt: bool,
    parent_ended: bool,
    parent_watchdog: Option<rimz::harness::parent_watch::ParentWatch>,
}

fn supervise_child(
    child: Child,
    run_monitor: Option<&RunExecContext>,
    self_cleanup: bool,
    stop_policy: StopPolicy,
    parent_watchdog: Option<rimz::harness::parent_watch::ParentWatch>,
) -> Result<ExecOutcome> {
    let (wake_tx, wake_rx) = mpsc::channel();
    let mut child = rimz::child_process::SupervisedChild::adopt(child, wake_tx.clone());
    #[cfg(unix)]
    let cleanup_signals = {
        use signal_hook::consts::signal::{SIGHUP, SIGTERM};
        vec![SIGHUP, SIGTERM]
    };
    #[cfg(not(unix))]
    let cleanup_signals = Vec::new();
    rimz::child_process::register_signal_wake(cleanup_signals, wake_tx)
        .context("registering cleanup signal wakeups")?;

    let mut signal_seen_at = cleanup_signal_received().then(Instant::now);
    let mut term_sent_at: Option<Instant> = None;
    let mut kill_sent = false;
    let mut run_completed = false;
    let mut parent_ended = false;
    let mut next_run_check = Instant::now();
    let mut monitor_state = RunMonitor {
        self_cleanup,
        stop_policy,
        next_receipt_check: next_run_check,
        reported_revision: None,
        next_park_check: next_run_check,
        previous: None,
    };
    loop {
        let now = Instant::now();
        match child.try_wait() {
            Ok(Some(status)) => {
                return Ok(ExecOutcome {
                    status,
                    abrupt: run_completed
                        || parent_ended
                        || signal_seen_at.is_some()
                        || cleanup_signal_received(),
                    parent_ended,
                    parent_watchdog,
                });
            }
            Ok(None) => {}
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err).context("waiting for agent process"),
        }

        if !parent_ended
            && parent_watchdog
                .as_ref()
                .is_some_and(|watchdog| watchdog.parent_ended())
        {
            parent_ended = true;
            if let Some(monitor) = run_monitor
                && let Err(err) =
                    rimz::harness::run::cancel_and_wake(&monitor.store, &monitor.run_id)
            {
                tracing::debug!(
                    run_id = %monitor.run_id,
                    error = &err as &dyn std::error::Error,
                    "could not cancel subagent run after parent exit",
                );
            }
            child.signal_term();
            term_sent_at = Some(now);
        }

        if !run_completed
            && let Some(monitor) = run_monitor
            && now >= next_run_check
        {
            next_run_check = now
                + if self_cleanup {
                    RUN_MONITOR_POLL
                } else {
                    PARK_STRAND_POLL
                };
            if monitor_state.poll(monitor, now) {
                run_completed = true;
                child.signal_term();
                term_sent_at = Some(now);
            }
        }

        if cleanup_signal_received() {
            let first_seen = *signal_seen_at.get_or_insert(now);
            if term_sent_at.is_none() && now.duration_since(first_seen) >= CHILD_SIGNAL_GRACE {
                child.signal_term();
                term_sent_at = Some(now);
            }
        }
        if let Some(sent_at) = term_sent_at
            && !kill_sent
            && now.duration_since(sent_at) >= CHILD_SIGNAL_GRACE
        {
            child.signal_kill();
            kill_sent = true;
        }

        let deadline = [
            (!run_completed && run_monitor.is_some()).then_some(next_run_check),
            signal_seen_at
                .filter(|_| term_sent_at.is_none())
                .map(|seen_at| seen_at + CHILD_SIGNAL_GRACE),
            term_sent_at
                .filter(|_| !kill_sent)
                .map(|sent_at| sent_at + CHILD_SIGNAL_GRACE),
        ]
        .into_iter()
        .flatten()
        .min();
        rimz::child_process::wait_wake(&wake_rx, deadline);
    }
}

fn subagent_parent_watchdog(
    request: &rimz::harness::launch::ExecRequest,
    run_context: Option<&RunExecContext>,
    launch_identity: Option<&LaunchIdentity>,
    keep: bool,
) -> Option<rimz::harness::parent_watch::ParentWatch> {
    if !request.subagent {
        return None;
    }
    let Some(context) = run_context else {
        tracing::debug!("subagent parent watchdog has no supervised run context");
        return None;
    };
    if keep {
        return None;
    }
    let Some(identity) = launch_identity else {
        tracing::debug!("subagent parent watchdog has no launch identity");
        return None;
    };
    Some(
        rimz::harness::parent_watch::ParentWatchdog::new(
            context.store.clone(),
            identity.kind.clone(),
            identity.agent_id.clone(),
            rimz::mux::ambient_pane_id(),
            context.session_name.clone(),
        )
        .start(),
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
mod tests {
    use super::*;

    /// A resumed session is bound to exactly one pane at a time, and the
    /// replacement stamps that binding before it spawns its provider. The
    /// exiting wrapper's argv-identity fallback therefore reads the binding to
    /// tell itself apart from a replacement that already took the session
    /// over: superseded, it ends nothing and retires nothing.
    #[test]
    fn a_session_bound_to_another_pane_supersedes_the_exiting_wrapper() {
        let kind = AgentKind::new_unchecked("claude");
        let session = AgentSessionId::from("resumed");
        let pane = |id: &str| rimz::ids::PaneId::parse(id).expect("normalized pane id");
        let bound = |id: Option<&str>| {
            let mut agent = rimz::testkit::agent_state("claude", "resumed", jiff::Timestamp::now());
            agent.pane = id.map(|id| rimz::pane::PaneRef::from_id(pane(id)));
            agent
        };
        let own = pane("tmux:%1");

        assert!(
            session_bound_to_another_pane(&[bound(Some("tmux:%2"))], &kind, &session, Some(&own)),
            "the replacement's pane supersedes this wrapper"
        );
        assert!(
            !session_bound_to_another_pane(&[bound(Some("tmux:%1"))], &kind, &session, Some(&own)),
            "the wrapper's own binding is not a supersession"
        );
        assert!(
            !session_bound_to_another_pane(&[bound(None)], &kind, &session, Some(&own)),
            "an unbound session leaves the fallback to decide"
        );
        assert!(
            !session_bound_to_another_pane(&[], &kind, &session, Some(&own)),
            "no row for the session is no evidence of a replacement"
        );
        assert!(
            session_bound_to_another_pane(&[bound(Some("tmux:%1"))], &kind, &session, None),
            "a wrapper with no pane of its own can claim no binding"
        );
        assert!(
            !session_bound_to_another_pane(
                &[bound(Some("tmux:%2"))],
                &AgentKind::new_unchecked("codex"),
                &session,
                Some(&own)
            ),
            "another provider's binding on the same session id is not this one"
        );
    }

    /// The reporter's workspace, scoped to a fixture's own tempdir.
    fn test_workspace(root: &std::path::Path) -> rimz::ResolvedWorkspace {
        rimz::ResolvedWorkspace {
            workspace_id: rimz::WorkspaceId::from_project_root(root),
            project_root: root.to_owned(),
            cwd_project_root: None,
            root_class: rimz::workspace::RootClass::Directory,
            worktree_root: root.to_owned(),
            worktree_branch: None,
            session_name: "room".to_owned(),
            mux_hint: None,
        }
    }

    #[test]
    fn monitor_settles_stranded_park_with_and_without_self_cleanup() {
        for self_cleanup in [false, true] {
            let state = tempfile::tempdir().unwrap();
            let workspace_id = rimz::WorkspaceId::from_project_root(state.path());
            let paths = rimz::StatePaths::under(workspace_id.clone(), state.path()).unwrap();
            let runtime =
                rimz::RuntimePaths::under(workspace_id.clone(), &state.path().join("rt")).unwrap();
            let store = rimz::Store::open(paths, runtime).unwrap();
            let mut record = rimz::store::run::RunRecord::new(
                workspace_id,
                AgentKind::new_unchecked("claude"),
                PermissionMode::Auto,
                "check".to_owned(),
                state.path().to_owned(),
            );
            record.status = rimz::store::run::RunStatus::Running;
            record.agent_id = Some("session".into());
            record.parked_at = Some(jiff::Timestamp::now());
            rimz::harness::run::create(store.paths(), &record).unwrap();
            let context = RunExecContext {
                run_id: record.run_id.clone(),
                store,
                session_name: "room".to_owned(),
                workspace: test_workspace(state.path()),
            };
            let now = Instant::now();
            let mut monitor = RunMonitor {
                self_cleanup,
                stop_policy: StopPolicy::RunTerminal,
                next_receipt_check: now,
                reported_revision: None,
                next_park_check: now,
                previous: None,
            };
            assert!(!monitor.poll(&context, now));
            assert_eq!(monitor.previous, record.parked_at);
            assert!(!monitor.poll(&context, now + RUN_MONITOR_POLL));
            assert!(
                !context.is_terminal(),
                "record ticks must not accelerate strand checks"
            );
            assert!(!monitor.poll(&context, now + PARK_STRAND_POLL));
            let failed = context.load_record().unwrap();
            assert_eq!(failed.status, rimz::store::run::RunStatus::Failed);
            assert!(failed.failure_tail.is_some());
            assert_eq!(failed.parked_at, None);
            assert_eq!(monitor.previous, None);
            assert_eq!(
                monitor.poll(&context, now + PARK_STRAND_POLL + RUN_MONITOR_POLL),
                self_cleanup
            );
        }
    }

    /// The one park nothing else can end is a settled fleet whose digest was
    /// lost: the child's reporter swallowed its error and the parent is at
    /// rest waiting for exactly that digest. The parked wrapper therefore
    /// repairs the fleet before each strand check, and the run lives to
    /// complete on the digest's turn instead of failing stranded.
    #[test]
    fn parked_monitor_repairs_a_lost_fleet_digest_instead_of_failing() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = rimz::WorkspaceId::from_project_root(dir.path());
        let paths =
            rimz::StatePaths::under(workspace_id.clone(), &dir.path().join("state")).unwrap();
        let runtime =
            rimz::RuntimePaths::under(workspace_id.clone(), &dir.path().join("rt")).unwrap();
        let store = rimz::Store::open(paths, runtime).unwrap();
        let kind = AgentKind::new_unchecked("codex");
        let register = |name: &str, pane: &str, parent: Option<&str>| {
            let mut observation = rimz::agents::AgentLifecycleObservation::new(
                Some(AgentSessionId::from(name)),
                rimz::agents::LifecycleSignal::Registered,
            );
            observation.agent_name = Some(name.to_owned());
            observation.pane_id = Some(rimz::ids::PaneId::parse(pane).unwrap());
            if let Some(parent) = parent {
                observation.launch.parent_agent_id = Some(AgentSessionId::from(parent));
                observation.launch.parent_agent_kind = Some(kind.clone());
                observation.launch.launch_depth = Some(1);
            }
            store
                .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
                    session_name: "room",
                    agent_kind: kind.clone(),
                    event_name: "test",
                    observation: &observation,
                    spawned_subagents: &[],
                })
                .unwrap();
        };
        register("parent", "tmux:%1", None);
        register("child", "tmux:%2", Some("parent"));

        let run = |session: &str, status: rimz::store::run::RunStatus| {
            let mut record = rimz::store::run::RunRecord::new(
                workspace_id.clone(),
                kind.clone(),
                PermissionMode::Auto,
                "work".to_owned(),
                dir.path().to_owned(),
            );
            record.status = status;
            record.agent_id = Some(AgentSessionId::from(session));
            record.agent_name = Some(session.to_owned());
            record
        };
        let mut parent_run = run("parent", rimz::store::run::RunStatus::Running);
        parent_run.parked_at = Some(jiff::Timestamp::now());
        let mut child_run = run("child", rimz::store::run::RunStatus::Completed);
        child_run.subagent = true;
        for record in [&parent_run, &child_run] {
            rimz::harness::run::create(store.paths(), record).unwrap();
        }
        let parent_id = AgentSessionId::from("parent");
        assert_eq!(
            rimz::harness::owed::owed_wake(&store, &kind, &parent_id).unwrap(),
            Some(rimz::harness::owed::OwedWake::Subagents),
            "a settled child nobody reported holds the park",
        );

        let context = RunExecContext {
            run_id: parent_run.run_id.clone(),
            store,
            session_name: "room".to_owned(),
            workspace: test_workspace(dir.path()),
        };
        let now = Instant::now();
        let mut monitor = RunMonitor {
            self_cleanup: false,
            stop_policy: StopPolicy::RunTerminal,
            next_receipt_check: now,
            reported_revision: None,
            next_park_check: now,
            previous: None,
        };
        assert!(!monitor.poll(&context, now));

        assert_eq!(
            rimz::harness::owed::owed_wake(&context.store, &kind, &parent_id).unwrap(),
            Some(rimz::harness::owed::OwedWake::WakeInFlight),
            "the repair turns the owed fleet into a digest in flight",
        );
        assert_eq!(context.store.list_messages().unwrap().len(), 1);
        let parked = context.load_record().unwrap();
        assert_eq!(parked.status, rimz::store::run::RunStatus::Running);
        assert_eq!(parked.parked_at, parent_run.parked_at);
        assert_eq!(
            monitor.previous, None,
            "a repaired park is live, not stranded"
        );
    }

    #[test]
    fn terminal_child_waits_for_its_receiving_parent_turn() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = test_workspace(dir.path());
        let paths = rimz::StatePaths::under(workspace.workspace_id.clone(), dir.path()).unwrap();
        let runtime =
            rimz::RuntimePaths::under(workspace.workspace_id.clone(), &dir.path().join("rt"))
                .unwrap();
        let store = rimz::Store::open(paths, runtime).unwrap();
        let kind = AgentKind::new_unchecked("codex");
        let observe = |name: &str, signal| {
            let mut observation =
                rimz::agents::AgentLifecycleObservation::new(Some(name.into()), signal);
            observation.agent_name = Some(name.into());
            observation.pane_id = Some(
                rimz::ids::PaneId::parse(if name == "child" {
                    "tmux:%2"
                } else {
                    "tmux:%1"
                })
                .unwrap(),
            );
            if name == "child" {
                observation.launch.parent_agent_id = Some("parent".into());
                observation.launch.parent_agent_kind = Some(kind.clone());
                observation.launch.launch_depth = Some(1);
            }
            store
                .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
                    session_name: "room",
                    agent_kind: kind.clone(),
                    event_name: "test",
                    observation: &observation,
                    spawned_subagents: &[],
                })
                .unwrap();
        };
        use rimz::agents::LifecycleSignal;
        let ended = || LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        };
        observe("parent", LifecycleSignal::Registered);
        observe("child", LifecycleSignal::Registered);
        observe("child", ended());
        let mut record = rimz::store::run::RunRecord::new(
            workspace.workspace_id.clone(),
            kind.clone(),
            PermissionMode::Auto,
            "work".into(),
            dir.path().into(),
        );
        record.agent_id = Some("child".into());
        record.agent_name = Some("child".into());
        record.subagent = true;
        record.status = rimz::store::run::RunStatus::Completed;
        rimz::harness::run::create(store.paths(), &record).unwrap();
        let context = RunExecContext {
            run_id: record.run_id.clone(),
            store: store.clone(),
            session_name: "room".into(),
            workspace,
        };
        let mut now = Instant::now();
        let mut monitor = RunMonitor {
            self_cleanup: true,
            stop_policy: StopPolicy::ParentReceived,
            next_receipt_check: now,
            reported_revision: None,
            next_park_check: now,
            previous: None,
        };
        assert!(
            !monitor.poll(&context, now),
            "unreceived output holds the child"
        );
        let reported = context.load_record().unwrap();
        assert!(
            reported.report_message_id.is_some(),
            "report before provider exit"
        );
        record = reported;
        let digest = store
            .list_messages()
            .unwrap()
            .into_iter()
            .find(|message| Some(&message.message_id) == record.report_message_id.as_ref())
            .unwrap();
        store.record_sent_batch(&[digest], "room").unwrap();
        observe("parent", LifecycleSignal::TurnStarted { turn_id: None });
        store
            .confirm_delivered_for_card(
                &kind,
                &"parent".into(),
                Some("parent"),
                rimz::store::writer::DeliveryAck::TurnStarted { prompt: None },
                "room",
            )
            .unwrap();
        now += Duration::from_secs(1);
        assert!(!monitor.poll(&context, now));
        observe("parent", ended());
        now += Duration::from_secs(1);
        assert!(monitor.poll(&context, now));
        record.report_message_id = None;
        record.joined_at = Some(jiff::Timestamp::now());
        rimz::harness::run::create(store.paths(), &record).unwrap();
        observe("child", LifecycleSignal::TurnStarted { turn_id: None });
        now += Duration::from_secs(1);
        assert!(!monitor.poll(&context, now));
        observe("child", ended());
        observe("parent", LifecycleSignal::Ended);
        now += Duration::from_secs(1);
        assert!(monitor.poll(&context, now));
        let child = store
            .snapshot_cached()
            .unwrap()
            .agents
            .into_iter()
            .find(|agent| agent.agent_id.as_str() == "child")
            .unwrap();
        let message = rimz::store::message::MessageRecord::new(
            record.workspace_id.clone(),
            &child,
            "follow up".into(),
            rimz::store::message::DeliveryGate::Done,
        );
        store.queue_message(&message, "room").unwrap();
        now += Duration::from_secs(1);
        assert!(
            !monitor.poll(&context, now),
            "queued follow-up holds the child"
        );
        store.record_sent_batch(&[message], "room").unwrap();
        now += Duration::from_secs(1);
        assert!(
            !monitor.poll(&context, now),
            "pane send before TurnStarted still holds the child"
        );
    }

    #[test]
    fn terminal_self_cleanup_defers_to_waiter_and_survives_rearm() {
        let state = tempfile::tempdir().unwrap();
        let runtime_root = tempfile::tempdir_in("/tmp").unwrap();
        let workspace_id = rimz::WorkspaceId::from_project_root(state.path());
        let paths = rimz::StatePaths::under(workspace_id.clone(), state.path()).unwrap();
        let runtime = rimz::RuntimePaths::under(workspace_id.clone(), runtime_root.path()).unwrap();
        paths.ensure_dirs().unwrap();
        runtime.ensure_dirs().unwrap();
        let mut record = rimz::store::run::RunRecord::new(
            workspace_id.clone(),
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "check".to_owned(),
            state.path().to_owned(),
        );
        record.status = rimz::store::run::RunStatus::Completed;
        rimz::harness::run::create(&paths, &record).unwrap();
        let context = RunExecContext {
            run_id: record.run_id.clone(),
            store: rimz::Store::open(paths, runtime).unwrap(),
            session_name: "room".to_owned(),
            workspace: test_workspace(state.path()),
        };
        assert!(
            context.ready_for_self_cleanup(&context.load_record().unwrap()),
            "background run has no waiter"
        );
        let waiter = rimz::harness::run_wake::RunWaiter::bind(
            context.store.runtime_paths(),
            rimz::harness::run_wake::ExpectedRunFrame {
                workspace_id,
                run_id: record.run_id.clone(),
            },
            rimz::harness::run::RunCancellation::new(),
        )
        .unwrap();
        assert!(
            !context.ready_for_self_cleanup(&context.load_record().unwrap()),
            "live waiter owns verification and evidence capture"
        );
        record.status = rimz::store::run::RunStatus::Running;
        rimz::harness::run::create(context.store.paths(), &record).unwrap();
        drop(waiter);
        assert!(
            !context.ready_for_self_cleanup(&context.load_record().unwrap()),
            "rearmed run remains active even without a waiter"
        );
        record.status = rimz::store::run::RunStatus::Completed;
        rimz::harness::run::create(context.store.paths(), &record).unwrap();
        assert!(
            context.ready_for_self_cleanup(&context.load_record().unwrap()),
            "terminal run is reclaimed once waiter leaves"
        );
    }
}
