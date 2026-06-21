//! `rimz gc` — remove stale runtime liveness hints.

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Args;

use super::render::{self, Table, cell, fmt_bytes, paint, palette};
use super::spinner::Spinner;
use super::{GlobalFlags, open_ledger};
use rimz::ledger::event_log::RepairOutcome;
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
    let spinner = Spinner::new("starting gc…");
    spinner.set("sweeping runtime hints…");
    let report = gc::collect_runtime(args.older_than).context("collecting runtime garbage")?;
    spinner.set("repairing ledger…");
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
                    spinner.set("abandoning dead items…");
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
    spinner.set("pruning dead workspaces…");
    let prune = gc::prune_dead_workspaces().context("pruning dead workspaces")?;
    let worktrees = sweep_worktrees(globals, &spinner);
    let outcome = GcOutcome {
        older_than: args.older_than,
        runtime: report,
        repaired,
        feed_abandoned: abandoned,
        queue_abandoned: messages_abandoned,
        prune,
        worktrees,
    };
    drop(spinner);
    let mut out = render::out();
    render_report(&outcome, &mut out)?;
    Ok(())
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct GcOutcome {
    older_than: Duration,
    runtime: gc::GcReport,
    repaired: Option<RepairOutcome>,
    feed_abandoned: usize,
    queue_abandoned: usize,
    prune: gc::WorkspacePruneReport,
    worktrees: WorktreeSweep,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct WorktreeSweep {
    swept: usize,
    bytes: u64,
}

fn sweep_worktrees(globals: &GlobalFlags, spinner: &Spinner) -> WorktreeSweep {
    let Ok(workspace) = WorkspaceResolver::resolve(".", globals.root.clone()) else {
        return WorktreeSweep::default();
    };
    if workspace.root_class != rimz::workspace::RootClass::Repo {
        return WorktreeSweep::default();
    }
    let live_cwds = match rimz::mux::auto_detect_backend(globals.mux) {
        Ok(mux) => rimz::mux::backend_for(mux)
            .list_panes(rimz::mux::PaneListOptions::default())
            .map(|listing| live_user_cwds(&listing.panes))
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    spinner.set("scanning worktrees…");
    let entries = match rimz::worktree::list(&workspace.project_root) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::debug!(error = %err, "worktree gc skipped");
            return WorktreeSweep::default();
        }
    };
    let candidates: Vec<_> = entries
        .into_iter()
        .filter(|entry| {
            !live_cwds
                .iter()
                .any(|cwd| rimz::worktree::path_inside(cwd, &entry.path))
                && !entry.dirty
                && entry.landed == Some(true)
        })
        .collect();
    let total = candidates.len();
    let mut sweep = WorktreeSweep::default();
    for (i, entry) in candidates.into_iter().enumerate() {
        spinner.set(format!(
            "removing worktree [{}/{}] {}",
            i + 1,
            total,
            entry.name
        ));
        let Some(marker) = rimz::worktree::read_marker_for_worktree(&entry.path)
            .ok()
            .flatten()
        else {
            continue;
        };
        let bytes = rimz::storage::dir_size(&entry.path);
        match rimz::worktree::remove_marked_worktree(
            &workspace.project_root,
            &entry.path,
            &marker,
            false,
        ) {
            Ok(branch) => {
                sweep.swept += 1;
                sweep.bytes = sweep.bytes.saturating_add(bytes);
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
    if sweep.swept > 0 {
        let _ = rimz::worktree::prune(&workspace.project_root);
    }
    sweep
}

fn live_user_cwds<'a>(panes: impl IntoIterator<Item = &'a rimz::pane::PaneRef>) -> Vec<PathBuf> {
    panes
        .into_iter()
        .filter(|pane| !pane.is_rimz_sidebar())
        .filter_map(|pane| pane.cwd.as_deref().map(PathBuf::from))
        .collect()
}

fn render_report(out: &GcOutcome, w: &mut impl Write) -> io::Result<()> {
    let reclaimed = out
        .runtime
        .bytes_removed
        .saturating_add(out.prune.bytes_removed())
        .saturating_add(out.worktrees.bytes);
    let active = reclaimed > 0
        || runtime_items(&out.runtime) > 0
        || out.feed_abandoned > 0
        || out.queue_abandoned > 0
        || out.repaired.as_ref().is_some_and(RepairOutcome::truncated)
        || !out.prune.removed.is_empty()
        || !out.prune.retained_unreadable.is_empty()
        || out.worktrees.swept > 0;

    if active {
        writeln!(
            w,
            "{}",
            paint(
                palette::ACCENT.bold(),
                &format!("gc reclaimed {}", fmt_bytes(reclaimed))
            )
        )?;
    } else {
        writeln!(
            w,
            "{}",
            paint(palette::ACCENT.bold(), "gc — nothing to reclaim")
        )?;
    }
    writeln!(
        w,
        "  {}",
        paint(
            palette::MUTED,
            &format!("older than {}s", out.older_than.as_secs())
        )
    )?;

    let mut table = Table::new(["CLASS", "COUNT", "BYTES", "DETAIL"]).right(&[2]);
    if out.worktrees.swept > 0 {
        table.row([
            cell("worktrees"),
            cell(plural(out.worktrees.swept, "swept", "swept")),
            cell(fmt_bytes(out.worktrees.bytes)),
            cell("landed checkouts"),
        ]);
    }
    if !out.prune.removed.is_empty() {
        table.row([
            cell("workspaces"),
            cell(plural(out.prune.removed.len(), "pruned", "pruned")),
            cell(fmt_bytes(out.prune.bytes_removed())),
            cell("dead ledgers"),
        ]);
    }
    let runtime_items = runtime_items(&out.runtime);
    if runtime_items > 0 {
        table.row([
            cell("runtime"),
            cell(plural(runtime_items, "item", "items")),
            cell(fmt_bytes(out.runtime.bytes_removed)),
            cell(runtime_breakdown(&out.runtime)),
        ]);
    }
    if out.worktrees.swept > 0 || !out.prune.removed.is_empty() || runtime_items > 0 {
        writeln!(w)?;
        table.render(w)?;
    }

    if out.feed_abandoned > 0 {
        report_note(w, &format!("feed abandoned: {}", out.feed_abandoned))?;
    }
    if out.queue_abandoned > 0 {
        report_note(w, &format!("queue abandoned: {}", out.queue_abandoned))?;
    }
    if let Some(repair) = out.repaired.filter(RepairOutcome::truncated) {
        report_note(
            w,
            &format!(
                "log repaired: {} cut ({} frames kept)",
                fmt_bytes(repair.bytes_truncated),
                repair.frames_kept
            ),
        )?;
    }
    for removed in &out.prune.removed {
        match &removed.project_root {
            Some(root) => report_note(
                w,
                &format!(
                    "workspace pruned: {} {} ({})",
                    removed.workspace_id,
                    root.display(),
                    fmt_bytes(removed.bytes)
                ),
            )?,
            None => report_note(
                w,
                &format!(
                    "workspace pruned: {} (abandoned scaffold, {})",
                    removed.workspace_id,
                    fmt_bytes(removed.bytes)
                ),
            )?,
        }
    }
    if !out.prune.retained_unreadable.is_empty() {
        report_note(
            w,
            &format!(
                "retained: {} (unreadable record + history)",
                out.prune.retained_unreadable.len()
            ),
        )?;
    }
    if out.runtime.runtime_roots_scanned > 0 {
        report_note(
            w,
            &format!(
                "scanned {} runtime roots",
                out.runtime.runtime_roots_scanned
            ),
        )?;
    }
    Ok(())
}

fn report_note(w: &mut impl Write, text: &str) -> io::Result<()> {
    writeln!(w, "  {}", paint(palette::MUTED, text))
}

fn runtime_items(report: &gc::GcReport) -> usize {
    report.heartbeat_files_removed
        + report.sidebar_sockets_removed
        + report.sidecar_files_removed
        + report.dirs_removed
}

fn runtime_breakdown(report: &gc::GcReport) -> String {
    [
        (report.heartbeat_files_removed, "heartbeat", "heartbeats"),
        (report.sidebar_sockets_removed, "socket", "sockets"),
        (report.sidecar_files_removed, "sidecar", "sidecars"),
        (report.dirs_removed, "dir", "dirs"),
    ]
    .into_iter()
    .filter(|(count, _, _)| *count > 0)
    .map(|(count, singular, plural_label)| plural(count, singular, plural_label))
    .collect::<Vec<_>>()
    .join(" · ")
}

fn plural(count: usize, singular: &str, plural: &str) -> String {
    let label = if count == 1 { singular } else { plural };
    format!("{count} {label}")
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

    #[test]
    fn render_report_groups_reclaimed_bytes_by_class() {
        let outcome = GcOutcome {
            older_than: Duration::from_secs(3600),
            runtime: gc::GcReport {
                runtime_roots_scanned: 2,
                heartbeat_files_removed: 1,
                sidecar_files_removed: 2,
                sidebar_sockets_removed: 0,
                dirs_removed: 1,
                bytes_removed: 13_018,
            },
            repaired: None,
            feed_abandoned: 3,
            queue_abandoned: 0,
            prune: gc::WorkspacePruneReport {
                removed: vec![gc::RemovedWorkspace {
                    workspace_id: rimz::WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
                    reason: gc::PruneReason::ProjectRootGone,
                    bytes: 2048,
                    project_root: Some(PathBuf::from("/gone")),
                }],
                kept: 0,
                retained_unreadable: Vec::new(),
            },
            worktrees: WorktreeSweep {
                swept: 2,
                bytes: 1_503_238_553,
            },
        };

        let out = strip_report(&outcome);
        assert!(out.contains("gc reclaimed"));
        assert!(out.contains("worktrees"));
        assert!(out.contains("workspaces"));
        assert!(out.contains("runtime"));
        assert!(out.contains("heartbeat"));
        assert!(out.contains("sidecars"));
        assert!(out.contains("1.4 GB"));
    }

    #[test]
    fn render_report_names_empty_runs() {
        let out = strip_report(&GcOutcome {
            older_than: Duration::from_secs(3600),
            ..GcOutcome::default()
        });
        assert!(out.contains("nothing to reclaim"));
    }

    fn strip_report(outcome: &GcOutcome) -> String {
        let mut stream = anstream::StripStream::new(Vec::new());
        render_report(outcome, &mut stream).expect("render report");
        String::from_utf8(stream.into_inner()).expect("utf-8")
    }

    fn pane(raw: &str, command: Option<&str>, cwd: Option<&str>) -> rimz::pane::PaneRef {
        rimz::pane::PaneRef {
            command: command.map(ToOwned::to_owned),
            cwd: cwd.map(ToOwned::to_owned),
            ..rimz::pane::PaneRef::from_id(PaneId::from_parts(MuxName::Zellij, raw))
        }
    }
}
