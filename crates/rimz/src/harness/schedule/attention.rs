//! The loop task verdict: whether a task needs you, why, in what words, and what blocks
//! a waiting condition. `rimz loop list`, `loop watch`, and the sidebar decide through
//! this one body. Read-only: it belongs to the sidebar import graph, takes the room's
//! paths from its caller, appends no record, runs no migration, and claims no run or watch
//! lock; its lock probes take only the directory gate the clock pass's probes take.

use std::collections::BTreeMap;
use std::path::Path;

use jiff::{Timestamp, Zoned};
use serde::Serialize;

use crate::config::TaskEntry;
use crate::disk::paths::{RuntimePaths, StatePaths};
use crate::harness::throttle::{self, Held};
use crate::harness::when::{self, CiSource, Verdict, WhenExpr, WindowReadings};
use crate::ids::HoldKind;
use crate::store::workflow::{RunRecord, RunState, Wait};
use crate::theme::fmt::{age_label, checkout_name, one_line};

use super::arming::{self, ArmState, DisabledReason};
use super::catalog::{LoadedTask, TaskCatalog};
use super::launch_ledger::{self, WorkflowLaunchRecord};
use super::run_log::{LoopRunStats, RunPolarity};
use super::{
    ParsedTrigger, TaskActionKind, TaskKind, TaskTiming, TaskTimingState, Trigger, fire, run_lock,
    signal, strikes,
};

#[cfg(test)]
mod tests;

/// Why a task needs you. The order is the order `loop list` prints its hints in; the
/// serde names are the `--json` `attention` values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Attention {
    CheckoutGone,
    Strikes,
    Failing,
    PrClosed,
    Held,
    Blocked,
    Invalid,
    Parked,
    Lost,
}

/// One start of the task held for capacity: a run's capacity wait or a queue ticket.
pub struct Hold<'a> {
    pub since: Timestamp,
    pub kind: Option<HoldKind>,
    pub reason: &'a str,
}

impl<'a> Hold<'a> {
    pub fn of_ticket(held: &'a Held) -> Self {
        Self {
            since: i64::try_from(held.since_ms)
                .ok()
                .and_then(|ms| Timestamp::from_millisecond(ms).ok())
                .unwrap_or(Timestamp::UNIX_EPOCH),
            kind: held.hold,
            reason: held.reason.as_deref().unwrap_or("waiting for its turn"),
        }
    }

    pub fn of_wait(wait: &'a Wait) -> Self {
        Self {
            since: wait.first,
            kind: wait.hold,
            reason: wait.detail.as_deref().unwrap_or_default(),
        }
    }
}

/// Every fact the verdict reads for one row.
pub struct Facts<'a> {
    pub holds: Vec<Hold<'a>>,
    /// `None` for a held run lock no catalog row claims.
    pub task: Option<TaskFacts<'a>>,
}

pub struct TaskFacts<'a> {
    pub task: &'a LoadedTask,
    pub timing: Option<&'a TaskTiming>,
    /// The stored checkout (`dir`), when the row names one.
    pub checkout: Option<&'a Path>,
    /// The task has fired, by [`fired`]: a gone checkout then means it is finished.
    pub fired: bool,
    /// The task has a run in progress, by [`run_in_progress`].
    pub in_progress: bool,
    /// Switched off, for any reason.
    pub disabled: bool,
    /// The acting fire since the row was armed failed.
    pub last_failed: bool,
    pub exit: Option<&'a str>,
    pub strikes: u32,
    pub watcher_alive: bool,
    /// The reader is the delivery's target.
    pub you: bool,
}

