//! Home-wide machine admission and workflow/step start limits.
//!
//! Blocking runs keep a ticket; condition fires try once and leave none when held.
//! Machine turns release on a provider report or ceiling. Workflow-scoped turns
//! wait for their agent row or their own ceiling, independently of machine
//! scope. Manual limited starts claim without waiting and stamp stagger at launch.
//! A later scanner releases each scope, so the launching process never lingers.
//! Admission samples limits under the queue lock; capacity scripts run off-lock.
//! Inspection reads the same limits without taking a turn.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::agents::{AgentState, AgentStatus};
use crate::config::{MachineConfig, TaskEntry, ThrottleConfig, WorktreeConfig};
use crate::disk::atomic::AtomicErr;
use crate::disk::lock::{LockErr, WorkspaceLock};
use crate::disk::paths::{self, RuntimePaths, StatePaths};
use crate::ids::{AgentKind, AgentSessionId, HoldKind, LoginName, WorkspaceId};
use crate::proc::pressure::{Pressure, Resource};
use crate::utils::size::decimal_bytes;
use crate::utils::time::format_duration_compact;

mod limits;
pub(in crate::harness) use limits::WorkflowStart;

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[schemars(deny_unknown_fields)]
pub struct Limit {
    #[schemars(range(min = 1))]
    pub max_parallel: Option<u32>,
    pub stagger: Option<String>,
}

impl Limit {
    pub(in crate::harness) fn stagger(
        &self,
    ) -> Result<Option<std::time::Duration>, crate::utils::time::TimeInputErr> {
        self.stagger
            .as_deref()
            .map(crate::harness::duration::duration)
            .transpose()
    }
}

const SAMPLE_EVERY_MS: u64 = 5_000;
const CHECK_REUSE_MS: u64 = 60_000;
const TURN_CEILING: Duration = Duration::from_secs(10);
const CHECK_CEILING: Duration = Duration::from_secs(120);
const NAMED_AGENTS: usize = 2;

/// Why the throttle could not decide or record a turn.
#[derive(Debug, thiserror::Error)]
pub enum ThrottleError {
    /// A configured limit this machine cannot read, with the fix.
    #[error("{0}")]
    Unreadable(String),
    #[error("this start opens {agents} agents, more than {key} = {cap} allows; {fix}")]
    Oversized {
        agents: usize,
        key: &'static str,
        cap: u32,
        fix: String,
    },
    #[error("reading {reading}: {detail}")]
    Reading {
        reading: &'static str,
        detail: String,
    },
    #[error(transparent)]
    Lock(#[from] LockErr),
    #[error("reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("loop throttle ticket {path} is corrupt ({source})")]
    CorruptTicket {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("writing a loop throttle ticket: {0}")]
    WriteTicket(#[source] AtomicErr),
    #[error("writing launch stagger: {0}")]
    WriteStamps(#[source] AtomicErr),
    #[error("loop throttle ticket {0} vanished from the queue")]
    Vanished(PathBuf),
}

type Result<T, E = ThrottleError> = std::result::Result<T, E>;

impl Resource {
    const ALL: [Self; 3] = [Self::Cpu, Self::Io, Self::Memory];

    const fn label(self) -> &'static str {
        match self {
            Self::Cpu => "cpu pressure",
            Self::Io => "io pressure",
            Self::Memory => "memory pressure",
        }
    }

    fn limit(self, config: &ThrottleConfig) -> Option<u8> {
        match self {
            Self::Cpu => config.cpu_pressure,
            Self::Io => config.io_pressure,
            Self::Memory => config.memory_pressure,
        }
    }
}

impl Pressure {
    fn worst(self) -> f32 {
        self.avg10.max(self.avg60)
    }
}

/// The process that owns a ticket until its launch is reported.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(super) struct Owner {
    pid: u32,
    start: Option<String>,
}

/// Everything the gate reads from outside its own queue. Tests replace it;
/// [`SystemHost`] is the machine.
pub(super) trait Host: Send + Sync {
    fn now_ms(&self) -> u64;
    fn queue_dir(&self) -> PathBuf;
    fn queue_lock(&self) -> PathBuf;
    fn check_cache(&self) -> PathBuf;
    fn start_stamps(&self) -> PathBuf;
    fn check(
        &self,
        config: &ThrottleConfig,
        run: &Run,
        group: &StartGroup,
        timeout: Duration,
    ) -> std::result::Result<crate::harness::check_run::CheckOutcome, String>;
    fn owner(&self) -> Owner;
    fn owner_is_live(&self, owner: &Owner) -> bool;
    /// `Err` is the missing source, for the fail-fast message.
    fn pressure(&self, resource: Resource) -> Result<Pressure, String>;
    fn memory_available(&self) -> Result<u64, String>;
    fn disk_free(&self, checkout: &Path) -> Result<u64, String>;
    fn workspaces(&self) -> Result<Vec<WorkspaceId>, String>;
    /// Every agent row of one workspace; `Err` names why it cannot be read.
    fn agents(&self, workspace: &WorkspaceId) -> Result<Vec<AgentState>, String>;
    fn workflows(
        &self,
        workspace: &WorkspaceId,
    ) -> Result<Vec<crate::store::workflow::RunRecord>, String>;
}

/// The real machine: the home's queue, `/proc`, and the known workspaces.
pub(super) struct SystemHost;

impl Host for SystemHost {
    fn check_cache(&self) -> PathBuf {
        paths::loop_throttle_check_cache()
    }

    fn start_stamps(&self) -> PathBuf {
        paths::loop_throttle_start_stamps()
    }

    fn check(
        &self,
        config: &ThrottleConfig,
        run: &Run,
        group: &StartGroup,
        timeout: Duration,
    ) -> std::result::Result<crate::harness::check_run::CheckOutcome, String> {
        let mut env = crate::workspace::pin_env(&run.workspace, &run.root);
        env.extend([
            (
                crate::harness::loop_task::LOOP_TASK_ENV.to_owned(),
                run.task.clone(),
            ),
            (
                crate::workspace::ENV_WORKTREE_PATH.to_owned(),
                run.checkout.display().to_string(),
            ),
            ("RIMZ_START_KIND".to_owned(), group.kind.to_string()),
            (
                "RIMZ_START_ACCOUNT".to_owned(),
                group
                    .account
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            ),
            ("RIMZ_START_AGENTS".to_owned(), group.agents.to_string()),
            ("RIMZ_START_TASK".to_owned(), run.task.clone()),
        ]);
        // check_groups only calls the host when the check is configured.
        let command = config.check.as_deref().expect("configured capacity check");
        crate::harness::check_run::run_command(
            &run.checkout,
            command,
            crate::harness::check_run::WatchDeadline::CapacityCheck(timeout),
            crate::harness::check_run::CheckEcho::Capture,
            &env,
            |_, _| {},
            &mut None,
        )
        .map_err(|error| format!("failed to start: {error:#}"))
    }

