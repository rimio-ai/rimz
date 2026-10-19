//! Room-scoped loop list model and presentation.

use super::*;
use rimz::config::WatchSpec;
use rimz::harness::schedule::{TaskKind, Trigger, launch_ledger, signal::watcher_info};
use serde::Serialize;

#[path = "list_render.rs"]
mod text;
pub(super) use text::action_text as action;
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
    text::write(&mut ui::out(), &model, &rooms, args.all, width)
}

pub(super) struct ListModel {
    pub(super) rooms: Vec<Room>,
    pub(super) caller_root: Option<PathBuf>,
    pub(super) now: Timestamp,
    pub(super) warnings: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct Room {
    pub(super) root: PathBuf,
    pub(super) open: bool,
    pub(super) here: bool,
    pub(super) spend_today_usd: f64,
    pub(super) tasks: Vec<TaskRow>,
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

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Section {
    NeedsYou,
    Routine,
    Waiting,
    Done,
    WakeUps,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Attention {
    CheckoutGone,
    Strikes,
    Failing,
    Held,
    Blocked,
    Invalid,
    Parked,
    Lost,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum TaskState {
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

    pub(super) fn observe(timing: &schedule::TaskTiming, running: bool, open: bool) -> Self {
        if running {
            return Self::Running;
        }
        match timing.arm_state() {
            ArmState::Disabled(DisabledReason::NotEnabledHere) => Self::NotEnabled,
            ArmState::Disabled(_) => Self::Off,
            ArmState::Paused(_) => Self::Paused,
            ArmState::Live
                if !open
                    && timing
                        .parsed()
                        .is_ok_and(|parsed| matches!(parsed.trigger, Trigger::Schedule(_))) =>
            {
                Self::WaitsForRoom
            }
            ArmState::Live => Self::Live,
        }
    }

    pub(super) fn held_text(state: &schedule::TaskTimingState, now: Timestamp) -> Option<String> {
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
pub(super) struct LastRun {
    pub(super) at: Timestamp,
    result: LoopRunResult,
    pub(super) ok: bool,
    streak: usize,
    #[serde(skip)]
    pub(super) record: LoopRunRecord,
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
pub(super) struct Heard {
    pub(super) signal: String,
    pub(super) at: Timestamp,
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
    pub(super) name: String,
    pub(super) section: Section,
    kind: TaskKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    condition: Option<schedule::when::ConditionCount>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) attention: Option<Attention>,
    pub(super) state: TaskState,
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
    owner: Option<Owner>,
    you: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) next_at: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) running_since: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) last: Option<LastRun>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) heard: Option<Heard>,
    spend_today_usd: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    budget_per_day_usd: Option<f64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) workflow_runs: Vec<WorkflowRunRow>,
    #[serde(skip)]
    head: String,
    #[serde(skip)]
    continuation: String,
    #[serde(skip)]
    reason: String,
    #[serde(skip)]
    pub(super) task: Option<LoadedTask>,
    #[serde(skip)]
    pub(super) timing: Option<schedule::TaskTiming>,
    #[serde(skip)]
    pub(super) running: Option<Option<RunLockInfo>>,
    #[serde(skip)]
    held: Vec<Held>,
    #[serde(skip)]
    worktree_here: bool,
    #[serde(skip)]
    watcher_live: bool,
    #[serde(skip)]
    launched_count: usize,
    #[serde(skip)]
    launch: Option<launch_ledger::LaunchRecord>,
    #[serde(skip)]
    exit: Option<String>,
}

struct RowContext<'a> {
    open: bool,
    stats: Option<&'a run_log::LoopRunStats>,
    count: u32,
    watcher_live: bool,
    launch: Option<&'a launch_ledger::LaunchRecord>,
    now: Timestamp,
}