/// The cause that needs you and its reason, by priority: a hold that needs a fix, an
/// unfired task's gone checkout, a blocked project, an invalid row, strikes, failing, a
/// waiting one-off's pull request closed without merging, a lost watcher.
pub fn decide(facts: &Facts<'_>, now: Timestamp) -> Option<(Attention, String)> {
    if let Some(hold) = facts
        .holds
        .iter()
        .filter(|hold| hold.kind.is_some_and(|kind| kind.needs_fix()))
        .min_by_key(|hold| hold.since)
    {
        return Some((Attention::Held, hold.reason.to_owned()));
    }
    let row = facts.task.as_ref()?;
    let task = row.task;
    if !row.fired
        && let Some(dir) = row.checkout
        && !dir.is_dir()
    {
        return Some((
            Attention::CheckoutGone,
            format!("checkout {} is gone", checkout_name(dir)),
        ));
    }
    if let Some(state) = task.source().blocked_state() {
        return Some((
            Attention::Blocked,
            format!("blocked · project {}", state.as_str()),
        ));
    }
    if let Err(error) = task.trigger() {
        return Some((Attention::Invalid, format!("invalid: {error}")));
    }
    if let Err(error) = task.action() {
        return Some((Attention::Invalid, format!("invalid: {error}")));
    }
    if let Some(timing) = row.timing
        && let ArmState::Disabled(DisabledReason::Strikes(strikes)) = timing.arm_state()
    {
        return Some((
            Attention::Strikes,
            format!("disabled after {strikes} strikes"),
        ));
    }
    if !row.in_progress && !row.disabled && row.last_failed {
        let strikes = strikes::threshold(task.entry())
            .map(|max| format!(" · {}/{max} strikes", row.strikes))
            .unwrap_or_default();
        let exit = row
            .exit
            .map(|exit| format!(" · exit {exit}"))
            .unwrap_or_default();
        return Some((Attention::Failing, format!("✗ failing{strikes}{exit}")));
    }
    if let Some(key) = closed_pr(row) {
        return Some((Attention::PrClosed, format!("{key} closed without merging")));
    }
    if super::watch_lost(task.entry(), row.watcher_alive, now) {
        let wake = task
            .entry()
            .wait
            .as_ref()
            .map(|target| {
                format!(
                    " · will not wake {}",
                    TaskActionKind::Deliver.text(&target.handle, Some(task.entry()), row.you)
                )
            })
            .unwrap_or_default();
        return Some((Attention::Lost, format!("watcher gone{wake}")));
    }
    None
}

/// The key of the closed pull request an unfired one-off waits on, when the condition
/// cannot hold without that pull request merging.
fn closed_pr(row: &TaskFacts<'_>) -> Option<String> {
    if row.fired || row.task.kind() == TaskKind::Routine {
        return None;
    }
    let timing = row.timing?;
    let Ok(ParsedTrigger {
        trigger: Trigger::Condition { expr, .. },
        ..
    }) = timing.parsed()
    else {
        return None;
    };
    let TaskTimingState::Waiting { readings, .. } = timing.state() else {
        return None;
    };
    expr.closed_pr_term(&readings).map(|term| term.key.clone())
}

/// Whether a parked run takes the row over: parked replaces only a lost watcher.
pub fn parked_overrides(current: Option<Attention>) -> bool {
    current.is_none_or(|attention| attention == Attention::Lost)
}

/// `<name> <node> <word>[: <text>]` and its age, kept apart so only the text clips.
pub fn run_summary(
    name: &str,
    node: &str,
    word: &str,
    text: Option<&str>,
    since: Timestamp,
    now: Timestamp,
) -> (String, String) {
    let text = text
        .map(|text| format!(": {}", one_line(text)))
        .unwrap_or_default();
    (
        format!("{name} {node} {word}{text}"),
        age_label(now.duration_since(since).as_secs().max(0) as u64),
    )
}

/// The parked reason from the first parked run's summary and how many more are parked,
/// as its text and its age tail; the row's reason is the two joined by a space.
pub fn parked_words(first: (String, String), more: usize) -> (String, String) {
    let (text, age) = first;
    let tail = if more > 0 {
        format!("{age} (+{more} more)")
    } else {
        age
    };
    (format!("{} run(s) parked: {text}", more + 1), tail)
}

/// Whether a run record falls at or after the row's wait was armed.
pub fn since_wait_armed(entry: Option<&TaskEntry>, at: Timestamp) -> bool {
    entry
        .and_then(|entry| entry.wait_meta.as_ref())
        .is_none_or(|meta| at >= meta.armed_at)
}

/// Whether a staying one-off has launched: the rule that puts it under DONE.
pub fn fired(task: &LoadedTask, launch: Option<&WorkflowLaunchRecord>) -> bool {
    task.kind() == TaskKind::OneOff && task.entry().stay && launch.is_some()
}

/// The active runs of `task` in `root` waiting on capacity, by the checkout each is scoped to.
pub fn capacity_waits<'a>(
    runs: &'a [RunRecord],
    task: &'a str,
    root: &'a Path,
) -> impl Iterator<Item = (&'a Path, &'a Wait)> + 'a {
    runs.iter()
        .filter(move |run| {
            run.started_by(task) && run.project_root == root && run.state == RunState::Active
        })
        .filter_map(|run| {
            let wait = run
                .wait
                .as_ref()
                .filter(|wait| wait.outcome == "capacity")?;
            Some((run.scope.path.as_path(), wait))
        })
}