    fn now_ms(&self) -> u64 {
        crate::utils::time::unix_now_ms()
    }

    fn queue_dir(&self) -> PathBuf {
        paths::loop_throttle_queue_dir()
    }

    fn queue_lock(&self) -> PathBuf {
        paths::loop_throttle_queue_lock()
    }

    fn owner(&self) -> Owner {
        let pid = std::process::id();
        Owner {
            pid,
            start: crate::proc::process_start_token(pid),
        }
    }

    fn owner_is_live(&self, owner: &Owner) -> bool {
        crate::proc::process_is_live(owner.pid, owner.start.as_deref())
    }

    fn pressure(&self, resource: Resource) -> Result<Pressure, String> {
        crate::proc::pressure::read(resource).map_err(|err| err.to_string())
    }

    fn memory_available(&self) -> Result<u64, String> {
        crate::proc::memory::sample()
            .map(|memory| memory.available_bytes)
            .map_err(|err| format!("/proc/meminfo: {err}"))
    }

    fn disk_free(&self, checkout: &Path) -> Result<u64, String> {
        crate::disk::usage::disk_space(checkout)
            .map(|space| space.free)
            .map_err(|err| format!("{}: {err}", checkout.display()))
    }

    fn workspaces(&self) -> Result<Vec<WorkspaceId>, String> {
        Ok(crate::workspace::known_workspaces_read_only()
            .map_err(|err| format!("cannot list workspaces: {err}"))?
            .into_iter()
            .map(|workspace| workspace.workspace_id)
            .collect())
    }

    fn agents(&self, workspace: &WorkspaceId) -> Result<Vec<AgentState>, String> {
        let unreadable =
            |err: &dyn fmt::Display| format!("workspace {workspace} unreadable: {err}");
        let state = StatePaths::for_workspace(workspace.clone()).map_err(|err| unreadable(&err))?;
        if !state.root.is_dir() {
            return Ok(Vec::new());
        }
        let runtime = RuntimePaths::for_state(&state).map_err(|err| unreadable(&err))?;
        let store = crate::store::Store::open_existing(state, runtime)
            .ok_or_else(|| unreadable(&"unsupported state layout"))?;
        Ok(std::sync::Arc::unwrap_or_clone(
            store
                .snapshot_cached()
                .map_err(|err| unreadable(&format!("{err:#}")))?
                .agents,
        ))
    }

