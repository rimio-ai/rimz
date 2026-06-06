//! `rimz tab` — open one laid-out tab/window in the current room.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args;

use super::{GlobalFlags, RoomTarget};
use rimz::mux::{LayoutPanes, PaneCmd, TabOptions};
use rimz::tab_layout::{Cell, LayoutSpec};
use rimz::workspace::{RootClass, WorkspaceResolver};

#[derive(Debug, Args)]
pub struct TabArgs {
    /// Layout name or inline spec (`claude,codex+term`).
    #[arg(long)]
    layout: Option<String>,
    /// Use a Rimz-owned worktree. Bare flag creates a fresh one; NAME reuses or creates it.
    #[arg(long, value_name = "NAME", num_args = 0..=1, default_missing_value = "")]
    worktree: Option<String>,
    /// Tab/window title.
    #[arg(long)]
    name: Option<String>,
    /// Prompt passed to agent cells.
    #[arg(long)]
    prompt: Option<String>,
    /// Open without moving focus to the new tab/window.
    #[arg(long)]
    no_focus: bool,
}

pub fn run(args: TabArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())
        .context("resolving current workspace")?;
    let machine_config = super::machine_config();
    let layout =
        rimz::tab_layout::resolve_layout(args.layout.as_deref(), &machine_config.agents.layouts)?;
    let launch = resolve_cwd(
        &workspace,
        &machine_config.worktree,
        args.worktree.as_deref(),
    )?;
    let title = args
        .name
        .unwrap_or_else(|| default_tab_title(&layout, &launch));
    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    let backend = rimz::mux::backend_for(mux);
    ensure_live_session(backend.as_ref(), &workspace.session_name)?;
    super::record_workspace(&workspace)?;

    let mux_config = rimz::config::MultiplexerConfig::from(&machine_config);
    let width = rimz::mux::SidebarWidth::from_config(&machine_config.sidebar);
    let detected_size = rimz::mux::detect_terminal_size();
    let room = RoomTarget {
        workspace_id: &workspace.workspace_id,
        project_root: &workspace.project_root,
        session_name: &workspace.session_name,
        cwd: &launch.cwd,
        mux_config: &mux_config,
        width,
        detected_size,
    };
    let sidebar = super::build_sidebar_opts(&room, Vec::new())?;
    let panes = layout_panes(
        &layout,
        &launch.cwd,
        args.prompt.as_deref(),
        args.worktree.is_some(),
    )?;
    backend.open_tab(&TabOptions {
        session_name: workspace.session_name,
        title,
        cwd: launch.cwd,
        panes,
        focus: !args.no_focus,
        sidebar,
    })?;
    Ok(())
}

pub(crate) fn resolve_cwd(
    workspace: &rimz::ResolvedWorkspace,
    config: &rimz::config::WorktreeConfig,
    worktree: Option<&str>,
) -> Result<ResolvedCwd> {
    let Some(raw_name) = worktree else {
        return Ok(ResolvedCwd {
            cwd: workspace.worktree_root.clone(),
            worktree_name: None,
        });
    };
    if workspace.root_class != RootClass::Repo {
        bail!("--worktree requires a git repository-backed room");
    }
    let name = raw_name.trim();
    let created = rimz::worktree::create(
        &workspace.project_root,
        config,
        (!name.is_empty()).then_some(name),
        None,
        None,
        !name.is_empty(),
    )?;
    Ok(ResolvedCwd {
        cwd: created.path,
        worktree_name: Some(created.name),
    })
}

pub(crate) struct ResolvedCwd {
    pub(crate) cwd: PathBuf,
    pub(crate) worktree_name: Option<String>,
}

fn default_tab_title(layout: &LayoutSpec, launch: &ResolvedCwd) -> String {
    if let Some(name) = launch.worktree_name.as_deref() {
        format!("⑂ {name}")
    } else {
        tab_title_name(&launch.cwd)
            .unwrap_or_else(|| rimz::tab_layout::default_tab_title(layout, &launch.cwd))
    }
}

