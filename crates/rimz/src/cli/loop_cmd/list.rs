//! Room-scoped loop list model and presentation.

use super::*;
use rimz::config::WatchSpec;
use rimz::harness::schedule::{Trigger, launch_ledger, signal::watcher_info};
use serde::Serialize;

#[path = "list_render.rs"]
mod text;
#[cfg(test)]
use text::wrap_trigger;

#[cfg(test)]
#[path = "list_tests.rs"]
mod tests;

pub(super) fn run(args: ListArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone()).ok();
    let model = load(workspace.as_ref())?;
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
    text::write(&mut ui::out(), &model, &rooms, all, width)
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
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Section {
    NeedsYou,
    Room,
    Worktrees,
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
}

#[derive(PartialEq, Eq, Serialize)]
struct Action {
    kind: &'static str,
    subject: String,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
struct Owner {
    kind: &'static str,
    name: String,
}

#[derive(Serialize)]
struct LastRun {
    at: Timestamp,
    result: LoopRunResult,
    ok: bool,
    streak: usize,
}

#[derive(Serialize)]
struct Heard {
    signal: String,
    at: Timestamp,
}

#[derive(Serialize)]
pub(super) struct TaskRow {
    pub(super) name: String,
    pub(super) section: Section,
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
    last: Option<LastRun>,
    #[serde(skip_serializing_if = "Option::is_none")]
    heard: Option<Heard>,
    spend_today_usd: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    budget_per_day_usd: Option<f64>,
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
    exit: Option<String>,
}

struct RowContext<'a> {
    open: bool,
    stats: Option<&'a run_log::LoopRunStats>,
    count: u32,
    now: Timestamp,
}

