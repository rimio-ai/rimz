//! `rimz gc` — remove stale runtime liveness hints.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Args;

use super::{GlobalFlags, open_ledger};
use rimz::ledger::gc;
use rimz::workspace::WorkspaceResolver;

#[derive(Debug, Args)]
pub struct GcArgs {
    /// Remove runtime artifacts older than this duration (`30s`, `5m`, `1h`).
    #[arg(long, default_value = "24h", value_parser = parse_duration)]
    older_than: Duration,
}

pub fn run(args: GcArgs, globals: &GlobalFlags) -> Result<()> {
    if args.older_than.is_zero() {
        bail!("--older-than must be greater than zero");
    }
    let report = gc::collect_runtime(args.older_than).context("collecting runtime garbage")?;
    let (abandoned, messages_abandoned, repaired) =
        match WorkspaceResolver::resolve(".", globals.root.clone()) {
            Ok(workspace) => match open_ledger(&workspace) {
                Ok(ledger) => {
                    // Repair before the sweep: the sweep's forced publish folds the
                    // log and would self-heal a corpse itself, leaving this explicit
                    // repair nothing to find — and the report below silent about a
                    // cut this very run made.
                    let repaired = ledger
                        .repair_event_log()
                        .context("repairing the event log")?;
                    let abandoned = ledger
                        .abandon_dead_owned_items(&workspace.session_name)
                        .context("abandoning dead owned feed items")?;
                    let messages_abandoned = ledger
                        .abandon_orphan_messages(&workspace.session_name)
                        .context("abandoning orphan queued messages")?;
                    (abandoned, messages_abandoned, Some(repaired))
                }
                Err(err) => {
                    tracing::debug!(
                        error = %err,
                        "workspace ledger unavailable; runtime gc continues"
                    );
                    (0, 0, None)
                }
            },
            Err(_) => (0, 0, None),
        };
    let prune = gc::prune_dead_workspaces().context("pruning dead workspaces")?;
    let worktrees_swept = sweep_worktrees(globals);
    #[expect(clippy::print_stdout, reason = "user-facing maintenance report")]
    {
        println!("gc complete");
        println!("  older than    : {}s", args.older_than.as_secs());
        println!("  feed abandoned: {abandoned}");
        println!("  queue abandoned: {messages_abandoned}");
        if let Some(repair) = repaired.filter(rimz::ledger::event_log::RepairOutcome::truncated) {
            println!(
                "  log repaired  : {} bytes cut ({} frames kept)",
                repair.bytes_truncated, repair.frames_kept
            );
        }
        println!("  runtime roots : {}", report.runtime_roots_scanned);
        println!("  heartbeats    : {}", report.heartbeat_files_removed);
        println!("  sidebar socks : {}", report.sidebar_sockets_removed);
        println!("  sidecars      : {}", report.sidecar_files_removed);
        println!("  dirs removed  : {}", report.dirs_removed);
        println!("  bytes removed : {}", report.bytes_removed);
        println!("  workspaces    : {}", prune.removed.len());
        println!("  worktrees swept: {worktrees_swept}");
        print_prune_removals(&prune);
        if !prune.retained_unreadable.is_empty() {
            println!(
                "  retained      : {} (unreadable record + history)",
                prune.retained_unreadable.len()
            );
        }
    }
    Ok(())
}

fn sweep_worktrees(globals: &GlobalFlags) -> usize {
    let Ok(workspace) = WorkspaceResolver::resolve(".", globals.root.clone()) else {
        return 0;
    };
    if workspace.root_class != rimz::workspace::RootClass::Repo {
        return 0;
    }
    let live_cwds = match rimz::mux::auto_detect_backend(globals.mux) {
        Ok(mux) => rimz::mux::backend_for(mux)
            .list_panes(rimz::mux::PaneListOptions::default())
            .map(|listing| live_user_cwds(&listing.panes))
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    let entries = match rimz::worktree::list(&workspace.project_root) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::debug!(error = %err, "worktree gc skipped");
            return 0;
        }
    };
    let mut swept = 0;
    for entry in entries {
        if live_cwds
            .iter()
            .any(|cwd| rimz::worktree::path_inside(cwd, &entry.path))
            || entry.dirty
            || entry.landed != Some(true)
        {
            continue;
        }
        let Some(marker) = rimz::worktree::read_marker_for_worktree(&entry.path)
            .ok()
            .flatten()
        else {
            continue;
        };
        match rimz::worktree::remove_marked_worktree(
            &workspace.project_root,
            &entry.path,
            &marker,
            false,
        ) {
            Ok(branch) => {
                swept += 1;
                if branch == rimz::worktree::BranchDeletion::KeptUnmerged {
                    tracing::debug!(
                        path = %entry.path.display(),
                        branch = %marker.branch,
                        "worktree gc removed checkout but kept branch not proven landed",
                    );
                }
            }
            Err(err) => tracing::debug!(
                path = %entry.path.display(),
                error = %err,
                "worktree gc removal skipped",
            ),
        }
    }
    if swept > 0 {
        let _ = rimz::worktree::prune(&workspace.project_root);
    }
    swept
}

fn live_user_cwds<'a>(panes: impl IntoIterator<Item = &'a rimz::feed::PaneRef>) -> Vec<PathBuf> {
    panes
        .into_iter()
        .filter(|pane| !pane.is_rimz_sidebar())
        .filter_map(|pane| pane.cwd.as_deref().map(PathBuf::from))
        .collect()
}

/// Render the per-workspace removal lines for the prune step.
#[expect(clippy::print_stdout, reason = "user-facing maintenance report")]
fn print_prune_removals(report: &gc::WorkspacePruneReport) {
    for removed in &report.removed {
        match &removed.project_root {
            Some(root) => println!(
                "  removed       : {} {}",
                removed.workspace_id,
                root.display()
            ),
            None => println!(
                "  removed       : {} (abandoned scaffold)",
                removed.workspace_id
            ),
        }
    }
}

fn parse_duration(raw: &str) -> std::result::Result<Duration, String> {
    super::parse::parse_duration_units(raw, &[("s", 1), ("m", 60), ("h", 3600)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::{MuxName, PaneId};

    #[test]
    fn live_user_cwds_excludes_sidebar_chrome() {
        let panes = vec![
            pane(
                "terminal_side",
                Some("rimz-sidebar"),
                Some("/repo-worktrees/demo"),
            ),
            pane(
                "terminal_agent",
                Some("codex"),
                Some("/repo-worktrees/demo"),
            ),
            pane(
                "terminal_shell",
                Some("zsh"),
                Some("/repo-worktrees/demo/src"),
            ),
            pane("terminal_unknown", None, Some("/repo-worktrees/demo")),
            pane("terminal_empty", Some("zsh"), None),
        ];

        assert_eq!(
            live_user_cwds(&panes),
            vec![
                PathBuf::from("/repo-worktrees/demo"),
                PathBuf::from("/repo-worktrees/demo/src"),
                PathBuf::from("/repo-worktrees/demo")
            ]
        );
    }

    fn pane(raw: &str, command: Option<&str>, cwd: Option<&str>) -> rimz::feed::PaneRef {
        rimz::feed::PaneRef {
            command: command.map(ToOwned::to_owned),
            cwd: cwd.map(ToOwned::to_owned),
            ..rimz::feed::PaneRef::from_id(PaneId::from_parts(MuxName::Zellij, raw))
        }
    }
}
