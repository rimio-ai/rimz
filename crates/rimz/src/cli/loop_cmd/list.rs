//! Room-scoped loop list model and presentation.

use super::*;
use crate::cli::wake_view::checkout_name;
use rimz::config::WatchSpec;
use rimz::harness::schedule::attention::{self, Attention, InProgress};
use rimz::harness::schedule::{TaskKind, Trigger, launch_ledger, signal::watcher_info};
use serde::Serialize;

#[path = "list_render.rs"]
mod text;
#[path = "list_watch.rs"]
mod watch;
pub(super) use crate::cli::wake_view::{action_text as action, worktree_name};
#[cfg(test)]
use text::wrap_trigger;

#[cfg(test)]
#[path = "list_tests.rs"]
mod tests;

pub(super) fn run(args: ListArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone()).ok();
    let mut held_stats = run_log::HeldStats::new(&rimz::disk::paths::logs_dir());
    let mut model = load(workspace.as_ref(), LoadScope::AllRooms, &mut held_stats)?;
    if globals.root.is_none()
        && workspace.as_ref().is_some_and(|workspace| {
            workspace.root_class == rimz::workspace::RootClass::Directory
                && StatePaths::for_project_root(&workspace.project_root)
                    .is_ok_and(|paths| !paths.workspace_record.exists())
        })
        && model
            .rooms
            .iter()
            .any(|room| room.here && room.tasks.is_empty() && room.retired.is_empty())
    {
        model.rooms.retain(|room| !room.here);
        model.caller_root = None;
    }
    let caller_session = workspace
        .as_ref()
        .and_then(|workspace| super::super::open_existing_store(workspace).ok().flatten())
        .and_then(|store| {
            let caller = super::super::send::resolve_caller(&store).ok().flatten()?;
            let projection = store.runtime_projection(rimz::RuntimeScope::Audit).ok()?;
            rimz::harness::ancestry::resolve_launch_caller(&projection.agents, &caller)
                .ok()
                .filter(|agent| agent.ended_at.is_none())
                .map(|agent| (agent.kind.clone(), agent.agent_id.clone()))
        });
    for row in model.rooms.iter_mut().flat_map(|room| &mut room.tasks) {
        row.you = row
            .task
            .as_ref()
            .and_then(|task| task.entry().wait.as_ref())
            .zip(caller_session.as_ref())
            .is_some_and(|(target, (kind, session))| {
                kind == &target.kind && session == &target.session
            });
        if row.you && row.attention == Some(Attention::Lost) {
            row.reason.push_str(" (you)");
        }
    }
    for warning in &model.warnings {
        writeln!(
            ui::err(),
            "{} {warning}",
            ui::paint(ui::palette::warn().bold(), "warning:")
        )?;
    }
    let all = args.all || model.caller_root.is_none();
    let rooms = model
        .rooms
        .iter()
        .filter(|room| all || room.here)
        .collect::<Vec<_>>();
    if args.json {
        return ui::json(&serde_json::json!({"rooms": rooms}));
    }
    let width = std::io::stdout()
        .is_terminal()
        .then(|| ui::terminal_columns(100));
    text::write(
        &mut ui::out(),
        &model,
        &rooms,
        args.all,
        width,
        workspace
            .as_ref()
            .map(|workspace| workspace.worktree_root.as_path()),
        globals.mux,
    )
}

pub(super) fn render_watch_frame(
    out: &mut impl Write,
    workspace: &rimz::ResolvedWorkspace,
    hold: bool,
    held_stats: &mut run_log::HeldStats,
) -> Result<()> {
    let model = load(Some(workspace), LoadScope::CallerRoom, held_stats)?;
    let (cols, rows) = rimz::mux::detect_terminal_size().unwrap_or((80, 24));
    // CallerRoom with a resolved workspace always loads exactly one room.
    watch::write(
        out,
        &model.rooms[0],
        model.now,
        usize::from(cols),
        usize::from(rows),
        hold,
    )?;
    Ok(())
}

struct ListModel {
    rooms: Vec<Room>,
    caller_root: Option<PathBuf>,
    now: Timestamp,
    warnings: Vec<String>,
    /// Whether the loop timer is active, read only when a loaded room is closed.
    timer_active: bool,
}

#[derive(Serialize)]
struct Room {
    root: PathBuf,
    open: bool,
    here: bool,
    spend_today_usd: f64,
    tasks: Vec<TaskRow>,
    retired: Vec<Retired>,
}

#[derive(Serialize)]
struct Retired {
    name: String,
    at: Timestamp,
    result: LoopRunResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    checkout: Option<PathBuf>,
    #[serde(skip)]
    worktree_here: bool,
}

enum DoneEntry<'a> {
    Task(&'a TaskRow),
    Retired(&'a Retired),
}

impl DoneEntry<'_> {
    fn name(&self) -> &str {
        match self {
            Self::Task(row) => &row.name,
            Self::Retired(row) => &row.name,
        }
    }

    fn at(&self) -> Timestamp {
        match self {
            Self::Task(row) => row
                .running_since
                .or_else(|| row.launch.as_ref().map(|launch| launch.at))
                .or_else(|| row.last.as_ref().map(|last| last.at))
                .unwrap_or(Timestamp::UNIX_EPOCH),
            Self::Retired(row) => row.at,
        }
    }

    fn running(&self) -> bool {
        matches!(self, Self::Task(row) if row.state == TaskState::Running)
    }
}

