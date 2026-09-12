//! Shared construction and lifecycle of session-pinned delivery tasks.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::time::Duration;

use jiff::Timestamp;

use super::catalog::{LoadedTask, TaskSource};
use super::signal::SignalSelector;
use super::{ParsedSchedule, Schedule, ScheduleErr};
use crate::agents::AgentState;
use crate::config::{CheckOn, TaskEntry, TaskTarget, WaitMeta};
use crate::ids::TeamInstanceId;
use crate::workspace::ResolvedWorkspace;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskName(String);

impl FromStr for TaskName {
    type Err = ScheduleErr;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        super::validate_name(value)?;
        Ok(Self(value.to_owned()))
    }
}

impl std::fmt::Display for TaskName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub enum DeliveryName {
    MintWait,
    Named(TaskName),
}

pub enum SubscriptionLifetime {
    Once,
    Standing,
}

pub enum DeliveryTrigger {
    Delay(Duration),
    Pid {
        pid: u32,
        timeout: Duration,
    },
    Watch {
        command: String,
        on: CheckOn,
        timeout: Duration,
    },
    Signal {
        selector: SignalSelector,
        matches: BTreeMap<String, String>,
        lifetime: SubscriptionLifetime,
    },
    Clock(ParsedSchedule),
}

pub enum DeliveryPrompt {
    None,
    Inline(String),
    File(PathBuf),
}

pub enum DeliveryProvenance {
    SelfWait,
    Loop,
    Team(TeamInstanceId),
}

pub struct DeliveryCheck {
    pub command: String,
    pub on: Option<CheckOn>,
    pub timeout: Option<String>,
}

pub struct DeliverySurplus {
    pub ratio: Option<String>,
    pub after: Option<String>,
}

pub struct DeliverySpec {
    pub name: DeliveryName,
    pub target: TaskTarget,
    pub trigger: DeliveryTrigger,
    pub prompt: DeliveryPrompt,
    pub provenance: DeliveryProvenance,
    pub check: Option<DeliveryCheck>,
    pub deadline: Option<Timestamp>,
    pub max_strikes: Option<u32>,
    pub surplus: Option<DeliverySurplus>,
}

pub enum ArmOutcome {
    Armed { name: String, task: Box<LoadedTask> },
    AlreadySubscribed { name: String },
}

