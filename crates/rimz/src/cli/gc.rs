//! `rimz gc` — reclaim stale maintenance state through domain assessments.

use std::io::{self, Write};
#[cfg(test)]
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Args;
use serde::Serialize;

use super::render::{self, fmt_bytes, paint, palette};
use super::spinner::Spinner;
use super::{GlobalFlags, open_store};
use rimz::store::event_log::RepairOutcome;
use rimz::store::gc;
use rimz::utils::time::{DurationUnit, parse_duration_units};
use rimz::workspace::WorkspaceResolver;
use rimz::worktree::{FailedWorktree, KeptReason, KeptWorktree, SweptWorktree, WorktreeSweep};

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
    let store_maintenance = if args.dry_run {
        StoreMaintenance::SkippedDryRun
    } else {
        spinner.set("repairing store…");
        match WorkspaceResolver::resolve(".", globals.root.clone()) {
            Ok(workspace) => match open_store(&workspace) {
                Ok(store) => {
                    // Repair before the sweep: the sweep's forced publish folds the
                    // log and would self-heal a corpse itself, leaving this explicit
                    // repair nothing to find — and the report below silent about a
                    // cut this very run made.
                    let repaired = store
                        .repair_event_log()
                        .context("repairing the event log")?;
                    spinner.set("archiving orphan messages…");
                    let messages_archived = store
                        .archive_orphan_messages(&workspace.session_name)
                        .context("archiving orphan messages")?;
                    let reconcile = store
                        .reconcile_stale_sent_messages(
                            &workspace.session_name,
                            jiff::Timestamp::now(),
                            rimz::message::max_delivery_attempts_from_env(),
                            |_| false,
                        )
                        .context("reconciling sent messages")?;
                    spinner.set("pruning store caches...");
                    let carryover_pruned = store
                        .prune_carryover(rimz::store::event_log::DEFAULT_RETENTION)
                        .context("pruning carryover agents")?;
                    StoreMaintenance::Done {
                        archived: messages_archived,
                        reconciled: reconcile.requeued + reconcile.timed_out,
                        repaired,
                        carryover_pruned,
                    }
                }
                Err(err) => {
                    tracing::debug!(
                        error = %err,
                        "workspace store unavailable; runtime gc continues"
                    );
                    StoreMaintenance::SkippedNoStore
                }
            },
            Err(_) => StoreMaintenance::SkippedNoStore,
        }
    };
    spinner.set("reaping dead schedules…");
    let schedules_reaped = if args.dry_run {
        0
    } else {
        let reaped = rimz::harness::schedule::catalog::TaskCatalog::reap_dead_deliveries()
            .context("reaping dead loop schedules")?;
        let project_root = WorkspaceResolver::resolve(".", globals.root.clone())
            .ok()
            .map(|workspace| workspace.project_root);
        rimz::harness::schedule::catalog::TaskCatalog::load(project_root.as_deref())?
            .prune_orphan_overlays()
            .context("pruning orphan loop arming state")?;
        reaped
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
        store_maintenance,
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
    store_maintenance: StoreMaintenance,
    schedules_reaped: usize,
    prune: gc::WorkspacePruneReport,
    worktrees: WorktreeSweepStatus,
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

#[derive(Clone, Debug, PartialEq, Eq)]
enum StoreMaintenance {
    Done {
        archived: usize,
        reconciled: usize,
        repaired: RepairOutcome,
        carryover_pruned: usize,
    },
    SkippedDryRun,
    SkippedNoStore,
}

impl Default for StoreMaintenance {
    fn default() -> Self {
        Self::Done {
            archived: 0,
            reconciled: 0,
            repaired: RepairOutcome::default(),
            carryover_pruned: 0,
        }
    }
}

impl StoreMaintenance {
    fn status_json(&self) -> &'static str {
        match self {
            Self::Done { .. } => "done",
            Self::SkippedDryRun => "skipped_dry_run",
            Self::SkippedNoStore => "skipped_no_store",
        }
    }

    fn skip_text(&self) -> Option<&'static str> {
        match self {
            Self::Done { .. } => None,
            Self::SkippedDryRun => Some("skipped (dry run)"),
            Self::SkippedNoStore => Some("skipped — no rimz store here"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum WorktreeSweepStatus {
    Swept(WorktreeSweep),
    Skipped(WorktreeSkip),
}

impl Default for WorktreeSweepStatus {
    fn default() -> Self {
        Self::Swept(WorktreeSweep::default())
    }
}

impl WorktreeSweepStatus {
    fn bytes(&self) -> u64 {
        match self {
            Self::Swept(sweep) => sweep.bytes(),
            Self::Skipped(_) => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorktreeSkip {
    NotARepo,
    NoStore,
    RosterUnavailable,
    ListFailed,
}

impl WorktreeSkip {
    fn text(self) -> &'static str {
        match self {
            Self::NotARepo => "not inside a git repo",
            Self::NoStore => "no rimz store here",
            Self::RosterUnavailable => "agent roster unavailable",
            Self::ListFailed => "worktree listing failed",
        }
    }

    fn json(self) -> &'static str {
        match self {
            Self::NotARepo => "not_a_repo",
            Self::NoStore => "no_store",
            Self::RosterUnavailable => "roster_unavailable",
            Self::ListFailed => "list_failed",
        }
    }
}

fn sweep_worktrees(globals: &GlobalFlags, spinner: &Spinner, dry_run: bool) -> WorktreeSweepStatus {
    let Ok(workspace) = WorkspaceResolver::resolve(".", globals.root.clone()) else {
        return WorktreeSweepStatus::Skipped(WorktreeSkip::NotARepo);
    };
    if workspace.root_class != rimz::workspace::RootClass::Repo {
        return WorktreeSweepStatus::Skipped(WorktreeSkip::NotARepo);
    }
    let store = match open_store_for_worktree_gc(&workspace, dry_run) {
        Ok(Some(store)) => store,
        Ok(None) => {
            tracing::debug!("workspace store absent; worktree gc skipped");
            return WorktreeSweepStatus::Skipped(WorktreeSkip::NoStore);
        }
        Err(err) => {
            tracing::debug!(
                error = %err,
                "workspace store unavailable; worktree gc skipped"
            );
            return WorktreeSweepStatus::Skipped(WorktreeSkip::NoStore);
        }
    };
    let protection = match super::worktree_protection::for_automatic_gc(&workspace, &store, globals)
    {
        Ok(protection) => protection,
        Err(err) => {
            tracing::debug!(
                error = %err,
                "agent roster unavailable; worktree gc skipped"
            );
            return WorktreeSweepStatus::Skipped(WorktreeSkip::RosterUnavailable);
        }
    };
    spinner.set(if dry_run {
        "checking worktrees…"
    } else {
        "sweeping worktrees…"
    });
    match rimz::worktree::sweep_owned(
        &workspace.project_root,
        &protection.protections,
        &store,
        &workspace.session_name,
        dry_run,
    ) {
        Ok(sweep) => WorktreeSweepStatus::Swept(sweep),
        Err(err) => {
            tracing::debug!(error = %err, "worktree gc skipped");
            WorktreeSweepStatus::Skipped(WorktreeSkip::ListFailed)
        }
    }
}

fn open_store_for_worktree_gc(
    workspace: &rimz::ResolvedWorkspace,
    dry_run: bool,
) -> Result<Option<rimz::Store>> {
    if !dry_run {
        return open_store(workspace).map(Some);
    }
    super::open_existing_store(workspace)
}

fn render_report(out: &GcOutcome, w: &mut impl Write) -> io::Result<()> {
    let reclaimed = out.reclaimed_bytes();
    let problems = problem_count(out);
    let header = if out.dry_run {
        if reclaimed > 0 {
            format!("gc — would reclaim {} (dry run)", fmt_bytes(reclaimed))
        } else {
            "gc — nothing to reclaim (dry run)".to_owned()
        }
    } else if reclaimed > 0 {
        format!("gc — reclaimed {}", fmt_bytes(reclaimed))
    } else if problems > 0 {
        "gc — no bytes reclaimed".to_owned()
    } else {
        "gc — all clean, nothing to reclaim".to_owned()
    };
    write!(w, "{}", paint(palette::header(), &header))?;
    if problems > 0 {
        write!(
            w,
            "{}",
            paint(
                palette::warn(),
                &format!(" · {}", plural(problems, "problem", "problems"))
            )
        )?;
    }
    writeln!(w)?;

    let skipped = skipped_area_count(out);
    let checked = GC_AREAS - skipped;
    let checked_text = if skipped > 0 {
        format!(
            "checked {checked} of {GC_AREAS} areas · cutoff {}",
            fmt_duration_compact(out.older_than)
        )
    } else {
        format!(
            "checked {GC_AREAS} areas · cutoff {}",
            fmt_duration_compact(out.older_than)
        )
    };
    writeln!(w, "  {}", paint(palette::muted(), &checked_text))?;
    writeln!(w)?;

    render_worktrees(out, w)?;
    render_workspaces(out, w)?;
    render_runtime(out, w)?;
    render_temps(out, w)?;
    render_messages(out, w)?;
    render_event_log(out, w)?;
    render_agent_cache(out, w)?;
    render_loop_schedules(out, w)?;
    Ok(())
}

const GC_AREAS: usize = 8;
const GC_AREA_LABEL_WIDTH: usize = 14;

#[derive(Clone, Copy)]
enum RowVerdict {
    Healthy,
    Acted,
    Warn,
    Alarm,
    Skipped,
}

impl RowVerdict {
    fn glyph(self) -> &'static str {
        match self {
            Self::Healthy => "✓",
            Self::Acted => "✦",
            Self::Warn => "⚠",
            Self::Alarm => "✗",
            Self::Skipped => "–",
        }
    }

    fn style(self) -> anstyle::Style {
        let role = match self {
            Self::Healthy | Self::Acted => crate::cli::render::status::StateRole::Success,
            Self::Warn => crate::cli::render::status::StateRole::Waiting,
            Self::Alarm => crate::cli::render::status::StateRole::Failed,
            Self::Skipped => crate::cli::render::status::StateRole::Neutral,
        };
        crate::cli::render::status::role(role)
    }
}

fn render_row(
    w: &mut impl Write,
    verdict: RowVerdict,
    area: &str,
    outcome: &str,
) -> io::Result<()> {
    let glyph = verdict.glyph();
    let style = verdict.style();
    let padded_area = format!("{area:<width$}", width = GC_AREA_LABEL_WIDTH);
    let line = format!("  {glyph} {padded_area}  {outcome}");
    match verdict {
        RowVerdict::Warn | RowVerdict::Alarm => writeln!(w, "{}", paint(style, &line)),
        RowVerdict::Healthy | RowVerdict::Acted | RowVerdict::Skipped => {
            writeln!(w, "  {} {padded_area}  {outcome}", paint(style, glyph),)
        }
    }
}

fn render_subline(w: &mut impl Write, style: anstyle::Style, text: &str) -> io::Result<()> {
    writeln!(w, "      {}", paint(style, text))
}

fn render_worktrees(out: &GcOutcome, w: &mut impl Write) -> io::Result<()> {
    match &out.worktrees {
        WorktreeSweepStatus::Skipped(skip) => render_row(
            w,
            RowVerdict::Skipped,
            "worktrees",
            &format!("skipped — {}", skip.text()),
        ),
        WorktreeSweepStatus::Swept(sweep) => {
            let removed = sweep.removed.len();
            let kept = sweep.kept.len();
            let outcome = if removed > 0 {
                let mut outcome = if out.dry_run {
                    format!("would remove {removed} · {}", fmt_bytes(sweep.bytes()))
                } else {
                    format!(
                        "{} · {}",
                        plural(removed, "removed", "removed"),
                        fmt_bytes(sweep.bytes())
                    )
                };
                if kept > 0 {
                    outcome.push_str(&format!(" · {kept} kept"));
                }
                outcome
            } else if !sweep.failed.is_empty() {
                plural(sweep.failed.len(), "removal failed", "removals failed")
            } else if kept > 0 {
                format!("{kept} kept — {}", kept_summary(&sweep.kept))
            } else {
                "none managed here".to_owned()
            };
            let verdict = if removed > 0 {
                RowVerdict::Acted
            } else if !sweep.failed.is_empty() {
                RowVerdict::Alarm
            } else {
                RowVerdict::Healthy
            };
            render_row(w, verdict, "worktrees", &outcome)?;
            for kept in &sweep.kept {
                render_subline(
                    w,
                    palette::muted(),
                    &format!("kept: {} — {}", kept.name, kept.reason.text()),
                )?;
            }
            let action = if out.dry_run {
                "would remove"
            } else {
                "removed"
            };
            for removed in &sweep.removed {
                let mut line = format!(
                    "{action}: {}  {}  {}",
                    removed.name,
                    fmt_bytes(removed.bytes),
                    worktree_removal_detail(removed)
                );
                let style = if let Some(err) = &removed.archive_error {
                    line.push_str(&format!(" — message archive failed: {err}"));
                    palette::warn()
                } else {
                    palette::muted()
                };
                render_subline(w, style, &line)?;
            }
            for failed in &sweep.failed {
                render_subline(
                    w,
                    palette::alarm(),
                    &format!("✗ failed: {} — {}", failed.path.display(), failed.error),
                )?;
            }
            Ok(())
        }
    }
}

fn render_workspaces(out: &GcOutcome, w: &mut impl Write) -> io::Result<()> {
    let removed = out.prune.removed.len();
    let unreadable = out.prune.retained_unreadable.len();
    let outcome = if removed > 0 {
        if out.dry_run {
            format!(
                "would prune {removed} · {}",
                fmt_bytes(out.prune.bytes_removed())
            )
        } else {
            format!(
                "{} · {}",
                plural(removed, "pruned", "pruned"),
                fmt_bytes(out.prune.bytes_removed())
            )
        }
    } else if unreadable > 0 {
        format!(
            "{} kept with unreadable record — history preserved",
            unreadable
        )
    } else if out.prune.kept > 0 {
        plural(out.prune.kept, "healthy", "healthy")
    } else {
        "none found".to_owned()
    };
    let verdict = if removed > 0 {
        RowVerdict::Acted
    } else if unreadable > 0 {
        RowVerdict::Warn
    } else {
        RowVerdict::Healthy
    };
    render_row(w, verdict, "workspaces", &outcome)?;
    let action = if out.dry_run { "would prune" } else { "pruned" };
    for removed in &out.prune.removed {
        render_subline(
            w,
            palette::muted(),
            &format!("{action}: {}", removed_workspace_detail(removed)),
        )?;
    }
    if removed > 0 && unreadable > 0 {
        render_subline(
            w,
            palette::warn(),
            &format!("{unreadable} kept with unreadable record — history preserved"),
        )?;
    }
    Ok(())
}

fn render_runtime(out: &GcOutcome, w: &mut impl Write) -> io::Result<()> {
    let items = runtime_items(&out.runtime);
    if items > 0 {
        let item_count = plural(items, "stale file", "stale files");
        let outcome = if out.dry_run {
            format!(
                "would remove {item_count} · {} — {}",
                fmt_bytes(out.runtime.bytes_removed),
                plural(
                    out.runtime.runtime_roots_scanned,
                    "root scanned",
                    "roots scanned"
                )
            )
        } else {
            format!(
                "{item_count} · {} — {}",
                fmt_bytes(out.runtime.bytes_removed),
                plural(
                    out.runtime.runtime_roots_scanned,
                    "root scanned",
                    "roots scanned"
                )
            )
        };
        render_row(w, RowVerdict::Acted, "runtime", &outcome)?;
        let action = if out.dry_run {
            "would remove"
        } else {
            "removed"
        };
        render_subline(
            w,
            palette::muted(),
            &format!("{action}: {}", runtime_breakdown(&out.runtime)),
        )
    } else {
        render_row(
            w,
            RowVerdict::Healthy,
            "runtime",
            &format!(
                "{}, all fresh",
                plural(
                    out.runtime.runtime_roots_scanned,
                    "root scanned",
                    "roots scanned"
                )
            ),
        )
    }
}

fn render_temps(out: &GcOutcome, w: &mut impl Write) -> io::Result<()> {
    if out.temps.files_removed > 0 {
        let orphaned = plural(out.temps.files_removed, "orphaned", "orphaned");
        let outcome = if out.dry_run {
            format!(
                "would remove {orphaned} · {}",
                fmt_bytes(out.temps.bytes_removed)
            )
        } else {
            format!("{orphaned} · {}", fmt_bytes(out.temps.bytes_removed))
        };
        render_row(w, RowVerdict::Acted, "temp files", &outcome)
    } else {
        render_row(w, RowVerdict::Healthy, "temp files", "none orphaned")
    }
}

fn render_messages(out: &GcOutcome, w: &mut impl Write) -> io::Result<()> {
    match &out.store_maintenance {
        StoreMaintenance::Done {
            archived,
            reconciled,
            ..
        } => {
            let mut clauses = Vec::new();
            if *archived > 0 {
                clauses.push(plural(*archived, "orphaned archived", "orphaned archived"));
            }
            if *reconciled > 0 {
                clauses.push(plural(*reconciled, "stuck reset", "stuck reset"));
            }
            if clauses.is_empty() {
                render_row(w, RowVerdict::Healthy, "messages", "queue clean")
            } else {
                render_row(w, RowVerdict::Acted, "messages", &clauses.join(" · "))
            }
        }
        skipped => render_row(
            w,
            RowVerdict::Skipped,
            "messages",
            skipped.skip_text().unwrap_or("skipped"),
        ),
    }
}

fn render_event_log(out: &GcOutcome, w: &mut impl Write) -> io::Result<()> {
    match &out.store_maintenance {
        StoreMaintenance::Done { repaired, .. } if repaired.truncated() => render_row(
            w,
            RowVerdict::Warn,
            "event log",
            &format!(
                "corruption repaired — {} cut, {} frames kept",
                fmt_bytes(repaired.bytes_truncated),
                repaired.frames_kept
            ),
        ),
        StoreMaintenance::Done { .. } => render_row(w, RowVerdict::Healthy, "event log", "intact"),
        skipped => render_row(
            w,
            RowVerdict::Skipped,
            "event log",
            skipped.skip_text().unwrap_or("skipped"),
        ),
    }
}

fn render_agent_cache(out: &GcOutcome, w: &mut impl Write) -> io::Result<()> {
    match &out.store_maintenance {
        StoreMaintenance::Done {
            carryover_pruned, ..
        } if *carryover_pruned > 0 => render_row(
            w,
            RowVerdict::Acted,
            "agent cache",
            &plural(
                *carryover_pruned,
                "expired entry pruned",
                "expired entries pruned",
            ),
        ),
        StoreMaintenance::Done { .. } => render_row(w, RowVerdict::Healthy, "agent cache", "clean"),
        skipped => render_row(
            w,
            RowVerdict::Skipped,
            "agent cache",
            skipped.skip_text().unwrap_or("skipped"),
        ),
    }
}

fn render_loop_schedules(out: &GcOutcome, w: &mut impl Write) -> io::Result<()> {
    if out.dry_run {
        render_row(
            w,
            RowVerdict::Skipped,
            "loop schedules",
            "skipped (dry run)",
        )
    } else if out.schedules_reaped > 0 {
        render_row(
            w,
            RowVerdict::Acted,
            "loop schedules",
            &plural(out.schedules_reaped, "dead reaped", "dead reaped"),
        )
    } else {
        render_row(w, RowVerdict::Healthy, "loop schedules", "none dead")
    }
}

fn problem_count(out: &GcOutcome) -> usize {
    let worktree_problems = match &out.worktrees {
        WorktreeSweepStatus::Swept(sweep) => {
            sweep.failed.len()
                + sweep
                    .removed
                    .iter()
                    .filter(|removed| removed.archive_error.is_some())
                    .count()
        }
        WorktreeSweepStatus::Skipped(_) => 0,
    };
    let event_log_problems = match &out.store_maintenance {
        StoreMaintenance::Done { repaired, .. } if repaired.truncated() => 1,
        _ => 0,
    };
    worktree_problems + out.prune.retained_unreadable.len() + event_log_problems
}

fn skipped_area_count(out: &GcOutcome) -> usize {
    let worktree = usize::from(matches!(&out.worktrees, WorktreeSweepStatus::Skipped(_)));
    let store = match &out.store_maintenance {
        StoreMaintenance::Done { .. } => 0,
        StoreMaintenance::SkippedDryRun | StoreMaintenance::SkippedNoStore => 3,
    };
    let schedules = usize::from(out.dry_run);
    worktree + store + schedules
}

fn kept_summary(kept: &[KeptWorktree]) -> String {
    let mut parts = Vec::new();
    let in_use = kept
        .iter()
        .filter(|worktree| worktree.reason == KeptReason::InUse)
        .count();
    if in_use > 0 {
        parts.push(plural(in_use, "in use", "in use"));
    }
    let dirty = kept
        .iter()
        .filter(|worktree| worktree.reason == KeptReason::Dirty)
        .count();
    if dirty > 0 {
        parts.push(plural(
            dirty,
            "with uncommitted changes",
            "with uncommitted changes",
        ));
    }
    let not_merged = kept
        .iter()
        .filter(|worktree| worktree.reason == KeptReason::NotMerged)
        .count();
    if not_merged > 0 {
        parts.push(plural(not_merged, "not merged yet", "not merged yet"));
    }
    parts.join(", ")
}

fn worktree_removal_detail(removed: &SweptWorktree) -> &'static str {
    match removed.branch_deletion {
        Some(rimz::worktree::BranchDeletion::Deleted) => "merged, branch deleted",
        Some(rimz::worktree::BranchDeletion::KeptUnmerged) => {
            "merged, branch kept (not proven merged)"
        }
        None => "merged",
    }
}

fn removed_workspace_detail(removed: &gc::RemovedWorkspace) -> String {
    match removed.reason {
        gc::PruneReason::ProjectRootGone => format!(
            "{} — project folder gone: {} ({})",
            removed.workspace_id,
            removed
                .project_root
                .as_deref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "unknown".to_owned()),
            fmt_bytes(removed.bytes)
        ),
        gc::PruneReason::AbandonedScaffold => format!(
            "{} — abandoned setup, never used ({})",
            removed.workspace_id,
            fmt_bytes(removed.bytes)
        ),
    }
}

fn fmt_duration_compact(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs >= 3_600 && secs.is_multiple_of(3_600) {
        format!("{}h", secs / 3_600)
    } else if secs >= 60 && secs.is_multiple_of(60) {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

fn print_json_report(outcome: &GcOutcome) -> Result<()> {
    render::json_pretty(&JsonReport::from(outcome))
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
    store_maintenance: &'static str,
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
                archived: messages_archived(&out.store_maintenance),
                reconciled: messages_reconciled(&out.store_maintenance),
            },
            carryover_pruned: carryover_pruned(&out.store_maintenance),
            schedules_reaped: out.schedules_reaped,
            repair: repair_outcome(&out.store_maintenance).map(JsonRepair::from),
            store_maintenance: out.store_maintenance.status_json(),
        }
    }
}