/// A task's run in progress, by [`run_in_progress`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InProgress<H> {
    /// The run lock is held; `H` is what the caller read of its holder.
    Locked(H),
    /// No run lock is held, and an active workflow run of the task is in progress.
    Run {
        /// When the run was created: the start a display that reads it running counts from.
        since: Timestamp,
        /// The run carries no wait on its record (a capacity wait, a guard's decline held
        /// inside the run): it is executing, not holding.
        executing: bool,
    },
}

impl<H> InProgress<H> {
    /// Whether the run reads as running: the lock is held, or the run executes.
    pub fn executing(&self) -> bool {
        match self {
            Self::Locked(_) => true,
            Self::Run { executing, .. } => *executing,
        }
    }
}

/// Whether task `task` has a run in progress in `root`: its held run lock, as the caller read
/// it, or else an active workflow run of the task, since a fire's run outlives its runner and
/// the lock. With an acting result since the row was armed (recorded at `acting_at`), only a
/// run created after it counts: a run's report stamps its record at the run's end, so a run
/// created at or before that instant (an `each_worktree` sibling) is not in progress. With
/// none (a first fire, a history of skips or refusals), every active run counts. A parked
/// run is never in progress. Of several, the earliest-created executing run, else the
/// earliest waiting one.
pub fn run_in_progress<H>(
    lock: Option<H>,
    runs: &[RunRecord],
    task: &str,
    root: &Path,
    acting_at: Option<Timestamp>,
) -> Option<InProgress<H>> {
    if let Some(holder) = lock {
        return Some(InProgress::Locked(holder));
    }
    runs.iter()
        .filter(|run| {
            run.ran_for(task)
                && run.project_root == root
                && run.state == RunState::Active
                && acting_at.is_none_or(|at| run.created_at > at)
        })
        .map(|run| (run.wait.is_none(), run.created_at))
        .min_by_key(|(executing, since)| (!executing, *since))
        .map(|(executing, since)| InProgress::Run { since, executing })
}

/// A room's condition readings, read once and shared by each task's observation.
pub struct RoomReadings<'a> {
    source: Option<CiSource>,
    windows: WindowReadings<'a>,
    states: BTreeMap<String, when::WhenState>,
    stamps: BTreeMap<String, Timestamp>,
}

impl<'a> RoomReadings<'a> {
    /// Forge readings come only from an open room's sidebar; `open` says whether one runs.
    pub fn new(runtime: Option<&'a RuntimePaths>, open: bool, now: Timestamp) -> Self {
        Self {
            source: runtime.filter(|_| open).map(CiSource::read),
            windows: WindowReadings::new(runtime, now),
            states: runtime.map(fire::last_when_states).unwrap_or_default(),
            stamps: runtime.map(fire::last_stamps).unwrap_or_default(),
        }
    }

    /// Task `name`'s condition read at `scope` under the freshness rule firing applies.
    fn verdict(
        &self,
        name: &str,
        entry: &TaskEntry,
        scope: &Path,
        expr: &WhenExpr,
        now: Timestamp,
    ) -> Verdict {
        when::evaluate(
            expr,
            scope,
            self.source.as_ref(),
            entry.provider.as_ref(),
            entry.account.as_ref(),
            &self.windows,
        )
        .freshen(
            expr,
            scope,
            self.source.as_ref(),
            self.stamps.get(name).copied(),
            Some(now),
            true,
        )
    }

    /// `timing` with a condition trigger's reading folded in; any other trigger as given.
    pub fn observe(
        &self,
        name: &str,
        entry: &TaskEntry,
        scope: &Path,
        timing: TaskTiming,
        now: Timestamp,
    ) -> TaskTiming {
        self.observe_blocking(name, entry, scope, timing, now).0
    }

    fn observe_blocking(
        &self,
        name: &str,
        entry: &TaskEntry,
        scope: &Path,
        timing: TaskTiming,
        now: Timestamp,
    ) -> (TaskTiming, Option<BlockingTerm>, Vec<String>) {
        let Ok(ParsedTrigger {
            trigger: Trigger::Condition { expr, .. },
            ..
        }) = timing.parsed()
        else {
            return (timing, None, Vec::new());
        };
        let verdict = self.verdict(name, entry, scope, expr, now);
        let blocking = (verdict.truth != Some(true))
            .then(|| BlockingTerm::of(expr, &verdict, self.source.as_ref(), scope));
        let mut unmet = Vec::new();
        if blocking.is_some() {
            for leaf in unmet_leaves(expr, &verdict) {
                if !unmet.contains(&leaf.key) {
                    unmet.push(leaf.key.clone());
                }
            }
        }
        (
            timing.with_condition(&verdict, self.states.get(name), now),
            blocking,
            unmet,
        )
    }
}

