use super::launch::*;
use super::*;
use crate::cli::{open_ledger, worktree};

pub(super) fn run_exec(args: ExecArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())
        .context("resolving the agent launch workspace")?;
    let run_context = run_exec_context(&args, &workspace)?;
    let launch_identity = exec_launch_identity(&args)?;
    if let Some(context) = run_context.as_ref() {
        record_own_run_pane(context);
    }
    if let Some(identity) = launch_identity.as_ref() {
        record_own_launch_pane(&workspace, identity, args.prompt.as_deref());
    }
    let adapter = rimz::agents::find_adapter(&args.kind)
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", args.kind))?;
    let argv = match args.resume.as_deref() {
        Some(session_id) => {
            let cwd = std::env::current_dir().context("reading the resume pane cwd")?;
            adapter
                .resume_command(session_id, &cwd)
                .ok_or_else(|| anyhow::anyhow!("agent `{}` has no resume command", args.kind))?
        }
        None => adapter
            .launch_command(&args.extra_args, args.prompt.as_deref())
            .ok_or_else(|| anyhow::anyhow!("agent `{}` has no launch command", args.kind))?,
    };
    let rimz_env = full_agent_launch_env(
        &workspace.project_root,
        adapter,
        AgentLaunchEnvIdentity {
            run_id: args.run_id.as_ref(),
            agent_name: args.agent_name.as_deref(),
            agent_profile: args.agent_profile.as_deref(),
            agent_role: args.agent_role.as_deref(),
            agent_model: args.agent_model.as_deref(),
            agent_effort: args.agent_effort.as_deref(),
        },
    )?;
    let argv = rimz::launch::login_shell_argv(&rimz_env, &argv);
    let (program, rest) = argv
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("agent `{}` produced an empty launch command", args.kind))?;
    if should_exec_agent_directly(&args) {
        match exec_agent_command(program, rest, &rimz_env) {
            Ok(()) => return Ok(()),
            Err(err) => {
                if let Some(identity) = launch_identity.as_ref()
                    && launch_is_still_provisional(&workspace, identity)
                {
                    record_launch_failed(&workspace, identity, args.prompt.as_deref());
                }
                return Err(err);
            }
        }
    }
    reset_cleanup_signal_flag();
    reset_term_signal_flag();
    install_cleanup_signal_handlers().context("installing cleanup signal handlers")?;
    let mut command = Command::new(program);
    command.args(rest);
    command.envs(&rimz_env);
    let child = command
        .spawn()
        .with_context(|| format!("running {program}"))?;
    let monitor = if args.exit_on_run_completion {
        Some(
            run_context
                .as_ref()
                .context("--exit-on-run-completion requires --run-id")?,
        )
    } else {
        None
    };
    let outcome = supervise_child(child, monitor).context("supervising agent process")?;
    if let Some(context) = run_context.as_ref() {
        fail_run_if_child_exited_first(context, RUN_EXIT_TERMINAL_GRACE);
    }
    if !outcome.status.success()
        && let Some(identity) = launch_identity.as_ref()
        && launch_is_still_provisional(&workspace, identity)
    {
        record_launch_failed(&workspace, identity, args.prompt.as_deref());
    }

    if let Some(path) = args.worktree_path.as_deref()
        && let Err(err) = cleanup_worktree_via_ondisk(path, globals, !outcome.signaled)
    {
        let _ = writeln!(
            std::io::stderr().lock(),
            "rimz: worktree cleanup did not complete: {err}"
        );
    }
    if should_record_end_trace(&args, term_signal_received()) {
        record_own_agent_end_trace(&workspace, &args);
    }
    if args.close_pane_on_exit {
        let session_name = run_context
            .as_ref()
            .map(|context| context.session_name.as_str())
            .unwrap_or(&workspace.session_name);
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

pub(super) fn should_record_end_trace(args: &ExecArgs, term_seen: bool) -> bool {
    !args.exit_on_run_completion && !term_seen
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
        match rimz::run::load(&self.paths, &self.run_id) {
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
    let ledger = open_ledger(workspace).context("opening supervised run ledger")?;
    Ok(Some(RunExecContext {
        run_id,
        paths: ledger.paths().clone(),
        runtime: ledger.runtime_paths().clone(),
        session_name: workspace.session_name.clone(),
    }))
}

fn exec_launch_identity(args: &ExecArgs) -> Result<Option<LaunchIdentity>> {
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
                profile: args.agent_profile.clone(),
                role: args.agent_role.clone(),
                run_id: args.run_id.clone(),
            }))
        }
    }
}

