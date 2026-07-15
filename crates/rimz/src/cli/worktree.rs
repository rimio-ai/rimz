//! `rimz worktree` — Rimz-owned git worktree lifecycle.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use super::{GlobalFlags, open_store};
use crate::cli::render;
use rimz::agents::AgentState;
use rimz::config::{WorktreeBase, WorktreeConfig};
use rimz::forge::PrTarget;
use rimz::mux::own_pane_id;
use rimz::workspace::{ResolvedWorkspace, RootClass, WorkspaceResolver};

const CLEANUP_SIGNAL_ROSTER_GRACE: Duration = Duration::from_millis(300);
pub(super) const WORKTREE_REMOVED_ARCHIVE_REASON: &str = "worktree removed";

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
        /// Create the worktree from a pull request number or URL.
        #[arg(long = "from-pr", value_name = "PR", value_parser = rimz::forge::parse, conflicts_with = "base")]
        from_pr: Option<rimz::forge::PrTarget>,
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
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::worktrees
        ))]
        name: String,
        /// Remove even when the worktree is dirty or has work not proven landed.
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
    let config = super::machine_config().agents.worktree.clone();
    match command {
        WorktreeSubcmd::New {
            name,
            base,
            from_pr,
            branch,
        } => new_worktree(&workspace, &config, name, base, from_pr, branch),
        WorktreeSubcmd::List { json } => list_worktrees(&workspace, json),
        WorktreeSubcmd::Remove { name, force } => remove_worktree(&workspace, &config, name, force),
        WorktreeSubcmd::Cleanup(_) => unreachable!("cleanup returned before workspace resolution"),
    }
}

fn new_worktree(
    workspace: &ResolvedWorkspace,
    config: &WorktreeConfig,
    name: Option<String>,
    base: Option<WorktreeBase>,
    from_pr: Option<PrTarget>,
    branch: Option<String>,
) -> Result<()> {
    let store = open_store(workspace)?;
    let requested_name = name
        .as_deref()
        .map(rimz::worktree::parse_requested_name)
        .transpose()?;
    if let Some(name) = requested_name
        .as_ref()
        .map(|requested| requested.name.as_str())
        && super::channel::named_channel_registered(&store, name)
    {
        bail!("channel `{name}` is a named channel; use `rimz channel new` or pick another name");
    }
    let created = if let Some(pr) = from_pr.as_ref() {
        rimz::worktree::create_from_pr(
            &workspace.project_root,
            config,
            pr,
            name.as_deref(),
            branch.as_deref(),
            false,
        )?
    } else {
        rimz::worktree::create(
            &workspace.project_root,
            config,
            name.as_deref(),
            base,
            branch.as_deref(),
            false,
        )?
    };
    store
        .archive_channel_messages(&created.name, "channel recreated", &workspace.session_name)
        .context("archiving messages for recreated worktree channel")?;
    report_created(&workspace.project_root, &created);
    Ok(())
}

#[expect(clippy::print_stdout, reason = "user-facing lifecycle report")]
fn report_created(repo_root: &Path, created: &rimz::worktree::CreatedWorktree) {
    println!("created {}", created.name);
    println!("  path   : {}", created.path.display());
    println!("  branch : {}", created.branch);
    if let Some((remote, merge_ref)) =
        rimz::worktree::fork_push_destination(repo_root, &created.branch)
    {
        println!("  pushes : {remote} {merge_ref}");
        if merge_ref.strip_prefix("refs/heads/") != Some(created.branch.as_str()) {
            let head = merge_ref.strip_prefix("refs/heads/").unwrap_or(&merge_ref);
            println!("  push   : git push {remote} HEAD:{head}");
        }
    }
    if let Some(base_branch) = created.base_branch.as_deref() {
        println!("  base branch: {base_branch}");
    }
    println!("  base   : {}", created.base_ref);
    if created.included > 0 {
        println!(
            "  seeded : {} file(s) from .worktreeinclude",
            created.included
        );
    }
    if created.linked > 0 {
        println!("  linked : {} dir(s) from .worktreelink", created.linked);
    }
}

