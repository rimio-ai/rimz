//! `rimz gc` — reclaim stale maintenance state.

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Args;
use serde::Serialize;

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
    /// Report what gc would remove without removing anything.
    #[arg(long)]
    dry_run: bool,
    /// Emit the garbage collection report as JSON.
    #[arg(long)]
    json: bool,
}

pub fn run(args: GcArgs, globals: &GlobalFlags) -> Result<()> {
    if args.older_than.is_zero() {
        bail!("--older-than must be greater than zero");
    }
    let spinner = Spinner::new("starting gc…");
    spinner.set("sweeping runtime hints…");
    let report =
        gc::collect_runtime(args.older_than, args.dry_run).context("collecting runtime garbage")?;
    let (messages_archived, messages_reconciled, repaired, carryover_pruned) = if args.dry_run {
        (0, 0, None, 0)
    } else {
        spinner.set("repairing ledger…");
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
                    spinner.set("archiving orphan messages…");
                    let messages_archived = ledger
                        .archive_orphan_messages(&workspace.session_name)
                        .context("archiving orphan messages")?;
                    let reconcile = ledger
                        .reconcile_stale_sent_messages(
                            &workspace.session_name,
                            jiff::Timestamp::now(),
                            rimz::message::delivery_window_from_env(),
                            rimz::message::max_delivery_attempts_from_env(),
                        )
                        .context("reconciling sent messages")?;
                    spinner.set("pruning ledger caches...");
                    let carryover_pruned = ledger
                        .prune_carryover(rimz::ledger::event_log::DEFAULT_RETENTION)
                        .context("pruning carryover agents")?;
                    (
                        messages_archived,
                        reconcile.requeued + reconcile.timed_out,
                        Some(repaired),
                        carryover_pruned,
                    )
                }
                Err(err) => {
                    tracing::debug!(
                        error = %err,
                        "workspace ledger unavailable; runtime gc continues"
                    );
                    (0, 0, None, 0)
                }
            },
            Err(_) => (0, 0, None, 0),
        }
    };
    spinner.set("reaping dead schedules…");
    let schedules_reaped = if args.dry_run {
        0
    } else {
        super::loop_cmd::reap_dead_delivery_schedules().context("reaping dead loop schedules")?
    };
    spinner.set("pruning dead workspaces…");
    let prune = gc::prune_dead_workspaces(args.dry_run).context("pruning dead workspaces")?;
    spinner.set("sweeping orphan temps…");
    let temps = gc::collect_orphan_temps(args.older_than, args.dry_run);
    let worktrees = sweep_worktrees(globals, &spinner, args.dry_run);
    let outcome = GcOutcome {
        dry_run: args.dry_run,
        older_than: args.older_than,
        runtime: report,
        temps,
        repaired,
        queue_archived: messages_archived,
        queue_reconciled: messages_reconciled,
        carryover_pruned,
        schedules_reaped,
        prune,
        worktrees,
    };
    drop(spinner);
    if args.json {
        print_json_report(&outcome)?;
    } else {
        let mut out = render::out();
        render_report(&outcome, &mut out)?;
    }
    Ok(())
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct GcOutcome {
    dry_run: bool,
    older_than: Duration,
    runtime: gc::GcReport,
    temps: gc::TempSweepReport,
    repaired: Option<RepairOutcome>,
    queue_archived: usize,
    queue_reconciled: usize,
    carryover_pruned: usize,
    schedules_reaped: usize,
    prune: gc::WorkspacePruneReport,
    worktrees: WorktreeSweep,
}