fn record_own_run_pane(context: &RunExecContext) {
    let Some(pane_id) = rimz::mux::ambient_pane_id() else {
        return;
    };
    if let Err(err) = rimz::run::record_pane(&context.paths, &context.run_id, pane_id.clone()) {
        tracing::debug!(
            run_id = %context.run_id,
            pane = %pane_id,
            error = %err,
            "could not persist supervised run pane id",
        );
    }
}

fn record_own_launch_pane(
    workspace: &rimz::ResolvedWorkspace,
    identity: &LaunchIdentity,
    prompt: Option<&str>,
) {
    let Some(pane_id) = rimz::mux::ambient_pane_id() else {
        return;
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| workspace.worktree_root.clone());
    match open_ledger(workspace).and_then(|ledger| {
        append_launch_event(
            &ledger,
            workspace,
            identity,
            LaunchEventParams {
                cwd: &cwd,
                worktree_name: None,
                prompt,
                state: rimz::schema::event::AgentLaunchState::Bound,
                pane_id: Some(pane_id.clone()),
            },
        )
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

fn record_launch_failed(
    workspace: &rimz::ResolvedWorkspace,
    identity: &LaunchIdentity,
    prompt: Option<&str>,
) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| workspace.worktree_root.clone());
    if let Err(err) = open_ledger(workspace).and_then(|ledger| {
        append_launch_event(
            &ledger,
            workspace,
            identity,
            LaunchEventParams {
                cwd: &cwd,
                worktree_name: None,
                prompt,
                state: rimz::schema::event::AgentLaunchState::Failed,
                pane_id: None,
            },
        )
    }) {
        tracing::debug!(
            agent_name = %identity.name,
            error = %err,
            "could not mark provisional agent launch failed",
        );
    }
}

fn record_own_agent_end_trace(workspace: &rimz::ResolvedWorkspace, args: &ExecArgs) {
    match resolve_own_agent_end_trace(workspace, args) {
        Ok(Some((kind, agent_id))) => append_agent_end_trace(workspace, kind, agent_id),
        Ok(None) => tracing::debug!("agent exit produced no pane binding to tombstone"),
        Err(err) => tracing::debug!(
            error = %err,
            "could not resolve agent exit tombstone",
        ),
    }
}

fn resolve_own_agent_end_trace(
    workspace: &rimz::ResolvedWorkspace,
    args: &ExecArgs,
) -> Result<Option<(AgentKind, AgentSessionId)>> {
    if let Some(pane_id) = rimz::mux::ambient_pane_id() {
        let ledger = open_ledger(workspace).context("opening ledger for agent exit tombstone")?;
        let projection = ledger
            .runtime_projection(rimz::RuntimeScope::Audit)
            .context("reading audit projection for agent exit tombstone")?;
        let pane = rimz::feed::PaneRef::from_id(pane_id);
        if let Some(agent) =
            rimz::ledger::snapshot::stamped_agent_for_pane(&pane, &projection.agents)
            && !agent.agent_id.is_empty()
        {
            return Ok(Some((agent.kind.clone(), agent.agent_id.clone())));
        }
    }
    Ok(args.resume.as_ref().map(|session_id| {
        (
            AgentKind::new_unchecked(args.kind.clone()),
            AgentSessionId::from(session_id.as_str()),
        )
    }))
}

fn append_agent_end_trace(
    workspace: &rimz::ResolvedWorkspace,
    kind: AgentKind,
    agent_id: AgentSessionId,
) {
    let appended = (|| -> Result<()> {
        let ledger = open_ledger(workspace).context("opening ledger for agent exit tombstone")?;
        let observation = rimz::agents::AgentLifecycleObservation::new(
            Some(agent_id.clone()),
            rimz::agents::LifecycleSignal::Ended,
        );
        let event = rimz::EventEnvelope::agent_lifecycle(
            workspace.workspace_id.clone(),
            &workspace.session_name,
            kind.as_str(),
            "rimz.agent-ended",
            &observation,
        );
        ledger.append_event(&event)?;
        Ok(())
    })();
    if let Err(err) = appended {
        tracing::warn!(
            kind = %kind,
            agent_id = %agent_id,
            error = %err,
            "could not record agent exit tombstone",
        );
    }
}

fn launch_is_still_provisional(
    workspace: &rimz::ResolvedWorkspace,
    identity: &LaunchIdentity,
) -> bool {
    match open_ledger(workspace).and_then(|ledger| ledger.snapshot_cached().map_err(Into::into)) {
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

pub(super) fn fail_run_if_child_exited_first(context: &RunExecContext, terminal_grace: Duration) {
    if wait_for_terminal_run(context, terminal_grace) {
        return;
    }
    match rimz::run::load(&context.paths, &context.run_id) {
        Ok(record) if record.status.is_terminal() => {}
        Ok(_) => match rimz::run::fail_if_nonterminal(&context.paths, &context.run_id) {
            Ok(Some(record)) => {
                if let Err(err) = rimz::ledger::wakeup::wake_run(&context.runtime, &record) {
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
                "could not mark supervised run failed after agent process exit",
            ),
        },
        Err(err) => tracing::debug!(
            run_id = %context.run_id,
            error = %err,
            "could not inspect supervised run after agent process exit",
        ),
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
    signaled: bool,
}

fn supervise_child(mut child: Child, run_monitor: Option<&RunExecContext>) -> Result<ExecOutcome> {
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
                    signaled: run_completed
                        || signal_seen_at.is_some()
                        || cleanup_signal_received(),
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
                signal_child(child.id(), ChildSignal::Term);
                term_sent_at = Some(now);
            }
        }

        if cleanup_signal_received() {
            let first_seen = *signal_seen_at.get_or_insert(now);
            if term_sent_at.is_none() && now.duration_since(first_seen) >= CHILD_SIGNAL_GRACE {
                signal_child(child.id(), ChildSignal::Term);
                term_sent_at = Some(now);
            }
        }
        if let Some(sent_at) = term_sent_at
            && !kill_sent
            && now.duration_since(sent_at) >= CHILD_SIGNAL_GRACE
        {
            signal_child(child.id(), ChildSignal::Kill);
            kill_sent = true;
        }

        std::thread::sleep(CHILD_WAIT_POLL);
    }
}

fn reset_cleanup_signal_flag() {
    cleanup_signal_flag().store(false, Ordering::SeqCst);
}

fn reset_term_signal_flag() {
    term_signal_flag().store(false, Ordering::SeqCst);
}

fn cleanup_signal_received() -> bool {
    cleanup_signal_flag().load(Ordering::SeqCst)
}

fn term_signal_received() -> bool {
    term_signal_flag().load(Ordering::SeqCst)
}

fn cleanup_signal_flag() -> &'static Arc<AtomicBool> {
    CLEANUP_SIGNAL_RECEIVED.get_or_init(|| Arc::new(AtomicBool::new(false)))
}

fn term_signal_flag() -> &'static Arc<AtomicBool> {
    TERM_SIGNAL_RECEIVED.get_or_init(|| Arc::new(AtomicBool::new(false)))
}

#[cfg(unix)]
fn install_cleanup_signal_handlers() -> Result<()> {
    use signal_hook::consts::signal::{SIGHUP, SIGTERM};

    for signal in [SIGHUP, SIGTERM] {
        signal_hook::flag::register(signal, cleanup_signal_flag().clone())?;
    }
    signal_hook::flag::register(SIGTERM, term_signal_flag().clone())?;
    Ok(())
}

#[cfg(not(unix))]
fn install_cleanup_signal_handlers() -> Result<()> {
    Ok(())
}

#[derive(Clone, Copy, Debug)]
enum ChildSignal {
    Term,
    Kill,
}

#[cfg(unix)]
fn signal_child(pid: u32, signal: ChildSignal) {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;

    let signal = match signal {
        ChildSignal::Term => Signal::SIGTERM,
        ChildSignal::Kill => Signal::SIGKILL,
    };
    let _ = kill(Pid::from_raw(pid as i32), signal);
}

#[cfg(not(unix))]
fn signal_child(_pid: u32, _signal: ChildSignal) {}

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
