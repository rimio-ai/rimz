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
use rimz::config::WorktreeBase;
use rimz::mux::own_pane_id;
use rimz::workspace::{RootClass, WorkspaceResolver};

const CLEANUP_SIGNAL_ROSTER_GRACE: Duration = Duration::from_millis(300);
const WORKTREE_REMOVED_ARCHIVE_REASON: &str = "worktree removed";

pub(super) struct RemovedWorktree {
    pub(super) branch_deletion: rimz::worktree::BranchDeletion,
    /// Archive outcome, surfaced so `worktree remove` can hard-fail while
    /// cleanup and gc downgrade to a debug log.
    pub(super) archive: Result<()>,
}

pub(super) fn remove_and_archive(
    marker: &rimz::worktree::WorktreeMarker,
    remove: impl FnOnce() -> Result<rimz::worktree::BranchDeletion>,
    archive_channel: impl FnOnce(&str, &str) -> Result<()>,
) -> Result<RemovedWorktree> {
    let branch_deletion = remove()?;
    let archive = archive_channel(&marker.name, WORKTREE_REMOVED_ARCHIVE_REASON);
    Ok(RemovedWorktree {
        branch_deletion,
        archive,
    })
}

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
        #[arg(long = "from-pr", value_name = "PR", value_parser = parse_pr, conflicts_with = "base")]
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
        } => {
            let store = open_store(&workspace)?;
            let requested_name = name
                .as_deref()
                .map(rimz::worktree::parse_requested_name)
                .transpose()?;
            if let Some(name) = requested_name
                .as_ref()
                .map(|requested| requested.name.as_str())
                && super::channel::named_channel_registered(&store, name)
            {
                bail!(
                    "channel `{name}` is a named channel; use `rimz channel new` or pick another name"
                );
            }
            let created = if let Some(pr) = from_pr.as_ref() {
                rimz::worktree::create_from_pr(
                    &workspace.project_root,
                    &config,
                    pr,
                    name.as_deref(),
                    branch.as_deref(),
                    false,
                )?
            } else {
                rimz::worktree::create(
                    &workspace.project_root,
                    &config,
                    name.as_deref(),
                    base,
                    branch.as_deref(),
                    false,
                )?
            };
            store
                .archive_channel_messages(
                    &created.name,
                    "channel recreated",
                    &workspace.session_name,
                )
                .context("archiving messages for recreated worktree channel")?;
            #[expect(clippy::print_stdout, reason = "user-facing lifecycle report")]
            {
                println!("created {}", created.name);
                println!("  path   : {}", created.path.display());
                println!("  branch : {}", created.branch);
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
                // Best-effort overlay: which agent-colleagues live in each channel.
                let snapshot = crate::cli::open_store(&workspace)
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
        WorktreeSubcmd::Remove { name, force } => {
            let store = open_store(&workspace)?;
            let path = rimz::worktree::worktree_path(&workspace.project_root, &config, &name)?;
            let marker = rimz::worktree::read_marker_for_worktree(&path)?.ok_or_else(|| {
                rimz::worktree::WorktreeErr::Unmarked {
                    name: name.clone(),
                    path: path.clone(),
                }
            })?;
            let removed = remove_and_archive(
                &marker,
                || {
                    rimz::worktree::remove(&workspace.project_root, &config, &name, force)
                        .map_err(Into::into)
                },
                |channel, reason| {
                    store
                        .archive_channel_messages(channel, reason, &workspace.session_name)
                        .map(|_| ())
                        .map_err(Into::into)
                },
            )?;
            removed
                .archive
                .context("archiving messages for removed worktree channel")?;
            #[expect(clippy::print_stdout, reason = "user-facing lifecycle report")]
            {
                println!("removed {name}");
                if removed.branch_deletion == rimz::worktree::BranchDeletion::KeptUnmerged {
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

fn parse_pr(raw: &str) -> std::result::Result<rimz::forge::PrTarget, String> {
    rimz::forge::parse(raw)
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
    let roster_bound = roster_binds_worktree_from_store(path, &marker, globals);
    match rimz::worktree::cleanup_decision(status, true, other_pane_inside || roster_bound) {
        rimz::worktree::CleanupDecision::RemoveClean => {
            let removed = remove_and_archive(
                &marker,
                || remove_after_leaving_worktree(path, &marker, false),
                |channel, reason| {
                    archive_removed_worktree_messages(&marker, globals, channel, reason)
                },
            )?;
            if let Err(err) = removed.archive {
                tracing::debug!(
                    branch = %marker.branch,
                    error = %err,
                    "could not archive messages for removed worktree",
                );
            }
            let _ = writeln!(
                std::io::stderr().lock(),
                "rimz: removed clean worktree {}",
                path.display()
            );
            report_kept_branch(removed.branch_deletion, &marker);
        }
        rimz::worktree::CleanupDecision::PromptDirty => {
            if interactive {
                match dirty_choice(path)? {
                    DirtyChoice::Keep => {}
                    DirtyChoice::Remove => {
                        let removed = remove_and_archive(
                            &marker,
                            || remove_after_leaving_worktree(path, &marker, true),
                            |channel, reason| {
                                archive_removed_worktree_messages(&marker, globals, channel, reason)
                            },
                        )?;
                        if let Err(err) = removed.archive {
                            tracing::debug!(
                                branch = %marker.branch,
                                error = %err,
                                "could not archive messages for removed worktree",
                            );
                        }
                        report_kept_branch(removed.branch_deletion, &marker);
                    }
                    DirtyChoice::Shell => exec_shell(path)?,
                }
            }
        }
        rimz::worktree::CleanupDecision::Skip => {}
    }
    Ok(())
}

fn roster_binds_worktree_from_store(
    path: &Path,
    marker: &rimz::worktree::WorktreeMarker,
    globals: &GlobalFlags,
) -> bool {
    let workspace = match WorkspaceResolver::resolve(&marker.repo_root, globals.root.clone()) {
        Ok(workspace) => workspace,
        Err(err) => {
            tracing::debug!(
                path = %path.display(),
                error = %err,
                "could not resolve workspace while checking worktree cleanup roster guard",
            );
            return false;
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
            return false;
        }
    };
    let own = rimz::mux::ambient_pane_id();
    roster_binds_worktree(&snapshot.agents, own.as_ref(), path)
}

fn roster_binds_worktree(agents: &[AgentState], own: Option<&rimz::PaneId>, path: &Path) -> bool {
    let target = rimz::worktree::normalize_path_lexical(path);
    agent_pinned_paths(agents, own)
        .iter()
        .any(|path| rimz::worktree::path_inside(path, &target))
}

pub(super) fn agent_pinned_paths(
    agents: &[AgentState],
    own: Option<&rimz::PaneId>,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for agent in agents.iter().filter(|agent| agent_is_not_own(agent, own)) {
        match rimz::store::runtime::agent_liveness(agent) {
            rimz::store::runtime::AgentLiveness::Dead => {}
            rimz::store::runtime::AgentLiveness::Unknown => {
                if let Some(path) = agent.worktree_path.as_deref() {
                    paths.push(rimz::worktree::normalize_path_lexical(Path::new(path)));
                }
            }
            rimz::store::runtime::AgentLiveness::Live { pid } => {
                if let Some(path) = agent.worktree_path.as_deref() {
                    paths.push(rimz::worktree::normalize_path_lexical(Path::new(path)));
                }
                if let Some(cwd) = rimz::proc::cwd(pid) {
                    paths.push(rimz::worktree::normalize_path_lexical(&cwd));
                }
            }
        }
    }
    paths
}

fn agent_is_not_own(agent: &AgentState, own: Option<&rimz::PaneId>) -> bool {
    match (agent.pane.as_ref(), own) {
        (Some(pane), Some(own)) => &pane.pane_id != own,
        _ => true,
    }
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

fn archive_removed_worktree_messages(
    marker: &rimz::worktree::WorktreeMarker,
    globals: &GlobalFlags,
    channel: &str,
    reason: &str,
) -> Result<()> {
    let workspace = WorkspaceResolver::resolve(&marker.repo_root, globals.root.clone())?;
    let store = open_store(&workspace)?;
    store.archive_channel_messages(channel, reason, &workspace.session_name)?;
    Ok(())
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
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone()).ok();
    let backend = rimz::mux::backend_for(mux);
    let Ok(listing) = backend.list_panes(rimz::mux::PaneListOptions {
        session_name: workspace
            .as_ref()
            .map(|workspace| workspace.session_name.clone()),
        workspace_id: workspace
            .as_ref()
            .map(|workspace| workspace.workspace_id.clone()),
        ..Default::default()
    }) else {
        return false;
    };
    let panes = listing.panes;
    other_live_user_pane_inside(&panes, &own, path)
}

fn other_live_user_pane_inside<'a>(
    panes: impl IntoIterator<Item = &'a rimz::pane::PaneRef>,
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

    #[test]
    fn roster_binds_worktree_filters_own_dead_and_other_worktrees() {
        let worktree = Path::new("/repo-worktrees/demo");
        let own = PaneId::from_parts(MuxName::Zellij, "terminal_own");
        let now = jiff::Timestamp::from_second(1_700_000_000).unwrap();

        assert!(roster_binds_worktree(
            &[agent(
                "inflight",
                Some("/repo/../repo-worktrees/demo"),
                None,
                now
            )],
            Some(&own),
            worktree,
        ));
        assert!(roster_binds_worktree(
            &[agent(
                "other-pane",
                Some("/repo-worktrees/demo/src"),
                Some("terminal_other"),
                now,
            )],
            Some(&own),
            worktree,
        ));
        assert!(!roster_binds_worktree(
            &[agent(
                "own",
                Some("/repo-worktrees/demo"),
                Some("terminal_own"),
                now,
            )],
            Some(&own),
            worktree,
        ));
        assert!(roster_binds_worktree(
            &[agent(
                "idle-live-unknown",
                Some("/repo-worktrees/demo"),
                None,
                now - Duration::from_secs(30),
            )],
            Some(&own),
            worktree,
        ));
        #[cfg(target_os = "linux")]
        {
            let mut dead = agent("dead", Some("/repo-worktrees/demo"), None, now);
            dead.runtime_owner = Some(rimz::RuntimeOwner::new(
                rimz::RuntimeOwnerKind::Agent,
                "dead",
                u32::MAX,
                None,
            ));
            assert!(!roster_binds_worktree(&[dead], Some(&own), worktree));
        }
        let mut live = agent("live", Some("/repo-worktrees/demo"), None, now);
        live.runtime_owner = Some(rimz::store::runtime::current_process_owner(
            rimz::RuntimeOwnerKind::Agent,
            "live",
        ));
        assert!(roster_binds_worktree(&[live], Some(&own), worktree));
        assert!(!roster_binds_worktree(
            &[agent(
                "other-worktree",
                Some("/repo-worktrees/other"),
                None,
                now
            )],
            Some(&own),
            worktree,
        ));
    }

    #[test]
    fn remove_and_archive_uses_worktree_name_as_channel() {
        let marker = rimz::worktree::WorktreeMarker {
            version: 3,
            name: "demo".to_owned(),
            branch: "scratch".to_owned(),
            base_branch: Some("main".to_owned()),
            base_ref: "base".to_owned(),
            repo_root: PathBuf::from("/repo"),
            worktree_path: PathBuf::from("/repo-worktrees/demo"),
            created_at: jiff::Timestamp::from_second(1_700_000_000).unwrap(),
        };
        let mut archived = None;

        let removed = remove_and_archive(
            &marker,
            || Ok(rimz::worktree::BranchDeletion::Deleted),
            |channel, reason| {
                archived = Some((channel.to_owned(), reason.to_owned()));
                Ok(())
            },
        )
        .expect("remove");

        assert_eq!(
            removed.branch_deletion,
            rimz::worktree::BranchDeletion::Deleted
        );
        removed.archive.expect("archive");
        assert_eq!(
            archived,
            Some((
                "demo".to_owned(),
                WORKTREE_REMOVED_ARCHIVE_REASON.to_owned()
            ))
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn agent_pinned_paths_includes_realtime_process_cwd() {
        let now = jiff::Timestamp::from_second(1_700_000_000).unwrap();
        let mut live = agent("live", Some("/repo-worktrees/other"), None, now);
        live.runtime_owner = Some(rimz::store::runtime::current_process_owner(
            rimz::RuntimeOwnerKind::Agent,
            "live",
        ));

        let current =
            rimz::worktree::normalize_path_lexical(&std::env::current_dir().expect("current dir"));

        assert!(agent_pinned_paths(&[live], None).contains(&current));
    }

    fn pane(raw: &str, command: Option<&str>, cwd: Option<&Path>) -> rimz::pane::PaneRef {
        rimz::pane::PaneRef {
            command: command.map(ToOwned::to_owned),
            cwd: cwd.map(|path| path.display().to_string()),
            ..rimz::pane::PaneRef::from_id(PaneId::from_parts(MuxName::Zellij, raw))
        }
    }

    fn agent(
        id: &str,
        worktree_path: Option<&str>,
        raw_pane: Option<&str>,
        last_seen: jiff::Timestamp,
    ) -> AgentState {
        AgentState {
            name: Some(id.to_owned()),
            pane: raw_pane.map(|raw| pane(raw, Some("codex"), None)),
            worktree_path: worktree_path.map(ToOwned::to_owned),
            ..rimz::testkit::agent_state("codex", id, last_seen)
        }
    }
}
