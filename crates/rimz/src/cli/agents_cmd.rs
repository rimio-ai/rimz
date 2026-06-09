//! `rimz agents` — launcher sugar plus the hidden supervised exec wrapper.

use std::io::{IsTerminal, Write};
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
const CLEANUP_SIGNAL_ROSTER_GRACE: Duration = Duration::from_millis(300);
const CHILD_WAIT_POLL: Duration = Duration::from_millis(25);
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
    let adapter = rimz::agents::find_adapter(&args.kind)
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", args.kind))?;
    let argv = adapter
        .launch_command(&args.extra_args, args.prompt.as_deref())
        .ok_or_else(|| anyhow::anyhow!("agent `{}` has no launch command", args.kind))?;
    let (program, rest) = argv
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("agent `{}` produced an empty launch command", args.kind))?;
    let child = Command::new(program)
        .args(rest)
        .spawn()
        .with_context(|| format!("running {program}"))?;
    let outcome = supervise_child(child).context("supervising agent process")?;

    if let Some(path) = args.worktree_path.as_deref()
        && let Err(err) = cleanup_worktree(path, globals, !outcome.signaled)
    {
        let _ = writeln!(
            std::io::stderr().lock(),
            "rimz: worktree cleanup did not complete: {err}"
        );
    }
    std::process::exit(outcome.status.code().unwrap_or(1));
}

#[derive(Debug)]
struct ExecOutcome {
    status: ExitStatus,
    signaled: bool,
}

fn supervise_child(mut child: Child) -> Result<ExecOutcome> {
    let mut signal_seen_at = cleanup_signal_received().then(Instant::now);
    let mut term_sent_at: Option<Instant> = None;
    let mut kill_sent = false;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Ok(ExecOutcome {
                    status,
                    signaled: signal_seen_at.is_some() || cleanup_signal_received(),
                });
            }
            Ok(None) => {}
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err).context("waiting for agent process"),
        }

        if cleanup_signal_received() {
            let now = Instant::now();
            let first_seen = *signal_seen_at.get_or_insert(now);
            if now.duration_since(first_seen) >= CHILD_SIGNAL_GRACE {
                match term_sent_at {
                    None => {
                        signal_child(child.id(), ChildSignal::Term);
                        term_sent_at = Some(now);
                    }
                    Some(sent_at)
                        if !kill_sent && now.duration_since(sent_at) >= CHILD_SIGNAL_GRACE =>
                    {
                        signal_child(child.id(), ChildSignal::Kill);
                        kill_sent = true;
                    }
                    Some(_) => {}
                }
            }
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

fn cleanup_worktree(path: &Path, globals: &GlobalFlags, interactive: bool) -> Result<()> {
    let Some(marker) = rimz::worktree::read_marker_for_worktree(path)? else {
        return Ok(());
    };
    let status = rimz::worktree::status(path, &marker.base_ref)?;
    if !interactive {
        std::thread::sleep(CLEANUP_SIGNAL_ROSTER_GRACE);
    }
    let other_pane_inside = other_live_pane_inside(path, globals);
    match rimz::worktree::cleanup_decision(status, true, other_pane_inside) {
        rimz::worktree::CleanupDecision::RemoveClean => {
            remove_after_leaving_worktree(path, &marker, false)?;
            let _ = writeln!(
                std::io::stderr().lock(),
                "rimz: removed clean worktree {}",
                path.display()
            );
        }
        rimz::worktree::CleanupDecision::PromptDirty => {
            if interactive {
                match dirty_choice(path)? {
                    DirtyChoice::Keep => {}
                    DirtyChoice::Remove => {
                        remove_after_leaving_worktree(path, &marker, true)?;
                    }
                    DirtyChoice::Shell => exec_shell(path)?,
                }
            }
        }
        rimz::worktree::CleanupDecision::Skip => {}
    }
    Ok(())
}

fn remove_after_leaving_worktree(
    path: &Path,
    marker: &rimz::worktree::WorktreeMarker,
    force: bool,
) -> Result<()> {
    std::env::set_current_dir(&marker.repo_root)
        .with_context(|| format!("leaving worktree before removing {}", path.display()))?;
    rimz::worktree::remove_marked_worktree(&marker.repo_root, path, marker, force)?;
    Ok(())
}

fn other_live_pane_inside(path: &Path, globals: &GlobalFlags) -> bool {
    let Ok(mux) = rimz::mux::auto_detect_backend(globals.mux) else {
        return false;
    };
    let Some(own) = own_pane_id(mux) else {
        return false;
    };
    let backend = rimz::mux::backend_for(mux);
    let Ok(panes) = backend.list_panes(rimz::mux::PaneListOptions::default()) else {
        return false;
    };
    other_live_user_pane_inside(&panes, &own, path)
}

fn other_live_user_pane_inside<'a>(
    panes: impl IntoIterator<Item = &'a rimz::feed::PaneRef>,
    own: &rimz::PaneId,
    path: &Path,
) -> bool {
    panes.into_iter().any(|pane| {
        &pane.pane_id != own
            && !pane.is_rimz_sidebar()
            && pane
                .cwd
                .as_deref()
                .map(Path::new)
                .is_some_and(|cwd| rimz::worktree::path_inside(cwd, path))
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirtyChoice {
    Keep,
    Remove,
    Shell,
}

fn dirty_choice(path: &Path) -> Result<DirtyChoice> {
    if !std::io::stdin().is_terminal() {
        return Ok(DirtyChoice::Keep);
    }
    let mut stderr = std::io::stderr().lock();
    writeln!(
        stderr,
        "rimz: worktree {} has local changes or commits.",
        path.display()
    )?;
    write!(stderr, "Choose keep/remove/shell [keep]: ")?;
    stderr.flush()?;
    drop(stderr);
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return Ok(DirtyChoice::Keep);
    }
    Ok(match answer.trim() {
        "remove" | "r" => DirtyChoice::Remove,
        "shell" | "s" => DirtyChoice::Shell,
        _ => DirtyChoice::Keep,
    })
}

#[cfg(unix)]
fn exec_shell(path: &Path) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_owned());
    let err = Command::new(&shell).current_dir(path).exec();
    Err::<(), _>(err).with_context(|| format!("execing {shell}"))
}

