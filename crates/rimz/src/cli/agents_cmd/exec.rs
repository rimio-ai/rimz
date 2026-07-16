use super::*;
use crate::cli::{open_store, worktree};
use std::sync::mpsc;

pub(super) fn run_exec(args: ExecArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())
        .context("resolving the agent launch workspace")?;
    let run_context = run_exec_context(&args, &workspace)?;
    let launch_params = launch_params(&args);
    let launch_identity = exec_launch_identity(&args, &launch_params)?;
    let action = exec_action(&args);
    let entered_worktree = match args.worktree_path.as_deref() {
        Some(path) => match enter_worktree(path) {
            Ok(path) => Some(path),
            Err(err) => {
                mark_launch_failed_if_provisional(&workspace, launch_identity.as_ref());
                fail_run_on_exec_precondition(run_context.as_ref());
                return Err(err);
            }
        },
        None => None,
    };
    let machine_config = crate::cli::machine_config();
    let provider_cwd = match action {
        rimz::harness::launch::ExecAction::Fork { .. } => {
            std::env::current_dir().context("reading the fork pane cwd")?
        }
        rimz::harness::launch::ExecAction::Resume { .. } => {
            std::env::current_dir().context("reading the resume pane cwd")?
        }
        rimz::harness::launch::ExecAction::Launch { .. } => entered_worktree
            .clone()
            .unwrap_or_else(|| workspace.worktree_root.clone()),
    };
    let exec_invocation = exec_invocation(&args, action, &launch_params);
    let process = rimz::harness::launch::compile_agent_process(
        &workspace.project_root,
        machine_config.harness.rtk,
        &exec_invocation,
        &provider_cwd,
    )?;
    if let Some(context) = run_context.as_ref() {
        record_own_run_pane(context);
    }
    if let Some(identity) = launch_identity.as_ref() {
        record_own_launch_pane(&workspace, identity);
    }
    let (program, rest) = process
        .argv
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("agent `{}` produced an empty launch command", args.kind))?;
    if should_exec_agent_directly(&args) {
        match exec_agent_command(program, rest, &process.env) {
            Ok(()) => return Ok(()),
            Err(err) => {
                mark_launch_failed_if_provisional(&workspace, launch_identity.as_ref());
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
    if let Some(path) = entered_worktree.as_deref() {
        command.current_dir(path);
    }
    let child = command
        .spawn()
        .with_context(|| format!("running {program}"))?;
    let monitor = if args.exit_on_run_completion {
        run_context.as_ref()
    } else {
        None
    };
    let outcome = supervise_child(child, monitor).context("supervising agent process")?;
    settle_after_exit(
        &args,
        globals,
        &workspace,
        run_context.as_ref(),
        launch_identity.as_ref(),
        entered_worktree.as_deref(),
        outcome,
    )
}

pub(super) fn exec_action(args: &ExecArgs) -> rimz::harness::launch::ExecAction<'_> {
    match (args.fork.as_deref(), args.resume.as_deref()) {
        (Some(session_id), _) => rimz::harness::launch::ExecAction::Fork {
            session_id,
            extra_args: &args.extra_args,
        },
        (None, Some(session_id)) => rimz::harness::launch::ExecAction::Resume {
            session_id,
            extra_args: &args.extra_args,
        },
        (None, None) => rimz::harness::launch::ExecAction::Launch {
            prompt: args.prompt.as_deref(),
            extra_args: &args.extra_args,
        },
    }
}

pub(super) fn exec_invocation<'a>(
    args: &'a ExecArgs,
    action: rimz::harness::launch::ExecAction<'a>,
    params: &'a rimz::agents::LaunchParams,
) -> rimz::harness::launch::ExecInvocation<'a> {
    rimz::harness::launch::ExecInvocation {
        kind: &args.kind,
        action,
        run_id: args.run_id.as_ref().map(|run_id| run_id.as_str()),
        worktree_path: args.worktree_path.as_deref(),
        close_pane_on_exit: args.close_pane_on_exit,
        exit_on_run_completion: args.exit_on_run_completion,
        identity: rimz::harness::launch::ExecIdentity {
            name: args.agent_name.as_deref(),
            name_explicit: args.agent_name_explicit,
            params: Some(params),
            ..rimz::harness::launch::ExecIdentity::default()
        },
    }
}

