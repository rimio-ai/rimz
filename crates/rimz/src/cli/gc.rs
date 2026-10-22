//! `rimz gc` — reclaim stale maintenance state through domain assessments.

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Args;
use serde::Serialize;

use super::render::{self, fmt_bytes, paint, palette};
use super::spinner::Spinner;
use super::{GlobalFlags, open_existing_store};
use rimz::config::MachineConfig;
use rimz::harness::assist_log::{self, Assist, AssistRecord};
use rimz::harness::auto_gc::{self, GcScope};
use rimz::store::event_log::RepairOutcome;
use rimz::store::gc;
use rimz::utils::time::format_duration_compact;
use rimz::workspace::WorkspaceResolver;
use rimz::worktree::{FailedWorktree, KeepReason, KeptWorktree, SweptWorktree, WorktreeSweep};

#[derive(Debug, Args)]
pub struct GcArgs {
    /// Remove runtime artifacts older than this duration (`8h`, `3d`);
    /// defaults to the `gc.older_than` setting (7d).
    #[arg(long, value_parser = rimz::config::parse_older_than)]
    older_than: Option<Duration>,
    /// Report what gc would remove without removing anything.
    #[arg(long)]
    dry_run: bool,
    /// Emit the garbage collection report as JSON.
    #[arg(long)]
    json: bool,
    /// Sweep every room and shared machine state.
    #[arg(long, conflicts_with = "unattended")]
    all: bool,
    /// Run as the room's automatic daily sweep: honour `gc.auto`, then record
    /// the assist and the workspace sweep stamp.
    #[arg(long, hide = true, conflicts_with = "dry_run")]
    unattended: bool,
    /// Reclaim ended temp units until the machine free-space floor is met.
    #[arg(long, hide = true, conflicts_with_all = ["all", "dry_run", "unattended", "older_than"])]
    tmp_floor: bool,
}

pub fn run(args: GcArgs, globals: &GlobalFlags) -> Result<()> {
    let gc_config = if args.unattended || args.tmp_floor {
        // Never default an unreadable core config: it may contain a gc opt-out.
        // Loop-source errors instead skip the worktree area below.
        let config = match MachineConfig::load() {
            Ok(config) => config,
            Err(_) if args.unattended && MachineConfig::load_loop().is_err() => {
                let config = MachineConfig::load_lenient();
                if config.require_readable_core("automatic gc").is_err() {
                    return Ok(());
                }
                std::sync::Arc::unwrap_or_clone(config)
            }
            Err(_) => return Ok(()),
        };
        if !config.gc.auto {
            return Ok(());
        }
        config.gc
    } else {
        MachineConfig::load_lenient().gc.clone()
    };
    if args.tmp_floor {
        if let Some(report) = rimz::harness::tmp_floor::run(&gc_config)? {
            if args.json {
                serde_json::to_writer_pretty(render::out(), &report)?;
            } else {
                let mut out = render::out();
                for record in &report.evictions {
                    if let Assist::TmpEvict {
                        workspace_id,
                        handle,
                        reclaimed_bytes,
                        error,
                        ..
                    } = &record.assist
                    {
                        writeln!(
                            out,
                            "{handle}: {} ({workspace_id}){}",
                            fmt_bytes(*reclaimed_bytes),
                            error
                                .as_ref()
                                .map(|error| format!(" failed: {error}"))
                                .unwrap_or_default()
                        )?;
                    }
                }
                writeln!(
                    out,
                    "free {} of {}, floor {}",
                    fmt_bytes(report.free_bytes),
                    fmt_bytes(report.total_bytes),
                    fmt_bytes(report.floor_bytes)
                )?;
            }
        }
        return Ok(());
    }
    let policy = gc::GcPolicy {
        older_than: args.older_than.unwrap_or(gc_config.older_than),
        tmp_grace: gc_config.tmp_grace,
        transcript_retention: gc_config.transcript_retention,
        team_notes_retention: gc_config.team_notes_retention,
    };
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())
        .context("resolving the room for gc; run inside a room or pass --root")?;
    let paths = rimz::StatePaths::for_project_root(&workspace.project_root)
        .context("preparing store paths")?;
    let machine_claim = args
        .unattended
        .then(|| auto_gc::claim_machine_sweep(jiff::Timestamp::now()))
        .flatten();
    let scope = if args.all || machine_claim.is_some() {
        GcScope::Machine
    } else {
        GcScope::Room
    };
    let result = sweep(policy, args.dry_run, scope, &workspace, &paths, globals);
    if args.unattended {
        // Stamp failed attempts too, so persistent failures retry daily rather than per room.
        let now = jiff::Timestamp::now();
        assist_log::append(&AssistRecord {
            at: now,
            assist: auto_gc_assist(
                &workspace.workspace_id,
                paths.dir_name.as_str(),
                scope,
                policy.older_than,
                &result,
            ),
        });
        if machine_claim.is_some() {
            auto_gc::write_machine_stamp(now)?;
        }
        if paths.root.is_dir() {
            auto_gc::write_stamp(&paths, now)?;
        }
    }
    let outcome = result?;
    if args.json {
        print_json_report(&outcome)?;
    } else {
        let mut out = render::out();
        render_report(&outcome, &mut out)?;
    }
    Ok(())
}

