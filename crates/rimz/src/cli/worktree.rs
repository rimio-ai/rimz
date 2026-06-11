//! `rimz worktree` — Rimz-owned git worktree lifecycle.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use super::GlobalFlags;
use rimz::config::WorktreeBase;
use rimz::mux::own_pane_id;
use rimz::workspace::{RootClass, WorkspaceResolver};

const CLEANUP_SIGNAL_ROSTER_GRACE: Duration = Duration::from_millis(300);

#[derive(Debug, Args)]
pub struct WorktreeArgs {
    #[command(subcommand)]
    command: WorktreeSubcmd,
}

#[derive(Debug, Subcommand)]
enum WorktreeSubcmd {
    /// Create a Rimz-owned worktree.
    New {
        #[arg(value_name = "NAME")]
        name: Option<String>,
        /// Base ref: `head`, `fresh`, or any git ref.
        #[arg(long, value_parser = parse_base)]
        base: Option<WorktreeBase>,
        /// Branch to create instead of `<name>`.
        #[arg(long)]
        branch: Option<String>,
    },
    /// List Rimz-owned worktrees.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Remove a Rimz-owned worktree.
    Remove {
        name: String,
        /// Remove even when the worktree is dirty or has unmerged commits.
        #[arg(long)]
        force: bool,
    },
    /// Hidden helper used by long-lived agent wrappers.
    #[command(hide = true)]
    Cleanup(CleanupArgs),
}

#[derive(Debug, Args)]
struct CleanupArgs {
    #[arg(value_name = "PATH")]
    path: PathBuf,
    /// Keep dirty worktrees without prompting.
    #[arg(long)]
    non_interactive: bool,
}

pub fn run(args: WorktreeArgs, globals: &GlobalFlags) -> Result<()> {
    let command = match args.command {
        WorktreeSubcmd::Cleanup(cleanup) => {
            return cleanup_worktree(&cleanup.path, globals, !cleanup.non_interactive);
        }
        command => command,
    };

    let workspace = WorkspaceResolver::resolve(".", globals.root.clone())
        .context("resolving current workspace")?;
    if workspace.root_class != RootClass::Repo {
        bail!("rimz worktree requires a git repository; cd into a repo checkout");
    }
    let config = super::machine_config().worktree;
    match command {
        WorktreeSubcmd::New { name, base, branch } => {
            let created = rimz::worktree::create(
                &workspace.project_root,
                &config,
                name.as_deref(),
                base,
                branch.as_deref(),
                false,
            )?;
            #[expect(clippy::print_stdout, reason = "user-facing lifecycle report")]
            {
                println!("created {}", created.name);
                println!("  path   : {}", created.path.display());
                println!("  branch : {}", created.branch);
                if let Some(base_branch) = created.base_branch.as_deref() {
                    println!("  base branch: {base_branch}");
                }
                println!("  base   : {}", created.base_ref);
            }
            Ok(())
        }
        WorktreeSubcmd::List { json } => {
            let entries = rimz::worktree::list(&workspace.project_root)?;
            if json {
                let rendered = serde_json::to_string_pretty(&entries)?;
                #[expect(clippy::print_stdout, reason = "json emitter")]
                {
                    println!("{rendered}");
                }
            } else {
                for entry in entries {
                    let commits_unmerged = entry
                        .commits_unmerged
                        .map_or_else(|| "?".to_owned(), |count| count.to_string());
                    #[expect(clippy::print_stdout, reason = "human listing")]
                    {
                        println!(
                            "{}\t{}\t{}\t{} unmerged{}",
                            entry.name,
                            entry.path.display(),
                            entry.branch.as_deref().unwrap_or("-"),
                            commits_unmerged,
                            if entry.dirty { " dirty" } else { "" }
                        );
                    }
                }
            }
            Ok(())
        }
        WorktreeSubcmd::Remove { name, force } => {
            let branch = rimz::worktree::remove(&workspace.project_root, &config, &name, force)?;
            #[expect(clippy::print_stdout, reason = "user-facing lifecycle report")]
            {
                println!("removed {name}");
                if branch == rimz::worktree::BranchDeletion::KeptUnmerged {
                    println!("  branch kept: work not proven merged into its base");
                }
            }
            Ok(())
        }
        WorktreeSubcmd::Cleanup(_) => unreachable!("cleanup returned before workspace resolution"),
    }
}

fn parse_base(raw: &str) -> std::result::Result<WorktreeBase, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("base ref cannot be empty".to_owned());
    }
    Ok(match trimmed {
        "head" => WorktreeBase::Head,
        "fresh" => WorktreeBase::Fresh,
        other => WorktreeBase::Explicit(other.to_owned()),
    })
}

pub(super) fn cleanup_worktree(
    path: &Path,
    globals: &GlobalFlags,
    interactive: bool,
) -> Result<()> {
    let Some(marker) = rimz::worktree::read_marker_for_worktree(path)? else {
        return Ok(());
    };
    let status = rimz::worktree::status(path, &marker)?;
    if !interactive {
        std::thread::sleep(CLEANUP_SIGNAL_ROSTER_GRACE);
    }
    let other_pane_inside = other_live_pane_inside(path, globals);
    match rimz::worktree::cleanup_decision(status, true, other_pane_inside) {
        rimz::worktree::CleanupDecision::RemoveClean => {
            let branch = remove_after_leaving_worktree(path, &marker, false)?;
            let _ = writeln!(
                std::io::stderr().lock(),
                "rimz: removed clean worktree {}",
                path.display()
            );
            report_kept_branch(branch, &marker);
        }
        rimz::worktree::CleanupDecision::PromptDirty => {
            if interactive {
                match dirty_choice(path)? {
                    DirtyChoice::Keep => {}
                    DirtyChoice::Remove => {
                        let branch = remove_after_leaving_worktree(path, &marker, true)?;
                        report_kept_branch(branch, &marker);
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
) -> Result<rimz::worktree::BranchDeletion> {
    std::env::set_current_dir(&marker.repo_root)
        .with_context(|| format!("leaving worktree before removing {}", path.display()))?;
    rimz::worktree::remove_marked_worktree(&marker.repo_root, path, marker, force)
        .map_err(Into::into)
}

fn report_kept_branch(
    branch: rimz::worktree::BranchDeletion,
    marker: &rimz::worktree::WorktreeMarker,
) {
    if branch == rimz::worktree::BranchDeletion::KeptUnmerged {
        let _ = writeln!(
            std::io::stderr().lock(),
            "rimz: kept branch {} because its work was not proven merged into its base",
            marker.branch
        );
    }
}

fn other_live_pane_inside(path: &Path, globals: &GlobalFlags) -> bool {
    let Ok(mux) = rimz::mux::auto_detect_backend(globals.mux) else {
        return false;
    };
    let Some(own) = own_pane_id(mux) else {
        return false;
    };
    let backend = rimz::mux::backend_for(mux);
    let Ok(listing) = backend.list_panes(rimz::mux::PaneListOptions::default()) else {
        return false;
    };
    let panes = listing.panes;
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
        "rimz: worktree {} has local changes or unmerged commits.",
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
    let shell = rimz::launch::user_shell_program();
    let err = Command::new(&shell).current_dir(path).exec();
    Err::<(), _>(err).with_context(|| format!("execing {shell}"))
}

#[cfg(not(unix))]
fn exec_shell(path: &Path) -> Result<()> {
    let shell = rimz::launch::user_shell_program();
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
    use rimz::{MuxName, PaneId};

    #[test]
    fn other_live_user_pane_inside_filters_sidebar_own_and_counts_user_panes() {
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

        let shell_dir = worktree.join("src");
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
