//! `rimz gc` — remove stale runtime liveness hints.

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
    let (abandoned, repaired) = match WorkspaceResolver::resolve(".", globals.root.clone()) {
        Ok(workspace) => {
            let ledger = open_ledger(&workspace)?;
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
            (abandoned, Some(repaired))
        }
        Err(_) => (0, None),
    };
    let prune = gc::prune_dead_workspaces().context("pruning dead workspaces")?;
    let worktrees_swept = sweep_worktrees(globals);
    #[expect(clippy::print_stdout, reason = "user-facing maintenance report")]
    {
        println!("gc complete");
        println!("  older than    : {}s", args.older_than.as_secs());
        println!("  feed abandoned: {abandoned}");
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
            .map(|panes| {
                panes
                    .into_iter()
                    .filter_map(|pane| pane.cwd.map(std::path::PathBuf::from))
                    .collect::<Vec<_>>()
            })
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
            || entry.commits_ahead != Some(0)
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
            Ok(()) => swept += 1,
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

    #[test]
    fn parse_duration_accepts_short_units() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert!(parse_duration("").is_err());
        assert!(parse_duration("30").is_err());
        assert!(parse_duration("30d").is_err());
    }
}
