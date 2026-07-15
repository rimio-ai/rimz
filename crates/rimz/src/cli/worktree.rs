//! `rimz worktree` — gather runtime protection facts, prompt, remove, and archive.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde::Serialize;

use super::{GlobalFlags, open_store};
use crate::cli::render;
use rimz::agents::AgentState;
use rimz::config::{WorktreeBase, WorktreeConfig};
use rimz::forge::PrTarget;
use rimz::workspace::{ResolvedWorkspace, RootClass, WorkspaceResolver};

const CLEANUP_SIGNAL_ROSTER_GRACE: Duration = Duration::from_millis(300);

struct InspectedWorktree {
    managed: rimz::worktree::ManagedWorktree,
    status: rimz::worktree::WorktreeStatus,
}

#[derive(Serialize)]
struct WorktreeListJson<'a> {
    name: &'a str,
    path: &'a Path,
    branch: Option<&'a str>,
    base_ref: &'a str,
    dirty: bool,
    landed: Option<bool>,
}

#[derive(Debug, Args)]
pub struct WorktreeArgs {
    #[command(subcommand)]
    command: WorktreeSubcmd,
}

#[derive(Debug, Subcommand)]
enum WorktreeSubcmd {
    /// Create a RimZ-owned worktree.
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
    /// List RimZ-owned worktrees.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Remove a RimZ-owned worktree.
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
        .archive_channel_messages(
            &created.marker.name,
            "channel recreated",
            &workspace.session_name,
        )
        .context("archiving messages for recreated worktree channel")?;
    report_created(&created);
    Ok(())
}

#[expect(clippy::print_stdout, reason = "user-facing lifecycle report")]
fn report_created(created: &rimz::worktree::CreatedWorktree) {
    let marker = &created.marker;
    println!("created {}", marker.name);
    println!("  path   : {}", marker.worktree_path.display());
    println!("  branch : {}", marker.branch);
    if let Some(destination) = created.push_destination.as_ref() {
        let remote = &destination.remote;
        let merge_ref = &destination.merge_ref;
        println!("  pushes : {remote} {merge_ref}");
        if merge_ref.strip_prefix("refs/heads/") != Some(marker.branch.as_str()) {
            let head = merge_ref.strip_prefix("refs/heads/").unwrap_or(merge_ref);
            println!("  push   : git push {remote} HEAD:{head}");
        }
    }
    if let Some(base_branch) = marker.base_branch.as_deref() {
        println!("  base branch: {base_branch}");
    }
    println!("  base   : {}", marker.base_ref);
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
    let entries = rimz::worktree::discover_owned(&workspace.project_root)?
        .into_iter()
        .map(|managed| {
            let status = rimz::worktree::status(&managed.path, &managed.marker)
                .unwrap_or_else(|_| rimz::worktree::WorktreeStatus::unknown());
            InspectedWorktree { managed, status }
        })
        .collect::<Vec<_>>();
    if json {
        render_worktree_json(&entries)?;
    } else {
        render_worktree_table(workspace, &entries)?;
    }
    Ok(())
}

fn render_worktree_json(entries: &[InspectedWorktree]) -> Result<()> {
    let rows = entries.iter().map(worktree_json_row).collect::<Vec<_>>();
    let rendered = serde_json::to_string_pretty(&rows)?;
    #[expect(clippy::print_stdout, reason = "json emitter")]
    {
        println!("{rendered}");
    }
    Ok(())
}

fn worktree_json_row(entry: &InspectedWorktree) -> WorktreeListJson<'_> {
    WorktreeListJson {
        name: &entry.managed.marker.name,
        path: &entry.managed.path,
        branch: entry.managed.branch.as_deref(),
        base_ref: &entry.managed.marker.base_ref,
        dirty: entry.status.dirty,
        landed: match entry.status.landed {
            rimz::worktree::LandedVerdict::Landed => Some(true),
            rimz::worktree::LandedVerdict::Pending => Some(false),
            rimz::worktree::LandedVerdict::Unknown => None,
        },
    }
}

fn render_worktree_table(
    workspace: &ResolvedWorkspace,
    entries: &[InspectedWorktree],
) -> Result<()> {
    let agents = root_agents(workspace);
    let mut table = render::Table::new(["WORKTREE", "BRANCH", "AGENTS", "DIRTY", "MERGED", "PATH"]);
    for entry in entries {
        append_worktree_row(&mut table, entry, &agents);
    }
    table.render(&mut render::out())?;
    Ok(())
}

fn root_agents(workspace: &ResolvedWorkspace) -> Vec<AgentState> {
    crate::cli::open_store(workspace)
        .ok()
        .and_then(|store| store.snapshot_cached().ok())
        .map(|snapshot| {
            snapshot
                .agents
                .into_iter()
                .filter(|agent| agent.parent_agent_id.is_none())
                .collect()
        })
        .unwrap_or_default()
}

