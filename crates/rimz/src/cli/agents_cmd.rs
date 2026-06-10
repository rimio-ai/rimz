//! `rimz agents` — launcher sugar plus the hidden supervised exec wrapper.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use super::{GlobalFlags, RoomTarget};
use rimz::mux::{TabOptions, own_pane_id};
use rimz::tab_layout::{Cell, LayoutSpec};
use rimz::workspace::WorkspaceResolver;

const CHILD_SIGNAL_GRACE: Duration = Duration::from_millis(300);
const CHILD_WAIT_POLL: Duration = Duration::from_millis(25);
const RUN_MONITOR_POLL: Duration = Duration::from_millis(250);
const RUN_EXIT_TERMINAL_GRACE: Duration = Duration::from_millis(500);
static CLEANUP_SIGNAL_RECEIVED: OnceLock<Arc<AtomicBool>> = OnceLock::new();

#[derive(Debug, Args)]
pub struct AgentsArgs {
    #[command(subcommand)]
    command: Option<AgentsSubcmd>,
    /// Agent kind to launch. Each kind opens in its own tab/window.
    #[arg(value_name = "KIND")]
    kinds: Vec<String>,
    /// Use Rimz-owned worktrees. Bare flag creates one fresh worktree per agent; NAME is shared.
    #[arg(long, value_name = "NAME", num_args = 0..=1, default_missing_value = "")]
    worktree: Option<String>,
    /// Prompt broadcast to every launched agent.
    #[arg(long)]
    prompt: Option<String>,
    /// Open tabs/windows without moving focus to them.
    #[arg(long)]
    no_focus: bool,
}

#[derive(Debug, Subcommand)]
enum AgentsSubcmd {
    /// Hidden wrapper used inside launched agent panes.
    #[command(hide = true)]
    Exec(ExecArgs),
}

#[derive(Debug, Args)]
struct ExecArgs {
    kind: String,
    #[arg(long)]
    run_id: Option<rimz::RunId>,
    #[arg(long, hide = true)]
    exit_on_run_completion: bool,
    #[arg(long, hide = true)]
    close_pane_on_exit: bool,
    #[arg(long)]
    worktree_path: Option<PathBuf>,
    #[arg(long)]
    prompt: Option<String>,
    #[arg(last = true)]
    extra_args: Vec<String>,
}