pub(super) enum LoadScope {
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

pub(super) fn load(
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
    let mut rooms = Vec::new();
    let mut warnings = Vec::new();
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
        let stamps = runtime
            .as_ref()
            .map(schedule::last_stamps)
            .unwrap_or_default();
        let stats = held_stats.stats(&root, &now_zoned);
        let ledger = StatePaths::for_project_root(&root)
            .ok()
            .and_then(|paths| launch_ledger::load(&paths).ok())
            .unwrap_or_default();
        let workflows = StatePaths::for_project_root(&root)
            .ok()
            .and_then(|paths| rimz::store::workflow::list(&paths).ok())
            .unwrap_or_default();
        let mut tasks = Vec::new();
        for (name, task) in catalog
            .visible()
            .iter()
            .filter(|(_, task)| task.entry().resolved_root() == root)
        {
            let running = locks.as_ref().and_then(|locks| match locks.state(name) {
                Ok(RunLockState::Held(holder)) => Some(holder),
                Ok(RunLockState::Available) => None,
                Err(error) => {
                    warnings.push(format!("cannot read the loop run lock of `{name}`, so its active run is not shown: {error:#}"));
                    None
                }
            });
            let timing =
                observe_task_timing(name, task, &stamps, arming.get(&task.key(name)), &now_zoned);
            let count = strikes.get(&task.key(name)).copied().unwrap_or_default();
            let mut row = TaskRow::new(
                name,
                task,
                timing,
                running,
                RowContext {
                    open,
                    stats: stats.get(name),
                    count,
                    watcher_live: runtime.as_ref().is_some_and(|runtime| {
                        watcher_info(runtime, name).ok().flatten().is_some()
                    }),
                    launch: ledger
                        .get(name)
                        .and_then(|launches| launches.get(&task.entry().run_dir())),
                    now,
                },
            );
            row.worktree_here = here
                && workspace.is_some_and(|workspace| match task.entry().worktree.as_deref() {
                    Some(name) => {
                        rimz::worktree::worktree_path(&root, &config.agents.worktree, name)
                            .is_ok_and(|path| path == workspace.worktree_root)
                    }
                    None => task.entry().run_dir() == workspace.worktree_root,
                });
            row.launched_count = ledger.get(name).map_or(0, BTreeMap::len);
            row.attach_workflow_runs(WorkflowRunRow::of_task(&workflows, name, &root), now);
            tasks.push(row);
        }
        if let Some(locks) = locks {
            let names = tasks
                .iter()
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>();
            for (name, holder) in locks.rowless(&names) {
                let history = stats.get(&name);
                tasks.push(TaskRow::rowless(name, holder, history));
            }
        }
        let retired = stats
            .iter()
            .filter_map(|(name, stats)| {
                if tasks.iter().any(|row| &row.name == name)
                    || stats.last_kind.map(|(_, kind)| kind) != Some(TaskKind::OneOff)
                {
                    return None;
                }
                let last = stats.last.as_ref()?;
                Some(Retired {
                    name: name.clone(),
                    at: last.at,
                    result: last.result,
                    checkout: last.checkout.clone(),
                    worktree_here: here
                        && workspace.is_some_and(|workspace| {
                            last.checkout.as_ref() == Some(&workspace.worktree_root)
                        }),
                })
            })
            .collect();
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
    })
}

