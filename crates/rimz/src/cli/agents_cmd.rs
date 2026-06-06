//! `rimz agents` — launcher sugar plus the hidden supervised exec wrapper.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use super::{GlobalFlags, RoomTarget};
use rimz::mux::{TabOptions, own_pane_id};
use rimz::tab_layout::{Cell, LayoutSpec};
use rimz::workspace::WorkspaceResolver;

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
        let layout = LayoutSpec::single(Cell::Agent(adapter.descriptor().kind_id()));
        let title = rimz::tab_layout::default_tab_title(&layout, &cwd);
        let room = RoomTarget {
            workspace_id: &workspace.workspace_id,
            project_root: &workspace.project_root,
            session_name: &workspace.session_name,
            cwd: &cwd,
            mux_config: &mux_config,
            width,
            detected_size,
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
    let adapter = rimz::agents::find_adapter(&args.kind)
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", args.kind))?;
    let argv = adapter
        .launch_command(args.prompt.as_deref())
        .ok_or_else(|| anyhow::anyhow!("agent `{}` has no launch command", args.kind))?;
    let (program, rest) = argv
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("agent `{}` produced an empty launch command", args.kind))?;
    let status = Command::new(program)
        .args(rest)
        .status()
        .with_context(|| format!("running {program}"))?;

    if let Some(path) = args.worktree_path.as_deref()
        && let Err(err) = cleanup_worktree(path, globals)
    {
        let _ = writeln!(
            std::io::stderr().lock(),
            "rimz: worktree cleanup skipped: {err}"
        );
    }
    std::process::exit(status.code().unwrap_or(1));
}

fn cleanup_worktree(path: &Path, globals: &GlobalFlags) -> Result<()> {
    let Some(marker) = rimz::worktree::read_marker_for_worktree(path)? else {
        return Ok(());
    };
    let status = rimz::worktree::status(path, &marker.base_ref)?;
    let other_pane_inside = other_live_pane_inside(path, globals);
    match rimz::worktree::cleanup_decision(status, true, other_pane_inside) {
        rimz::worktree::CleanupDecision::RemoveClean => {
            rimz::worktree::remove_marked_worktree(&marker.repo_root, path, &marker, false)?;
            let _ = writeln!(
                std::io::stderr().lock(),
                "rimz: removed clean worktree {}",
                path.display()
            );
        }
        rimz::worktree::CleanupDecision::PromptDirty => match dirty_choice(path)? {
            DirtyChoice::Keep => {}
            DirtyChoice::Remove => {
                rimz::worktree::remove_marked_worktree(&marker.repo_root, path, &marker, true)?;
            }
            DirtyChoice::Shell => exec_shell(path)?,
        },
        rimz::worktree::CleanupDecision::Skip => {}
    }
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
    panes.into_iter().any(|pane| {
        pane.pane_id != own
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