fn settle_after_exit(
    args: &ExecArgs,
    globals: &GlobalFlags,
    workspace: &rimz::ResolvedWorkspace,
    run_context: Option<&RunExecContext>,
    launch_identity: Option<&LaunchIdentity>,
    entered_worktree: Option<&Path>,
    outcome: ExecOutcome,
) -> ! {
    if let Some(context) = run_context {
        fail_run_if_child_exited_first(context, globals, RUN_EXIT_TERMINAL_GRACE);
    }
    let startup_failure =
        !outcome.status.success() && mark_launch_failed_if_provisional(workspace, launch_identity);

    let session_name = run_context
        .map(|context| context.session_name.as_str())
        .unwrap_or(&workspace.session_name);
    let abrupt = outcome.abrupt || cleanup_signal_received();
    let session_accepts_close = !abrupt || session_accepts_agent_close(globals, session_name);
    let deliberate = close_is_deliberate(abrupt, session_accepts_close);
    if deliberate && should_record_end_trace(args) {
        record_own_agent_end_trace(workspace, args);
    }
    if should_drop_to_shell(args, abrupt) {
        // The trace above stamps the agent ended; gc reclaims any worktree later.
        drop_to_shell_after_agent_exit(args, &outcome.status, startup_failure);
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
    if args.close_pane_on_exit {
        close_own_pane(globals, session_name);
    }
    std::process::exit(outcome.status.code().unwrap_or(1));
}

pub(super) fn should_exec_agent_directly(args: &ExecArgs) -> bool {
    cfg!(unix)
        && args.run_id.is_none()
        && args.worktree_path.is_none()
        && !args.exit_on_run_completion
        && !args.close_pane_on_exit
}

pub(super) fn should_record_end_trace(args: &ExecArgs) -> bool {
    !args.exit_on_run_completion
}

pub(super) fn should_drop_to_shell(args: &ExecArgs, abrupt: bool) -> bool {
    (args.close_pane_on_exit || args.worktree_path.is_some()) && args.run_id.is_none() && !abrupt
}

pub(super) fn relaunch_command(args: &ExecArgs) -> String {
    match (
        args.agent_team.as_deref(),
        args.agent_role.as_deref(),
        args.agent_profile.as_deref(),
    ) {
        (Some(team), Some(role), _) => format!("rimz agents {team}.{role}"),
        (_, _, Some(profile)) => format!("rimz agents {profile}"),
        _ => format!("rimz agents {}", args.kind),
    }
}

pub(super) fn exit_hint(
    kind: &str,
    status: &ExitStatus,
    startup_failure: bool,
    relaunch: &str,
) -> String {
    if startup_failure {
        format!("rimz: agent `{kind}` failed to start ({status}); relaunch with `{relaunch}`\r\n")
    } else {
        format!("rimz: agent `{kind}` exited ({status}); relaunch with `{relaunch}`\r\n")
    }
}

#[cfg(unix)]
fn drop_to_shell_after_agent_exit(args: &ExecArgs, status: &ExitStatus, startup_failure: bool) {
    use std::os::unix::process::CommandExt;

    let hint = exit_hint(&args.kind, status, startup_failure, &relaunch_command(args));
    let _ = write!(std::io::stderr().lock(), "{hint}");
    let shell = rimz::harness::launch::user_shell_program();
    let err = Command::new(&shell).exec();
    tracing::debug!(shell = %shell, error = %err, "could not exec idle shell after agent exit");
}

#[cfg(not(unix))]
fn drop_to_shell_after_agent_exit(_args: &ExecArgs, _status: &ExitStatus, _startup_failure: bool) {}

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
    Ok(rimz::worktree::normalize_path_lexical(&path))
}