fn list_worktrees(workspace: &ResolvedWorkspace, json: bool) -> Result<()> {
    let entries = rimz::worktree::list(&workspace.project_root)?;
    if json {
        let rendered = serde_json::to_string_pretty(&entries)?;
        #[expect(clippy::print_stdout, reason = "json emitter")]
        {
            println!("{rendered}");
        }
    } else {
        // Best-effort overlay: which agent-colleagues live in each channel.
        let snapshot = crate::cli::open_store(workspace)
            .ok()
            .and_then(|store| store.snapshot_cached().ok());
        let agents: Vec<&AgentState> = snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .agents
                    .iter()
                    .filter(|agent| agent.parent_agent_id.is_none())
                    .collect()
            })
            .unwrap_or_default();
        let mut table =
            render::Table::new(["WORKTREE", "BRANCH", "AGENTS", "DIRTY", "MERGED", "PATH"]);
        for entry in entries {
            let path_str = entry.path.to_string_lossy().into_owned();
            let here: Vec<&AgentState> = agents
                .iter()
                .copied()
                .filter(|agent| {
                    agent.worktree_path.as_deref() == Some(path_str.as_str())
                        || (entry.branch.is_some() && agent.worktree_branch == entry.branch)
                })
                .collect();
            let chips = if here.is_empty() {
                "-".to_owned()
            } else {
                here.iter()
                    .map(|agent| rimz::harness::target::agent_handle(agent, &here, false))
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            let merged = match entry.landed {
                Some(true) => render::cell("yes"),
                Some(false) => render::cell("pending").fg(render::palette::WARN),
                None => render::cell("?"),
            };
            let branch = entry.branch.clone().unwrap_or_else(|| "-".to_owned());
            let dirty_cell = if entry.dirty {
                render::cell("dirty").fg(render::palette::WARN)
            } else {
                render::cell("-").dash()
            };
            let path_display = render::home_relative(&path_str);
            table.row([
                render::cell(entry.name).fg(render::palette::ACCENT),
                render::cell(branch).dash(),
                render::cell(chips).fg(render::palette::ACCENT).dash(),
                dirty_cell,
                merged,
                render::cell(path_display).dash(),
            ]);
        }
        table.render(&mut render::out())?;
    }
    Ok(())
}

fn remove_worktree(
    workspace: &ResolvedWorkspace,
    config: &WorktreeConfig,
    name: String,
    force: bool,
) -> Result<()> {
    let store = open_store(workspace)?;
    let removed = rimz::worktree::remove(&workspace.project_root, config, &name, force)?;
    store
        .archive_channel_messages(
            removed.worktree_name(),
            WORKTREE_REMOVED_ARCHIVE_REASON,
            &workspace.session_name,
        )
        .context("archiving messages for removed worktree channel")?;
    #[expect(clippy::print_stdout, reason = "user-facing lifecycle report")]
    {
        println!("removed {name}");
        if removed.branch_deletion() == rimz::worktree::BranchDeletion::KeptUnmerged {
            println!("  branch kept: work not proven merged into its base");
        }
    }
    Ok(())
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
    let protection = cleanup_protection(path, &marker, globals);
    match rimz::worktree::removal_assessment(path, status, &protection) {
        rimz::worktree::RemovalAssessment::Removable => {
            let removed = remove_for_cleanup(path, &marker, globals, false)?;
            let _ = writeln!(
                std::io::stderr().lock(),
                "rimz: removed clean worktree {}",
                path.display()
            );
            report_kept_branch(&removed);
        }
        rimz::worktree::RemovalAssessment::Kept(
            rimz::worktree::RemovalReason::Dirty | rimz::worktree::RemovalReason::NotLanded,
        ) => {
            if interactive {
                match dirty_choice(path)? {
                    DirtyChoice::Keep => {}
                    DirtyChoice::Remove => {
                        let removed = remove_for_cleanup(path, &marker, globals, true)?;
                        report_kept_branch(&removed);
                    }
                    DirtyChoice::Shell => exec_shell(path)?,
                }
            }
        }
        rimz::worktree::RemovalAssessment::Kept(rimz::worktree::RemovalReason::InUse) => {}
    }
    Ok(())
}

fn remove_for_cleanup(
    path: &Path,
    marker: &rimz::worktree::WorktreeMarker,
    globals: &GlobalFlags,
    force: bool,
) -> Result<rimz::worktree::RemovalOutcome> {
    let removed = remove_after_leaving_worktree(path, marker, force)?;
    if let Err(err) =
        archive_removed_worktree_messages(&removed, globals, WORKTREE_REMOVED_ARCHIVE_REASON)
    {
        tracing::debug!(
            branch = %removed.branch(),
            error = %err,
            "could not archive messages for removed worktree",
        );
    }
    Ok(removed)
}