pub fn run(args: AgentsArgs, globals: &GlobalFlags) -> Result<()> {
    if let Some(command) = args.command {
        return match command {
            AgentsSubcmd::Exec(exec) => run_exec(exec, globals),
        };
    }
    if args.kinds.is_empty() {
        bail!("expected at least one agent kind");
    }
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())
        .context("resolving current workspace")?;
    let machine_config = super::machine_config();
    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    let backend = rimz::mux::backend_for(mux);
    super::tab::ensure_live_session(backend.as_ref(), &workspace.session_name)?;
    super::record_workspace(&workspace)?;

    let mux_config = rimz::config::MultiplexerConfig::from(&machine_config);
    let width = rimz::mux::SidebarWidth::from_config(&machine_config.sidebar);
    let detected_size = rimz::mux::detect_terminal_size();
    for kind in args.kinds {
        let adapter = rimz::agents::find_adapter(&kind)
            .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{kind}`"))?;
        let launch = super::tab::resolve_cwd(
            &workspace,
            &machine_config.worktree,
            args.worktree.as_deref(),
        )?;
        let cwd = launch.cwd;
        let layout = LayoutSpec::single(Cell::agent(adapter.descriptor().kind_id()));
        let title = rimz::tab_layout::default_tab_title(&layout, &cwd);
        let room = RoomTarget {
            workspace_id: &workspace.workspace_id,
            project_root: &workspace.project_root,
            session_name: &workspace.session_name,
            cwd: &cwd,
            mux_config: &mux_config,
            width,
            detected_size,
            refresh_ms: None,
        };
        let sidebar = super::build_sidebar_opts(&room, Vec::new())?;
        let panes = super::tab::layout_panes(
            &layout,
            &cwd,
            args.prompt.as_deref(),
            args.worktree.is_some(),
        )?;
        backend.open_tab(&TabOptions {
            session_name: workspace.session_name.clone(),
            title,
            cwd,
            panes,
            focus: !args.no_focus,
            sidebar,
        })?;
    }
    Ok(())
}

fn run_exec(args: ExecArgs, globals: &GlobalFlags) -> Result<()> {
    if args.worktree_path.is_some() {
        reset_cleanup_signal_flag();
        install_cleanup_signal_handlers().context("installing cleanup signal handlers")?;
    }
    let run_context = run_exec_context(&args, globals)?;
    if let Some(context) = run_context.as_ref() {
        record_own_run_pane(context);
    }
    let adapter = rimz::agents::find_adapter(&args.kind)
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", args.kind))?;
    let argv = adapter
        .launch_command(&args.extra_args, args.prompt.as_deref())
        .ok_or_else(|| anyhow::anyhow!("agent `{}` has no launch command", args.kind))?;
    let (program, rest) = argv
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("agent `{}` produced an empty launch command", args.kind))?;
    let mut command = Command::new(program);
    command.args(rest);
    if let Some(run_id) = args.run_id.as_ref() {
        command.env(rimz::run::ENV_RUN_ID, run_id.as_str());
    }
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

    if let Some(path) = args.worktree_path.as_deref()
        && let Err(err) = cleanup_worktree_via_ondisk(path, globals, !outcome.signaled)
    {
        let _ = writeln!(
            std::io::stderr().lock(),
            "rimz: worktree cleanup did not complete: {err}"
        );
    }
    if args.close_pane_on_exit
        && let Some(context) = run_context.as_ref()
    {
        close_own_pane(globals, &context.session_name);
    }
    std::process::exit(outcome.status.code().unwrap_or(1));
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
        return super::worktree::cleanup_worktree(path, globals, interactive);
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
            super::worktree::cleanup_worktree(path, globals, interactive)
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
struct RunExecContext {
    run_id: rimz::RunId,
    paths: rimz::StatePaths,
    runtime: rimz::RuntimePaths,
    session_name: String,
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

fn run_exec_context(args: &ExecArgs, globals: &GlobalFlags) -> Result<Option<RunExecContext>> {
    if args.exit_on_run_completion && args.run_id.is_none() {
        bail!("--exit-on-run-completion requires --run-id");
    }
    let Some(run_id) = args.run_id.clone() else {
        return Ok(None);
    };
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())
        .context("resolving supervised run workspace")?;
    let ledger = super::open_ledger(&workspace).context("opening supervised run ledger")?;
    Ok(Some(RunExecContext {
        run_id,
        paths: ledger.paths().clone(),
        runtime: ledger.runtime_paths().clone(),
        session_name: workspace.session_name,
    }))
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

fn fail_run_if_child_exited_first(context: &RunExecContext, terminal_grace: Duration) {
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

fn cleanup_signal_received() -> bool {
    cleanup_signal_flag().load(Ordering::SeqCst)
}

fn cleanup_signal_flag() -> &'static Arc<AtomicBool> {
    CLEANUP_SIGNAL_RECEIVED.get_or_init(|| Arc::new(AtomicBool::new(false)))
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use rimz::bridge::{ExpectedRunFrame, RunWakeOutcome};
    use rimz::ids::{AgentKind, WorkspaceId};
    use rimz::run::{PermissionMode, RunRecord, RunStatus};

    #[derive(Debug, Parser)]
    struct ExecHarness {
        #[command(subcommand)]
        command: AgentsSubcmd,
    }

    #[test]
    fn exec_subcommand_captures_agent_args_after_separator() {
        let parsed = ExecHarness::try_parse_from([
            "rimz",
            "exec",
            "codex",
            "--run-id",
            "run_0123456789abcdef0123456789abcdef",
            "--exit-on-run-completion",
            "--close-pane-on-exit",
            "--worktree-path",
            "/x",
            "--prompt",
            "hi",
            "--",
            "--model",
            "gpt-5-codex",
        ])
        .expect("parse exec");

        let AgentsSubcmd::Exec(args) = parsed.command;
        assert_eq!(args.kind, "codex");
        assert_eq!(
            args.run_id.as_ref().map(rimz::RunId::as_str),
            Some("run_0123456789abcdef0123456789abcdef")
        );
        assert!(args.exit_on_run_completion);
        assert!(args.close_pane_on_exit);
        assert_eq!(args.worktree_path, Some(PathBuf::from("/x")));
        assert_eq!(args.prompt.as_deref(), Some("hi"));
        assert_eq!(args.extra_args, ["--model", "gpt-5-codex"]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn child_exit_marks_nonterminal_run_failed_and_wakes_waiter() {
        let state = tempfile::tempdir().expect("state dir");
        let runtime_root = tempfile::tempdir().expect("runtime dir");
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let paths = rimz::StatePaths::under(workspace_id.clone(), state.path()).expect("paths");
        let runtime =
            rimz::RuntimePaths::under(workspace_id.clone(), runtime_root.path()).expect("runtime");
        paths.ensure_dirs().expect("state dirs");
        runtime.ensure_dirs().expect("runtime dirs");
        let record = RunRecord::new(
            workspace_id.clone(),
            AgentKind::new_unchecked("codex"),
            PermissionMode::Auto,
            "summarize".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        let run_id = record.run_id.clone();
        rimz::run::create(&paths, &record).expect("create run");
        let (sock, _sock_path) = rimz::bridge::bind_run(&runtime, &run_id).expect("bind run");
        let context = RunExecContext {
            run_id: run_id.clone(),
            paths: paths.clone(),
            runtime,
            session_name: "rimz-test".to_owned(),
        };

        fail_run_if_child_exited_first(&context, Duration::ZERO);

        let failed = rimz::run::load(&paths, &run_id).expect("load failed run");
        assert_eq!(failed.status, RunStatus::Failed);
        let outcome = rimz::bridge::wait_for_run_completion_owning(
            sock,
            ExpectedRunFrame {
                workspace_id,
                run_id,
            },
            Some(Duration::from_secs(1)),
        )
        .await
        .expect("run wait");
        assert_eq!(outcome, RunWakeOutcome::Completed(RunStatus::Failed));
    }
}