fn auto_gc_assist(
    workspace_id: &rimz::WorkspaceId,
    room_name: &str,
    scope: GcScope,
    older_than: Duration,
    result: &Result<GcOutcome>,
) -> Assist {
    let (outcome, error) = match result {
        Ok(outcome) => (outcome.clone(), None),
        Err(err) => (GcOutcome::default(), Some(format!("{err:#}"))),
    };
    let (worktrees_removed, messages_archived) = (
        match &outcome.worktrees {
            WorktreeSweepStatus::Swept(sweep) => sweep.removed.len(),
            WorktreeSweepStatus::Skipped(_) => 0,
        },
        match &outcome.store_maintenance {
            StoreMaintenance::Done { archived, .. } => *archived,
            StoreMaintenance::SkippedDryRun | StoreMaintenance::SkippedNoStore => 0,
        },
    );
    let mut class_bytes = std::collections::BTreeMap::new();
    for room in outcome
        .runtime
        .rooms
        .iter()
        .filter(|room| room.name == room_name)
    {
        for class in &room.classes {
            *class_bytes.entry(class.class.clone()).or_default() += class.bytes_removed;
        }
    }
    Assist::AutoGc {
        workspace_id: workspace_id.clone(),
        scope,
        older_than_secs: older_than.as_secs(),
        reclaimed_bytes: outcome.reclaimed_bytes(),
        class_bytes,
        worktrees_removed,
        worktree_paths: match &outcome.worktrees {
            WorktreeSweepStatus::Swept(sweep) => sweep
                .removed
                .iter()
                .take(50)
                .map(|tree| tree.path.clone())
                .collect(),
            WorktreeSweepStatus::Skipped(_) => Vec::new(),
        },
        workspaces_pruned: outcome.prune.removed.len(),
        files_removed: outcome.runtime.heartbeat_files_removed
            + outcome.runtime.sidecar_files_removed
            + outcome.runtime.sidebar_sockets_removed
            + outcome.runtime.probe_markers_removed
            + outcome
                .prune
                .empty_dirs
                .iter()
                .map(|tree| tree.files_removed)
                .sum::<usize>()
            + outcome.temps.files_removed
            + outcome.runtime.state_files_removed
            + outcome.budget_ledgers.ledgers,
        messages_archived,
        problems: problem_count(&outcome),
        error,
    }
}