/// The condition term that keeps a waiting task from firing, as the condition's own
/// blocking pick names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockingTerm {
    /// The term's raw key, such as `pr[feature]`.
    pub key: String,
    /// The term's reading; `None` while it is unread.
    pub reading: Option<String>,
    /// The cached pull request number behind a forge key, when the cache holds one.
    pub pr: Option<u64>,
    /// How many other terms are unmet, a repeat of the blocking term's key included.
    pub more: usize,
}

/// A term's reading as the blocking pick names it: `None` while it is unread.
fn term_reading(verdict: &Verdict, key: &str) -> Option<String> {
    if verdict.unread.contains_key(key) {
        return None;
    }
    verdict.readings.get(key).cloned().flatten()
}

/// The condition's leaves that are not met, a repeat of one key included.
fn unmet_leaves<'e>(
    expr: &'e WhenExpr,
    verdict: &'e Verdict,
) -> impl Iterator<Item = &'e when::WhenTerm> + 'e {
    expr.leaves()
        .filter(|(leaf, negated)| {
            leaf.truth(term_reading(verdict, &leaf.key).as_deref())
                .map(|truth| truth != *negated)
                != Some(true)
        })
        .map(|(leaf, _)| leaf)
}

impl BlockingTerm {
    fn of(expr: &WhenExpr, verdict: &Verdict, source: Option<&CiSource>, scope: &Path) -> Self {
        let (term, _) = expr.blocking_term(verdict);
        let more = unmet_leaves(expr, verdict)
            .filter(|leaf| !std::ptr::eq(*leaf, term))
            .count();
        Self {
            key: term.key.clone(),
            reading: term_reading(verdict, &term.key),
            pr: source.and_then(|source| source.term_pr_number(&term.key, scope)),
            more,
        }
    }
}

/// One task's verdict as the room's records state it.
#[derive(Debug)]
pub struct TaskReading {
    /// The cause that puts the task under NEEDS YOU, with its reason.
    pub attention: Option<(Attention, String)>,
    /// Whether the task has fired, by the rule DONE uses.
    pub fired: bool,
    /// What blocks a waiting condition trigger.
    pub blocking: Option<BlockingTerm>,
    /// The key of every unmet term of a waiting condition trigger, each once, in the
    /// condition's order; empty when nothing blocks.
    pub unmet: Vec<String>,
    /// The launch the ledger records for a fired task.
    pub launch: Option<WorkflowLaunchRecord>,
}

/// The facts a reader cannot take from the room's paths.
pub struct ReadContext<'a> {
    /// A sidebar runs in the room, so forge readings are live.
    pub open: bool,
    /// Each task's run-log stats by name, from the caller's fold over the room's run log.
    pub stats: &'a BTreeMap<String, LoopRunStats>,
    pub now: &'a Zoned,
}

/// The verdict of every visible task of the room at `root`, whose state and runtime trees
/// the caller names, by task name. Each input (the catalog, the workflow runs, the launch
/// ledger, the condition readings, the run locks, arming, strikes) is read once per call,
/// however many tasks the room holds. Machine-wide records (the catalog's machine rows,
/// arming, strikes, the start queue) are read where every loop reader reads them.
pub fn read_room(
    root: &Path,
    state: &StatePaths,
    runtime: &RuntimePaths,
    context: ReadContext<'_>,
) -> BTreeMap<String, TaskReading> {
    let catalog = TaskCatalog::load_room_lenient(root, state);
    read_catalog(&catalog, root, state, runtime, context)
}