#[derive(Serialize)]
struct JsonWorktrees {
    removed: Vec<JsonSweptWorktree>,
    failed: Vec<JsonFailedWorktree>,
    kept: Vec<JsonKeptWorktree>,
    skipped: Option<&'static str>,
}

impl From<&WorktreeSweepStatus> for JsonWorktrees {
    fn from(status: &WorktreeSweepStatus) -> Self {
        match status {
            WorktreeSweepStatus::Swept(sweep) => Self {
                removed: sweep.removed.iter().map(JsonSweptWorktree::from).collect(),
                failed: sweep.failed.iter().map(JsonFailedWorktree::from).collect(),
                kept: sweep.kept.iter().map(JsonKeptWorktree::from).collect(),
                skipped: None,
            },
            WorktreeSweepStatus::Skipped(skip) => Self {
                removed: Vec::new(),
                failed: Vec::new(),
                kept: Vec::new(),
                skipped: Some(skip.json()),
            },
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
struct JsonKeptWorktree {
    name: String,
    path: String,
    reason: &'static str,
}

impl From<&KeptWorktree> for JsonKeptWorktree {
    fn from(worktree: &KeptWorktree) -> Self {
        Self {
            name: worktree.name.clone(),
            path: path_string(&worktree.path),
            reason: worktree.reason.json(),
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

fn messages_archived(maintenance: &StoreMaintenance) -> usize {
    match maintenance {
        StoreMaintenance::Done { archived, .. } => *archived,
        StoreMaintenance::SkippedDryRun | StoreMaintenance::SkippedNoStore => 0,
    }
}

fn messages_reconciled(maintenance: &StoreMaintenance) -> usize {
    match maintenance {
        StoreMaintenance::Done { reconciled, .. } => *reconciled,
        StoreMaintenance::SkippedDryRun | StoreMaintenance::SkippedNoStore => 0,
    }
}

fn carryover_pruned(maintenance: &StoreMaintenance) -> usize {
    match maintenance {
        StoreMaintenance::Done {
            carryover_pruned, ..
        } => *carryover_pruned,
        StoreMaintenance::SkippedDryRun | StoreMaintenance::SkippedNoStore => 0,
    }
}

fn repair_outcome(maintenance: &StoreMaintenance) -> Option<RepairOutcome> {
    match maintenance {
        StoreMaintenance::Done { repaired, .. } => Some(*repaired),
        StoreMaintenance::SkippedDryRun | StoreMaintenance::SkippedNoStore => None,
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
    parse_duration_units(
        raw,
        &[
            DurationUnit::Second,
            DurationUnit::Minute,
            DurationUnit::Hour,
        ],
    )
    .map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_report_names_all_clean_checks() {
        let out = strip_report(&clean_outcome());

        assert!(out.contains("gc — all clean, nothing to reclaim"));
        assert!(out.contains("checked 8 areas · cutoff 1h"));
        assert!(out.contains("✓ worktrees"));
        assert!(out.contains("3 kept — 2 in use, 1 not merged yet"));
        assert!(out.contains("✓ workspaces"));
        assert!(out.contains("4 healthy"));
        assert!(out.contains("✓ runtime"));
        assert!(out.contains("6 roots scanned, all fresh"));
        assert!(out.contains("✓ temp files"));
        assert!(out.contains("✓ messages"));
        assert!(out.contains("✓ event log"));
        assert!(out.contains("✓ agent cache"));
        assert!(out.contains("✓ loop schedules"));
    }

    #[test]
    fn render_report_names_every_kept_worktree_and_reason() {
        let out = strip_report(&GcOutcome {
            older_than: Duration::from_secs(3600),
            worktrees: WorktreeSweepStatus::Swept(WorktreeSweep {
                kept: vec![
                    KeptWorktree {
                        name: "active".to_owned(),
                        path: PathBuf::from("/repo-worktrees/active"),
                        reason: KeptReason::InUse,
                    },
                    KeptWorktree {
                        name: "dirty".to_owned(),
                        path: PathBuf::from("/repo-worktrees/dirty"),
                        reason: KeptReason::Dirty,
                    },
                    KeptWorktree {
                        name: "pending".to_owned(),
                        path: PathBuf::from("/repo-worktrees/pending"),
                        reason: KeptReason::NotMerged,
                    },
                ],
                ..WorktreeSweep::default()
            }),
            ..GcOutcome::default()
        });

        assert!(out.contains("3 kept — 1 in use, 1 with uncommitted changes, 1 not merged yet"));
        assert!(out.contains("kept: active — in use"));
        assert!(out.contains("kept: dirty — uncommitted changes"));
        assert!(out.contains("kept: pending — not merged yet"));
    }

    #[test]
    fn render_report_lists_active_checklist_details() {
        let out = strip_report(&full_outcome(false));

        assert!(out.contains("gc — reclaimed"));
        assert!(out.contains("· 3 problems"));
        assert!(out.contains("✦ worktrees"));
        assert!(out.contains("2 removed · 1.4 GB · 3 kept"));
        assert!(out.contains("kept: active — in use"));
        assert!(out.contains("kept: shell — in use"));
        assert!(out.contains("kept: pending — not merged yet"));
        assert!(out.contains("removed: demo"));
        assert!(out.contains("merged, branch deleted"));
        assert!(out.contains("removed: gc-info"));
        assert!(out.contains("merged, branch kept (not proven merged)"));
        assert!(out.contains("message archive failed: archive boom"));
        assert!(out.contains("✗ failed: /repo-worktrees/wip — remove boom"));
        assert!(
            out.contains("pruned: ws_0123456789abcdef01234567 — project folder gone: /gone (2 KB)")
        );
        assert!(out.contains("⚠ event log"));
        assert!(out.contains("corruption repaired — 9 B cut, 3 frames kept"));
    }

    #[test]
    fn render_report_uses_dry_run_framing() {
        let out = strip_report(&full_outcome(true));

        assert!(out.contains("gc — would reclaim"));
        assert!(out.contains("(dry run)"));
        assert!(out.contains("checked 4 of 8 areas · cutoff 1h"));
        assert!(out.contains("would remove 2 · 1.4 GB · 3 kept"));
        assert!(out.contains("kept: active — in use"));
        assert!(out.contains("kept: shell — in use"));
        assert!(out.contains("kept: pending — not merged yet"));
        assert!(out.contains("would prune 1 · 2 KB"));
        assert!(out.contains("would remove 5 stale files"));
        assert!(out.contains("would remove 2 orphaned"));
        assert!(out.contains("would remove: demo"));
        assert!(out.contains("would prune: ws_0123456789abcdef01234567"));
        assert!(out.contains("– messages        skipped (dry run)"));
        assert!(out.contains("– event log       skipped (dry run)"));
        assert!(out.contains("– agent cache     skipped (dry run)"));
        assert!(out.contains("– loop schedules  skipped (dry run)"));
        assert!(!out.contains("branch kept (not proven merged)"));
        assert!(!out.contains("message archive failed"));
    }

    #[test]
    fn render_report_names_skipped_worktree_area() {
        let out = strip_report(&GcOutcome {
            older_than: Duration::from_secs(3600),
            worktrees: WorktreeSweepStatus::Skipped(WorktreeSkip::NotARepo),
            ..GcOutcome::default()
        });

        assert!(out.contains("checked 7 of 8 areas · cutoff 1h"));
        assert!(out.contains("– worktrees       skipped — not inside a git repo"));
    }

    #[test]
    fn render_report_counts_failures_as_problems() {
        let out = strip_report(&GcOutcome {
            older_than: Duration::from_secs(3600),
            worktrees: WorktreeSweepStatus::Swept(WorktreeSweep {
                failed: vec![FailedWorktree {
                    path: PathBuf::from("/bad"),
                    error: "boom".to_owned(),
                }],
                ..WorktreeSweep::default()
            }),
            ..GcOutcome::default()
        });

        assert!(out.contains("· 1 problem"));
        assert!(out.contains("✗ worktrees       1 removal failed"));
        assert!(out.contains("✗ failed: /bad — boom"));
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
        assert_eq!(value["worktrees"]["kept"][0]["reason"], "in_use");
        assert_eq!(value["worktrees"]["kept"][2]["reason"], "not_merged");
        assert!(value["worktrees"]["skipped"].is_null());
        assert_eq!(
            value["workspaces"]["removed"][0]["reason"],
            "project_root_gone"
        );
        assert_eq!(value["runtime"]["roots_scanned"], 2);
        assert_eq!(value["messages"]["archived"], 1);
        assert_eq!(value["messages"]["reconciled"], 1);
        assert_eq!(value["repair"]["bytes_truncated"], 9);
        assert_eq!(value["store_maintenance"], "done");

        let dry_run = serde_json::to_value(JsonReport::from(&full_outcome(true))).unwrap();
        assert!(dry_run["repair"].is_null());
        assert!(dry_run["worktrees"]["removed"][0]["branch_deleted"].is_null());
        assert_eq!(dry_run["store_maintenance"], "skipped_dry_run");

        let no_store = serde_json::to_value(JsonReport::from(&GcOutcome {
            store_maintenance: StoreMaintenance::SkippedNoStore,
            worktrees: WorktreeSweepStatus::Skipped(WorktreeSkip::NoStore),
            ..GcOutcome::default()
        }))
        .unwrap();
        assert_eq!(no_store["store_maintenance"], "skipped_no_store");
        assert_eq!(no_store["worktrees"]["skipped"], "no_store");
    }

    fn clean_outcome() -> GcOutcome {
        GcOutcome {
            older_than: Duration::from_secs(3600),
            runtime: gc::GcReport {
                runtime_roots_scanned: 6,
                ..gc::GcReport::default()
            },
            prune: gc::WorkspacePruneReport {
                kept: 4,
                ..gc::WorkspacePruneReport::default()
            },
            worktrees: WorktreeSweepStatus::Swept(WorktreeSweep {
                kept: kept_worktrees(),
                ..WorktreeSweep::default()
            }),
            ..GcOutcome::default()
        }
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
            store_maintenance: if dry_run {
                StoreMaintenance::SkippedDryRun
            } else {
                StoreMaintenance::Done {
                    archived: 1,
                    reconciled: 1,
                    repaired: RepairOutcome {
                        bytes_truncated: 9,
                        frames_kept: 3,
                    },
                    carryover_pruned: 1,
                }
            },
            schedules_reaped: usize::from(!dry_run),
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
            worktrees: WorktreeSweepStatus::Swept(WorktreeSweep {
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
                kept: kept_worktrees(),
                failed: if dry_run {
                    Vec::new()
                } else {
                    vec![FailedWorktree {
                        path: PathBuf::from("/repo-worktrees/wip"),
                        error: "remove boom".to_owned(),
                    }]
                },
            }),
        }
    }

    fn kept_worktrees() -> Vec<KeptWorktree> {
        vec![
            KeptWorktree {
                name: "active".to_owned(),
                path: PathBuf::from("/repo-worktrees/active"),
                reason: KeptReason::InUse,
            },
            KeptWorktree {
                name: "shell".to_owned(),
                path: PathBuf::from("/repo-worktrees/shell"),
                reason: KeptReason::InUse,
            },
            KeptWorktree {
                name: "pending".to_owned(),
                path: PathBuf::from("/repo-worktrees/pending"),
                reason: KeptReason::NotMerged,
            },
        ]
    }

    fn strip_report(outcome: &GcOutcome) -> String {
        let mut stream = anstream::StripStream::new(Vec::new());
        render_report(outcome, &mut stream).expect("render report");
        String::from_utf8(stream.into_inner()).expect("utf-8")
    }
}