#[cfg(not(unix))]
fn exec_shell(path: &Path) -> Result<()> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_owned());
    let status = Command::new(&shell)
        .current_dir(path)
        .status()
        .with_context(|| format!("running {shell}"))?;
    if !status.success() {
        bail!("shell exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use rimz::{MuxName, PaneId};

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
        assert_eq!(args.worktree_path, Some(PathBuf::from("/x")));
        assert_eq!(args.prompt.as_deref(), Some("hi"));
        assert_eq!(args.extra_args, ["--model", "gpt-5-codex"]);
    }

    #[test]
    fn other_live_user_pane_inside_ignores_sidebar_and_own_pane() {
        let worktree = Path::new("/repo-worktrees/demo");
        let own = PaneId::from_parts(MuxName::Zellij, "terminal_own");
        let panes = vec![
            pane("terminal_side", Some("rimz-sidebar"), Some(worktree)),
            pane("terminal_outside", Some("zsh"), Some(Path::new("/repo"))),
            pane("terminal_own", Some("codex"), Some(worktree)),
        ];

        assert!(
            !other_live_user_pane_inside(&panes, &own, worktree),
            "sidebar, outside pane, and own pane do not pin cleanup"
        );
    }

    #[test]
    fn other_live_user_pane_inside_counts_agent_or_shell_under_worktree() {
        let worktree = Path::new("/repo-worktrees/demo");
        let shell_dir = worktree.join("src");
        let own = PaneId::from_parts(MuxName::Zellij, "terminal_own");
        let agent = vec![pane("terminal_agent", Some("codex"), Some(worktree))];
        let shell = vec![pane("terminal_shell", Some("zsh"), Some(&shell_dir))];

        assert!(other_live_user_pane_inside(&agent, &own, worktree));
        assert!(other_live_user_pane_inside(&shell, &own, worktree));
    }

    fn pane(raw: &str, command: Option<&str>, cwd: Option<&Path>) -> rimz::feed::PaneRef {
        rimz::feed::PaneRef {
            command: command.map(ToOwned::to_owned),
            cwd: cwd.map(|path| path.display().to_string()),
            ..rimz::feed::PaneRef::from_id(PaneId::from_parts(MuxName::Zellij, raw))
        }
    }
}