fn append_worktree_row(
    table: &mut render::Table,
    entry: &InspectedWorktree,
    agents: &[AgentState],
) {
    let path = entry.managed.path.to_string_lossy().into_owned();
    let here = agents_for_worktree(agents, &entry.managed, &path);
    let chips = if here.is_empty() {
        "-".to_owned()
    } else {
        here.iter()
            .map(|agent| rimz::harness::target::agent_handle(agent, &here, false))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let merged = match entry.status.landed {
        rimz::worktree::LandedVerdict::Landed => render::cell("yes"),
        rimz::worktree::LandedVerdict::Pending => render::cell("pending").fg(render::palette::WARN),
        rimz::worktree::LandedVerdict::Unknown => render::cell("?"),
    };
    let dirty = if entry.status.dirty {
        render::cell("dirty").fg(render::palette::WARN)
    } else {
        render::cell("-").dash()
    };
    table.row([
        render::cell(&entry.managed.marker.name).fg(render::palette::ACCENT),
        render::cell(entry.managed.branch.as_deref().unwrap_or("-")).dash(),
        render::cell(chips).fg(render::palette::ACCENT).dash(),
        dirty,
        merged,
        render::cell(render::home_relative(&path)).dash(),
    ]);
}

fn agents_for_worktree<'a>(
    agents: &'a [AgentState],
    entry: &rimz::worktree::ManagedWorktree,
    path: &str,
) -> Vec<&'a AgentState> {
    agents
        .iter()
        .filter(|agent| {
            agent.worktree_path.as_deref() == Some(path)
                || entry
                    .branch
                    .as_deref()
                    .is_some_and(|branch| agent.worktree_branch.as_deref() == Some(branch))
        })
        .collect()
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
            rimz::worktree::WORKTREE_REMOVED_ARCHIVE_REASON,
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
    raw.parse()
        .map_err(|_| "base ref cannot be empty".to_owned())
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
    let protections = runtime_protection_set(&marker, globals);
    match protections.assess(path, status) {
        rimz::worktree::RemovalAssessment::Removable => {
            let removed = remove_for_cleanup(path, &marker, globals, false)?;
            let _ = writeln!(
                std::io::stderr().lock(),
                "rimz: removed clean worktree {}",
                path.display()
            );
            report_kept_branch(&removed);
        }
        rimz::worktree::RemovalAssessment::Dirty | rimz::worktree::RemovalAssessment::NotLanded => {
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
        rimz::worktree::RemovalAssessment::InUse => {}
    }
    Ok(())
}

fn remove_for_cleanup(
    path: &Path,
    marker: &rimz::worktree::WorktreeMarker,
    globals: &GlobalFlags,
    force: bool,
) -> Result<rimz::worktree::RemovalOutcome> {
    let removed = rimz::worktree::remove_marked_worktree(&marker.repo_root, path, marker, force)?;
    if let Err(err) = archive_removed_worktree_messages(
        &removed,
        globals,
        rimz::worktree::WORKTREE_REMOVED_ARCHIVE_REASON,
    ) {
        tracing::debug!(
            branch = %marker.branch,
            error = %err,
            "could not archive messages for removed worktree",
        );
    }
    Ok(removed)
}

fn runtime_protection_set(
    marker: &rimz::worktree::WorktreeMarker,
    globals: &GlobalFlags,
) -> rimz::worktree::ProtectionSet {
    let workspace = match WorkspaceResolver::resolve(&marker.repo_root, globals.root.clone()) {
        Ok(workspace) => Some(workspace),
        Err(err) => {
            tracing::debug!(
                path = %marker.worktree_path.display(),
                error = %err,
                "could not resolve workspace while checking worktree cleanup roster guard",
            );
            None
        }
    };
    let (panes, own) = rimz::mux::auto_detect_backend(globals.mux)
        .ok()
        .map(|mux| {
            let panes = rimz::mux::backend_for(mux)
                .list_panes(rimz::mux::PaneListOptions {
                    session_name: workspace
                        .as_ref()
                        .map(|workspace| workspace.session_name.clone()),
                    workspace_id: workspace
                        .as_ref()
                        .map(|workspace| workspace.workspace_id.clone()),
                    ..Default::default()
                })
                .map(|listing| listing.panes)
                .unwrap_or_default();
            (panes, rimz::mux::own_pane_id(mux))
        })
        .unwrap_or_default();
    let agents = match workspace.as_ref().and_then(|workspace| {
        super::open_store(workspace)
            .and_then(|store| {
                super::alive_snapshot(&store, store.runtime_paths(), &workspace.session_name)
            })
            .ok()
    }) {
        Some(snapshot) => snapshot.agents,
        None => Vec::new(),
    };
    rimz::worktree::protection_set_from_runtime(&panes, &agents, own.as_ref())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_json_maps_typed_landing_without_marker_fields() {
        for (verdict, landed) in [
            (rimz::worktree::LandedVerdict::Landed, Some(true)),
            (rimz::worktree::LandedVerdict::Pending, Some(false)),
            (rimz::worktree::LandedVerdict::Unknown, None),
        ] {
            let entry = inspected_worktree(verdict);
            let value = serde_json::to_value(worktree_json_row(&entry)).expect("serialize row");

            assert_eq!(
                value,
                serde_json::json!({
                    "name": "demo",
                    "path": "/repo-worktrees/demo",
                    "branch": "feature/demo",
                    "base_ref": "abc123",
                    "dirty": false,
                    "landed": landed,
                })
            );
        }
    }

    fn inspected_worktree(landed: rimz::worktree::LandedVerdict) -> InspectedWorktree {
        let path = PathBuf::from("/repo-worktrees/demo");
        InspectedWorktree {
            managed: rimz::worktree::ManagedWorktree {
                marker: rimz::worktree::WorktreeMarker {
                    version: 4,
                    name: "demo".to_owned(),
                    branch: "recorded/demo".to_owned(),
                    base_branch: Some("main".to_owned()),
                    from_pr: Some(42),
                    base_ref: "abc123".to_owned(),
                    repo_root: PathBuf::from("/repo"),
                    worktree_path: PathBuf::from("/recorded/demo"),
                    created_at: jiff::Timestamp::now(),
                },
                path,
                branch: Some("feature/demo".to_owned()),
            },
            status: rimz::worktree::WorktreeStatus {
                dirty: false,
                landed,
            },
        }
    }
}