    fn workflows(
        &self,
        workspace: &WorkspaceId,
    ) -> Result<Vec<crate::store::workflow::RunRecord>, String> {
        let unreadable =
            |err: &dyn fmt::Display| format!("workspace {workspace} unreadable: {err}");
        let state = StatePaths::for_workspace(workspace.clone()).map_err(|err| unreadable(&err))?;
        let mut runs = Vec::new();
        for paths in state.workflow_runs().map_err(|err| unreadable(&err))? {
            let record = match crate::store::workflow::load(&paths) {
                Ok(record) => record,
                Err(crate::store::workflow::WorkflowStoreErr::Io { source, .. })
                    if source.kind() == io::ErrorKind::NotFound =>
                {
                    continue;
                }
                Err(err) => return Err(unreadable(&err)),
            };
            if !record.state.is_terminal() {
                runs.push(record);
            }
        }
        Ok(runs)
    }
}

/// The run asking for a turn.
#[derive(Clone, Debug)]
pub(in crate::harness) struct Run {
    pub(in crate::harness) task: String,
    pub(in crate::harness) root: PathBuf,
    pub(in crate::harness) checkout: PathBuf,
    /// Where the launch writes, read for the free-disk floor.
    pub(in crate::harness) disk: PathBuf,
    pub(in crate::harness) workspace: WorkspaceId,
    /// Distinct provider-kind/account groups of the agents this start opens.
    pub(in crate::harness) groups: Vec<StartGroup>,
    /// Absolute UNIX milliseconds, or no bound for blocking admission.
    pub(in crate::harness) gives_up_ms: Option<u64>,
    pub(in crate::harness) machine_gated: bool,
    pub(in crate::harness) limits: Option<WorkflowStart>,
}

impl Run {
    fn kinds(&self) -> Vec<AgentKind> {
        let mut kinds: Vec<_> = self.groups.iter().map(|group| group.kind.clone()).collect();
        kinds.sort();
        kinds.dedup();
        kinds
    }
}

#[derive(Clone, Debug)]
pub struct StartGroup {
    pub(in crate::harness) kind: AgentKind,
    pub(in crate::harness) account: Option<LoginName>,
    pub(in crate::harness) agents: usize,
}

#[derive(Debug)]
pub(in crate::harness) enum Attempt {
    Open,
    Turn(Turn),
    Held(Hold),
}

/// Try once at the queue's tail. A held start leaves no ticket behind, and no
/// turn is granted at or past the run's give-up instant.
pub(in crate::harness) fn try_admit(config: &ThrottleConfig, run: &Run) -> Result<Attempt> {
    try_admit_in(&(Arc::new(SystemHost) as Arc<dyn Host>), config, run)
}

pub(super) fn try_admit_in(
    host: &Arc<dyn Host>,
    config: &ThrottleConfig,
    run: &Run,
) -> Result<Attempt> {
    if !run.machine_gated && !caps(config) && run.limits.is_none() {
        return Ok(Attempt::Open);
    }
    if run.machine_gated
        && let Some(problem) = unreadable(host.as_ref(), config)
    {
        return Err(ThrottleError::Unreadable(problem));
    }
    let turn = {
        let _lock = WorkspaceLock::acquire(&host.queue_lock())?;
        let now = host.now_ms();
        let started_past_limit = match pass(host.as_ref(), config, run, None, now, &mut None)? {
            Pass::Admitted(note) => note,
            Pass::Held(hold) => return Ok(Attempt::Held(hold)),
        };
        let path = paths::loop_throttle_ticket_in(&host.queue_dir(), now);
        let mut ticket = Ticket::new(host.as_ref(), run, now);
        ticket.state = TicketState::Admitted;
        write_ticket(&path, &ticket)?;
        Turn::new(host, path, started_past_limit)
    };
    match check_groups(host.as_ref(), config, run)? {
        Some(hold) => Ok(Attempt::Held(hold)),
        // A check cut at the give-up instant answered nothing, so it grants no turn.
        None if run.limits.as_ref().is_none_or(|start| start.may_wait)
            && run.gives_up_ms.is_some_and(|until| host.now_ms() >= until) =>
        {
            Ok(Attempt::Held(Hold {
                kind: HoldKind::Capacity,
                text: "wait deadline reached".to_owned(),
                kinds: run.kinds(),
            }))
        }
        None => {
            if run.machine_gated && config.check.is_some() {
                let _lock = WorkspaceLock::acquire(&host.queue_lock())?;
                if let Some(hold) = pass(
                    host.as_ref(),
                    config,
                    run,
                    Some(&turn.0.ticket),
                    host.now_ms(),
                    &mut None,
                )?
                .hold()
                {
                    return Ok(Attempt::Held(hold));
                }
            }
            Ok(Attempt::Turn(turn))
        }
    }
}

/// The latest capacity-check errors, read without running a check or taking a turn.
pub fn check_errors() -> Vec<String> {
    let config = MachineConfig::load_lenient();
    check_errors_in(&SystemHost, &config.throttle)
}

fn check_errors_in(host: &dyn Host, config: &ThrottleConfig) -> Vec<String> {
    if config.check.is_none() {
        return Vec::new();
    }
    read_verdicts(host)
        .into_iter()
        .filter(|verdict| verdict.outcome == CheckAnswer::Error)
        .map(|verdict| verdict.detail)
        .collect()
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CheckAnswer {
    Room,
    NoRoom,
    Error,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Verdict {
    kind: AgentKind,
    account: Option<LoginName>,
    at_ms: u64,
    outcome: CheckAnswer,
    detail: String,
}

fn read_verdicts(host: &dyn Host) -> Vec<Verdict> {
    std::fs::read(host.check_cache())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn check_groups(host: &dyn Host, config: &ThrottleConfig, run: &Run) -> Result<Option<Hold>> {
    if !run.machine_gated || config.check.is_none() {
        return Ok(None);
    }
    let mut held = Hold {
        kind: HoldKind::Capacity,
        text: String::new(),
        kinds: Vec::new(),
    };
    let mut details = Vec::new();
    for group in &run.groups {
        let cached = {
            let _lock = WorkspaceLock::acquire(&host.queue_lock())?;
            let now = host.now_ms();
            read_verdicts(host).into_iter().find(|verdict| {
                verdict.kind == group.kind
                    && verdict.account == group.account
                    && verdict.outcome != CheckAnswer::Room
                    && now
                        .checked_sub(verdict.at_ms)
                        .is_some_and(|age| age < CHECK_REUSE_MS)
            })
        };
        let verdict = match cached {
            Some(verdict) => verdict,
            None => {
                let at_ms = host.now_ms();
                let timeout = run.gives_up_ms.map_or(config.check_timeout(), |until| {
                    config
                        .check_timeout()
                        .min(Duration::from_millis(until.saturating_sub(at_ms)))
                });
                if timeout.is_zero() {
                    return Ok(None);
                }
                let checked = host.check(config, run, group, timeout);
                if run.gives_up_ms.is_some_and(|until| host.now_ms() >= until) {
                    return Ok(None);
                }
                let (outcome, detail) = check_answer(checked, timeout);
                let label = match &group.account {
                    Some(account) => format!("{}/{account}", group.kind),
                    None => group.kind.to_string(),
                };
                let verdict = Verdict {
                    kind: group.kind.clone(),
                    account: group.account.clone(),
                    at_ms,
                    outcome,
                    detail: match outcome {
                        CheckAnswer::Error => format!("capacity check failing: {label} {detail}"),
                        _ => format!("{label}: {detail}"),
                    },
                };
                let _lock = WorkspaceLock::acquire(&host.queue_lock())?;
                let mut verdicts = read_verdicts(host);
                let current = verdicts
                    .iter_mut()
                    .find(|current| current.kind == group.kind && current.account == group.account);
                match current {
                    Some(current) if current.at_ms <= at_ms => *current = verdict.clone(),
                    None => verdicts.push(verdict.clone()),
                    _ => {}
                }
                if let Err(error) = crate::disk::atomic::write_temp_then_rename_cache(
                    &host.check_cache(),
                    &verdicts,
                ) {
                    tracing::warn!(%error, "failed to cache the capacity check");
                }
                verdict
            }
        };
        if verdict.outcome == CheckAnswer::Room {
            continue;
        }
        if verdict.outcome == CheckAnswer::Error {
            held.kind = HoldKind::CheckFailed;
        }
        details.push((verdict.outcome != CheckAnswer::Error, verdict.detail));
        if !held.kinds.contains(&group.kind) {
            held.kinds.push(group.kind.clone());
        }
    }
    details.sort_by_key(|(capacity, _)| *capacity);
    held.text = details
        .into_iter()
        .map(|(_, text)| text)
        .collect::<Vec<_>>()
        .join("; ");
    Ok((!held.kinds.is_empty()).then_some(held))
}

fn check_answer(
    checked: std::result::Result<crate::harness::check_run::CheckOutcome, String>,
    timeout: Duration,
) -> (CheckAnswer, String) {
    let checked = match checked {
        Ok(checked) => checked,
        Err(error) => return (CheckAnswer::Error, error),
    };
    let detail = if checked.timed_out() {
        format!("timed out after {}", format_duration_compact(timeout))
    } else if checked.interrupted() {
        "interrupted".to_owned()
    } else {
        match checked.code() {
            Some(0) => return (CheckAnswer::Room, "capacity".to_owned()),
            Some(75) => return (CheckAnswer::NoRoom, "no capacity (your check)".to_owned()),
            Some(code) => format!("exit {code}"),
            None => "terminated by signal".to_owned(),
        }
    };
    let detail = if checked.stderr_line().is_empty() {
        detail
    } else {
        format!("{detail}: {}", checked.stderr_line())
    };
    (CheckAnswer::Error, detail)
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(in crate::harness) struct Hold {
    pub(in crate::harness) kind: HoldKind,
    pub(in crate::harness) text: String,
    /// The providers the hold waits on, read by two consumers. On a ticket they decide
    /// step-aside in `queue_hold`, so a propagated needs-a-fix hold keeps them empty there.
    /// On the hold `pass` returns they are the providers of the starts ahead on the
    /// provider-only path, else the run's own, and the run record saves them as
    /// `capacity_wait.kinds`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(in crate::harness) kinds: Vec<AgentKind>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
enum TicketState {
    Waiting {
        reason: Option<Hold>,
    },
    Admitted,
    Launched {
        at_ms: u64,
        workspace: WorkspaceId,
        agent: AgentSessionId,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct Ticket {
    owner: Owner,
    task: String,
    root: PathBuf,
    checkout: PathBuf,
    enqueued_ms: u64,
    #[serde(default = "machine_gated_default")]
    machine_gated: bool,
    #[serde(default = "one_agent")]
    agents: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit: Option<LimitScope>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    kinds: Vec<AgentKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gives_up_ms: Option<u64>,
    #[serde(flatten)]
    state: TicketState,
}

fn one_agent() -> usize {
    1
}

fn machine_gated_default() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct LimitScope {
    workflow: String,
    step: String,
}

impl Ticket {
    fn new(host: &dyn Host, run: &Run, enqueued_ms: u64) -> Self {
        Self {
            owner: host.owner(),
            task: run.task.clone(),
            root: run.root.clone(),
            checkout: run.checkout.clone(),
            enqueued_ms,
            machine_gated: run.machine_gated,
            agents: run.groups.iter().map(|group| group.agents).sum(),
            limit: run.limits.as_ref().map(|start| LimitScope {
                workflow: start.workflow.clone(),
                step: start.step.clone(),
            }),
            kinds: run.kinds(),
            gives_up_ms: run.gives_up_ms,
            state: TicketState::Waiting { reason: None },
        }
    }

    fn shares_scope(&self, other: &Self) -> bool {
        (self.machine_gated && other.machine_gated) || self.shares_limit(other)
    }

    fn shares_limit(&self, other: &Self) -> bool {
        self.limit
            .as_ref()
            .zip(other.limit.as_ref())
            .is_some_and(|(mine, theirs)| mine.workflow == theirs.workflow)
    }

    fn is_waiting(&self) -> bool {
        matches!(self.state, TicketState::Waiting { .. })
    }
}

/// The turn one admitted run holds until its launch is reported or it is
/// dropped unreported.
#[derive(Clone)]
pub struct Turn(Arc<TurnInner>);

struct TurnInner {
    host: Arc<dyn Host>,
    ticket: PathBuf,
    started_past_limit: Option<String>,
    reported: AtomicBool,
}

impl fmt::Debug for Turn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Turn")
            .field("ticket", &self.0.ticket)
            .finish_non_exhaustive()
    }
}

impl Turn {
    pub(in crate::harness) fn started_past_limit(&self) -> Option<&str> {
        self.0.started_past_limit.as_deref()
    }

    fn new(host: &Arc<dyn Host>, ticket: PathBuf, started_past_limit: Option<String>) -> Self {
        Self(Arc::new(TurnInner {
            host: Arc::clone(host),
            ticket,
            started_past_limit,
            reported: AtomicBool::new(false),
        }))
    }

    /// Stamp the committed launch and machine/workflow/step stagger under the queue lock.
    /// Machine and workflow scopes release independently on their report or ceiling.
    /// A second report, which a verify retry produces, changes nothing.
    pub fn report_launch(&self, workspace: &WorkspaceId, agent: &AgentSessionId) {
        if self.0.reported.swap(true, Ordering::SeqCst) {
            return;
        }
        let host = &self.0.host;
        let stamped = (|| -> Result<()> {
            let _lock = WorkspaceLock::acquire(&host.queue_lock())?;
            let Some(mut ticket) = read_ticket(&self.0.ticket)? else {
                return Ok(());
            };
            let at_ms = host.now_ms();
            if ticket.machine_gated || ticket.limit.is_some() {
                limits::stamp(
                    host.as_ref(),
                    ticket.machine_gated,
                    ticket.limit.as_ref(),
                    at_ms,
                )?;
            }
            ticket.state = TicketState::Launched {
                at_ms,
                workspace: workspace.clone(),
                agent: agent.clone(),
            };
            write_ticket(&self.0.ticket, &ticket)
        })();
        if let Err(err) = stamped {
            // An unstamped ticket would hold the turn for as long as this
            // process lives; releasing it early only loosens the pacing.
            tracing::warn!(error = %err, "failed to stamp the loop throttle launch; releasing the turn");
            remove_ticket(&self.0.ticket);
        }
    }
}

impl Drop for TurnInner {
    fn drop(&mut self) {
        if !self.reported.load(Ordering::SeqCst) {
            remove_ticket(&self.ticket);
        }
    }
}

fn caps(config: &ThrottleConfig) -> bool {
    config.max_parallel_agents.is_some() || config.max_parallel_automation.is_some()
}

/// The configured limit this host cannot read, as the fail-fast message.
fn unreadable(host: &dyn Host, config: &ThrottleConfig) -> Option<String> {
    for resource in Resource::ALL {
        if resource.limit(config).is_some()
            && let Err(source) = host.pressure(resource)
        {
            return Some(format!(
                "`throttle.{}-pressure` is set but pressure cannot be read ({source}); remove the key from config.toml, or enable PSI (boot with `psi=1`)",
                resource.name()
            ));
        }
    }
    if config.min_memory.is_some()
        && let Err(source) = host.memory_available()
    {
        return Some(format!(
            "`throttle.min-memory` is set but available memory cannot be read ({source}); remove the key from config.toml"
        ));
    }
    None
}

/// Refuse when a configured limit cannot be read on this machine.
pub fn preflight(config: &ThrottleConfig) -> Result<()> {
    match unreadable(&SystemHost, config) {
        Some(problem) => Err(ThrottleError::Unreadable(problem)),
        None => Ok(()),
    }
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

pub(in crate::harness) fn check_group_size(config: &ThrottleConfig, run: &Run) -> Result<()> {
    let agents = run.groups.iter().map(|group| group.agents).sum::<usize>();
    if let Some(start) = &run.limits {
        for (scope, name, limit) in [
            ("workflow", &start.workflow, &start.workflow_limit),
            ("step", &start.step, &start.step_limit),
        ] {
            if let Some(cap) = limit.as_ref().and_then(|limit| limit.max_parallel)
                && agents > cap as usize
            {
                return Err(ThrottleError::Oversized {
                    agents,
                    key: "max-parallel",
                    cap,
                    fix: format!("raise max-parallel in {scope} {name}'s definition"),
                });
            }
        }
    }
    for (key, cap) in [
        ("max-parallel-agents", config.max_parallel_agents),
        ("max-parallel-automation", config.max_parallel_automation),
    ] {
        if let Some(cap) = cap
            && agents > cap as usize
        {
            return Err(ThrottleError::Oversized {
                agents,
                key,
                cap,
                fix: "raise it in ~/.rimz/config.toml".into(),
            });
        }
    }
    Ok(())
}

enum Pass {
    Admitted(Option<String>),
    Held(Hold),
}

impl Pass {
    fn hold(self) -> Option<Hold> {
        match self {
            Self::Admitted(_) => None,
            Self::Held(hold) => Some(hold),
        }
    }
}

/// One pass over the queue, under its lock. A manual admission carries the limit
/// it skipped. Without `mine`, inspect the tail without changing it.
fn pass(
    host: &dyn Host,
    config: &ThrottleConfig,
    run: &Run,
    mine: Option<&Path>,
    now: u64,
    blocked: &mut Option<(u64, Hold)>,
) -> Result<Pass> {
    let manual = run.limits.as_ref().is_some_and(|start| !start.may_wait);
    if manual
        && run
            .limits
            .as_ref()
            .is_some_and(|start| start.workflow_limit.is_none() && start.step_limit.is_none())
    {
        return Ok(Pass::Admitted(None));
    }
    if !manual {
        check_group_size(config, run)?;
    }
    let ceiling = if config.check.is_some() {
        CHECK_CEILING
    } else {
        TURN_CEILING
    };
    let queue = match sweep(host, Some(ceiling), now) {
        Ok(queue) => queue,
        Err(_) if manual => return Ok(Pass::Admitted(None)),
        Err(error) => return Err(error),
    };
    let position = match mine {
        Some(mine) => queue
            .iter()
            .position(|(path, _)| path == mine)
            .ok_or_else(|| ThrottleError::Vanished(mine.to_path_buf()))?,
        None => queue.len(),
    };
    let mut ticket = match mine {
        Some(_) => queue[position].1.clone(),
        None => Ticket::new(host, run, now),
    };
    // A limit hold is reported before the machine's, an in-flight turn of the
    // limited workflow included, so the waiter keeps its FIFO standing.
    let mut hold = run
        .limits
        .as_ref()
        .and_then(|start| match launching(&queue, &ticket, mine) {
            0 => match limits::evaluate(
                host,
                start,
                run.groups.iter().map(|group| group.agents).sum(),
            ) {
                Ok(hold) => hold,
                Err(_) if manual => None,
                Err(error) => Some(store_hold(error)),
            },
            count => Some(limits::launching(start, count)),
        });
    if manual {
        return Ok(Pass::Admitted(hold.map(|hold| hold.text)));
    }
    if hold.is_none() && run.machine_gated {
        hold = queue_hold(&queue, position, &ticket);
    }
    if hold.is_none() {
        hold = match blocked {
            Some((at, hold))
                if hold.kind != HoldKind::Stagger && now.saturating_sub(*at) < SAMPLE_EVERY_MS =>
            {
                Some(hold.clone())
            }
            _ => {
                let in_flight = queue
                    .iter()
                    .filter(|(path, ticket)| {
                        mine != Some(path.as_path())
                            && matches!(ticket.state, TicketState::Admitted)
                    })
                    .map(|(_, ticket)| ticket.agents)
                    .sum();
                let sampled = limit_hold(host, config, run, in_flight)?;
                *blocked = sampled.clone().map(|hold| (now, hold));
                sampled
            }
        };
        if let Some(hold) = &mut hold {
            hold.kinds = run.kinds();
        }
    }
    let state = match &hold {
        Some(hold) => TicketState::Waiting {
            reason: Some(hold.clone()),
        },
        None => TicketState::Admitted,
    };
    if let Some(mine) = mine
        && ticket.state != state
    {
        ticket.state = state;
        write_ticket(mine, &ticket)?;
    }
    // The ticket keeps a propagated hold's empty kinds, so no disjoint start steps
    // aside for it; the caller's record still names the providers this run waits for.
    if let Some(hold) = &mut hold
        && hold.kinds.is_empty()
    {
        hold.kinds = run.kinds();
    }
    Ok(match hold {
        Some(hold) => Pass::Held(hold),
        None => Pass::Admitted(None),
    })
}

fn launching(queue: &[(PathBuf, Ticket)], mine: &Ticket, path: Option<&Path>) -> usize {
    queue
        .iter()
        .filter(|(other, ticket)| {
            path != Some(other.as_path()) && ticket.shares_limit(mine) && !ticket.is_waiting()
        })
        .count()
}

fn turn_held(queue: &[(PathBuf, Ticket)], mine: &Ticket, position: usize) -> bool {
    queue.iter().enumerate().any(|(index, (_, ticket))| {
        index != position && ticket.shares_scope(mine) && !ticket.is_waiting()
    })
}

/// What in the queue keeps the ticket at `position` from being the candidate:
/// a held turn, or a ticket ahead that nothing lets it step over.
fn queue_hold(queue: &[(PathBuf, Ticket)], position: usize, mine: &Ticket) -> Option<Hold> {
    let turn_held = turn_held(queue, mine, position);
    let blocks_me = |index: usize| {
        let ticket = &queue[index].1;
        if !ticket.shares_scope(mine) {
            return false;
        }
        if !ticket.is_waiting() {
            return true;
        }
        if !mine.machine_gated || !ticket.machine_gated {
            return false;
        }
        if let TicketState::Waiting { reason: Some(hold) } = &ticket.state
            && matches!(hold.kind, HoldKind::Capacity | HoldKind::CheckFailed)
            && !hold.kinds.is_empty()
            && !mine.kinds.is_empty()
            && mine.kinds.iter().all(|kind| !hold.kinds.contains(kind))
        {
            return false;
        }
        true
    };
    let mut ahead = 0;
    let mut capacity_only = true;
    let mut kind = HoldKind::Capacity;
    let mut kinds = Vec::new();
    let mut text = None;
    let mut needs_fix = None;
    for index in (0..position).filter(|&index| blocks_me(index)) {
        ahead += 1;
        if let TicketState::Waiting { reason: Some(hold) } = &queue[index].1.state
            && hold.kind.needs_fix()
        {
            needs_fix.get_or_insert(hold);
        }
        match &queue[index].1.state {
            TicketState::Waiting { reason: Some(hold) }
                if matches!(hold.kind, HoldKind::Capacity | HoldKind::CheckFailed)
                    && !hold.kinds.is_empty() =>
            {
                if hold.kind == HoldKind::CheckFailed && kind != HoldKind::CheckFailed {
                    kind = HoldKind::CheckFailed;
                    text = Some(hold.text.clone());
                }
                text.get_or_insert_with(|| hold.text.clone());
                kinds.extend(hold.kinds.iter().cloned());
            }
            _ => capacity_only = false,
        }
    }
    if !turn_held && ahead == 0 {
        return None;
    }
    let starts = ahead.max(1);
    if turn_held || !capacity_only {
        kind = HoldKind::Queue;
        kinds.clear();
        text = None;
    }
    if let Some(hold) = needs_fix {
        kind = hold.kind;
        text = Some(hold.text.clone());
    }
    kinds.sort();
    kinds.dedup();
    Some(Hold {
        kind,
        kinds,
        text: text.unwrap_or_else(|| {
            format!(
                "queued: {starts} start{} ahead",
                if starts == 1 { "" } else { "s" }
            )
        }),
    })
}

/// Read the queue in order and remove every ticket that is over: a waiting or
/// admitted one whose owner died, and a launched one whose turn has passed.
/// `ceiling` is `None` for a reader that only reaps the dead.
fn sweep(host: &dyn Host, ceiling: Option<Duration>, now: u64) -> Result<Vec<(PathBuf, Ticket)>> {
    let mut queue = Vec::new();
    for path in ticket_paths(&host.queue_dir())? {
        let mut ticket = match read_ticket(&path) {
            Ok(Some(ticket)) => ticket,
            Ok(None) => continue,
            Err(err @ ThrottleError::CorruptTicket { .. }) => {
                tracing::warn!(error = %err, "removing corrupt loop throttle ticket");
                remove_ticket(&path);
                continue;
            }
            Err(err) => return Err(err),
        };
        let scopes = (ticket.machine_gated, ticket.limit.clone());
        let over = match &ticket.state {
            TicketState::Waiting { .. } | TicketState::Admitted => {
                !host.owner_is_live(&ticket.owner)
            }
            TicketState::Launched {
                at_ms,
                workspace,
                agent,
            } => {
                if let Some(ceiling) = ceiling {
                    let elapsed = now.saturating_sub(*at_ms);
                    let mut machine_done = !ticket.machine_gated || elapsed >= duration_ms(ceiling);
                    let mut limit_done =
                        ticket.limit.is_none() || elapsed >= duration_ms(CHECK_CEILING);
                    if (!machine_done || !limit_done)
                        && let Ok(agents) = host.agents(workspace)
                    {
                        machine_done = machine_done || provider_reported(&agents, agent);
                        limit_done = limit_done || limits::reported(&agents, agent);
                    }
                    ticket.machine_gated &= !machine_done;
                    if limit_done {
                        ticket.limit = None;
                    }
                }
                !ticket.machine_gated && ticket.limit.is_none()
            }
        };
        if over {
            remove_ticket(&path);
        } else {
            if scopes != (ticket.machine_gated, ticket.limit.clone()) {
                write_ticket(&path, &ticket)?;
            }
            queue.push((path, ticket));
        }
    }
    Ok(queue)
}

/// Reap the tickets of runs that died waiting, as `rimz loop stop` leaves one.
pub(super) fn reap() {
    let host = SystemHost;
    let reaped = WorkspaceLock::acquire(&host.queue_lock())
        .map_err(ThrottleError::from)
        .and_then(|_lock| sweep(&host, None, host.now_ms()));
    if let Err(err) = reaped {
        tracing::warn!(error = %err, "failed to reap the loop throttle queue");
    }
}

/// The provider reported the launch `reference` in: its row was adopted under
/// the provider's own id, or the launch is over or gone.
fn provider_reported(agents: &[AgentState], reference: &AgentSessionId) -> bool {
    let adopted = agents
        .iter()
        .any(|agent| agent.launch_id.as_ref() == Some(reference) && &agent.agent_id != reference);
    let provisional = agents.iter().find(|agent| &agent.agent_id == reference);
    adopted
        || provisional
            .is_none_or(|agent| agent.ended_at.is_some() || agent.status == AgentStatus::Failed)
}

/// A full agent session still open, regardless of turn or compaction state.
fn open(agent: &AgentState) -> bool {
    crate::agents::state::molt_stands(agent)
        && !agent.is_provider_subagent()
        && crate::store::runtime::agent_liveness(agent)
            != crate::store::runtime::AgentLiveness::Dead
}

/// Name the counted agents, up to the few a reason has room for.
fn name_counted(names: &mut Vec<String>, agents: &[AgentState], counted: &[&AgentState]) {
    let peers: Vec<&AgentState> = agents
        .iter()
        .filter(|agent| !agent.is_provider_subagent())
        .collect();
    names.extend(
        counted
            .iter()
            .take(NAMED_AGENTS.saturating_sub(names.len()))
            .map(|agent| crate::address::agent_handle(agent, &peers, false)),
    );
}

/// The open agents on this machine, then the automated among them, each
/// counted with the few it names, in one read of every workspace.
fn machine_counts(host: &dyn Host) -> Result<[(usize, Vec<String>); 2], String> {
    let mut counts: [(usize, Vec<String>); 2] = Default::default();
    for workspace in host.workspaces()? {
        let agents = host.agents(&workspace)?;
        let opened: Vec<&AgentState> = agents.iter().filter(|agent| open(agent)).collect();
        let automated: Vec<&AgentState> = opened
            .iter()
            .copied()
            .filter(|agent| agent.automation.is_some())
            .collect();
        for ((count, names), counted) in counts.iter_mut().zip([opened, automated]) {
            name_counted(names, &agents, &counted);
            *count += counted.len();
        }
    }
    Ok(counts)
}

fn store_hold(text: String) -> Hold {
    Hold {
        kind: HoldKind::Store,
        kinds: Vec::new(),
        text: format!("cannot count open agents: {text}"),
    }
}

fn disk_hold(host: &dyn Host, floor: u64, path: &Path) -> Result<Option<Hold>> {
    let free = host
        .disk_free(path)
        .map_err(|detail| ThrottleError::Reading {
            reading: "free disk",
            detail,
        })?;
    Ok((free < floor).then(|| Hold {
        kind: HoldKind::Disk,
        kinds: Vec::new(),
        text: format!(
            "free disk {} < {} at {}",
            decimal_bytes(free),
            decimal_bytes(floor),
            path.display()
        ),
    }))
}

fn cap_hold(
    kind: HoldKind,
    prefix: &str,
    cap: u32,
    count: usize,
    incoming: usize,
    in_flight: usize,
    mut names: Vec<String>,
) -> Option<Hold> {
    if count.saturating_add(in_flight).saturating_add(incoming) <= cap as usize {
        return None;
    }
    if count > names.len() {
        names.push(format!("+{}", count - names.len()));
    }
    let automated = if kind == HoldKind::AutomationCap {
        "automated "
    } else {
        ""
    };
    let suffix = if names.is_empty() {
        String::new()
    } else {
        format!(" ({})", names.join(", "))
    };
    let mut text = format!("{prefix}{count} of {cap} {automated}agents open{suffix}");
    if in_flight > 0 {
        text.push_str(&format!(", {in_flight} more starting"));
    }
    if incoming > 1 {
        text.push_str(&format!("; this start opens {incoming}"));
    }
    Some(Hold {
        kind,
        kinds: Vec::new(),
        text,
    })
}

/// The first configured cap or limit that is not clear for `run`, sampled now.
/// A reading that cannot be taken is an error; a store that cannot be read is
/// a hold, never a short count.
fn limit_hold(
    host: &dyn Host,
    config: &ThrottleConfig,
    run: &Run,
    in_flight: usize,
) -> Result<Option<Hold>> {
    let counts = if caps(config) {
        match machine_counts(host) {
            Ok(counts) => counts,
            Err(text) => return Ok(Some(store_hold(text))),
        }
    } else {
        Default::default()
    };
    for ((kind, cap), (count, names)) in [
        (HoldKind::MachineCap, config.max_parallel_agents),
        (HoldKind::AutomationCap, config.max_parallel_automation),
    ]
    .into_iter()
    .zip(counts)
    {
        let Some(cap) = cap else { continue };
        if let Some(hold) = cap_hold(
            kind,
            "",
            cap,
            count,
            run.groups.iter().map(|group| group.agents).sum(),
            in_flight,
            names,
        ) {
            return Ok(Some(hold));
        }
    }
    if !run.machine_gated {
        return Ok(None);
    }
    if let Some(gap) = config.stagger() {
        match limits::machine_hold(host, gap) {
            Ok(Some(hold)) => return Ok(Some(hold)),
            Err(error) => return Ok(Some(store_hold(error))),
            Ok(None) => {}
        }
    }
    for resource in Resource::ALL {
        let Some(limit) = resource.limit(config) else {
            continue;
        };
        let worst = host
            .pressure(resource)
            .map_err(|detail| ThrottleError::Reading {
                reading: resource.label(),
                detail,
            })?
            .worst();
        if worst >= f32::from(limit) {
            return Ok(Some(Hold {
                kind: HoldKind::Pressure,
                kinds: Vec::new(),
                text: format!("{} pressure {worst:.0}% ≥ {limit}%", resource.name()),
            }));
        }
    }
    if let Some(floor) = config.min_memory_bytes() {
        let available = host
            .memory_available()
            .map_err(|detail| ThrottleError::Reading {
                reading: "available memory",
                detail,
            })?;
        if available < floor {
            return Ok(Some(Hold {
                kind: HoldKind::Memory,
                kinds: Vec::new(),
                text: format!(
                    "available memory {} < {}",
                    decimal_bytes(available),
                    decimal_bytes(floor)
                ),
            }));
        }
    }
    if let Some(floor) = config.min_disk_bytes() {
        return disk_hold(host, floor, &run.disk);
    }
    Ok(None)
}

/// A run of one task waiting for its turn, as `rimz loop show` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    /// The process waiting, as its run lock names it.
    pub pid: u32,
    pub checkout: PathBuf,
    pub since_ms: u64,
    pub kinds: Vec<AgentKind>,
    pub gives_up_ms: Option<u64>,
    /// The blocking reason, once the run's first pass recorded one.
    pub reason: Option<String>,
    pub hold: Option<HoldKind>,
    /// 1-based place among the runs waiting or holding the turn.
    pub position: usize,
}

/// The waiting runs of task `name` under `root`, in queue order. Read-only
/// and lock-free: a ticket is replaced by atomic rename, so every read is whole.
pub fn held(name: &str, root: &Path) -> Vec<Held> {
    held_in(&SystemHost, name, root)
}

fn held_in(host: &dyn Host, name: &str, root: &Path) -> Vec<Held> {
    waiting_in(host, Some(name), root)
        .into_iter()
        .map(|(_, held)| held)
        .collect()
}

/// Live waiting starts in this room, paired with their task names. Read-only
/// and lock-free, using the same owner check as the task lookup.
pub fn room_held(root: &Path) -> Vec<(String, Held)> {
    waiting_in(&SystemHost, None, root)
}

fn waiting_in(host: &dyn Host, name: Option<&str>, root: &Path) -> Vec<(String, Held)> {
    let Ok(paths) = ticket_paths(&host.queue_dir()) else {
        return Vec::new();
    };
    paths
        .iter()
        // Display only: an unreadable ticket is left out, never an error.
        .filter_map(|path| read_ticket(path).ok().flatten())
        .enumerate()
        .filter_map(|(index, ticket)| {
            let TicketState::Waiting { reason } = &ticket.state else {
                return None;
            };
            if name.is_some_and(|name| ticket.task != name)
                || ticket.root != root
                || !host.owner_is_live(&ticket.owner)
            {
                return None;
            }
            let held = Held {
                pid: ticket.owner.pid,
                checkout: ticket.checkout.clone(),
                since_ms: ticket.enqueued_ms,
                kinds: ticket.kinds.clone(),
                gives_up_ms: ticket.gives_up_ms,
                reason: reason.as_ref().map(|hold| hold.text.clone()),
                hold: reason.as_ref().map(|hold| hold.kind),
                position: index + 1,
            };
            Some((ticket.task, held))
        })
        .collect()
}

/// The machine's current readings beside any configured limit, one
/// `(label, text)` row each, for task `name`; a reading this host cannot take
/// is left out.
pub fn readings(
    config: &MachineConfig,
    name: &str,
    entry: &TaskEntry,
) -> Vec<(&'static str, String)> {
    let root = entry.resolved_root();
    let Ok(state) = StatePaths::for_project_root(&root) else {
        return Vec::new();
    };
    let checkout = entry.run_dir();
    let run = Run {
        machine_gated: true,
        limits: None,
        task: name.to_owned(),
        root,
        disk: disk_at(entry, &config.agents.worktree, checkout.clone()),
        checkout,
        workspace: state.workspace_id,
        groups: Vec::new(),
        gives_up_ms: None,
    };
    readings_in(&SystemHost, &config.throttle, &run)
}

/// A current needs-a-fix reading, independent of tickets and cached verdicts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DiagnosticHold {
    pub hold: HoldKind,
    pub text: String,
    #[serde(skip)]
    pub disk_path: Option<PathBuf>,
}

/// Read configured disk floors and agent counts without locks, writes, tickets,
/// or capacity checks. Unavailable disk samples are omitted, as in [`readings`].
pub fn diagnostic_holds(
    config: &MachineConfig,
    tasks: &[(&str, &TaskEntry)],
) -> Vec<DiagnosticHold> {
    diagnostic_holds_in(&SystemHost, config, tasks)
}

fn diagnostic_holds_in(
    host: &dyn Host,
    config: &MachineConfig,
    tasks: &[(&str, &TaskEntry)],
) -> Vec<DiagnosticHold> {
    let mut holds = Vec::new();
    if let Err(text) = diagnostic_agent_count(host, &config.throttle) {
        holds.push(DiagnosticHold {
            hold: HoldKind::Store,
            text: store_hold(text).text,
            disk_path: None,
        });
    }
    if let Some(floor) = config.throttle.min_disk_bytes() {
        let paths = tasks
            .iter()
            .flat_map(|(_, entry)| {
                let checkouts = if entry.each_worktree {
                    crate::worktree::discover_owned(&entry.resolved_root())
                        .unwrap_or_default()
                        .into_iter()
                        .map(|tree| {
                            crate::utils::path::normalize_path_lexical(&tree.marker.worktree_path)
                        })
                        .collect()
                } else {
                    vec![entry.run_dir()]
                };
                checkouts
                    .into_iter()
                    .map(move |checkout| disk_at(entry, &config.agents.worktree, checkout))
            })
            .collect::<std::collections::BTreeSet<_>>();
        for path in paths {
            if let Ok(Some(hold)) = disk_hold(host, floor, &path) {
                holds.push(DiagnosticHold {
                    hold: hold.kind,
                    text: hold.text,
                    disk_path: Some(path),
                });
            }
        }
    }
    holds
}

fn diagnostic_agent_count(host: &dyn Host, config: &ThrottleConfig) -> Result<(), String> {
    if caps(config) {
        machine_counts(host)?;
    }
    Ok(())
}

/// Where a launch of `entry` into `checkout` writes, for the free-disk floor: a
/// fresh worktree's target, at its nearest existing ancestor, else `checkout`.
pub(super) fn disk_at(entry: &TaskEntry, worktrees: &WorktreeConfig, checkout: PathBuf) -> PathBuf {
    let name = match entry.task_worktree() {
        Some(crate::config::TaskWorktree::Named(name)) => name.trim(),
        Some(crate::config::TaskWorktree::Fresh { .. }) => "",
        None => return checkout,
    };
    // The launch takes the task root as its repo root, as `--root` does.
    let root = entry.resolved_root();
    let repo_root = root.canonicalize().unwrap_or(root);
    let target = if name.is_empty() {
        crate::worktree::worktree_parent(&repo_root, worktrees)
    } else {
        crate::worktree::worktree_path(&repo_root, worktrees, name)
    };
    target
        .ok()
        .and_then(|target| {
            target
                .ancestors()
                .find(|dir| dir.exists())
                .map(Path::to_path_buf)
        })
        .unwrap_or(checkout)
}

fn readings_in(host: &dyn Host, config: &ThrottleConfig, run: &Run) -> Vec<(&'static str, String)> {
    let limited = |text: String, limit: Option<String>| match limit {
        Some(limit) => format!("{text} (limit {limit})"),
        None => text,
    };
    let mut rows = Vec::new();
    for resource in Resource::ALL {
        if let Ok(pressure) = host.pressure(resource) {
            rows.push((
                resource.label(),
                limited(
                    format!("avg10 {:.0}%", pressure.avg10),
                    resource.limit(config).map(|limit| format!("{limit}%")),
                ),
            ));
        }
    }
    if let Ok(available) = host.memory_available() {
        rows.push((
            "memory",
            limited(
                format!("{} available", decimal_bytes(available)),
                config
                    .min_memory_bytes()
                    .map(|floor| format!("min {}", decimal_bytes(floor))),
            ),
        ));
    }
    if let Ok(free) = host.disk_free(&run.disk) {
        rows.push((
            "disk",
            limited(
                format!("{} free", decimal_bytes(free)),
                config
                    .min_disk_bytes()
                    .map(|floor| format!("min {}", decimal_bytes(floor))),
            ),
        ));
    }
    if let Ok(counts) = machine_counts(host) {
        for ((label, cap), (count, _)) in [
            ("open agents", config.max_parallel_agents),
            ("automated agents", config.max_parallel_automation),
        ]
        .into_iter()
        .zip(counts)
        {
            rows.push((
                label,
                limited(count.to_string(), cap.map(|cap| format!("max {cap}"))),
            ));
        }
    }
    rows
}

/// Ticket files in queue order. A name sorts by its enqueue time; the writer's
/// temp files and anything else in the directory are not tickets.
fn ticket_paths(directory: &Path) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(ThrottleError::Io {
                path: directory.to_path_buf(),
                source,
            });
        }
    };
    let mut tickets = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| ThrottleError::Io {
            path: directory.to_path_buf(),
            source,
        })?;
        let name = entry.file_name();
        let is_ticket = name
            .to_str()
            .and_then(|name| name.split_once('-'))
            .is_some_and(|(time, nonce)| {
                time.len() == 20
                    && time.parse::<u64>().is_ok()
                    && uuid::Uuid::parse_str(nonce).is_ok()
            });
        if is_ticket {
            tickets.push(entry.path());
        }
    }
    tickets.sort();
    Ok(tickets)
}

/// A ticket that is gone is `None`: its owner removed it. One that cannot be
/// read or parsed is an error, since it may be the held turn.
fn read_ticket(path: &Path) -> Result<Option<Ticket>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ThrottleError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|source| ThrottleError::CorruptTicket {
            path: path.to_path_buf(),
            source,
        })
}

fn write_ticket(path: &Path, ticket: &Ticket) -> Result<()> {
    crate::disk::atomic::write_temp_then_rename_cache(path, ticket)
        .map_err(ThrottleError::WriteTicket)
}

fn remove_ticket(path: &Path) {
    if let Err(err) = std::fs::remove_file(path)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), error = %err, "failed to remove a loop throttle ticket");
    }
}

#[cfg(test)]
#[path = "throttle/tests.rs"]
pub(crate) mod tests;