impl GcOutcome {
    fn reclaimed_bytes(&self) -> u64 {
        self.runtime
            .bytes_removed
            .saturating_add(self.temps.bytes_removed)
            .saturating_add(self.prune.bytes_removed())
            .saturating_add(self.worktrees.bytes())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct WorktreeSweep {
    removed: Vec<SweptWorktree>,
    failed: Vec<FailedWorktree>,
}

impl WorktreeSweep {
    fn swept(&self) -> usize {
        self.removed.len()
    }

    fn bytes(&self) -> u64 {
        self.removed
            .iter()
            .fold(0_u64, |total, removed| total.saturating_add(removed.bytes))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SweptWorktree {
    name: String,
    branch: String,
    path: PathBuf,
    bytes: u64,
    branch_deletion: Option<rimz::worktree::BranchDeletion>,
    archive_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FailedWorktree {
    path: PathBuf,
    error: String,
}

fn sweep_worktrees(globals: &GlobalFlags, spinner: &Spinner, dry_run: bool) -> WorktreeSweep {
    let Ok(workspace) = WorkspaceResolver::resolve(".", globals.root.clone()) else {
        return WorktreeSweep::default();
    };
    if workspace.root_class != rimz::workspace::RootClass::Repo {
        return WorktreeSweep::default();
    }
    let ledger = match open_ledger_for_worktree_gc(&workspace, dry_run) {
        Ok(Some(ledger)) => ledger,
        Ok(None) => {
            tracing::debug!("workspace ledger absent; worktree gc skipped");
            return WorktreeSweep::default();
        }
        Err(err) => {
            tracing::debug!(
                error = %err,
                "workspace ledger unavailable; worktree gc skipped"
            );
            return WorktreeSweep::default();
        }
    };
    let snapshot =
        match super::alive_snapshot(&ledger, ledger.runtime_paths(), &workspace.session_name) {
            Ok(snapshot) => snapshot,
            Err(err) => {
                tracing::debug!(
                    error = %err,
                    "agent roster unavailable; worktree gc skipped"
                );
                return WorktreeSweep::default();
            }
        };
    let mut protected_paths = match rimz::mux::auto_detect_backend(globals.mux) {
        Ok(mux) => rimz::mux::backend_for(mux)
            .list_panes(rimz::mux::PaneListOptions {
                session_name: Some(workspace.session_name.clone()),
                workspace_id: Some(workspace.workspace_id.clone()),
                ..Default::default()
            })
            .map(|listing| live_user_cwds(&listing.panes))
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    protected_paths.extend(super::worktree::agent_pinned_paths(&snapshot.agents, None));
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
            let path = rimz::worktree::normalize_path_lexical(&entry.path);
            !protected_paths
                .iter()
                .any(|cwd| rimz::worktree::path_inside(cwd, &path))
                && !entry.dirty
                && entry.landed == Some(true)
        })
        .collect();
    let total = candidates.len();
    let mut sweep = WorktreeSweep::default();
    for (i, entry) in candidates.into_iter().enumerate() {
        let action = if dry_run { "checking" } else { "removing" };
        spinner.set(format!(
            "{action} worktree [{}/{}] {}",
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
        if dry_run {
            sweep.removed.push(SweptWorktree {
                name: marker.name,
                branch: marker.branch,
                path: entry.path,
                bytes,
                branch_deletion: None,
                archive_error: None,
            });
            continue;
        }
        match super::worktree::remove_and_archive(
            &marker,
            || {
                rimz::worktree::remove_marked_worktree(
                    &workspace.project_root,
                    &entry.path,
                    &marker,
                    false,
                )
                .map_err(Into::into)
            },
            |channel, reason| {
                ledger
                    .archive_channel_messages(channel, reason, &workspace.session_name)
                    .map(|_| ())
                    .map_err(Into::into)
            },
        ) {
            Ok(removed) => {
                sweep.removed.push(SweptWorktree {
                    name: marker.name,
                    branch: marker.branch,
                    path: entry.path,
                    bytes,
                    branch_deletion: Some(removed.branch_deletion),
                    archive_error: removed.archive.err().map(|err| err.to_string()),
                });
            }
            Err(err) => sweep.failed.push(FailedWorktree {
                path: entry.path,
                error: err.to_string(),
            }),
        }
    }
    if sweep.swept() > 0 && !dry_run {
        let _ = rimz::worktree::prune(&workspace.project_root);
    }
    sweep
}

fn open_ledger_for_worktree_gc(
    workspace: &rimz::ResolvedWorkspace,
    dry_run: bool,
) -> Result<Option<rimz::Ledger>> {
    if !dry_run {
        return open_ledger(workspace).map(Some);
    }
    let paths = rimz::StatePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing ledger paths")?;
    let runtime = rimz::RuntimePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing runtime paths")?;
    Ok(rimz::Ledger::open_existing(paths, runtime))
}

fn live_user_cwds<'a>(panes: impl IntoIterator<Item = &'a rimz::pane::PaneRef>) -> Vec<PathBuf> {
    panes
        .into_iter()
        .filter(|pane| !pane.is_rimz_sidebar())
        .filter_map(|pane| pane.cwd.as_deref().map(PathBuf::from))
        .collect()
}

fn render_report(out: &GcOutcome, w: &mut impl Write) -> io::Result<()> {
    let reclaimed = out.reclaimed_bytes();
    let active = reclaimed > 0
        || runtime_items(&out.runtime) > 0
        || out.temps.files_removed > 0
        || out.queue_archived > 0
        || out.queue_reconciled > 0
        || out.carryover_pruned > 0
        || out.schedules_reaped > 0
        || out.repaired.as_ref().is_some_and(RepairOutcome::truncated)
        || !out.prune.removed.is_empty()
        || !out.prune.retained_unreadable.is_empty()
        || out.worktrees.swept() > 0
        || !out.worktrees.failed.is_empty();

    if active {
        let header = if out.dry_run {
            format!("gc would reclaim {} (dry run)", fmt_bytes(reclaimed))
        } else {
            format!("gc reclaimed {}", fmt_bytes(reclaimed))
        };
        writeln!(w, "{}", paint(palette::ACCENT.bold(), &header))?;
    } else {
        let header = if out.dry_run {
            "gc — nothing to reclaim (dry run)"
        } else {
            "gc — nothing to reclaim"
        };
        writeln!(w, "{}", paint(palette::ACCENT.bold(), header))?;
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
    if out.worktrees.swept() > 0 {
        table.row([
            cell("worktrees"),
            cell(plural(out.worktrees.swept(), "swept", "swept")),
            cell(fmt_bytes(out.worktrees.bytes())),
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
    if out.temps.files_removed > 0 {
        table.row([
            cell("temp"),
            cell(plural(out.temps.files_removed, "orphan", "orphans")),
            cell(fmt_bytes(out.temps.bytes_removed)),
            cell("interrupted writes"),
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
    if out.worktrees.swept() > 0
        || !out.prune.removed.is_empty()
        || out.temps.files_removed > 0
        || runtime_items > 0
    {
        writeln!(w)?;
        table.render(w)?;
    }

    if out.queue_archived > 0 {
        report_note(w, &format!("messages archived: {}", out.queue_archived))?;
    }
    if out.queue_reconciled > 0 {
        report_note(w, &format!("messages reconciled: {}", out.queue_reconciled))?;
    }
    if out.carryover_pruned > 0 {
        report_note(
            w,
            &format!("carryover agents pruned: {}", out.carryover_pruned),
        )?;
    }
    if out.schedules_reaped > 0 {
        report_note(w, &format!("schedules reaped: {}", out.schedules_reaped))?;
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
    if out.dry_run {
        report_note(w, "ledger maintenance skipped (dry run)")?;
    }
    let worktree_verb = if out.dry_run {
        "would sweep worktree"
    } else {
        "worktree swept"
    };
    for removed in &out.worktrees.removed {
        let mut note = format!(
            "{worktree_verb}: {} {} ({})",
            removed.name,
            removed.path.display(),
            fmt_bytes(removed.bytes)
        );
        if !out.dry_run {
            if removed.branch_deletion == Some(rimz::worktree::BranchDeletion::KeptUnmerged) {
                note.push_str(" — branch kept: not proven merged");
            }
            if let Some(err) = &removed.archive_error {
                note.push_str(&format!(" — message archive failed: {err}"));
            }
        }
        report_note(w, &note)?;
    }
    for failed in &out.worktrees.failed {
        report_note(
            w,
            &format!(
                "worktree failed: {} — {}",
                failed.path.display(),
                failed.error
            ),
        )?;
    }
    let workspace_verb = if out.dry_run {
        "would prune workspace"
    } else {
        "workspace pruned"
    };
    for removed in &out.prune.removed {
        match &removed.project_root {
            Some(root) => report_note(
                w,
                &format!(
                    "{workspace_verb}: {} {} ({})",
                    removed.workspace_id,
                    root.display(),
                    fmt_bytes(removed.bytes)
                ),
            )?,
            None => report_note(
                w,
                &format!(
                    "{workspace_verb}: {} (abandoned scaffold, {})",
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

fn print_json_report(outcome: &GcOutcome) -> Result<()> {
    let rendered = serde_json::to_string_pretty(&JsonReport::from(outcome))?;
    #[expect(clippy::print_stdout, reason = "json emitter")]
    {
        println!("{rendered}");
    }
    Ok(())
}

#[derive(Serialize)]
struct JsonReport {
    dry_run: bool,
    older_than_secs: u64,
    reclaimed_bytes: u64,
    worktrees: JsonWorktrees,
    workspaces: JsonWorkspaces,
    temps: JsonTemps,
    runtime: JsonRuntime,
    messages: JsonMessages,
    carryover_pruned: usize,
    schedules_reaped: usize,
    repair: Option<JsonRepair>,
    ledger_maintenance_skipped: bool,
}

impl From<&GcOutcome> for JsonReport {
    fn from(out: &GcOutcome) -> Self {
        Self {
            dry_run: out.dry_run,
            older_than_secs: out.older_than.as_secs(),
            reclaimed_bytes: out.reclaimed_bytes(),
            worktrees: JsonWorktrees::from(&out.worktrees),
            workspaces: JsonWorkspaces::from(&out.prune),
            temps: JsonTemps::from(&out.temps),
            runtime: JsonRuntime::from(&out.runtime),
            messages: JsonMessages {
                archived: out.queue_archived,
                reconciled: out.queue_reconciled,
            },
            carryover_pruned: out.carryover_pruned,
            schedules_reaped: out.schedules_reaped,
            repair: out.repaired.map(JsonRepair::from),
            ledger_maintenance_skipped: out.dry_run,
        }
    }
}

#[derive(Serialize)]
struct JsonWorktrees {
    removed: Vec<JsonSweptWorktree>,
    failed: Vec<JsonFailedWorktree>,
}

impl From<&WorktreeSweep> for JsonWorktrees {
    fn from(sweep: &WorktreeSweep) -> Self {
        Self {
            removed: sweep.removed.iter().map(JsonSweptWorktree::from).collect(),
            failed: sweep.failed.iter().map(JsonFailedWorktree::from).collect(),
        }
    }
}

#[derive(Serialize)]
struct JsonSweptWorktree {
    name: String,
    branch: String,
    path: String,
    bytes: u64,
    branch_deleted: Option<bool>,
    archive_error: Option<String>,
}

impl From<&SweptWorktree> for JsonSweptWorktree {
    fn from(worktree: &SweptWorktree) -> Self {
        Self {
            name: worktree.name.clone(),
            branch: worktree.branch.clone(),
            path: path_string(&worktree.path),
            bytes: worktree.bytes,
            branch_deleted: worktree
                .branch_deletion
                .map(|deletion| deletion == rimz::worktree::BranchDeletion::Deleted),
            archive_error: worktree.archive_error.clone(),
        }
    }
}

#[derive(Serialize)]
struct JsonFailedWorktree {
    path: String,
    error: String,
}

impl From<&FailedWorktree> for JsonFailedWorktree {
    fn from(failed: &FailedWorktree) -> Self {
        Self {
            path: path_string(&failed.path),
            error: failed.error.clone(),
        }
    }
}

#[derive(Serialize)]
struct JsonWorkspaces {
    removed: Vec<JsonRemovedWorkspace>,
    retained_unreadable: Vec<JsonRetainedWorkspace>,
    kept: usize,
}

impl From<&gc::WorkspacePruneReport> for JsonWorkspaces {
    fn from(report: &gc::WorkspacePruneReport) -> Self {
        Self {
            removed: report
                .removed
                .iter()
                .map(JsonRemovedWorkspace::from)
                .collect(),
            retained_unreadable: report
                .retained_unreadable
                .iter()
                .map(JsonRetainedWorkspace::from)
                .collect(),
            kept: report.kept,
        }
    }
}

#[derive(Serialize)]
struct JsonRemovedWorkspace {
    workspace_id: String,
    reason: &'static str,
    project_root: Option<String>,
    bytes: u64,
}

impl From<&gc::RemovedWorkspace> for JsonRemovedWorkspace {
    fn from(workspace: &gc::RemovedWorkspace) -> Self {
        Self {
            workspace_id: workspace.workspace_id.to_string(),
            reason: prune_reason_json(workspace.reason),
            project_root: workspace.project_root.as_deref().map(path_string),
            bytes: workspace.bytes,
        }
    }
}

#[derive(Serialize)]
struct JsonRetainedWorkspace {
    workspace_id: String,
    error: String,
}

impl From<&(rimz::WorkspaceId, String)> for JsonRetainedWorkspace {
    fn from((workspace_id, error): &(rimz::WorkspaceId, String)) -> Self {
        Self {
            workspace_id: workspace_id.to_string(),
            error: error.clone(),
        }
    }
}

#[derive(Serialize)]
struct JsonTemps {
    files_removed: usize,
    bytes_removed: u64,
}

impl From<&gc::TempSweepReport> for JsonTemps {
    fn from(report: &gc::TempSweepReport) -> Self {
        Self {
            files_removed: report.files_removed,
            bytes_removed: report.bytes_removed,
        }
    }
}

#[derive(Serialize)]
struct JsonRuntime {
    roots_scanned: usize,
    heartbeats_removed: usize,
    sidecars_removed: usize,
    sockets_removed: usize,
    probe_markers_removed: usize,
    dirs_removed: usize,
    bytes_removed: u64,
}

impl From<&gc::GcReport> for JsonRuntime {
    fn from(report: &gc::GcReport) -> Self {
        Self {
            roots_scanned: report.runtime_roots_scanned,
            heartbeats_removed: report.heartbeat_files_removed,
            sidecars_removed: report.sidecar_files_removed,
            sockets_removed: report.sidebar_sockets_removed,
            probe_markers_removed: report.probe_markers_removed,
            dirs_removed: report.dirs_removed,
            bytes_removed: report.bytes_removed,
        }
    }
}

#[derive(Serialize)]
struct JsonMessages {
    archived: usize,
    reconciled: usize,
}

#[derive(Serialize)]
struct JsonRepair {
    bytes_truncated: u64,
    frames_kept: usize,
}

impl From<RepairOutcome> for JsonRepair {
    fn from(repair: RepairOutcome) -> Self {
        Self {
            bytes_truncated: repair.bytes_truncated,
            frames_kept: repair.frames_kept,
        }
    }
}

fn path_string(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

fn prune_reason_json(reason: gc::PruneReason) -> &'static str {
    match reason {
        gc::PruneReason::ProjectRootGone => "project_root_gone",
        gc::PruneReason::AbandonedScaffold => "abandoned_scaffold",
    }
}

fn report_note(w: &mut impl Write, text: &str) -> io::Result<()> {
    writeln!(w, "  {}", paint(palette::MUTED, text))
}

fn runtime_items(report: &gc::GcReport) -> usize {
    report.heartbeat_files_removed
        + report.sidebar_sockets_removed
        + report.sidecar_files_removed
        + report.probe_markers_removed
        + report.dirs_removed
}

fn runtime_breakdown(report: &gc::GcReport) -> String {
    [
        (report.heartbeat_files_removed, "heartbeat", "heartbeats"),
        (report.sidebar_sockets_removed, "socket", "sockets"),
        (report.sidecar_files_removed, "sidecar", "sidecars"),
        (report.probe_markers_removed, "probe", "probes"),
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
        let out = strip_report(&full_outcome(false));
        assert!(out.contains("gc reclaimed"));
        assert!(out.contains("worktrees"));
        assert!(out.contains("workspaces"));
        assert!(out.contains("runtime"));
        assert!(out.contains("temp"));
        assert!(out.contains("carryover agents pruned"));
        assert!(out.contains("orphans"));
        assert!(out.contains("heartbeat"));
        assert!(out.contains("sidecars"));
        assert!(out.contains("probe"));
        assert!(out.contains("1.4 GB"));
    }

    #[test]
    fn render_report_lists_worktree_outcomes() {
        let out = strip_report(&full_outcome(false));

        assert!(out.contains("worktree swept: demo /repo-worktrees/demo"));
        assert!(out.contains(
            "worktree swept: gc-info /repo-worktrees/gc-info (12 B) — branch kept: not proven merged"
        ));
        assert!(out.contains("message archive failed: archive boom"));
        assert!(out.contains("worktree failed: /repo-worktrees/wip — remove boom"));
    }

    #[test]
    fn render_report_uses_dry_run_framing() {
        let out = strip_report(&full_outcome(true));

        assert!(out.contains("gc would reclaim"));
        assert!(out.contains("ledger maintenance skipped (dry run)"));
        assert!(out.contains("would sweep worktree: demo /repo-worktrees/demo"));
        assert!(out.contains("would prune workspace: ws_0123456789abcdef01234567 /gone"));
        assert!(!out.contains("branch kept"));
        assert!(!out.contains("message archive failed"));
    }

    #[test]
    fn json_report_serializes_gc_schema() {
        let value = serde_json::to_value(JsonReport::from(&full_outcome(false))).unwrap();

        assert_eq!(value["dry_run"], false);
        assert_eq!(value["older_than_secs"], 3600);
        assert_eq!(value["worktrees"]["removed"][0]["name"], "demo");
        assert_eq!(value["worktrees"]["removed"][0]["branch_deleted"], true);
        assert_eq!(value["worktrees"]["removed"][1]["branch_deleted"], false);
        assert_eq!(
            value["worktrees"]["removed"][1]["archive_error"],
            "archive boom"
        );
        assert_eq!(value["worktrees"]["failed"][0]["error"], "remove boom");
        assert_eq!(
            value["workspaces"]["removed"][0]["reason"],
            "project_root_gone"
        );
        assert_eq!(value["runtime"]["roots_scanned"], 2);
        assert_eq!(value["messages"]["archived"], 1);
        assert_eq!(value["repair"]["bytes_truncated"], 9);

        let dry_run = serde_json::to_value(JsonReport::from(&full_outcome(true))).unwrap();
        assert!(dry_run["repair"].is_null());
        assert!(dry_run["worktrees"]["removed"][0]["branch_deleted"].is_null());
        assert_eq!(dry_run["ledger_maintenance_skipped"], true);
    }

    #[test]
    fn render_report_names_empty_runs() {
        let out = strip_report(&GcOutcome {
            older_than: Duration::from_secs(3600),
            ..GcOutcome::default()
        });
        assert!(out.contains("nothing to reclaim"));
    }

    fn full_outcome(dry_run: bool) -> GcOutcome {
        GcOutcome {
            dry_run,
            older_than: Duration::from_secs(3600),
            runtime: gc::GcReport {
                runtime_roots_scanned: 2,
                heartbeat_files_removed: 1,
                sidecar_files_removed: 2,
                sidebar_sockets_removed: 0,
                probe_markers_removed: 1,
                dirs_removed: 1,
                bytes_removed: 13_018,
            },
            temps: gc::TempSweepReport {
                files_removed: 2,
                bytes_removed: 68,
            },
            repaired: (!dry_run).then_some(RepairOutcome {
                bytes_truncated: 9,
                frames_kept: 3,
            }),
            queue_archived: usize::from(!dry_run),
            queue_reconciled: 0,
            carryover_pruned: usize::from(!dry_run),
            schedules_reaped: 0,
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
                removed: vec![
                    SweptWorktree {
                        name: "demo".to_owned(),
                        branch: "demo".to_owned(),
                        path: PathBuf::from("/repo-worktrees/demo"),
                        bytes: 1_503_238_553,
                        branch_deletion: (!dry_run)
                            .then_some(rimz::worktree::BranchDeletion::Deleted),
                        archive_error: None,
                    },
                    SweptWorktree {
                        name: "gc-info".to_owned(),
                        branch: "gc-info".to_owned(),
                        path: PathBuf::from("/repo-worktrees/gc-info"),
                        bytes: 12,
                        branch_deletion: (!dry_run)
                            .then_some(rimz::worktree::BranchDeletion::KeptUnmerged),
                        archive_error: (!dry_run).then_some("archive boom".to_owned()),
                    },
                ],
                failed: if dry_run {
                    Vec::new()
                } else {
                    vec![FailedWorktree {
                        path: PathBuf::from("/repo-worktrees/wip"),
                        error: "remove boom".to_owned(),
                    }]
                },
            },
        }
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