impl TaskRow {
    fn new(
        name: &str,
        task: &LoadedTask,
        timing: schedule::TaskTiming,
        running: Option<Option<RunLockInfo>>,
        context: RowContext<'_>,
    ) -> Self {
        let RowContext {
            open,
            stats,
            count,
            watcher_live,
            launch,
            now,
        } = context;
        let entry = task.entry();
        let state = TaskState::observe(&timing, running.is_some(), open);
        let (head, continuation) = trigger_text(entry, &timing, state, now);
        let action = task.action().ok().map(|action| Action {
            kind: ActionKind::from(action),
            subject: match action {
                TaskAction::Spawn(subject) | TaskAction::Workflow(subject) => subject.clone(),
                TaskAction::Deliver(target) => target.handle.clone(),
                TaskAction::CheckOnly => "check".into(),
            },
        });
        let dir = entry.dir.as_ref().map(|_| entry.run_dir());
        let worktree = worktree_name(entry);
        let owner = entry
            .team
            .as_ref()
            .map(|team| Owner {
                kind: OwnerKind::Team,
                name: team.to_string(),
            })
            .or_else(|| {
                entry.loop_task.as_ref().map(|name| Owner {
                    kind: OwnerKind::Loop,
                    name: name.clone(),
                })
            });
        let held = if running.is_some() {
            schedule::throttle::held(name, &entry.resolved_root())
        } else {
            Vec::new()
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
            owner,
            you: false,
            next_at: timing.next_timestamp(),
            running_since: running.flatten().map(|holder| holder.started_at),
            last: None,
            heard: None,
            spend_today_usd: 0.0,
            budget_per_day_usd: entry
                .budget_per_day
                .as_deref()
                .and_then(|raw| raw.parse::<rimz::harness::budget::BudgetSpec>().ok())
                .map(|budget| budget.cap_usd),
            workflow_runs: Vec::new(),
            head,
            continuation,
            reason: String::new(),
            task: Some(task.clone()),
            timing: Some(timing),
            running,
            held,
            worktree_here: false,
            watcher_live,
            launched_count: 0,
            launch: launch.filter(|_| entry.stay).cloned(),
            exit: None,
        };
        row.set_stats(stats);
        let (attention, reason) = attention(&row, count, now);
        row.attention = attention;
        row.reason = reason;
        row.section = if row.attention.is_some() {
            Section::NeedsYou
        } else {
            match row.kind {
                TaskKind::Routine => Section::Routine,
                TaskKind::WakeUp => Section::WakeUps,
                TaskKind::OneOff if entry.stay && row.launch.is_some() => Section::Done,
                TaskKind::OneOff => Section::Waiting,
            }
        };
        row
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
        let exit = last
            .as_ref()
            .and_then(|last| render::record_exit(&last.record));
        last_run_text(
            last.as_ref(),
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
        if self
            .attention
            .is_some_and(|attention| attention != Attention::Lost)
        {
            return;
        }
        let mut parked = self.workflow_runs.iter().filter(|run| run.reason.is_some());
        let Some(first) = parked.next() else {
            return;
        };
        let more = parked.count();
        self.attention = Some(Attention::Parked);
        self.section = Section::NeedsYou;
        self.reason = format!(
            "{} run(s) parked: {}{}",
            more + 1,
            first.summary(now),
            if more > 0 {
                format!(" (+{more} more)")
            } else {
                String::new()
            }
        );
    }

    fn rowless(
        name: String,
        holder: Option<RunLockInfo>,
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
            owner: None,
            you: false,
            next_at: None,
            running_since: holder.map(|holder| holder.started_at),
            last: None,
            heard: None,
            spend_today_usd: 0.0,
            budget_per_day_usd: None,
            workflow_runs: Vec::new(),
            head: "one-shot fired".into(),
            continuation: String::new(),
            reason: String::new(),
            task: None,
            timing: None,
            running: Some(holder),
            held,
            worktree_here: false,
            watcher_live: false,
            launched_count: 0,
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
        self.last = stats
            .acting
            .as_ref()
            .filter(|acting| since_wait_armed(self.task.as_ref(), acting.record.at))
            .map(LastRun::from);
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

    pub(super) fn state_text(&self, now: Timestamp) -> String {
        if self.state == TaskState::Running {
            return render::in_flight_text(self.running.flatten(), &self.held, now);
        }
        if matches!(
            self.attention,
            Some(
                Attention::CheckoutGone
                    | Attention::Strikes
                    | Attention::Blocked
                    | Attention::Invalid
                    | Attention::Parked
            )
        ) {
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

    pub(super) fn last_text(&self, now: Timestamp) -> String {
        let mut parts = vec![match self.state {
            TaskState::Running => self.state_text(now),
            TaskState::Off | TaskState::NotEnabled => self.state.label().into(),
            _ => last_run_text(
                self.last.as_ref(),
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
                if !self.watcher_live {
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

fn since_wait_armed(task: Option<&LoadedTask>, at: Timestamp) -> bool {
    task.and_then(|task| task.entry().wait_meta.as_ref())
        .is_none_or(|meta| at >= meta.armed_at)
}

fn last_run_text(
    last: Option<&LastRun>,
    heard: Option<&Heard>,
    action: Option<ActionKind>,
    exit: Option<&str>,
    now: Timestamp,
) -> Option<String> {
    let Some(last) = last else {
        return heard.map(|heard| {
            format!(
                "never fired · heard {} {}",
                heard.signal,
                ui::rel_age(heard.at, now)
            )
        });
    };
    Some(if last.ok {
        let mut text = format!("✓ {}", ui::rel_age(last.at, now));
        if last.streak > 1 && action != Some(ActionKind::Wake) {
            text.push_str(&format!(" · {} in a row", last.streak));
        }
        text
    } else {
        format!(
            "✗ failed {}{}",
            ui::rel_age(last.at, now),
            exit.map(|exit| format!(" · exit {exit}"))
                .unwrap_or_default()
        )
    })
}

/// One nonterminal workflow run a loop task started.
#[derive(Serialize)]
pub(super) struct WorkflowRunRow {
    pub(super) id: rimz::ids::WorkflowRunId,
    pub(super) checkout: PathBuf,
    pub(super) state: &'static str,
    pub(super) node: String,
    pub(super) since: Timestamp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) text: Option<String>,
    #[serde(skip)]
    workflow: String,
    #[serde(skip)]
    pub(super) detail: String,
}

impl WorkflowRunRow {
    pub(super) fn of_task(
        records: &[rimz::store::workflow::RunRecord],
        task: &str,
        root: &Path,
    ) -> Vec<Self> {
        records
            .iter()
            .filter(|record| record.started_by(task) && record.project_root == root)
            .map(|record| Self {
                id: record.id.clone(),
                checkout: record.scope.path.clone(),
                state: record.state.as_str(),
                node: record.node.clone(),
                since: record.entered_at,
                reason: record.park.as_ref().map(|park| park.reason.as_str()),
                text: record.park.as_ref().and_then(|park| park.text.clone()),
                workflow: record.workflow.clone(),
                detail: crate::cli::workflow::waiting(record)
                    .or_else(|| record.park.as_ref().map(rimz::store::workflow::Park::label))
                    .unwrap_or_default(),
            })
            .collect()
    }

    /// `<workflow> <node> <reason>[: <text>] <age> <checkout-name>`.
    pub(super) fn summary(&self, now: Timestamp) -> String {
        let text = self
            .text
            .as_ref()
            .map(|text| format!(": {text}"))
            .unwrap_or_default();
        format!(
            "{} {} {}{text} {} {}",
            self.workflow,
            self.node,
            self.reason.unwrap_or(self.state),
            elapsed(self.since, now),
            checkout_name(&self.checkout)
        )
    }
}

pub(super) fn worktree_name(entry: &TaskEntry) -> Option<String> {
    if let Some(name) = &entry.worktree {
        return Some(name.clone());
    }
    (entry.wait.is_some() || entry.dir.is_some()).then(|| {
        entry
            .dir
            .as_ref()
            .map(|_| checkout_name(&entry.run_dir()))
            .or_else(|| {
                entry.wait.as_ref().and_then(|target| {
                    target
                        .handle
                        .split_once('#')
                        .map(|(_, channel)| channel.to_owned())
                })
            })
            .unwrap_or_else(|| checkout_name(&entry.resolved_root()))
    })
}

fn checkout_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}
fn elapsed(at: Timestamp, now: Timestamp) -> String {
    ui::age_label(now.duration_since(at).as_secs().max(0) as u64)
}

fn trigger_text(
    entry: &TaskEntry,
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
                            !(key.as_str() == "path" && entry.run_dir() == Path::new(value))
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
    let Some(task) = &row.task else {
        return (None, String::new());
    };
    if let Some(dir) = &row.dir
        && !dir.is_dir()
    {
        return (
            Some(Attention::CheckoutGone),
            format!("checkout {} is gone", checkout_name(dir)),
        );
    }
    if let Some(state) = task.source().blocked_state() {
        return (
            Some(Attention::Blocked),
            format!("blocked · project {}", state.as_str()),
        );
    }
    if let Err(error) = task.trigger() {
        return (Some(Attention::Invalid), format!("invalid: {error}"));
    }
    if let Err(error) = task.action() {
        return (Some(Attention::Invalid), format!("invalid: {error}"));
    }
    if let Some(timing) = &row.timing
        && let ArmState::Disabled(DisabledReason::Strikes(strikes)) = timing.arm_state()
    {
        return (
            Some(Attention::Strikes),
            format!("disabled after {strikes} strikes"),
        );
    }
    if let (Some(holder), others) = render::split_held(row.running.flatten(), &row.held) {
        let at = Timestamp::from_millisecond(i64::try_from(holder.since_ms).unwrap_or(i64::MAX))
            .unwrap_or(now);
        let others = render::others_held_text(others)
            .map(|others| format!(" · {others}"))
            .unwrap_or_default();
        return (
            Some(Attention::Held),
            format!(
                "held: {} · {}{others}",
                holder.reason.as_deref().unwrap_or("waiting for its turn"),
                elapsed(at, now)
            ),
        );
    }
    if row.running.is_none()
        && !matches!(row.state, TaskState::Off | TaskState::NotEnabled)
        && row.last.as_ref().is_some_and(|last| !last.ok)
    {
        let strikes = strikes::threshold(task.entry())
            .map(|max| format!(" · {count}/{max} strikes"))
            .unwrap_or_default();
        let exit = row
            .exit
            .as_ref()
            .map(|exit| format!(" · exit {exit}"))
            .unwrap_or_default();
        return (
            Some(Attention::Failing),
            format!("✗ failing{strikes}{exit}"),
        );
    }
    if task.entry().watch.is_some() && !row.watcher_live {
        let wake = task
            .entry()
            .wait
            .as_ref()
            .map(|target| {
                format!(
                    " · will not wake {}",
                    action(
                        TaskActionKind::Deliver,
                        &target.handle,
                        Some(task.entry()),
                        row.you
                    )
                )
            })
            .unwrap_or_default();
        return (Some(Attention::Lost), format!("watcher gone{wake}"));
    }
    (None, String::new())
}