fn tab_title_name(cwd: &Path) -> Option<String> {
    cwd.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
}

pub(crate) fn layout_panes(
    layout: &LayoutSpec,
    cwd: &Path,
    prompt: Option<&str>,
    cleanup_worktree: bool,
) -> Result<LayoutPanes> {
    let rimz_bin = std::env::current_exe().context("locating the rimz executable")?;
    let columns = layout
        .columns
        .iter()
        .map(|column| {
            column
                .rows
                .iter()
                .map(|cell| pane_cmd(cell, &rimz_bin, cwd, prompt, cleanup_worktree))
                .collect::<Result<Vec<_>>>()
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(LayoutPanes { columns })
}

fn pane_cmd(
    cell: &Cell,
    rimz_bin: &Path,
    cwd: &Path,
    prompt: Option<&str>,
    cleanup_worktree: bool,
) -> Result<PaneCmd> {
    let argv = match cell {
        Cell::Term => vec![std::env::var("SHELL").unwrap_or_else(|_| "sh".to_owned())],
        Cell::Agent(kind) => {
            let mut argv = vec![
                rimz_bin.to_string_lossy().into_owned(),
                "agents".to_owned(),
                "exec".to_owned(),
                kind.as_str().to_owned(),
            ];
            if cleanup_worktree {
                argv.extend([
                    "--worktree-path".to_owned(),
                    cwd.to_string_lossy().into_owned(),
                ]);
            }
            if let Some(prompt) = prompt.filter(|value| !value.is_empty()) {
                argv.extend(["--prompt".to_owned(), prompt.to_owned()]);
            }
            argv
        }
    };
    Ok(PaneCmd { argv })
}

pub(crate) fn ensure_live_session(
    backend: &dyn rimz::mux::MuxBackend,
    session_name: &str,
) -> Result<()> {
    let sessions = backend.list_sessions()?;
    if sessions.iter().any(|session| session == session_name) {
        Ok(())
    } else {
        bail!("no live Rimz room `{session_name}`; run `rimz start` first")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::ids::AgentKind;
    use rimz::tab_layout::{Column, LayoutSpec};

    #[test]
    fn layout_panes_wraps_agents_and_shells_terms() {
        let layout = LayoutSpec {
            columns: vec![Column {
                rows: vec![Cell::Agent(AgentKind::new_unchecked("codex")), Cell::Term],
            }],
        };
        let panes =
            layout_panes(&layout, Path::new("/repo-wt/a"), Some("hi"), true).expect("panes");
        let agent = &panes.columns[0][0].argv;
        assert!(agent.iter().any(|arg| arg == "agents"));
        assert!(agent.iter().any(|arg| arg == "exec"));
        assert!(agent.iter().any(|arg| arg == "codex"));
        assert!(agent.iter().any(|arg| arg == "--worktree-path"));
        assert!(agent.iter().any(|arg| arg == "hi"));
        assert!(!panes.columns[0][1].argv.is_empty());
    }

    #[test]
    fn worktree_tab_title_uses_resolved_worktree_name() {
        let layout = LayoutSpec {
            columns: vec![Column {
                rows: vec![
                    Cell::Agent(AgentKind::new_unchecked("claude")),
                    Cell::Agent(AgentKind::new_unchecked("codex")),
                    Cell::Term,
                ],
            }],
        };
        let launch = ResolvedCwd {
            cwd: PathBuf::from("/repo-worktrees/worktree-name"),
            worktree_name: Some("worktree-name".to_owned()),
        };

        assert_eq!(default_tab_title(&layout, &launch), "⑂ worktree-name");
    }

    #[test]
    fn non_worktree_tab_title_uses_plain_directory_name() {
        let layout = LayoutSpec {
            columns: vec![Column {
                rows: vec![Cell::Agent(AgentKind::new_unchecked("claude"))],
            }],
        };
        let launch = ResolvedCwd {
            cwd: PathBuf::from("/repo/main"),
            worktree_name: None,
        };

        assert_eq!(default_tab_title(&layout, &launch), "main");
    }
}