pub(super) fn load(workspace: Option<&rimz::ResolvedWorkspace>) -> Result<ListModel> {
    let caller_root = workspace.map(|workspace| workspace.project_root.clone());
    let caller = workspace
        .and_then(|workspace| super::super::open_existing_store(workspace).ok().flatten())
        .and_then(|store| super::super::send::resolve_caller(&store).ok().flatten())
        .or_else(rimz::harness::ancestry::CallerIdentity::from_env);
    let machine = TaskCatalog::load(None)?;
    let mut roots = schedule::catalog::workspace_instance_roots();
    roots.extend(
        machine
            .visible()
            .values()
            .map(|task| task.entry().resolved_root()),
    );
    roots.extend(caller_root.iter().cloned());
    let arming = arming::load();
    let strikes = strikes::load();
    let now = Timestamp::now();
    let now_zoned = now.to_zoned(MachineConfig::load_lenient().time_zone());
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
        let stats = run_log::stats(&rimz::disk::paths::logs_dir(), &now_zoned, Some(&root));
        let ledger = StatePaths::for_project_root(&root)
            .ok()
            .and_then(|paths| launch_ledger::load(&paths).ok())
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
                    now,
                },
            );
            row.you =
                task.entry()
                    .wait
                    .as_ref()
                    .zip(caller.as_ref())
                    .is_some_and(|(target, caller)| {
                        caller.kind == target.kind
                            && caller.launch_id.as_ref() == Some(&target.session)
                    });
            row.worktree_here = here
                && workspace
                    .is_some_and(|workspace| task.entry().run_dir() == workspace.worktree_root);
            row.launched_count = ledger.get(name).map_or(0, BTreeMap::len);
            row.watcher_live = runtime
                .as_ref()
                .is_some_and(|runtime| watcher_info(runtime, name).ok().flatten().is_some());
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
        rooms.push(Room {
            root,
            open,
            here,
            spend_today_usd: stats.values().map(|stats| stats.spend_today_usd).sum(),
            tasks,
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
            now,
        } = context;
        let entry = task.entry();
        let state = TaskState::observe(&timing, running.is_some(), open);
        let (head, continuation) = trigger_text(entry, &timing, state, now);
        let action = task.action().ok().map(|action| match action {
            TaskAction::Spawn(subject) => Action {
                kind: "start",
                subject: subject.clone(),
            },
            TaskAction::Deliver(target) => Action {
                kind: "wake",
                subject: target.handle.clone(),
            },
            _ => Action {
                kind: "check",
                subject: "check".into(),
            },
        });
        let dir = entry.dir.clone();
        let worktree = (entry.wait.is_some() || dir.is_some()).then(|| {
            dir.as_deref()
                .map(checkout_name)
                .or_else(|| {
                    entry.wait.as_ref().and_then(|target| {
                        target
                            .handle
                            .split_once('#')
                            .map(|(_, channel)| channel.to_owned())
                    })
                })
                .unwrap_or_else(|| checkout_name(&entry.resolved_root()))
        });
        let owner = entry
            .team
            .as_ref()
            .map(|team| Owner {
                kind: "team",
                name: team.to_string(),
            })
            .or_else(|| {
                entry.loop_task.as_ref().map(|name| Owner {
                    kind: "loop",
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
            section: Section::Room,
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
            head,
            continuation,
            reason: String::new(),
            task: Some(task.clone()),
            timing: Some(timing),
            running,
            held,
            worktree_here: false,
            watcher_live: false,
            launched_count: 0,
            exit: None,
        };
        row.set_stats(stats);
        let (attention, reason) = attention(&row, count, now);
        row.attention = attention;
        row.reason = reason;
        row.section = if row.attention.is_some() {
            Section::NeedsYou
        } else if row.worktree.is_some() {
            Section::Worktrees
        } else {
            Section::Room
        };
        row
    }

    fn rowless(
        name: String,
        holder: Option<RunLockInfo>,
        stats: Option<&run_log::LoopRunStats>,
    ) -> Self {
        let held = Vec::new();
        let mut row = Self {
            name,
            section: Section::Room,
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
        self.last = stats.acting.as_ref().map(|acting| LastRun {
            at: acting.record.at,
            result: acting.record.result,
            ok: acting.polarity == run_log::RunPolarity::Good,
            streak: acting.streak,
        });
        self.exit = stats
            .acting
            .as_ref()
            .and_then(|acting| render::record_exit(&acting.record));
        self.heard = stats.heard.as_ref().map(|heard| Heard {
            signal: heard.signal.to_string(),
            at: heard.at,
        });
    }

    pub(super) fn last_text(&self, now: Timestamp) -> String {
        let mut parts = vec![match self.state {
            TaskState::Running => render::in_flight_text(self.running.flatten(), &self.held, now),
            TaskState::Off => "off".into(),
            TaskState::NotEnabled => "off · repo task, enable here to run".into(),
            _ => match &self.last {
                Some(last) if last.ok => {
                    let mut text = format!("✓ {}", ui::rel_age(last.at, now));
                    if last.streak > 1
                        && self
                            .action
                            .as_ref()
                            .is_none_or(|action| action.kind != "wake")
                    {
                        text.push_str(&format!(" · {} in a row", last.streak));
                    }
                    text
                }
                Some(last) => format!(
                    "✗ failed {}{}",
                    ui::rel_age(last.at, now),
                    self.exit
                        .as_ref()
                        .map(|exit| format!(" · exit {exit}"))
                        .unwrap_or_default()
                ),
                None => match &self.heard {
                    Some(heard) => format!("heard {} {}", heard.signal, ui::rel_age(heard.at, now)),
                    None if self
                        .task
                        .as_ref()
                        .is_some_and(|task| task.entry().watch.is_some()) =>
                    {
                        if self.watcher_live {
                            let elapsed = self
                                .task
                                .as_ref()
                                .and_then(|task| task.entry().wait_meta.as_ref())
                                .map(|meta| format!(" {}", elapsed(meta.armed_at, now)))
                                .unwrap_or_default();
                            format!("watching{elapsed}")
                        } else {
                            "lost".into()
                        }
                    }
                    None => "never fired".into(),
                },
            },
        }];
        if self
            .action
            .as_ref()
            .is_some_and(|action| action.kind == "wake")
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
                    head.push_str(" · waits for room");
                } else if let Some(next) = timing.next_timestamp() {
                    head.push_str(&format!(" · next {}", ui::rel_until(next, now)));
                }
                head
            }
        },
        Err(_) => "invalid trigger".into(),
    };
    if let Some(label) = &entry.label {
        head.push_str(&format!(" · {label}"));
    }
    if let ArmState::Paused(until) = timing.arm_state() {
        head.push_str(&format!(" · paused, resumes {}", ui::rel_until(until, now)));
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
    if row.running.is_none() && row.last.as_ref().is_some_and(|last| !last.ok) {
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
    (None, String::new())
}