#[derive(Debug, thiserror::Error)]
pub enum ArmFailure {
    #[error(transparent)]
    Schedule(#[from] ScheduleErr),
    #[error("{0}")]
    InvalidProvenance(&'static str),
    #[error("updating delivery task state: {0}")]
    State(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("starting watched command: {0}")]
    Watcher(#[source] std::io::Error),
    #[error("{0}")]
    Scope(#[from] DeliveryScopeFailure),
    #[error("loop task `{0}` is configuration-owned; rename it or choose another delivery name")]
    ConfigOwned(String),
}

pub fn arm_delivery(
    workspace: &ResolvedWorkspace,
    spec: DeliverySpec,
) -> Result<ArmOutcome, ArmFailure> {
    let (name, task) = build_entry(workspace, spec, Timestamp::now())?;
    let entry = task.entry();
    let catalog = super::catalog::TaskCatalog::load(Some(&workspace.project_root))
        .map_err(|err| ArmFailure::State(err.into()))?;
    let name = match &name {
        DeliveryName::MintWait => None,
        DeliveryName::Named(name) => Some(name.0.as_str()),
    };
    if let Some(name) = name
        && catalog.visible().get(name).is_some_and(|task| {
            matches!(task.source(), super::catalog::TaskSource::Project { .. })
                || (entry.team.is_some() && task.source() == super::catalog::TaskSource::Config)
        })
    {
        return Err(ArmFailure::ConfigOwned(name.to_owned()));
    }
    let paths = crate::disk::paths::StatePaths::for_workspace(workspace.workspace_id.clone())
        .map_err(|err| ArmFailure::State(Box::new(err)))?;
    let taken = catalog
        .visible()
        .iter()
        .filter(|(_, task)| task.source() != super::catalog::TaskSource::Instance)
        .map(|(name, _)| name.clone())
        .collect();
    let (name, duplicate) = super::instances::insert_delivery(&paths.root, name, entry, &taken)
        .map_err(|err| ArmFailure::State(Box::new(err)))?;
    if duplicate {
        return Ok(ArmOutcome::AlreadySubscribed { name });
    }
    if entry.team.is_none() {
        super::config_edit::remove(super::config_edit::TaskStore::Machine, &name)
            .map_err(|err| ArmFailure::State(err.into()))?;
    }
    if entry.watch.is_some() {
        let spawn = || -> std::io::Result<()> {
            paths.ensure_tmp_dir().map_err(std::io::Error::other)?;
            let path = super::signal::wait_output_path(&paths, &name);
            let output = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(path)?;
            let mut command = Command::new(crate::proc::rimz_exe());
            command
                .arg("--root")
                .arg(&workspace.project_root)
                .args(["wait", "watch", &name])
                .current_dir(&workspace.project_root)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::from(output))
                .process_group(0);
            crate::child_process::spawn_detached_reaped(&mut command, "wait-watch")?;
            Ok(())
        };
        if let Err(error) = spawn() {
            super::instances::remove(&paths.root, &name, Some(entry))
                .map_err(|err| ArmFailure::State(Box::new(err)))?;
            return Err(ArmFailure::Watcher(error));
        }
    }
    Ok(ArmOutcome::Armed {
        name,
        task: Box::new(task),
    })
}

#[derive(Debug, thiserror::Error)]
#[error("retiring session deliveries: {0}")]
pub struct RetireFailure(String);

pub fn retire_session(
    workspace: &ResolvedWorkspace,
    kind: &crate::ids::AgentKind,
    session: &crate::ids::AgentSessionId,
) -> Result<usize, RetireFailure> {
    let paths = crate::disk::paths::StatePaths::for_workspace(workspace.workspace_id.clone())
        .map_err(|err| RetireFailure(err.to_string()))?;
    let names = super::instances::retire_session(&paths.root, kind, session)
        .map_err(|err| RetireFailure(err.to_string()))?;
    let runtime = crate::disk::paths::RuntimePaths::for_workspace(workspace.workspace_id.clone())
        .map_err(|err| RetireFailure(err.to_string()))?;
    let mut failures = Vec::new();
    for name in &names {
        let key = super::arming::TaskKey::for_task(
            name,
            super::catalog::TaskSource::Instance,
            &workspace.project_root,
        );
        if let Err(err) = super::signal::stop_watcher(&runtime, name) {
            failures.push(format!("{name}: {err}"));
        }
        if let Err(err) = super::arming::remove(&key) {
            failures.push(format!("{name}: {err}"));
        }
        if let Err(err) = super::strikes::clear(&key) {
            failures.push(format!("{name}: {err}"));
        }
    }
    if !failures.is_empty() {
        return Err(RetireFailure(failures.join("; ")));
    }
    Ok(names.len())
}

#[derive(Debug, thiserror::Error)]
pub enum DeliveryScopeFailure {
    #[error(
        "CI on the root checkout is not watched: RimZ polls the forge for worktree branches. Pass --match branch=<name> or --match path=<worktree-path>, or watch it with: rimz wait -- gh run watch --exit-status"
    )]
    RootCheckout,
    #[error("team.* waits need a team member; pass --match instance=<team#channel>")]
    NoTeam,
    #[error(
        "--wait on an agent.* signal requires --match handle=<other> or --match session=<other> to avoid waking the target from its own lifecycle signal"
    )]
    SelfSignal,
}

pub fn default_signal_matches(
    workspace: &ResolvedWorkspace,
    agents: &[AgentState],
    scope: &AgentState,
    selector: &SignalSelector,
    matches: &mut BTreeMap<String, String>,
) -> Result<(), DeliveryScopeFailure> {
    match selector.family() {
        "ci" | "pr" if !matches.contains_key("path") && !matches.contains_key("branch") => {
            let path = scope
                .worktree_path
                .as_deref()
                .map(std::path::Path::new)
                .unwrap_or(&workspace.worktree_root);
            if path == workspace.project_root {
                return Err(DeliveryScopeFailure::RootCheckout);
            }
            matches.insert("path".to_owned(), path.display().to_string());
        }
        "team" if !matches.contains_key("team") && !matches.contains_key("instance") => {
            let cohort = crate::address::team_cohorts(agents)
                .into_iter()
                .find(|cohort| {
                    cohort.members.iter().any(|member| {
                        member.kind == scope.kind && member.agent_id == scope.agent_id
                    })
                })
                .ok_or(DeliveryScopeFailure::NoTeam)?;
            matches.insert(
                "instance".to_owned(),
                format!("{}#{}", cohort.team, cohort.channel),
            );
        }
        _ => {}
    }
    Ok(())
}

pub fn validate_self_signal(
    selector: &SignalSelector,
    matches: &BTreeMap<String, String>,
    target: &TaskTarget,
) -> Result<(), DeliveryScopeFailure> {
    if selector.family() != "agent" {
        return Ok(());
    }
    fn handle_name(handle: &str) -> &str {
        handle.split_once('#').map_or(handle, |(name, _)| name)
    }
    let other = matches.get("handle").is_some_and(|handle| {
        !handle.is_empty() && handle_name(handle) != handle_name(&target.handle)
    }) || matches
        .get("session")
        .is_some_and(|session| !session.is_empty() && session != &target.session);
    if other {
        return Ok(());
    }
    Err(DeliveryScopeFailure::SelfSignal)
}