fn sweep(
    policy: gc::GcPolicy,
    dry_run: bool,
    scope: GcScope,
    workspace: &rimz::ResolvedWorkspace,
    paths: &rimz::StatePaths,
    globals: &GlobalFlags,
) -> Result<GcOutcome> {
    let older_than = policy.older_than;
    let spinner = Spinner::new("starting gc…");
    let prune = if scope == GcScope::Machine {
        spinner.set("pruning dead workspaces…");
        gc::prune_dead_workspaces(dry_run).context("pruning dead workspaces")?
    } else {
        gc::WorkspacePruneReport::default()
    };
    spinner.set("sweeping room state…");
    let watcher_is_live = |runtime: &rimz::RuntimePaths, name: &str| {
        rimz::harness::schedule::signal::watcher_info(runtime, name)
            .map(|watcher| watcher.is_some())
    };
    let report = match scope {
        GcScope::Room => gc::collect_room(paths, policy, dry_run, watcher_is_live, |agents| {
            let owners = rimz::harness::launch_plan::AgentTempOwners::new(agents.iter());
            move |agent: &rimz::agents::AgentState| owners.owner(agent).map(str::to_owned)
        }),
        GcScope::Machine => {
            let pruned = prune
                .removed
                .iter()
                .map(|removed| removed.dir_name.clone())
                .collect();
            gc::collect_classes(policy, dry_run, &pruned, watcher_is_live, |agents| {
                let owners = rimz::harness::launch_plan::AgentTempOwners::new(agents.iter());
                move |agent: &rimz::agents::AgentState| owners.owner(agent).map(str::to_owned)
            })
        }
    }
    .context("collecting room garbage")?;
    let store_maintenance = if dry_run {
        StoreMaintenance::SkippedDryRun
    } else {
        spinner.set("repairing store…");
        match open_existing_store(workspace) {
            Ok(Some(store)) => {
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
                    .reconcile_stale_messages(
                        &workspace.session_name,
                        jiff::Timestamp::now(),
                        rimz::message::max_delivery_attempts_from_env(),
                    )
                    .context("reconciling stale messages")?;
                if reconcile.held_on_ingress > 0
                    && let Err(error) = rimz::harness::hook_drain::nudge(&store, 0)
                {
                    tracing::warn!(%error, "could not wake hook drainer for held messages");
                }
                spinner.set("pruning store caches...");
                let carryover_pruned = store
                    .prune_carryover(rimz::store::event_log::DEFAULT_RETENTION)
                    .context("pruning carryover agents")?;
                StoreMaintenance::Done {
                    archived: messages_archived,
                    reconciled: reconcile.requeued
                        + reconcile.timed_out
                        + reconcile.abandoned
                        + reconcile.expired,
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
            Ok(None) => StoreMaintenance::SkippedNoStore,
        }
    };
    spinner.set("reaping dead schedules…");
    let (schedules_reaped, wait_logs_pruned) = if dry_run {
        (0, report.wait_outputs_removed)
    } else {
        let reaped = match scope {
            GcScope::Room => {
                rimz::harness::schedule::catalog::TaskCatalog::reap_dead_deliveries_for(
                    &workspace.project_root,
                )
            }
            GcScope::Machine => {
                rimz::harness::schedule::catalog::TaskCatalog::reap_dead_deliveries()
            }
        }
        .context("reaping dead loop schedules")?;
        match rimz::harness::schedule::catalog::TaskCatalog::load(Some(&workspace.project_root)) {
            Ok(catalog) => {
                catalog
                    .prune_orphan_overlays()
                    .context("pruning orphan loop arming state")?;
            }
            Err(error) => {
                if !rimz::harness::schedule::catalog::TaskCatalog::source_unavailable(&error) {
                    return Err(error);
                }
                tracing::debug!(%error, "loop tasks unavailable; orphan arming state kept");
            }
        }
        (reaped, report.wait_outputs_removed)
    };
    spinner.set("sweeping orphan temps…");
    let temps = match scope {
        GcScope::Room => gc::collect_room_orphan_temps(paths, older_than, dry_run)?,
        GcScope::Machine => gc::collect_orphan_temps(older_than, dry_run),
    };
    spinner.set("sweeping orphan account budget ledgers…");
    let budget_ledgers = rimz::harness::budget::sweep_orphan_account_ledgers(
        &rimz::RuntimePaths::shared(),
        &MachineConfig::load_lenient(),
        dry_run,
    );
    let worktrees = sweep_worktrees(workspace, globals, &spinner, dry_run);
    Ok(GcOutcome {
        scope,
        room_name: paths.dir_name.to_string(),
        project_root: workspace.project_root.clone(),
        dry_run,
        older_than,
        runtime: report,
        temps,
        store_maintenance,
        schedules_reaped,
        wait_logs_pruned,
        budget_ledgers,
        prune,
        worktrees,
    })
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct GcOutcome {
    scope: GcScope,
    room_name: String,
    project_root: PathBuf,
    dry_run: bool,
    older_than: Duration,
    runtime: gc::GcReport,
    temps: gc::TempSweepReport,
    store_maintenance: StoreMaintenance,
    schedules_reaped: usize,
    wait_logs_pruned: usize,
    budget_ledgers: rimz::harness::budget::AccountLedgerSweep,
    prune: gc::WorkspacePruneReport,
    worktrees: WorktreeSweepStatus,
}

impl GcOutcome {
    fn reclaimed_bytes(&self) -> u64 {
        self.runtime
            .bytes_removed
            .saturating_add(self.runtime.state_bytes_removed)
            .saturating_add(self.temps.bytes_removed)
            .saturating_add(self.budget_ledgers.bytes)
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

    fn skip_text(&self, root: &std::path::Path) -> Option<String> {
        match self {
            Self::Done { .. } => None,
            Self::SkippedDryRun => Some("skipped (dry run)".to_owned()),
            Self::SkippedNoStore => Some(format!(
                "skipped — {}",
                super::no_room(format_args!("at {}", root.display()))
            )),
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

#[derive(Clone, Debug, PartialEq, Eq)]
enum WorktreeSkip {
    NotARepo,
    NoStore,
    RosterUnavailable,
    LoopTasksUnavailable(String),
    ListFailed,
}

impl WorktreeSkip {
    fn text(&self, root: &std::path::Path) -> String {
        match self {
            Self::NotARepo => "not inside a git repo".to_owned(),
            Self::NoStore => super::no_room(format_args!("at {}", root.display())),
            Self::RosterUnavailable => "agent roster unavailable".to_owned(),
            Self::LoopTasksUnavailable(error) => format!("loop tasks unavailable: {error}"),
            Self::ListFailed => "worktree listing failed".to_owned(),
        }
    }

    fn json(&self) -> &'static str {
        match self {
            Self::NotARepo => "not_a_repo",
            Self::NoStore => "no_store",
            Self::RosterUnavailable => "roster_unavailable",
            Self::LoopTasksUnavailable(_) => "loop_tasks_unavailable",
            Self::ListFailed => "list_failed",
        }
    }
}

fn sweep_worktrees(
    workspace: &rimz::ResolvedWorkspace,
    globals: &GlobalFlags,
    spinner: &Spinner,
    dry_run: bool,
) -> WorktreeSweepStatus {
    if workspace.root_class != rimz::workspace::RootClass::Repo {
        return WorktreeSweepStatus::Skipped(WorktreeSkip::NotARepo);
    }
    let store = match open_existing_store(workspace) {
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
    let protection = match super::worktree_protection::for_automatic_gc(workspace, &store, globals)
    {
        Ok(protection) => protection,
        Err(err) => {
            tracing::debug!(
                error = %err,
                "protection facts unavailable; worktree gc skipped"
            );
            return WorktreeSweepStatus::Skipped(match err {
                super::worktree_protection::ProtectionErr::Roster(_) => {
                    WorktreeSkip::RosterUnavailable
                }
                super::worktree_protection::ProtectionErr::LoopTasks(error) => {
                    WorktreeSkip::LoopTasksUnavailable(format!("{error:#}"))
                }
                super::worktree_protection::ProtectionErr::WorktreeListing(_) => {
                    WorktreeSkip::ListFailed
                }
            });
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
        &super::machine_config().agents.worktree.hooks,
    ) {
        Ok(sweep) => WorktreeSweepStatus::Swept(sweep),
        Err(err) => {
            tracing::debug!(error = %err, "worktree gc skipped");
            WorktreeSweepStatus::Skipped(WorktreeSkip::ListFailed)
        }
    }
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
    } else if out.schedules_reaped > 0 || out.wait_logs_pruned > 0 {
        "gc — maintenance complete".to_owned()
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
    let mut checked_text = if skipped > 0 {
        format!(
            "checked {checked} of {GC_AREAS} areas · cutoff {}",
            format_duration_compact(out.older_than)
        )
    } else {
        format!(
            "checked {GC_AREAS} areas · cutoff {}",
            format_duration_compact(out.older_than)
        )
    };
    match out.scope {
        GcScope::Room => checked_text.push_str(&format!(" · room {}", out.room_name)),
        GcScope::Machine => checked_text.push_str(" · machine"),
    }
    writeln!(w, "  {}", paint(palette::muted(), &checked_text))?;
    writeln!(w)?;

    for room in &out.runtime.rooms {
        writeln!(w, "  {}", paint(palette::header(), &room.name))?;
        for class in &room.classes {
            if class.locks_would_check > 0 {
                writeln!(
                    w,
                    "    {} · {} would check",
                    class.class,
                    plural(class.locks_would_check, "file", "files")
                )?;
                continue;
            }
            write!(
                w,
                "    {} · {} · {}",
                class.class,
                plural(class.files_removed, "file", "files"),
                fmt_bytes(class.bytes_removed)
            )?;
            if let Some(kept) = class.bytes_retained {
                write!(w, " · {} kept", fmt_bytes(kept))?;
            }
            writeln!(w)?;
        }
    }

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
            &format!("skipped — {}", skip.text(&out.project_root)),
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
                let reasons = kept
                    .reasons
                    .iter()
                    .map(|reason| reason.text())
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut line = format!("kept: {} — {reasons}", kept.name);
                if !kept.holders.is_empty() {
                    line.push_str(&format!(" ({})", kept.holders));
                }
                render_subline(w, palette::muted(), &line)?;
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
    if out.scope == GcScope::Room {
        return render_row(
            w,
            RowVerdict::Skipped,
            "workspaces",
            "skipped — machine area, run with --all",
        );
    }
    let removed = out.prune.removed.len();
    let empty_removed = out
        .prune
        .empty_dirs
        .iter()
        .any(|tree| tree.files_removed > 0 || tree.bytes > 0);
    let acted = removed > 0 || empty_removed;
    let failed = out.prune.failed.len();
    let unreadable = out.prune.retained_unreadable.len();
    let outcome = if empty_removed && removed == 0 {
        format!(
            "{} empty-dir state · {}",
            if out.dry_run { "would prune" } else { "pruned" },
            fmt_bytes(out.prune.bytes_removed())
        )
    } else if removed > 0 {
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
    } else if failed > 0 {
        plural(failed, "removal failed", "removals failed")
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
    let verdict = if acted {
        RowVerdict::Acted
    } else if failed > 0 {
        RowVerdict::Alarm
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
    for failed in &out.prune.failed {
        render_subline(
            w,
            palette::alarm(),
            &format!("✗ failed: {} — {}", failed.dir_name, failed.error),
        )?;
    }
    for tree in &out.prune.empty_dirs {
        if tree.files_removed > 0 || tree.bytes > 0 {
            render_subline(
                w,
                palette::muted(),
                &format!(
                    "{action}: empty-dir tree {} — {} ({})",
                    tree.dir_name,
                    plural(tree.files_removed, "file", "files"),
                    fmt_bytes(tree.bytes)
                ),
            )?;
        }
        if tree.agent_dirs_kept > 0 {
            render_subline(
                w,
                palette::muted(),
                &format!(
                    "kept: empty-dir tree {} — {} within the 7-day grace",
                    tree.dir_name,
                    plural(tree.agent_dirs_kept, "agent dir", "agent dirs")
                ),
            )?;
        }
    }
    if acted && unreadable > 0 {
        render_subline(
            w,
            palette::warn(),
            &format!("{unreadable} kept with unreadable record — history preserved"),
        )?;
    }
    Ok(())
}

fn render_runtime(out: &GcOutcome, w: &mut impl Write) -> io::Result<()> {
    let items = runtime_items(out);
    let bytes = out
        .runtime
        .bytes_removed
        .saturating_add(out.budget_ledgers.bytes);
    if items > 0 {
        let item_count = plural(items, "stale file", "stale files");
        let outcome = if out.dry_run {
            format!(
                "would remove {item_count} · {} — {}",
                fmt_bytes(bytes),
                plural(
                    out.runtime.runtime_roots_scanned,
                    "root scanned",
                    "roots scanned"
                )
            )
        } else {
            format!(
                "{item_count} · {} — {}",
                fmt_bytes(bytes),
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
            &format!("{action}: {}", runtime_breakdown(out)),
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
            skipped
                .skip_text(&out.project_root)
                .as_deref()
                .unwrap_or("skipped"),
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
            skipped
                .skip_text(&out.project_root)
                .as_deref()
                .unwrap_or("skipped"),
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
            skipped
                .skip_text(&out.project_root)
                .as_deref()
                .unwrap_or("skipped"),
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
    } else if out.schedules_reaped > 0 || out.wait_logs_pruned > 0 {
        let mut counts = Vec::new();
        if out.schedules_reaped > 0 {
            counts.push(plural(out.schedules_reaped, "dead reaped", "dead reaped"));
        }
        if out.wait_logs_pruned > 0 {
            counts.push(plural(
                out.wait_logs_pruned,
                "wait log pruned",
                "wait logs pruned",
            ));
        }
        render_row(w, RowVerdict::Acted, "loop schedules", &counts.join(" · "))
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
    worktree_problems
        + out.prune.failed.len()
        + out.prune.retained_unreadable.len()
        + event_log_problems
}

fn skipped_area_count(out: &GcOutcome) -> usize {
    let worktree = usize::from(matches!(&out.worktrees, WorktreeSweepStatus::Skipped(_)));
    let store = match &out.store_maintenance {
        StoreMaintenance::Done { .. } => 0,
        StoreMaintenance::SkippedDryRun | StoreMaintenance::SkippedNoStore => 3,
    };
    let schedules = usize::from(out.dry_run);
    worktree + store + schedules + usize::from(out.scope == GcScope::Room)
}

fn kept_summary(kept: &[KeptWorktree]) -> String {
    let mut parts = Vec::new();
    for (reason, label) in [
        (KeepReason::InUse, "in use"),
        (KeepReason::Dirty, "with uncommitted changes"),
        (KeepReason::Unlanded, "with unlanded work"),
        (KeepReason::Unknown, "unknown"),
    ] {
        let count = kept
            .iter()
            .filter(|tree| tree.reasons.first() == Some(&reason))
            .count();
        if count > 0 {
            parts.push(format!("{count} {label}"));
        }
    }
    parts.join(", ")
}

fn worktree_removal_detail(removed: &SweptWorktree) -> &'static str {
    match removed.branch_deletion {
        Some(rimz::worktree::BranchDeletion::Deleted) => "landed, branch deleted",
        Some(rimz::worktree::BranchDeletion::KeptUnmerged) => {
            "landed, branch kept (not proven landed)"
        }
        None => "landed",
    }
}

fn removed_workspace_detail(removed: &gc::RemovedWorkspace) -> String {
    let name = removed
        .workspace_id
        .as_ref()
        .map_or(removed.dir_name.as_str(), rimz::WorkspaceId::as_str);
    match removed.reason {
        gc::PruneReason::IncompatibleLayout => format!(
            "{} — written by an older RimZ (layout 1), unusable ({})",
            name,
            fmt_bytes(removed.bytes)
        ),
        gc::PruneReason::ProjectRootGone => format!(
            "{} — project folder gone: {} ({})",
            name,
            removed
                .project_root
                .as_deref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "unknown".to_owned()),
            fmt_bytes(removed.bytes)
        ),
        gc::PruneReason::AbandonedScaffold => format!(
            "{} — abandoned setup, never used ({})",
            name,
            fmt_bytes(removed.bytes)
        ),
    }
}

fn print_json_report(outcome: &GcOutcome) -> Result<()> {
    render::json_pretty(&JsonReport::from(outcome))
}

#[derive(Serialize)]
struct JsonReport {
    scope: GcScope,
    dry_run: bool,
    older_than_secs: u64,
    reclaimed_bytes: u64,
    worktrees: JsonWorktrees,
    workspaces: JsonWorkspaces,
    temps: JsonTemps,
    runtime: JsonRuntime,
    rooms: Vec<gc::RoomReport>,
    messages: JsonMessages,
    carryover_pruned: usize,
    schedules_reaped: usize,
    wait_logs_pruned: usize,
    budget_ledgers_removed: usize,
    repair: Option<JsonRepair>,
    store_maintenance: &'static str,
}

impl From<&GcOutcome> for JsonReport {
    fn from(out: &GcOutcome) -> Self {
        Self {
            scope: out.scope,
            dry_run: out.dry_run,
            older_than_secs: out.older_than.as_secs(),
            reclaimed_bytes: out.reclaimed_bytes(),
            worktrees: JsonWorktrees::from(&out.worktrees),
            workspaces: JsonWorkspaces {
                skipped: (out.scope == GcScope::Room).then_some("room_scope"),
                ..JsonWorkspaces::from(&out.prune)
            },
            temps: JsonTemps::from(&out.temps),
            runtime: JsonRuntime::from(&out.runtime),
            rooms: out.runtime.rooms.clone(),
            messages: JsonMessages {
                archived: messages_archived(&out.store_maintenance),
                reconciled: messages_reconciled(&out.store_maintenance),
            },
            carryover_pruned: carryover_pruned(&out.store_maintenance),
            schedules_reaped: out.schedules_reaped,
            wait_logs_pruned: out.wait_logs_pruned,
            budget_ledgers_removed: out.budget_ledgers.ledgers,
            repair: repair_outcome(&out.store_maintenance).map(JsonRepair::from),
            store_maintenance: out.store_maintenance.status_json(),
        }
    }
}

#[derive(Serialize)]
struct JsonWorktrees {
    removed: Vec<SweptWorktree>,
    failed: Vec<FailedWorktree>,
    kept: Vec<KeptWorktree>,
    skipped: Option<&'static str>,
}

impl From<&WorktreeSweepStatus> for JsonWorktrees {
    fn from(status: &WorktreeSweepStatus) -> Self {
        match status {
            WorktreeSweepStatus::Swept(sweep) => Self {
                removed: sweep.removed.clone(),
                failed: sweep.failed.clone(),
                kept: sweep.kept.clone(),
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
struct JsonWorkspaces {
    skipped: Option<&'static str>,
    removed: Vec<JsonRemovedWorkspace>,
    failed: Vec<JsonFailedWorkspace>,
    retained_unreadable: Vec<JsonRetainedWorkspace>,
    kept: usize,
    empty_dirs: Vec<JsonOrphanEmptyTree>,
}

impl From<&gc::WorkspacePruneReport> for JsonWorkspaces {
    fn from(report: &gc::WorkspacePruneReport) -> Self {
        Self {
            skipped: None,
            removed: report
                .removed
                .iter()
                .map(JsonRemovedWorkspace::from)
                .collect(),
            failed: report
                .failed
                .iter()
                .map(|workspace| JsonFailedWorkspace {
                    dir_name: workspace.dir_name.clone(),
                    error: workspace.error.clone(),
                })
                .collect(),
            retained_unreadable: report
                .retained_unreadable
                .iter()
                .map(JsonRetainedWorkspace::from)
                .collect(),
            kept: report.kept,
            empty_dirs: report
                .empty_dirs
                .iter()
                .map(|tree| JsonOrphanEmptyTree {
                    dir_name: tree.dir_name.clone(),
                    files_removed: tree.files_removed,
                    bytes: tree.bytes,
                    agent_dirs_kept: tree.agent_dirs_kept,
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct JsonFailedWorkspace {
    dir_name: String,
    error: String,
}

#[derive(Serialize)]
struct JsonOrphanEmptyTree {
    dir_name: String,
    files_removed: usize,
    bytes: u64,
    agent_dirs_kept: usize,
}

#[derive(Serialize)]
struct JsonRemovedWorkspace {
    /// `null` for a dir that never recorded its workspace.
    workspace_id: Option<String>,
    dir_name: String,
    reason: &'static str,
    project_root: Option<String>,
    bytes: u64,
}

impl From<&gc::RemovedWorkspace> for JsonRemovedWorkspace {
    fn from(workspace: &gc::RemovedWorkspace) -> Self {
        Self {
            workspace_id: workspace.workspace_id.as_ref().map(ToString::to_string),
            dir_name: workspace.dir_name.clone(),
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

impl From<&(String, String)> for JsonRetainedWorkspace {
    fn from((workspace_id, error): &(String, String)) -> Self {
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
        gc::PruneReason::IncompatibleLayout => "incompatible_layout",
        gc::PruneReason::ProjectRootGone => "project_root_gone",
        gc::PruneReason::AbandonedScaffold => "abandoned_scaffold",
    }
}

fn runtime_items(out: &GcOutcome) -> usize {
    let report = &out.runtime;
    report.heartbeat_files_removed
        + report.sidebar_sockets_removed
        + report.sidecar_files_removed
        + report.probe_markers_removed
        + report.dirs_removed
        + out.budget_ledgers.ledgers
}

fn runtime_breakdown(out: &GcOutcome) -> String {
    let report = &out.runtime;
    [
        (report.heartbeat_files_removed, "heartbeat", "heartbeats"),
        (report.sidebar_sockets_removed, "socket", "sockets"),
        (report.sidecar_files_removed, "sidecar", "sidecars"),
        (report.probe_markers_removed, "probe", "probes"),
        (report.dirs_removed, "dir", "dirs"),
        (
            out.budget_ledgers.ledgers,
            "account budget ledger",
            "account budget ledgers",
        ),
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

#[cfg(test)]
#[path = "gc_tests.rs"]
mod tests;