fn cleanup_protection(
    path: &Path,
    marker: &rimz::worktree::WorktreeMarker,
    globals: &GlobalFlags,
) -> rimz::worktree::RemovalProtection {
    let mut protection = rimz::worktree::RemovalProtection::default();
    match rimz::mux::auto_detect_backend(globals.mux) {
        Ok(mux) => match own_pane_id(mux) {
            Some(own) => {
                let workspace =
                    WorkspaceResolver::resolve_participant(".", globals.root.clone()).ok();
                match rimz::mux::backend_for(mux).list_panes(rimz::mux::PaneListOptions {
                    session_name: workspace
                        .as_ref()
                        .map(|workspace| workspace.session_name.clone()),
                    workspace_id: workspace
                        .as_ref()
                        .map(|workspace| workspace.workspace_id.clone()),
                    ..Default::default()
                }) {
                    Ok(listing) => protection.fold_panes(&listing.panes, Some(&own)),
                    Err(err) => tracing::debug!(
                        path = %path.display(),
                        error = %err,
                        "could not list panes while checking worktree cleanup guard",
                    ),
                }
            }
            None => tracing::debug!(
                path = %path.display(),
                "could not identify own pane while checking worktree cleanup guard",
            ),
        },
        Err(err) => tracing::debug!(
            path = %path.display(),
            error = %err,
            "could not detect mux while checking worktree cleanup guard",
        ),
    }

    let workspace = match WorkspaceResolver::resolve(&marker.repo_root, globals.root.clone()) {
        Ok(workspace) => workspace,
        Err(err) => {
            tracing::debug!(
                path = %path.display(),
                error = %err,
                "could not resolve workspace while checking worktree cleanup roster guard",
            );
            return protection;
        }
    };
    let snapshot = match super::open_store(&workspace).and_then(|store| {
        super::alive_snapshot(&store, store.runtime_paths(), &workspace.session_name)
    }) {
        Ok(snapshot) => snapshot,
        Err(err) => {
            tracing::debug!(
                path = %path.display(),
                error = %err,
                "could not read agent roster while checking worktree cleanup guard",
            );
            return protection;
        }
    };
    let own = rimz::mux::ambient_pane_id();
    protection.fold_agents(&snapshot.agents, own.as_ref());
    protection
}

fn remove_after_leaving_worktree(
    path: &Path,
    marker: &rimz::worktree::WorktreeMarker,
    force: bool,
) -> Result<rimz::worktree::RemovalOutcome> {
    std::env::set_current_dir(&marker.repo_root)
        .with_context(|| format!("leaving worktree before removing {}", path.display()))?;
    rimz::worktree::remove_marked_worktree(&marker.repo_root, path, marker, force)
        .map_err(Into::into)
}

fn archive_removed_worktree_messages(
    removed: &rimz::worktree::RemovalOutcome,
    globals: &GlobalFlags,
    reason: &str,
) -> Result<()> {
    let workspace = WorkspaceResolver::resolve(removed.repo_root(), globals.root.clone())?;
    let store = open_store(&workspace)?;
    store.archive_channel_messages(removed.worktree_name(), reason, &workspace.session_name)?;
    Ok(())
}

fn report_kept_branch(removed: &rimz::worktree::RemovalOutcome) {
    if removed.branch_deletion() == rimz::worktree::BranchDeletion::KeptUnmerged {
        let _ = writeln!(
            std::io::stderr().lock(),
            "rimz: kept branch {} because its work was not proven merged into its base",
            removed.branch()
        );
    }
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
        "rimz: worktree {} has local changes or work not proven landed.",
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
    let shell = rimz::harness::launch::user_shell_program();
    let err = Command::new(&shell).current_dir(path).exec();
    Err::<(), _>(err).with_context(|| format!("execing {shell}"))
}

#[cfg(not(unix))]
fn exec_shell(path: &Path) -> Result<()> {
    let shell = rimz::harness::launch::user_shell_program();
    let status = Command::new(&shell)
        .current_dir(path)
        .status()
        .with_context(|| format!("running {shell}"))?;
    if !status.success() {
        bail!("shell exited with {status}");
    }
    Ok(())
}