#[cfg(unix)]
fn exec_agent_command(
    program: &str,
    rest: &[String],
    env: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    use std::os::unix::process::CommandExt;

    let mut command = Command::new(program);
    command.args(rest);
    command.envs(env);
    let err = command.exec();
    Err(err).with_context(|| format!("running {program}"))
}

#[cfg(not(unix))]
fn exec_agent_command(
    _program: &str,
    _rest: &[String],
    _env: &std::collections::BTreeMap<String, String>,
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

#[derive(Clone, Debug)]
pub(super) struct RunExecContext {
    pub(super) run_id: rimz::RunId,
    pub(super) paths: rimz::StatePaths,
    pub(super) runtime: rimz::RuntimePaths,
    pub(super) session_name: String,
}

impl RunExecContext {
    fn is_terminal(&self) -> bool {
        match rimz::harness::run::load(&self.paths, &self.run_id) {
            Ok(record) => record.status.is_terminal(),
            Err(err) => {
                tracing::debug!(
                    run_id = %self.run_id,
                    error = %err,
                    "could not read supervised run record while monitoring pane",
                );
                false
            }
        }
    }
}

fn run_exec_context(
    args: &ExecArgs,
    workspace: &rimz::ResolvedWorkspace,
) -> Result<Option<RunExecContext>> {
    if args.exit_on_run_completion && args.run_id.is_none() {
        bail!("--exit-on-run-completion requires --run-id");
    }
    let Some(run_id) = args.run_id.clone() else {
        return Ok(None);
    };
    let store = open_store(workspace).context("opening supervised run store")?;
    Ok(Some(RunExecContext {
        run_id,
        paths: store.paths().clone(),
        runtime: store.runtime_paths().clone(),
        session_name: workspace.session_name.clone(),
    }))
}

pub(super) fn exec_launch_identity(
    args: &ExecArgs,
    params: &rimz::agents::LaunchParams,
) -> Result<Option<LaunchIdentity>> {
    match (args.launch_id.as_deref(), args.agent_name.as_deref()) {
        (None, None) => Ok(None),
        (Some(_), None) => bail!("--launch-id requires --agent-name"),
        (None, Some(_)) => Ok(None),
        (Some(launch_id), Some(name)) => {
            validate_agent_name(name)?;
            Ok(Some(LaunchIdentity {
                kind: AgentKind::new_unchecked(args.kind.clone()),
                agent_id: AgentSessionId::from(launch_id),
                name: name.to_owned(),
                name_explicit: args.agent_name_explicit,
                launch: params.clone(),
                run_id: args.run_id.clone(),
                prompt: args.prompt.clone(),
            }))
        }
    }
}

pub(super) fn launch_params(args: &ExecArgs) -> rimz::agents::LaunchParams {
    rimz::agents::LaunchParams {
        profile: args.agent_profile.clone(),
        mode: args.agent_mode,
        role: args.agent_role.clone(),
        model: args.agent_model.clone(),
        effort: args.agent_effort.clone(),
        budget: args.agent_budget.clone(),
        team: args.agent_team.clone(),
        launch_group: args.launch_group.clone(),
        launch_ordinal: args.launch_ordinal,
        channel: args.agent_channel.clone(),
        kind_ordinal: None,
    }
}

fn record_own_run_pane(context: &RunExecContext) {
    let Some(pane_id) = rimz::mux::ambient_pane_id() else {
        return;
    };
    if let Err(err) =
        rimz::harness::run::record_pane(&context.paths, &context.run_id, pane_id.clone())
    {
        tracing::debug!(
            run_id = %context.run_id,
            pane = %pane_id,
            error = %err,
            "could not persist supervised run pane id",
        );
    }
}

fn record_own_launch_pane(workspace: &rimz::ResolvedWorkspace, identity: &LaunchIdentity) {
    let Some(pane_id) = rimz::mux::ambient_pane_id() else {
        return;
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| workspace.worktree_root.clone());
    match open_store(workspace).and_then(|store| {
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

fn record_launch_failed(workspace: &rimz::ResolvedWorkspace, identity: &LaunchIdentity) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| workspace.worktree_root.clone());
    if let Err(err) = open_store(workspace).and_then(|store| {
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
    workspace: &rimz::ResolvedWorkspace,
    identity: Option<&LaunchIdentity>,
) -> bool {
    let Some(identity) = identity else {
        return false;
    };
    if !launch_is_still_provisional(workspace, identity) {
        return false;
    }
    record_launch_failed(workspace, identity);
    true
}

fn record_own_agent_end_trace(workspace: &rimz::ResolvedWorkspace, args: &ExecArgs) {
    match resolve_own_agent_end_trace(workspace, args) {
        Ok(Some((kind, agent_id))) => append_agent_lifecycle_trace(
            workspace,
            kind,
            agent_id,
            rimz::agents::LifecycleSignal::Ended,
            "rimz.agent-ended",
            "agent exit end stamp",
        ),
        Ok(None) => tracing::debug!("agent exit produced no pane binding to stamp ended"),
        Err(err) => tracing::debug!(
            error = %err,
            "could not resolve agent exit end stamp",
        ),
    }
}

fn resolve_own_agent_end_trace(
    workspace: &rimz::ResolvedWorkspace,
    args: &ExecArgs,
) -> Result<Option<(AgentKind, AgentSessionId)>> {
    if let Some(pane_id) = rimz::mux::ambient_pane_id() {
        let store = open_store(workspace).context("opening store for agent exit end stamp")?;
        let projection = store
            .runtime_projection(rimz::RuntimeScope::Audit)
            .context("reading audit projection for agent exit end stamp")?;
        let pane = rimz::pane::PaneRef::from_id(pane_id);
        if let Some(agent) =
            rimz::store::snapshot::stamped_agent_for_pane(&pane, &projection.agents)
            && !agent.agent_id.is_empty()
        {
            return Ok(Some((agent.kind.clone(), agent.agent_id.clone())));
        }
    }
    // A resumed pane owns the resumed session and can safely fall back to its
    // argv id. A fork's provider-assigned id is unknown here; falling back to
    // the source id would stamp the original session ended when the fork exits.
    Ok(args.resume.as_ref().map(|session_id| {
        (
            AgentKind::new_unchecked(args.kind.clone()),
            AgentSessionId::from(session_id.as_str()),
        )
    }))
}

fn append_agent_lifecycle_trace(
    workspace: &rimz::ResolvedWorkspace,
    kind: AgentKind,
    agent_id: AgentSessionId,
    signal: rimz::agents::LifecycleSignal,
    event_name: &'static str,
    label: &'static str,
) {
    let appended = (|| -> Result<()> {
        let store = open_store(workspace).context("opening store for agent lifecycle trace")?;
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
    workspace: &rimz::ResolvedWorkspace,
    identity: &LaunchIdentity,
) -> bool {
    match open_store(workspace).and_then(|store| store.snapshot_cached().map_err(Into::into)) {
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
    match rimz::harness::run::fail_if_nonterminal(&context.paths, &context.run_id) {
        Ok(Some(record)) => {
            if let Err(err) = rimz::store::wakeup::wake_run(&context.runtime, &record) {
                tracing::debug!(
                    run_id = %context.run_id,
                    error = %err,
                    "could not wake supervised run waiter after agent process exit",
                );
            }
        }
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
        rimz::harness::run::record_failure_tail(&context.paths, &context.run_id, &tail)
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

#[derive(Debug)]
struct ExecOutcome {
    status: ExitStatus,
    abrupt: bool,
}

fn supervise_child(child: Child, run_monitor: Option<&RunExecContext>) -> Result<ExecOutcome> {
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
    let mut next_run_check = Instant::now();
    loop {
        let now = Instant::now();
        match child.try_wait() {
            Ok(Some(status)) => {
                return Ok(ExecOutcome {
                    status,
                    abrupt: run_completed || signal_seen_at.is_some() || cleanup_signal_received(),
                });
            }
            Ok(None) => {}
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err).context("waiting for agent process"),
        }

        if !run_completed
            && let Some(monitor) = run_monitor
            && now >= next_run_check
        {
            next_run_check = now + RUN_MONITOR_POLL;
            if monitor.is_terminal() {
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