fn done_entries(room: &Room) -> Vec<DoneEntry<'_>> {
    let mut entries = room
        .tasks
        .iter()
        .filter(|row| row.section == Section::Done)
        .map(DoneEntry::Task)
        .chain(room.retired.iter().map(DoneEntry::Retired))
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| {
        b.running()
            .cmp(&a.running())
            .then(b.at().cmp(&a.at()))
            .then(a.name().cmp(b.name()))
    });
    entries
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Section {
    NeedsYou,
    Routine,
    Waiting,
    Done,
    WakeUps,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum TaskState {
    Live,
    Off,
    NotEnabled,
    Paused,
    Running,
    WaitsForRoom,
}

impl TaskState {
    fn label(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Off => "off",
            Self::NotEnabled => "off · repo task, enable here to run",
            Self::Paused => "paused",
            Self::Running => "running",
            Self::WaitsForRoom => "waits for room",
        }
    }

    fn observe(timing: &schedule::TaskTiming, running: bool, clocks_kept: bool) -> Self {
        if running {
            return Self::Running;
        }
        match timing.arm_state() {
            ArmState::Disabled(DisabledReason::NotEnabledHere) => Self::NotEnabled,
            ArmState::Disabled(_) => Self::Off,
            ArmState::Paused(_) => Self::Paused,
            ArmState::Live
                if !clocks_kept
                    && timing
                        .parsed()
                        .is_ok_and(|parsed| matches!(parsed.trigger, Trigger::Schedule(_))) =>
            {
                Self::WaitsForRoom
            }
            ArmState::Live => Self::Live,
        }
    }

    fn held_text(state: &schedule::TaskTimingState, now: Timestamp) -> Option<String> {
        Some(match state {
            schedule::TaskTimingState::Disabled(DisabledReason::NotEnabledHere) => {
                Self::NotEnabled.label().into()
            }
            schedule::TaskTimingState::Disabled(_) => Self::Off.label().into(),
            schedule::TaskTimingState::Paused(until) => {
                format!(
                    "{}, resumes {}",
                    Self::Paused.label(),
                    ui::rel_until(*until, now)
                )
            }
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ActionKind {
    Check,
    Start,
    Wake,
    Workflow,
}

impl From<&TaskAction> for ActionKind {
    fn from(action: &TaskAction) -> Self {
        match action {
            TaskAction::CheckOnly => Self::Check,
            TaskAction::Spawn(_) => Self::Start,
            TaskAction::Workflow(_) => Self::Workflow,
            TaskAction::Deliver(_) => Self::Wake,
        }
    }
}

#[derive(PartialEq, Eq, Serialize)]
struct Action {
    kind: ActionKind,
    subject: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum OwnerKind {
    Team,
    Loop,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
struct Owner {
    kind: OwnerKind,
    name: String,
}

#[derive(Serialize)]
struct LastRun {
    at: Timestamp,
    result: LoopRunResult,
    ok: bool,
    streak: usize,
    #[serde(skip)]
    record: LoopRunRecord,
}

impl From<&run_log::ActingRun> for LastRun {
    fn from(acting: &run_log::ActingRun) -> Self {
        Self {
            at: acting.record.at,
            result: acting.record.result,
            ok: acting.polarity == run_log::RunPolarity::Good,
            streak: acting.streak,
            record: acting.record.clone(),
        }
    }
}

#[derive(Serialize)]
struct Heard {
    signal: String,
    at: Timestamp,
}

impl From<&run_log::HeardSignal> for Heard {
    fn from(heard: &run_log::HeardSignal) -> Self {
        Self {
            signal: heard.signal.to_string(),
            at: heard.at,
        }
    }
}

#[derive(Serialize)]
pub(super) struct TaskRow {
    name: String,
    section: Section,
    kind: TaskKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    condition: Option<rimz::harness::when::ConditionCount>,
    #[serde(skip_serializing_if = "Option::is_none")]
    attention: Option<Attention>,
    state: TaskState,
    trigger: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    action: Option<Action>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dir: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    worktree: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fresh_worktree: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner: Option<Owner>,
    you: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    booked_by: Vec<rimz::agents::LaunchedBy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_at: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    running_since: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last: Option<LastRun>,
    #[serde(skip_serializing_if = "Option::is_none")]
    heard: Option<Heard>,
    /// The task's quiet fires, which leave no history row.
    #[serde(skip_serializing_if = "Option::is_none")]
    quiet: Option<run_log::QuietFires>,
    /// The newest fire, read when no fire acted (a canceled run): it still fired.
    #[serde(skip)]
    newest: Option<LoopRunRecord>,
    spend_today_usd: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    budget_per_day_usd: Option<f64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    workflow_runs: Vec<WorkflowRunRow>,
    #[serde(rename = "capacity_wait", serialize_with = "capacity_wait_json")]
    capacity: Vec<CapacityStart>,
    #[serde(skip)]
    head: String,
    #[serde(skip)]
    continuation: String,
    #[serde(skip)]
    reason: String,
    #[serde(skip)]
    task: Option<LoadedTask>,
    #[serde(skip)]
    timing: Option<schedule::TaskTiming>,
    /// The task's run in progress, by [`attention::run_in_progress`]: it holds back a failure,
    /// and reads as running while the lock is held or the run executes.
    #[serde(skip)]
    in_progress: Option<InProgress<Option<RunLockInfo>>>,
    #[serde(skip)]
    held: Vec<Held>,
    #[serde(skip)]
    worktree_here: bool,
    #[serde(skip)]
    watcher_pid: Option<u32>,
    #[serde(skip)]
    launched_count: usize,
    #[serde(skip)]
    launch: Option<launch_ledger::WorkflowLaunchRecord>,
    #[serde(skip)]
    exit: Option<String>,
}

struct RowContext<'a> {
    run_dir: &'a Path,
    /// The room's project root and its workflow runs.
    root: &'a Path,
    runs: &'a [rimz::store::workflow::RunRecord],
    /// The room is open, or the loop timer fires its clocks while it is closed.
    clocks_kept: bool,
    stats: Option<&'a run_log::LoopRunStats>,
    count: u32,
    watcher_pid: Option<u32>,
    launch: Option<&'a launch_ledger::WorkflowLaunchRecord>,
    now: Timestamp,
}

#[derive(Clone, Serialize)]
pub(super) struct CapacityWaitView {
    kinds: Vec<rimz::ids::AgentKind>,
    pub(super) since: Timestamp,
    gives_up_at: Option<Timestamp>,
    reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    hold: Option<rimz::ids::HoldKind>,
}

impl From<&rimz::store::workflow::Wait> for CapacityWaitView {
    fn from(wait: &rimz::store::workflow::Wait) -> Self {
        let hold = attention::Hold::of_wait(wait);
        Self {
            kinds: wait.kinds.clone(),
            since: hold.since,
            gives_up_at: wait.gives_up_at,
            reason: hold.reason.to_owned(),
            hold: hold.kind,
        }
    }
}

impl CapacityWaitView {
    pub(super) fn text(&self, now: Timestamp) -> String {
        rimz::theme::fmt::held_start(self.since, self.gives_up_at, now)
    }

    pub(super) fn reason(&self) -> &str {
        &self.reason
    }
}

impl From<&Held> for CapacityWaitView {
    fn from(held: &Held) -> Self {
        let hold = attention::Hold::of_ticket(held);
        Self {
            kinds: held.kinds.clone(),
            since: hold.since,
            gives_up_at: held
                .gives_up_ms
                .and_then(|ms| i64::try_from(ms).ok())
                .and_then(|ms| Timestamp::from_millisecond(ms).ok()),
            reason: hold.reason.to_owned(),
            hold: hold.kind,
        }
    }
}

#[derive(Clone)]
struct CapacityStart {
    checkout: PathBuf,
    wait: CapacityWaitView,
}

fn capacity_wait_json<S: serde::Serializer>(
    starts: &[CapacityStart],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    starts
        .iter()
        .min_by_key(|start| start.wait.since)
        .map(|start| &start.wait)
        .serialize(serializer)
}

/// The capacity waits of a task's active runs, by the checkout each run is scoped to.
pub(super) fn capacity_waits<'a>(
    runs: &'a [rimz::store::workflow::RunRecord],
    task: &'a str,
    root: &'a Path,
) -> impl Iterator<Item = (&'a Path, CapacityWaitView)> + 'a {
    attention::capacity_waits(runs, task, root)
        .map(|(checkout, wait)| (checkout, CapacityWaitView::from(wait)))
}

enum LoadScope {
    CallerRoom,
    AllRooms,
}

pub(super) fn room_roots(
    caller_root: Option<&Path>,
) -> Result<std::collections::BTreeSet<PathBuf>> {
    let machine = TaskCatalog::load(None)?;
    let mut roots = schedule::catalog::workspace_instance_roots();
    roots.extend(
        machine
            .visible()
            .values()
            .map(|task| task.entry().resolved_root()),
    );
    roots.extend(caller_root.map(Path::to_owned));
    Ok(roots)
}

fn load(
    workspace: Option<&rimz::ResolvedWorkspace>,
    scope: LoadScope,
    held_stats: &mut run_log::HeldStats,
) -> Result<ListModel> {
    let caller_root = workspace.map(|workspace| workspace.project_root.clone());
    let roots = match (scope, caller_root.as_ref()) {
        (LoadScope::CallerRoom, Some(root)) => std::collections::BTreeSet::from([root.clone()]),
        _ => room_roots(caller_root.as_deref())?,
    };
    let arming = arming::load();
    let strikes = strikes::load();
    let now = Timestamp::now();
    let config = MachineConfig::load_lenient();
    let now_zoned = now.to_zoned(config.time_zone());
    let timer_active = std::cell::OnceCell::new();
    let mut rooms = Vec::new();
    let mut warnings = config
        .notices
        .skipped_loop_tasks
        .values()
        .cloned()
        .collect::<Vec<_>>();
    for root in roots {
        let here = caller_root.as_ref() == Some(&root);
        let catalog = match TaskCatalog::load_room(&root) {
            Ok(catalog) => catalog,
            Err(error) if here => return Err(error),
            Err(error) => {
                warnings.push(format!(
                    "cannot load loop tasks in {}: {error:#}",
                    root.display()
                ));
                continue;
            }
        };
        let locks = match RunLocks::list(&root) {
            Ok(locks) => Some(locks),
            Err(error) => {
                warnings.push(format!(
                    "cannot list the loop run locks in {}, so no active run is shown: {error:#}",
                    root.display()
                ));
                None
            }
        };
        let runtime = runtime_for_root(&root);
        let open = runtime.as_ref().is_some_and(fresh_sidebar_present);
        let clocks_kept = open || *timer_active.get_or_init(timer::active);
        let mut tickets: BTreeMap<String, Vec<Held>> = BTreeMap::new();
        for (name, held) in rimz::harness::throttle::room_held(&root) {
            tickets.entry(name).or_default().push(held);
        }
        let conditions = std::cell::OnceCell::new();
        let stamps = runtime
            .as_ref()
            .map(schedule::last_stamps)
            .unwrap_or_default();
        let stats = held_stats.stats(&root, &now_zoned);
        let entries = catalog
            .visible()
            .values()
            .map(LoadedTask::entry)
            .filter(|entry| entry.resolved_root() == root)
            .collect::<Vec<_>>();
        let workflow_ledger = if entries.iter().any(|entry| entry.dedupes_checkout()) {
            load_launch_memory(&root, launch_ledger::load_workflows, &mut warnings)
        } else {
            Default::default()
        };
        let workflows = StatePaths::for_project_root(&root)
            .ok()
            .and_then(|paths| rimz::store::workflow::live_runs(&paths).ok())
            .unwrap_or_default();
        let workflow_specs = StatePaths::for_project_root(&root)
            .map(|state| crate::cli::workflow_view::sealed_specs(&state, &workflows))
            .unwrap_or_default();
        let mut tasks = Vec::new();
        for (name, task) in catalog.visible() {
            let entry = task.entry();
            let task_root = entry.resolved_root();
            if task_root != root {
                continue;
            }
            let run_dir = entry
                .dir
                .as_ref()
                .map(|_| entry.run_dir())
                .unwrap_or_else(|| task_root.clone());
            let key = task.key_for_root(name, &task_root);
            let running = locks.as_ref().and_then(|locks| match locks.state(name) {
                Ok(RunLockState::Held(holder)) => Some(holder),
                Ok(RunLockState::Available) => None,
                Err(error) => {
                    warnings.push(format!("cannot read the loop run lock of `{name}`, so its active run is not shown: {error:#}"));
                    None
                }
            });
            let timing = schedule::TaskTiming::evaluate(
                task.trigger(),
                task.source(),
                stamps.get(name).copied(),
                arming.get(&key),
                &now_zoned,
            );
            let timing = if timing
                .parsed()
                .is_ok_and(|parsed| matches!(parsed.trigger, Trigger::Condition { .. }))
            {
                conditions
                    .get_or_init(|| attention::RoomReadings::new(runtime.as_ref(), open, now))
                    .observe(name, entry, &run_dir, timing, now)
            } else {
                timing
            };
            let count = strikes.get(&key).copied().unwrap_or_default();
            let mut row = TaskRow::new(
                name,
                task,
                timing,
                running,
                RowContext {
                    run_dir: &run_dir,
                    root: &root,
                    runs: &workflows,
                    clocks_kept,
                    stats: stats.get(name),
                    count,
                    watcher_pid: runtime
                        .as_ref()
                        .and_then(|runtime| watcher_info(runtime, name).ok().flatten())
                        .map(|info| info.pid),
                    launch: launch_ledger::launched(task.entry(), name, &run_dir, &workflow_ledger),
                    now,
                },
            );
            row.capacity.extend(
                capacity_waits(&workflows, name, &root).map(|(checkout, wait)| CapacityStart {
                    checkout: checkout.to_path_buf(),
                    wait,
                }),
            );
            row.held = tickets.remove(name).unwrap_or_default();
            row.refresh_attention(count, now);
            row.worktree_here = here
                && workspace.is_some_and(|workspace| match task.entry().task_worktree() {
                    Some(rimz::config::TaskWorktree::Named(name)) => {
                        rimz::worktree::worktree_path(&root, &config.agents.worktree, name)
                            .is_ok_and(|path| path == workspace.worktree_root)
                    }
                    _ => run_dir == workspace.worktree_root,
                });
            row.launched_count = launched_count(task.entry(), name, &workflow_ledger);
            row.attach_workflow_runs(
                WorkflowRunRow::of_task(&workflows, name, &root, &workflow_specs, now),
                now,
            );
            tasks.push(row);
        }
        if let Some(locks) = locks {
            let names = tasks
                .iter()
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>();
            for (name, holder) in locks.rowless(&names) {
                let history = stats.get(&name);
                let held = tickets.remove(&name).unwrap_or_default();
                let mut row = TaskRow::rowless(name, InProgress::Locked(holder), history);
                row.held = held;
                let (attention, reason) = attention(&row, 0, now);
                if attention.is_some() {
                    row.attention = attention;
                    row.reason = reason;
                    row.section = Section::NeedsYou;
                }
                tasks.push(row);
            }
        }
        let mut retired = Vec::new();
        for (name, stats) in stats.iter() {
            if tasks.iter().any(|row| &row.name == name)
                || stats.last_kind.map(|(_, kind)| kind) != Some(TaskKind::OneOff)
            {
                continue;
            }
            let Some(last) = stats.last.as_ref() else {
                continue;
            };
            let acting_at = stats.acting.as_ref().map(|acting| acting.record.at);
            // A consumed one-shot whose run still executes is a run in progress, as under its
            // lock.
            if let Some(run) = attention::run_in_progress(None, &workflows, name, &root, acting_at)
                .filter(InProgress::executing)
            {
                tasks.push(TaskRow::rowless(name.clone(), run, Some(stats)));
                continue;
            }
            retired.push(Retired {
                name: name.clone(),
                at: last.at,
                result: last.result,
                checkout: last.checkout.clone(),
                worktree_here: here
                    && workspace.is_some_and(|workspace| {
                        last.checkout.as_ref() == Some(&workspace.worktree_root)
                    }),
            });
        }
        rooms.push(Room {
            root,
            open,
            here,
            spend_today_usd: stats
                .values()
                .fold(0.0, |total, stats| total + stats.spend_today_usd),
            tasks,
            retired,
        });
    }
    rooms.sort_by(|a, b| b.here.cmp(&a.here).then(a.root.cmp(&b.root)));
    Ok(ListModel {
        rooms,
        caller_root,
        now,
        warnings,
        timer_active: timer_active.get().copied().unwrap_or(false),
    })
}

fn load_launch_memory<T: Default>(
    root: &Path,
    read: impl FnOnce(&StatePaths) -> Result<T, launch_ledger::LedgerErr>,
    warnings: &mut Vec<String>,
) -> T {
    let load = || -> Result<_> { Ok(read(&StatePaths::for_project_root(root)?)?) };
    load().unwrap_or_else(|error| {
        warnings.push(format!(
            "cannot load loop launch memory in {}: {error:#}",
            root.display()
        ));
        T::default()
    })
}

impl TaskRow {
    fn owner_class(&self) -> schedule::DeliveryOwner<'_> {
        self.task
            .as_ref()
            .map_or(schedule::DeliveryOwner::Loop(&self.name), |task| {
                schedule::delivery_owner(&self.name, task.entry())
            })
    }

    fn new(
        name: &str,
        task: &LoadedTask,
        timing: schedule::TaskTiming,
        running: Option<Option<RunLockInfo>>,
        context: RowContext<'_>,
    ) -> Self {
        let RowContext {
            run_dir,
            root,
            runs,
            clocks_kept,
            stats,
            count,
            watcher_pid,
            launch,
            now,
        } = context;
        let entry = task.entry();
        let acting_at = stats
            .and_then(|stats| stats.acting.as_ref())
            .map(|acting| acting.record.at)
            .filter(|at| since_wait_armed(Some(task), *at));
        let in_progress = attention::run_in_progress(running, runs, name, root, acting_at);
        let state = TaskState::observe(
            &timing,
            in_progress.as_ref().is_some_and(InProgress::executing),
            clocks_kept,
        );
        let (head, continuation) = trigger_text(entry, run_dir, &timing, state, now);
        let action = task.action().ok().map(|action| Action {
            kind: ActionKind::from(action),
            subject: match action {
                TaskAction::Spawn(subject) | TaskAction::Workflow(subject) => subject.clone(),
                TaskAction::Deliver(target) => target.handle.clone(),
                TaskAction::CheckOnly => "check".into(),
            },
        });
        let dir = entry.dir.as_ref().map(|_| run_dir.to_owned());
        let worktree = worktree_name(entry, run_dir);
        let owner = match schedule::delivery_owner(name, entry) {
            schedule::DeliveryOwner::Team(team) => Some(Owner {
                kind: OwnerKind::Team,
                name: team.to_string(),
            }),
            schedule::DeliveryOwner::Resident(name) => Some(Owner {
                kind: OwnerKind::Loop,
                name: name.to_owned(),
            }),
            schedule::DeliveryOwner::SelfWait | schedule::DeliveryOwner::Loop(_) => None,
        };
        let mut row = Self {
            name: name.into(),
            section: Section::Routine,
            kind: task.kind(),
            condition: timing.condition_count().filter(|_| !entry.each_worktree),
            attention: None,
            state,
            trigger: [head.as_str(), continuation.as_str()]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" · "),
            label: entry.label.clone(),
            action,
            dir,
            worktree,
            fresh_worktree: entry.fresh_worktree.clone(),
            owner,
            you: false,
            booked_by: entry.booked_by.clone(),
            next_at: timing.next_timestamp(),
            running_since: running_since(in_progress.as_ref()),
            last: None,
            heard: None,
            newest: None,
            spend_today_usd: 0.0,
            budget_per_day_usd: entry
                .budget_per_day
                .as_deref()
                .and_then(|raw| raw.parse::<rimz::harness::budget::BudgetSpec>().ok())
                .map(|budget| budget.cap_usd),
            workflow_runs: Vec::new(),
            capacity: Vec::new(),
            head,
            continuation,
            reason: String::new(),
            task: Some(task.clone()),
            timing: Some(timing),
            in_progress,
            held: Vec::new(),
            worktree_here: false,
            watcher_pid,
            launched_count: 0,
            quiet: None,
            launch: launch.filter(|_| entry.stay).cloned(),
            exit: None,
        };
        row.set_stats(stats);
        row.refresh_attention(count, now);
        row
    }

    fn refresh_attention(&mut self, count: u32, now: Timestamp) {
        let (attention, reason) = attention(self, count, now);
        self.attention = attention;
        self.reason = reason;
        self.section = if self.attention.is_some() {
            Section::NeedsYou
        } else {
            match self.kind {
                TaskKind::Routine => Section::Routine,
                TaskKind::WakeUp => Section::WakeUps,
                TaskKind::OneOff
                    if self
                        .task
                        .as_ref()
                        .is_some_and(|task| attention::fired(task, self.launch.as_ref())) =>
                {
                    Section::Done
                }
                TaskKind::OneOff => Section::Waiting,
            }
        };
    }

    pub(super) fn subscription_last(
        task: &LoadedTask,
        stats: Option<&run_log::LoopRunStats>,
        now: &jiff::Zoned,
    ) -> String {
        let last = stats
            .and_then(|stats| stats.acting.as_ref())
            .filter(|acting| since_wait_armed(Some(task), acting.record.at))
            .map(LastRun::from);
        let heard = stats
            .and_then(|stats| stats.heard.as_ref())
            .filter(|heard| since_wait_armed(Some(task), heard.at))
            .map(Heard::from);
        let newest = stats
            .and_then(|stats| stats.last.as_ref())
            .filter(|newest| since_wait_armed(Some(task), newest.at));
        let exit = last
            .as_ref()
            .and_then(|last| render::record_exit(&last.record));
        last_run_text(
            last.as_ref(),
            newest,
            heard.as_ref(),
            task.action().ok().map(ActionKind::from),
            exit.as_deref(),
            now.timestamp(),
        )
        .unwrap_or_else(|| "never fired".into())
    }

    /// A task owning a parked run needs you, unless a stronger reason already does.
    fn attach_workflow_runs(&mut self, runs: Vec<WorkflowRunRow>, now: Timestamp) {
        self.workflow_runs = runs;
        if !attention::parked_overrides(self.attention) {
            return;
        }
        let Some((text, age)) = self.parked_reason(now) else {
            return;
        };
        self.attention = Some(Attention::Parked);
        self.section = Section::NeedsYou;
        self.reason = format!("{text} {age}");
    }

    fn parked_reason(&self, now: Timestamp) -> Option<(String, String)> {
        let mut parked = self.workflow_runs.iter().filter(|run| run.reason.is_some());
        let first = parked.next()?;
        let more = parked.count();
        Some(attention::parked_words(first.summary(now), more))
    }

    /// A row for a run in progress no catalog row claims: a held run lock, or a consumed
    /// one-shot's executing run.
    fn rowless(
        name: String,
        in_progress: InProgress<Option<RunLockInfo>>,
        stats: Option<&run_log::LoopRunStats>,
    ) -> Self {
        let held = Vec::new();
        let mut row = Self {
            name,
            section: Section::Done,
            kind: TaskKind::OneOff,
            condition: None,
            attention: None,
            state: TaskState::Running,
            trigger: "one-shot fired".into(),
            label: None,
            action: None,
            dir: None,
            worktree: None,
            fresh_worktree: None,
            owner: None,
            you: false,
            booked_by: Vec::new(),
            next_at: None,
            running_since: running_since(Some(&in_progress)),
            last: None,
            heard: None,
            newest: None,
            spend_today_usd: 0.0,
            budget_per_day_usd: None,
            workflow_runs: Vec::new(),
            capacity: Vec::new(),
            head: "one-shot fired".into(),
            continuation: String::new(),
            reason: String::new(),
            task: None,
            timing: None,
            in_progress: Some(in_progress),
            held,
            worktree_here: false,
            watcher_pid: None,
            launched_count: 0,
            quiet: None,
            launch: None,
            exit: None,
        };
        row.set_stats(stats);
        row
    }

    fn set_stats(&mut self, stats: Option<&run_log::LoopRunStats>) {
        let Some(stats) = stats else {
            return;
        };
        self.spend_today_usd = stats.spend_today_usd;
        self.quiet = stats.quiet.clone();
        self.last = stats
            .acting
            .as_ref()
            .filter(|acting| since_wait_armed(self.task.as_ref(), acting.record.at))
            .map(LastRun::from);
        self.newest = stats
            .last
            .clone()
            .filter(|newest| since_wait_armed(self.task.as_ref(), newest.at));
        self.exit = self
            .last
            .as_ref()
            .and_then(|last| render::record_exit(&last.record));
        self.heard = stats
            .heard
            .as_ref()
            .filter(|heard| since_wait_armed(self.task.as_ref(), heard.at))
            .map(Heard::from);
    }

    fn state_text(&self, now: Timestamp) -> String {
        let count = self.self_clearing_count();
        if self.state == TaskState::Running {
            return render::in_flight_text(
                self.holder(),
                self.running_since,
                &self.held,
                count,
                now,
            );
        }
        if count > 0 {
            return format!("○ {count} held");
        }
        if self.attention == Some(Attention::Held) {
            return self.reason.clone();
        }
        if self.state == TaskState::WaitsForRoom {
            return self.state.label().into();
        }
        let Some(timing) = &self.timing else {
            return "—".into();
        };
        if let Some(text) = TaskState::held_text(&timing.state(), now) {
            return text;
        }
        match timing.state() {
            schedule::TaskTimingState::Due(_) => "due".into(),
            schedule::TaskTimingState::Upcoming(next) => ui::until_label(next, now),
            schedule::TaskTimingState::Listening { .. }
            | schedule::TaskTimingState::Watching { .. } => self.head.clone(),
            state => state.condition_label().unwrap_or_else(|| "—".into()),
        }
    }

    /// The held run lock's holder, when the lock is held and names one.
    fn holder(&self) -> Option<RunLockInfo> {
        match self.in_progress {
            Some(InProgress::Locked(holder)) => holder,
            _ => None,
        }
    }

    fn held_starts(&self) -> impl Iterator<Item = CapacityStart> + '_ {
        self.capacity
            .iter()
            .cloned()
            .chain(self.held.iter().map(|held| CapacityStart {
                checkout: held.checkout.clone(),
                wait: CapacityWaitView::from(held),
            }))
    }

    fn self_clearing_count(&self) -> usize {
        self.held_starts()
            .filter(|start| !start.wait.hold.is_some_and(|kind| kind.needs_fix()))
            .count()
    }

    fn running_agents(&self) -> bool {
        self.state == TaskState::Running
            && render::split_held(self.holder(), &self.held).0.is_none()
    }

    fn last_text(&self, now: Timestamp) -> String {
        let mut parts = vec![match self.state {
            TaskState::Running => self.state_text(now),
            TaskState::Off | TaskState::NotEnabled => self.state.label().into(),
            _ => last_run_text(
                self.last.as_ref(),
                self.newest.as_ref(),
                self.heard.as_ref(),
                self.action.as_ref().map(|action| action.kind),
                self.exit.as_deref(),
                now,
            )
            .unwrap_or_else(|| {
                if !self
                    .task
                    .as_ref()
                    .is_some_and(|task| task.entry().watch.is_some())
                {
                    return "never fired".into();
                }
                if self.watcher_pid.is_none() {
                    return "lost".into();
                }
                let elapsed = self
                    .task
                    .as_ref()
                    .and_then(|task| task.entry().wait_meta.as_ref())
                    .map(|meta| format!(" {}", elapsed(meta.armed_at, now)))
                    .unwrap_or_default();
                format!("watching{elapsed}")
            }),
        }];
        if self
            .action
            .as_ref()
            .is_some_and(|action| action.kind == ActionKind::Wake)
            && let Some(last) = &self.last
            && last.ok
            && last.streak > 1
        {
            parts.push(format!("{} delivered", last.streak));
        }
        if self
            .task
            .as_ref()
            .is_some_and(|task| task.entry().each_worktree)
            && self.launched_count > 0
        {
            parts.push(format!("{} worktrees", self.launched_count));
        }
        if let Some(cap) = self.budget_per_day_usd {
            parts.push(format!(
                "${:.2} / {} today",
                self.spend_today_usd,
                render::format_budget_cap(cap)
            ));
        } else if self.spend_today_usd > 0.0 {
            parts.push(format!("${:.2} today", self.spend_today_usd));
        }
        parts.join(" · ")
    }
}

/// When the running run started: the lock holder's start, or an executing lock-free run's
/// creation.
fn running_since(in_progress: Option<&InProgress<Option<RunLockInfo>>>) -> Option<Timestamp> {
    match in_progress? {
        InProgress::Locked(holder) => holder.map(|holder| holder.started_at),
        InProgress::Run {
            since,
            executing: true,
        } => Some(*since),
        InProgress::Run { .. } => None,
    }
}

fn since_wait_armed(task: Option<&LoadedTask>, at: Timestamp) -> bool {
    attention::since_wait_armed(task.map(LoadedTask::entry), at)
}

fn last_run_text(
    last: Option<&LastRun>,
    newest: Option<&LoopRunRecord>,
    heard: Option<&Heard>,
    action: Option<ActionKind>,
    exit: Option<&str>,
    now: Timestamp,
) -> Option<String> {
    let Some(last) = last else {
        if let Some(newest) = newest {
            return Some(newest_fire_text(newest, now));
        }
        return heard.map(|heard| {
            format!(
                "never fired · heard {} {}",
                heard.signal,
                ui::rel_age(heard.at, now)
            )
        });
    };
    let mut text = acting_run_text(last, exit, now);
    if last.ok && last.streak > 1 && action != Some(ActionKind::Wake) {
        text.push_str(&format!(" · {} in a row", last.streak));
    }
    Some(text)
}

/// A fire whose outcome joins no streak, such as a canceled run.
fn newest_fire_text(newest: &LoopRunRecord, now: Timestamp) -> String {
    format!(
        "{} {} {}",
        render::loop_result_mark(newest.result).glyph,
        newest.result.label(),
        ui::rel_age(newest.at, now)
    )
}

fn acting_run_text(last: &LastRun, exit: Option<&str>, now: Timestamp) -> String {
    if last.ok {
        format!("✓ {}", ui::rel_age(last.at, now))
    } else {
        format!(
            "✗ failed {}{}",
            ui::rel_age(last.at, now),
            exit.map(|exit| format!(" · exit {exit}"))
                .unwrap_or_default()
        )
    }
}

fn launched_count(
    entry: &TaskEntry,
    task: &str,
    workflows: &launch_ledger::WorkflowLedger,
) -> usize {
    if !entry.dedupes_checkout() {
        return 0;
    }
    workflows.tasks.get(task).map_or(0, BTreeMap::len)
}

/// One ledgered or live workflow run, shared by list, show, and checkout joins.
#[derive(Serialize)]
pub(super) struct WorkflowRunRow {
    pub(super) id: rimz::ids::WorkflowRunId,
    #[serde(skip_serializing_if = "Option::is_none")]
    address: Option<String>,
    pub(super) checkout: PathBuf,
    pub(super) state: &'static str,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(super) node: String,
    pub(super) since: Timestamp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) text: Option<String>,
    #[serde(skip)]
    parked_at: Option<Timestamp>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(super) detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    terminal_at: Option<Timestamp>,
    #[serde(skip)]
    pub(super) live: bool,
    #[serde(skip)]
    target: Option<String>,
    #[serde(skip)]
    pub(super) command: Option<String>,
    #[serde(skip)]
    pub(super) evidence: Option<Box<RunEvidence>>,
}

/// A run's sealed graph and journal, read for the loop views' per-fire forensics.
pub(super) struct RunEvidence {
    pub(super) spec: rimz::harness::workflow::Definition,
    pub(super) journal: Vec<rimz::store::workflow::JournalEntry>,
    /// The run's home where it is now, which the journal's recorded paths are relocated into.
    pub(super) paths: rimz::disk::paths::WorkflowPaths,
}

impl WorkflowRunRow {
    pub(super) fn for_show(
        records: &[rimz::store::workflow::RunRecord],
        ended: &[(rimz::ids::WorkflowRunId, rimz::store::workflow::RunHeader)],
        task: &str,
        root: &Path,
        launches: &BTreeMap<PathBuf, launch_ledger::WorkflowLaunchRecord>,
        specs: &BTreeMap<rimz::ids::WorkflowRunId, rimz::harness::workflow::Definition>,
        now: Timestamp,
    ) -> Vec<Self> {
        let mut runs = Vec::new();
        // A resident launch remembered before the cutover names a leader, not a run.
        for (checkout, launch, run) in launches
            .iter()
            .filter_map(|(checkout, launch)| Some((checkout, launch, launch.run.as_ref()?)))
        {
            if runs.iter().any(|listed: &Self| listed.id == *run) {
                continue;
            }
            runs.push(
                records
                    .iter()
                    .find(|record| record.id == *run)
                    .map(|record| Self::of_record(record, specs.get(&record.id), now))
                    .or_else(|| {
                        ended
                            .iter()
                            .find(|(id, _)| id == run)
                            .map(|(id, header)| Self::of_header(id, header, checkout, now))
                    })
                    .unwrap_or_else(|| Self {
                        id: run.clone(),
                        address: None,
                        checkout: checkout.clone(),
                        state: "gone",
                        node: String::new(),
                        since: launch.at,
                        reason: None,
                        text: None,
                        parked_at: None,
                        detail: String::new(),
                        terminal_at: None,
                        live: false,
                        target: None,
                        command: None,
                        evidence: None,
                    }),
            );
        }
        let mut others = records
            .iter()
            .filter(|record| record.ran_for(task) && record.project_root == root)
            .filter(|record| !runs.iter().any(|listed| listed.id == record.id))
            .map(|record| Self::of_record(record, specs.get(&record.id), now))
            .chain(
                ended
                    .iter()
                    .filter(|(_, header)| {
                        header.task.as_deref() == Some(task)
                            && header.project_root.as_deref() == Some(root)
                    })
                    .filter(|(id, _)| !runs.iter().any(|listed| &listed.id == id))
                    .map(|(id, header)| Self::of_header(id, header, root, now)),
            )
            .collect::<Vec<_>>();
        others.sort_by_key(|run| run.finished_at().map(std::cmp::Reverse));
        runs.extend(others);
        runs
    }

    /// When a finished or gone run ended, as far as its row knows; `None` while live.
    pub(super) fn finished_at(&self) -> Option<Timestamp> {
        (!self.live).then(|| self.terminal_at.unwrap_or(self.since))
    }

    pub(super) fn of_task(
        records: &[rimz::store::workflow::RunRecord],
        task: &str,
        root: &Path,
        specs: &BTreeMap<rimz::ids::WorkflowRunId, rimz::harness::workflow::Definition>,
        now: Timestamp,
    ) -> Vec<Self> {
        records
            .iter()
            .filter(|record| record.started_by(task) && record.project_root == root)
            .map(|record| Self::of_record(record, specs.get(&record.id), now))
            .collect()
    }

    pub(super) fn of_record(
        record: &rimz::store::workflow::RunRecord,
        spec: Option<&rimz::harness::workflow::Definition>,
        now: Timestamp,
    ) -> Self {
        let park = record.park.as_ref();
        Self {
            id: record.id.clone(),
            address: Some(record.address().to_string()),
            checkout: record.scope.path.clone(),
            state: record.state.as_str(),
            node: record.node.clone(),
            since: record.entered_at,
            reason: park.map(|park| park.reason.as_str()),
            text: park.and_then(|park| park.text.clone()),
            parked_at: park
                .filter(|_| record.state == rimz::store::workflow::RunState::Parked)
                .map(|park| park.at),
            detail: crate::cli::workflow_view::detail(record, spec, now, None),
            terminal_at: record.terminal_at.filter(|_| record.state.is_terminal()),
            live: !record.state.is_terminal(),
            target: None,
            command: None,
            evidence: None,
        }
    }

    pub(super) fn of_header(
        id: &rimz::ids::WorkflowRunId,
        header: &rimz::store::workflow::RunHeader,
        root: &Path,
        now: Timestamp,
    ) -> Self {
        Self {
            id: id.clone(),
            address: None,
            checkout: header
                .scope_path
                .clone()
                .unwrap_or_else(|| root.to_path_buf()),
            state: header
                .state
                .map_or("unknown", rimz::store::workflow::RunState::as_str),
            node: header.node.clone().unwrap_or_default(),
            since: header.terminal_at.or(header.created_at).unwrap_or(now),
            reason: None,
            text: None,
            parked_at: None,
            detail: String::new(),
            terminal_at: header.terminal_at,
            live: false,
            target: None,
            command: None,
            evidence: None,
        }
    }

    /// Reads the run's sealed graph and journal from its live or finished home; a run whose home
    /// is gone or unreadable keeps none. A live home that moves while it is read is read again in
    /// its finished home, so the graph and the journal come from one home.
    pub(super) fn read_evidence(
        &mut self,
        state: &StatePaths,
        record: &rimz::store::workflow::RunRecord,
    ) {
        let paths = match rimz::store::workflow::find(state, &record.id) {
            Ok(Some(found)) => found.paths,
            _ => state.workflow(&record.id),
        };
        let evidence = |paths: rimz::disk::paths::WorkflowPaths| {
            let spec = rimz::harness::workflow::load(&paths.home)?;
            let journal = rimz::store::workflow::journal(&paths)?;
            anyhow::Ok(RunEvidence {
                spec,
                journal,
                paths,
            })
        };
        let mut read = evidence(paths.clone());
        if !paths.home.exists() {
            read = evidence(state.finished_workflow(&record.id));
        }
        match read {
            Ok(evidence) => self.evidence = Some(Box::new(evidence)),
            Err(error) => {
                tracing::warn!(run = %record.id, %error, "workflow run evidence unreadable")
            }
        }
    }

    /// Names the run as `rimz workflow show` selects it: `target` and its full `command`.
    pub(super) fn named(&mut self, target: String, command: String) {
        self.target = Some(target);
        self.command = Some(command);
    }

    pub(super) fn name(&self) -> &str {
        self.target
            .as_deref()
            .or(self.address.as_deref())
            .unwrap_or(self.id.as_str())
    }

    /// `<address> <node> <reason>[: <text>]` and its age, kept apart so only the text clips.
    pub(super) fn summary(&self, now: Timestamp) -> (String, String) {
        attention::run_summary(
            self.name(),
            &self.node,
            self.reason.unwrap_or(self.state),
            self.text.as_deref(),
            self.parked_at.unwrap_or(self.since),
            now,
        )
    }
}

fn elapsed(at: Timestamp, now: Timestamp) -> String {
    ui::age_label(now.duration_since(at).as_secs().max(0) as u64)
}

fn trigger_text(
    entry: &TaskEntry,
    run_dir: &Path,
    timing: &schedule::TaskTiming,
    state: TaskState,
    now: Timestamp,
) -> (String, String) {
    let mut head = match timing.parsed() {
        Ok(parsed) => match &parsed.trigger {
            Trigger::Signal { selector, matches } => {
                let mut parts = vec![format!("on {selector}")];
                parts.extend(
                    matches
                        .iter()
                        .filter(|(key, value)| {
                            !(key.as_str() == "path" && run_dir == Path::new(value))
                        })
                        .map(|(key, value)| format!("{key}={value}")),
                );
                parts.join(" · ")
            }
            Trigger::Condition { expr, .. } => format!("when {expr}"),
            Trigger::Watch(spec) => match spec {
                WatchSpec::Command(_) => "on command exit".into(),
                WatchSpec::Pid { pid } => format!("on pid {pid} exit"),
                WatchSpec::File { file, .. } => format!("on file {}", checkout_name(file)),
                WatchSpec::Check { .. } => "on check".into(),
            },
            Trigger::Schedule(_) => {
                let mut head = parsed.describe();
                if state == TaskState::WaitsForRoom {
                    head.push_str(&format!(" · {}", state.label()));
                }
                head
            }
        },
        Err(_) => "invalid trigger".into(),
    };
    if let Some(label) = &entry.label {
        head.push_str(&format!(" · {label}"));
    }
    if let ArmState::Paused(until) = timing.arm_state()
        && let Some(text) = TaskState::held_text(&schedule::TaskTimingState::Paused(until), now)
    {
        head.push_str(&format!(" · {text}"));
    }
    let mut continuation = Vec::new();
    if let Some(hold) = &entry.hold {
        continuation.push(format!("for {hold}"));
    }
    if entry.each_worktree {
        continuation.push("each worktree".into());
    }
    (head, continuation.join(" · "))
}

fn attention(row: &TaskRow, count: u32, now: Timestamp) -> (Option<Attention>, String) {
    let starts = row.held_starts().collect::<Vec<_>>();
    let facts = attention::Facts {
        holds: starts
            .iter()
            .map(|start| attention::Hold {
                since: start.wait.since,
                kind: start.wait.hold,
                reason: &start.wait.reason,
            })
            .collect(),
        task: row.task.as_ref().map(|task| attention::TaskFacts {
            task,
            timing: row.timing.as_ref(),
            checkout: row.dir.as_deref(),
            fired: attention::fired(task, row.launch.as_ref()),
            in_progress: row.in_progress.is_some(),
            disabled: matches!(row.state, TaskState::Off | TaskState::NotEnabled),
            last_failed: row.last.as_ref().is_some_and(|last| !last.ok),
            exit: row.exit.as_deref(),
            strikes: count,
            watcher_alive: row.watcher_pid.is_some(),
            you: row.you,
        }),
    };
    match attention::decide(&facts, now) {
        Some((attention, reason)) => (Some(attention), reason),
        None => (None, String::new()),
    }
}