/// [`read_room`] over a catalog the caller already loaded for the room at `root`.
pub fn read_catalog(
    catalog: &TaskCatalog,
    root: &Path,
    state: &StatePaths,
    runtime: &RuntimePaths,
    context: ReadContext<'_>,
) -> BTreeMap<String, TaskReading> {
    let ReadContext { open, stats, now } = context;
    let tasks = catalog
        .visible()
        .iter()
        .filter(|(_, task)| task.entry().resolved_root() == root)
        .collect::<Vec<_>>();
    if tasks.is_empty() {
        return BTreeMap::new();
    }
    let names = tasks
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    let stamps = fire::last_stamps(runtime);
    let arming = arming::load();
    let strikes = strikes::load();
    let readings = RoomReadings::new(Some(runtime), open, now.timestamp());
    let workflows = list_runs(state);
    let mut tickets = BTreeMap::<String, Vec<Held>>::new();
    for (task, held) in throttle::room_held(root) {
        tickets.entry(task).or_default().push(held);
    }
    let ledger = if tasks
        .iter()
        .any(|(_, task)| task.entry().dedupes_checkout())
    {
        launch_ledger::load_workflows(state).unwrap_or_default()
    } else {
        Default::default()
    };
    let running = run_lock::held_tasks(runtime, &names).unwrap_or_default();
    tasks
        .into_iter()
        .map(|(name, task)| {
            let entry = task.entry();
            let run_dir = entry
                .dir
                .as_ref()
                .map(|_| entry.run_dir())
                .unwrap_or_else(|| root.to_path_buf());
            let key = task.key_for_root(name, root);
            let timing = TaskTiming::evaluate(
                task.trigger(),
                task.source(),
                stamps.get(name).copied(),
                arming.get(&key),
                now,
            );
            let (timing, blocking, unmet) =
                readings.observe_blocking(name, entry, &run_dir, timing, now.timestamp());
            let acting = stats
                .get(name)
                .and_then(|stats| stats.acting.as_ref())
                .filter(|acting| since_wait_armed(Some(entry), acting.record.at));
            let exit = acting.and_then(|acting| acting.record.exit_label());
            let launch =
                launch_ledger::launched(entry, name, &run_dir, &ledger).filter(|_| entry.stay);
            let fired = fired(task, launch);
            let held = tickets
                .get(name.as_str())
                .map(Vec::as_slice)
                .unwrap_or_default();
            let facts = Facts {
                holds: capacity_waits(&workflows, name, root)
                    .map(|(_, wait)| Hold::of_wait(wait))
                    .chain(held.iter().map(Hold::of_ticket))
                    .collect(),
                task: Some(TaskFacts {
                    task,
                    timing: Some(&timing),
                    checkout: entry.dir.as_ref().map(|_| run_dir.as_path()),
                    fired,
                    in_progress: run_in_progress(
                        running.contains(name.as_str()).then_some(()),
                        &workflows,
                        name,
                        root,
                        acting.map(|acting| acting.record.at),
                    )
                    .is_some(),
                    disabled: matches!(timing.arm_state(), ArmState::Disabled(_)),
                    last_failed: acting.is_some_and(|acting| acting.polarity != RunPolarity::Good),
                    exit: exit.as_deref(),
                    strikes: strikes.get(&key).copied().unwrap_or_default(),
                    watcher_alive: signal::watcher_info(runtime, name).ok().flatten().is_some(),
                    you: false,
                }),
            };
            let mut attention = decide(&facts, now.timestamp());
            if parked_overrides(attention.as_ref().map(|(cause, _)| *cause)) {
                let mut parked = workflows.iter().filter(|run| {
                    run.started_by(name) && run.project_root == root && run.park.is_some()
                });
                if let Some(first) = parked.next() {
                    let (text, tail) =
                        parked_words(parked_summary(first, now.timestamp()), parked.count());
                    attention = Some((Attention::Parked, format!("{text} {tail}")));
                }
            }
            let reading = TaskReading {
                attention,
                fired,
                blocking,
                unmet,
                launch: launch.filter(|_| fired).cloned(),
            };
            (name.clone(), reading)
        })
        .collect()
}

/// The room's workflow runs, listed once per [`read_room`] call.
fn list_runs(state: &StatePaths) -> Vec<RunRecord> {
    #[cfg(test)]
    tests::RUN_LISTINGS.set(tests::RUN_LISTINGS.get() + 1);
    crate::store::workflow::live_runs(state).unwrap_or_default()
}

fn parked_summary(run: &RunRecord, now: Timestamp) -> (String, String) {
    let park = run.park.as_ref();
    let since = park
        .filter(|_| run.state == RunState::Parked)
        .map_or(run.entered_at, |park| park.at);
    run_summary(
        &run.address().to_string(),
        &run.node,
        park.map_or(run.state.as_str(), |park| park.reason.as_str()),
        park.and_then(|park| park.text.as_deref()),
        since,
        now,
    )
}