fn build_entry(
    workspace: &ResolvedWorkspace,
    spec: DeliverySpec,
    now: Timestamp,
) -> Result<(DeliveryName, LoadedTask), ArmFailure> {
    let self_wait = matches!(spec.provenance, DeliveryProvenance::SelfWait);
    if self_wait
        && (!matches!(
            spec.trigger,
            DeliveryTrigger::Delay(_) | DeliveryTrigger::Pid { .. } | DeliveryTrigger::Watch { .. }
        ) || !matches!(spec.prompt, DeliveryPrompt::None)
            || spec.check.is_some()
            || spec.surplus.is_some()
            || spec.deadline.is_some())
    {
        return Err(ArmFailure::InvalidProvenance(
            "self waits require a timer or watched command without a prompt or guard",
        ));
    }
    if matches!(spec.provenance, DeliveryProvenance::Team(_))
        && !matches!(
            spec.trigger,
            DeliveryTrigger::Signal {
                lifetime: SubscriptionLifetime::Standing,
                ..
            }
        )
    {
        return Err(ArmFailure::InvalidProvenance(
            "team bindings require a standing signal subscription",
        ));
    }
    let mut entry = TaskEntry {
        wait: Some(spec.target),
        root: workspace.project_root.clone(),
        dir: (workspace.worktree_root != workspace.project_root)
            .then(|| workspace.worktree_root.clone()),
        deadline: spec.deadline,
        max_strikes: spec.max_strikes,
        ..TaskEntry::default()
    };
    if let DeliveryProvenance::Team(instance) = spec.provenance {
        entry.team = Some(instance);
    }
    match spec.prompt {
        DeliveryPrompt::None => {}
        DeliveryPrompt::Inline(prompt) => entry.prompt = Some(prompt),
        DeliveryPrompt::File(path) => entry.prompt_file = Some(path),
    }
    if let Some(check) = spec.check {
        entry.check = Some(check.command);
        entry.on = check.on;
        entry.timeout = check.timeout;
    }
    if let Some(surplus) = spec.surplus {
        entry.surplus = surplus.ratio;
        entry.surplus_after = surplus.after;
    }
    let (delay, pid) = match spec.trigger {
        DeliveryTrigger::Delay(delay) => {
            entry.at = Some(super::delayed_at(delay).map_err(|err| ArmFailure::State(err.into()))?);
            (Some(duration_label(delay)), None)
        }
        DeliveryTrigger::Pid { pid, timeout } => {
            entry.watch = Some(pid_watch_command(pid));
            entry.on = Some(CheckOn::Any);
            entry.timeout = Some(duration_label(timeout));
            (None, Some(pid))
        }
        DeliveryTrigger::Watch {
            command,
            on,
            timeout,
        } => {
            entry.watch = Some(command);
            entry.on = Some(on);
            entry.timeout = Some(duration_label(timeout));
            (None, None)
        }
        DeliveryTrigger::Signal {
            selector,
            matches,
            lifetime,
        } => {
            validate_self_signal(
                &selector,
                &matches,
                entry.wait.as_ref().expect("delivery target was set above"),
            )?;
            entry.signal = Some(selector.to_string());
            entry.matches = (!matches.is_empty()).then_some(matches);
            entry.once = matches!(lifetime, SubscriptionLifetime::Once).then_some(true);
            (None, None)
        }
        DeliveryTrigger::Clock(parsed) => {
            match parsed.schedule {
                Schedule::RawCron(cron) => entry.cron = Some(cron),
                Schedule::Interval(interval) => {
                    entry.every = Some(format!("{}m", interval.minutes))
                }
                Schedule::Calendar(calendar) => {
                    entry.at = Some(format!("{:02}:{:02}", calendar.hour, calendar.minute));
                    if !parsed.once {
                        entry.every = Some(if calendar.weekdays.is_empty() {
                            "day".to_owned()
                        } else {
                            calendar
                                .weekdays
                                .iter()
                                .map(|day| super::short_day_name(*day))
                                .collect::<Vec<_>>()
                                .join(",")
                        });
                    }
                }
            }
            (None, None)
        }
    };
    if self_wait {
        entry.wait_meta = Some(WaitMeta {
            armed_at: now,
            delay,
            pid,
        });
    }
    let name = match &spec.name {
        DeliveryName::MintWait => "wait",
        DeliveryName::Named(name) => &name.0,
    };
    let task = LoadedTask::new(name, entry, TaskSource::Instance);
    task.trigger().as_ref().map_err(Clone::clone)?;
    Ok((spec.name, task))
}

fn pid_watch_command(pid: u32) -> String {
    format!("while kill -0 {pid} 2>/dev/null; do sleep 1; done")
}

pub fn duration_label(duration: Duration) -> String {
    let seconds = duration.as_secs();
    for (unit, suffix) in [(86_400, "d"), (3_600, "h"), (60, "m")] {
        if seconds >= unit && seconds.is_multiple_of(unit) {
            return format!("{}{suffix}", seconds / unit);
        }
    }
    format!("{seconds}s")
}
