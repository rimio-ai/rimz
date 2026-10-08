//! Loop start throttle: one queue per RimZ home that every loop run starting
//! an agent takes a turn in.
//!
//! A gated run writes a ticket and rechecks the queue until it holds the turn.
//! The turn passes on when the launch it admitted is reported and the provider
//! then reports the new agent, or `pace` passes, whichever is first; a later
//! scanner releases it, so the launching process never lingers. Caps and
//! resource limits are sampled only by the run at the head of the queue, in
//! the pass that would admit it.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::{AgentState, AgentStatus};
use crate::config::{MachineConfig, TaskEntry, ThrottleConfig, WorktreeConfig};
use crate::disk::atomic::AtomicErr;
use crate::disk::lock::{LockErr, WorkspaceLock};
use crate::disk::paths::{self, RuntimePaths, StatePaths};
use crate::ids::{AgentSessionId, WorkspaceId};
use crate::store::run::RunRecord;
use crate::utils::size::decimal_bytes;
use crate::utils::time::format_duration_compact;

const RECHECK: Duration = Duration::from_secs(1);
const SAMPLE_EVERY_MS: u64 = 5_000;
const NAMED_AGENTS: usize = 2;

/// Why the throttle could not decide or record a turn.
#[derive(Debug, thiserror::Error)]
pub enum ThrottleError {
    /// A configured limit this machine cannot read, with the fix.
    #[error("{0}")]
    Unreadable(String),
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
    #[error("loop throttle ticket {0} vanished from the queue")]
    Vanished(PathBuf),
    #[error("listening for Ctrl-C while held: {0}")]
    Interrupts(#[source] io::Error),
}

type Result<T, E = ThrottleError> = std::result::Result<T, E>;

/// A pressure-stall resource RimZ can hold a start on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Resource {
    Cpu,
    Io,
    Memory,
}

impl Resource {
    const ALL: [Self; 3] = [Self::Cpu, Self::Io, Self::Memory];

    const fn name(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Io => "io",
            Self::Memory => "memory",
        }
    }

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

/// The `some` line of one pressure file, in percent of wall time stalled.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Pressure {
    pub(super) avg10: f32,
    pub(super) avg60: f32,
}

impl Pressure {
    fn worst(self) -> f32 {
        self.avg10.max(self.avg60)
    }
}

/// Parse the `some` line of a `/proc/pressure/<resource>` file.
fn parse_pressure(text: &str) -> Option<Pressure> {
    let mut fields = text
        .lines()
        .find_map(|line| line.strip_prefix("some "))?
        .split_ascii_whitespace();
    let mut average = |key: &str| {
        fields
            .next()?
            .strip_prefix(key)?
            .strip_prefix('=')?
            .parse::<f32>()
            .ok()
    };
    Some(Pressure {
        avg10: average("avg10")?,
        avg60: average("avg60")?,
    })
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
    fn sleep(&self, duration: Duration);
    fn queue_dir(&self) -> PathBuf;
    fn queue_lock(&self) -> PathBuf;
    fn owner(&self) -> Owner;
    fn owner_is_live(&self, owner: &Owner) -> bool;
    /// Start listening for Ctrl-C, until the returned listener is dropped.
    fn interrupts(&self) -> io::Result<Interrupts>;
    /// `Err` is the missing source, for the fail-fast message.
    fn pressure(&self, resource: Resource) -> Result<Pressure, String>;
    fn memory_available(&self) -> Result<u64, String>;
    fn disk_free(&self, checkout: &Path) -> Result<u64, String>;
    fn workspaces(&self) -> Result<Vec<WorkspaceId>, String>;
    /// Every agent row of one workspace; `Err` names why it cannot be read.
    fn agents(&self, workspace: &WorkspaceId) -> Result<Vec<AgentState>, String>;
    /// The open supervised runs one loop task owns in a workspace.
    fn task_runs(&self, workspace: &WorkspaceId, task: &str) -> Result<Vec<RunRecord>, String>;
}

/// The real machine: the home's queue, `/proc`, and the known workspaces.
pub(super) struct SystemHost;

impl Host for SystemHost {
    fn now_ms(&self) -> u64 {
        crate::utils::time::unix_now_ms()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
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

    fn interrupts(&self) -> io::Result<Interrupts> {
        let raised = Arc::new(AtomicBool::new(false));
        let id = signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&raised))?;
        Ok(Interrupts {
            raised,
            id: Some(id),
        })
    }

    fn pressure(&self, resource: Resource) -> Result<Pressure, String> {
        let path = format!("/proc/pressure/{}", resource.name());
        let text = std::fs::read_to_string(&path).map_err(|err| format!("{path}: {err}"))?;
        parse_pressure(&text).ok_or_else(|| format!("{path}: no `some` line"))
    }

    fn memory_available(&self) -> Result<u64, String> {
        crate::proc::memory::sample()
            .map(|memory| memory.available_bytes)
            .map_err(|err| format!("/proc/meminfo: {err}"))
    }

    // The statvfs field widths differ by platform; the conversion is a no-op on Linux.
    #[allow(clippy::useless_conversion)]
    fn disk_free(&self, checkout: &Path) -> Result<u64, String> {
        let stat = nix::sys::statvfs::statvfs(checkout)
            .map_err(|err| format!("{}: {err}", checkout.display()))?;
        Ok(u64::from(stat.blocks_available()).saturating_mul(u64::from(stat.fragment_size())))
    }

    fn workspaces(&self) -> Result<Vec<WorkspaceId>, String> {
        Ok(crate::workspace::known_workspaces()
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
        Ok(store
            .snapshot_cached()
            .map_err(|err| unreadable(&format!("{err:#}")))?
            .agents)
    }

    fn task_runs(&self, workspace: &WorkspaceId, task: &str) -> Result<Vec<RunRecord>, String> {
        let unreadable =
            |err: &dyn fmt::Display| format!("workspace {workspace} unreadable: {err}");
        let state = StatePaths::for_workspace(workspace.clone()).map_err(|err| unreadable(&err))?;
        let mut runs =
            crate::harness::run::list(&state).map_err(|err| unreadable(&format!("{err:#}")))?;
        runs.retain(|run| !run.status.is_terminal() && run.loop_task.as_deref() == Some(task));
        Ok(runs)
    }
}

/// Ctrl-C while a run is held. A check's handler, once dropped, leaves
/// SIGINT ignored, so a fire that runs a check listens from before it and
/// hands this listener to the hold; a fire that ran none listens once held.
pub(super) struct Interrupts {
    raised: Arc<AtomicBool>,
    id: Option<signal_hook::SigId>,
}

impl Interrupts {
    /// A listener raised through `raised` rather than a signal.
    #[cfg(test)]
    pub(super) fn raised_by(raised: Arc<AtomicBool>) -> Self {
        Self { raised, id: None }
    }

    pub(super) fn raised(&self) -> bool {
        self.raised.load(Ordering::SeqCst)
    }
}

impl Drop for Interrupts {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            signal_hook::low_level::unregister(id);
        }
    }
}

/// The run asking for a turn.
#[derive(Clone, Debug)]
pub(super) struct Run {
    pub(super) task: String,
    pub(super) root: PathBuf,
    pub(super) checkout: PathBuf,
    /// Where the launch writes, read for the free-disk floor.
    pub(super) disk: PathBuf,
    pub(super) workspace: WorkspaceId,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum HoldKind {
    Queue,
    TaskCap,
    MachineCap,
    Pressure,
    Memory,
    Disk,
    Store,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct Hold {
    kind: HoldKind,
    text: String,
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
    #[serde(flatten)]
    state: TicketState,
}

impl Ticket {
    fn same_task(&self, other: &Self) -> bool {
        self.task == other.task && self.root == other.root
    }
}

/// The turn one admitted run holds until its launch is reported or it is
/// dropped unreported.
#[derive(Clone)]
pub struct Turn(Arc<TurnInner>);

struct TurnInner {
    host: Arc<dyn Host>,
    ticket: PathBuf,
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
    /// Stamp the committed launch on the ticket. The turn then passes on once
    /// the provider reports the agent or `pace` has passed. A second report,
    /// which a verify retry produces, changes nothing.
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
            ticket.state = TicketState::Launched {
                at_ms: host.now_ms(),
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

/// How one gated run left the queue.
#[derive(Debug)]
pub(super) enum Admission {
    /// Nothing is configured to gate on; no ticket was written.
    Open,
    Turn {
        turn: Turn,
        /// How long the run was held, when it was held at all.
        waited: Option<Duration>,
    },
    Skipped {
        reason: String,
    },
    /// Ctrl-C ended the hold; the ticket is gone and nothing launches.
    Interrupted,
}

/// Whether any key makes the gate do something.
fn gates(config: &ThrottleConfig) -> bool {
    !config.pace().is_zero()
        || config.max_active.is_some()
        || config.max_active_per_task.is_some()
        || Resource::ALL
            .iter()
            .any(|resource| resource.limit(config).is_some())
        || config.min_memory.is_some()
        || config.min_disk.is_some()
}

/// The configured limit this host cannot read, as the fail-fast message.
fn unreadable(host: &dyn Host, config: &ThrottleConfig) -> Option<String> {
    for resource in Resource::ALL {
        if resource.limit(config).is_some()
            && let Err(source) = host.pressure(resource)
        {
            return Some(format!(
                "`loop.throttle.{}-pressure` is set but pressure cannot be read ({source}); remove the key from loop.toml, or enable PSI (boot with `psi=1`)",
                resource.name()
            ));
        }
    }
    if config.min_memory.is_some()
        && let Err(source) = host.memory_available()
    {
        return Some(format!(
            "`loop.throttle.min-memory` is set but available memory cannot be read ({source}); remove the key from loop.toml"
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

/// Take a turn for `run`, waiting up to `max-wait`. `on_hold` hears each new
/// blocking reason. A caller already `listening` for Ctrl-C hands its
/// listener over; without one, the run listens once it is held.
pub(super) fn admit(
    host: &Arc<dyn Host>,
    config: &ThrottleConfig,
    run: &Run,
    listening: Option<Interrupts>,
    on_hold: &mut dyn FnMut(&str),
) -> Result<Admission> {
    if !gates(config) {
        return Ok(Admission::Open);
    }
    if let Some(problem) = unreadable(host.as_ref(), config) {
        return Err(ThrottleError::Unreadable(problem));
    }
    let enqueued_ms = host.now_ms();
    let path = {
        let _lock = WorkspaceLock::acquire(&host.queue_lock())?;
        enqueue(host.as_ref(), run, enqueued_ms)?
    };
    // From here an early return drops the turn, which removes the ticket.
    let turn = Turn(Arc::new(TurnInner {
        host: Arc::clone(host),
        ticket: path.clone(),
        reported: AtomicBool::new(false),
    }));
    let max_wait_ms = duration_ms(config.max_wait());
    let skipped = |reason: &str| {
        Ok(Admission::Skipped {
            reason: format!(
                "{reason}; held {}",
                format_duration_compact(config.max_wait())
            ),
        })
    };
    let mut blocked: Option<(u64, Hold)> = None;
    let mut heard: Option<String> = None;
    let mut interrupts = listening;
    loop {
        let (waited_ms, hold) = {
            let _lock = WorkspaceLock::acquire(&host.queue_lock())?;
            // A Ctrl-C heard while sleeping or taking the lock wins over both
            // the deadline and an admission.
            if interrupts.as_ref().is_some_and(Interrupts::raised) {
                return Ok(Admission::Interrupted);
            }
            // Read after the lock, so time spent waiting for it counts.
            let now = host.now_ms();
            let waited_ms = now.saturating_sub(enqueued_ms);
            // A held run past its deadline never gets another chance to start.
            if let Some(reason) = &heard
                && waited_ms >= max_wait_ms
            {
                return skipped(reason);
            }
            let hold = pass(host.as_ref(), config, run, &path, now, &mut blocked)?;
            (waited_ms, hold)
        };
        let Some(hold) = hold else {
            return Ok(Admission::Turn {
                turn,
                waited: heard.is_some().then(|| Duration::from_millis(waited_ms)),
            });
        };
        if waited_ms >= max_wait_ms {
            return skipped(&hold.text);
        }
        // Listen before announcing the hold, so a Ctrl-C prompted by it lands.
        if interrupts.is_none() {
            interrupts = Some(host.interrupts().map_err(ThrottleError::Interrupts)?);
        }
        if heard.as_deref() != Some(hold.text.as_str()) {
            on_hold(&hold.text);
            heard = Some(hold.text);
        }
        host.sleep(RECHECK);
    }
}

fn enqueue(host: &dyn Host, run: &Run, enqueued_ms: u64) -> Result<PathBuf> {
    let path = paths::loop_throttle_ticket_in(&host.queue_dir(), enqueued_ms);
    write_ticket(
        &path,
        &Ticket {
            owner: host.owner(),
            task: run.task.clone(),
            root: run.root.clone(),
            checkout: run.checkout.clone(),
            enqueued_ms,
            state: TicketState::Waiting { reason: None },
        },
    )?;
    Ok(path)
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// One pass over the queue, under its lock: `None` admits the ticket at
/// `mine`, and a hold is what it waits on.
fn pass(
    host: &dyn Host,
    config: &ThrottleConfig,
    run: &Run,
    mine: &Path,
    now: u64,
    blocked: &mut Option<(u64, Hold)>,
) -> Result<Option<Hold>> {
    let queue = sweep(host, Some(config.pace()), now)?;
    let position = queue
        .iter()
        .position(|(path, _)| path == mine)
        .ok_or_else(|| ThrottleError::Vanished(mine.to_path_buf()))?;
    let mut ticket = queue[position].1.clone();
    let hold = match queue_hold(&queue, position) {
        Some(hold) => Some(hold),
        None => match blocked {
            Some((at, hold)) if now.saturating_sub(*at) < SAMPLE_EVERY_MS => Some(hold.clone()),
            _ => {
                let sampled = limit_hold(host, config, run)?;
                *blocked = sampled.clone().map(|hold| (now, hold));
                sampled
            }
        },
    };
    let state = match &hold {
        Some(hold) => TicketState::Waiting {
            reason: Some(hold.clone()),
        },
        None => TicketState::Admitted,
    };
    if ticket.state != state {
        ticket.state = state;
        write_ticket(mine, &ticket)?;
    }
    Ok(hold)
}

/// What in the queue keeps the ticket at `position` from being the candidate:
/// a held turn, or a ticket ahead that nothing lets it step over.
fn queue_hold(queue: &[(PathBuf, Ticket)], position: usize) -> Option<Hold> {
    let mine = &queue[position].1;
    let turn_held = queue.iter().any(|(_, ticket)| {
        matches!(
            ticket.state,
            TicketState::Admitted | TicketState::Launched { .. }
        )
    });
    let task_cap = |ticket: &Ticket| {
        matches!(
            &ticket.state,
            TicketState::Waiting { reason: Some(reason) } if reason.kind == HoldKind::TaskCap
        )
    };
    // The cap is the task's: once a waiting run records it, that run and
    // every later waiting run of its task step aside for other tasks, and
    // each keeps its place among its own.
    let blocks_me = |index: usize| {
        let ticket = &queue[index].1;
        if !matches!(ticket.state, TicketState::Waiting { .. }) {
            return true;
        }
        let behind_its_cap = queue[..=index]
            .iter()
            .any(|(_, earlier)| earlier.same_task(ticket) && task_cap(earlier));
        ticket.same_task(mine) || !behind_its_cap
    };
    let ahead = (0..position).filter(|&index| blocks_me(index)).count();
    if !turn_held && ahead == 0 {
        return None;
    }
    let starts = ahead.max(1);
    Some(Hold {
        kind: HoldKind::Queue,
        text: format!("{starts} start{} ahead", if starts == 1 { "" } else { "s" }),
    })
}

/// Read the queue in order and remove every ticket that is over: a waiting or
/// admitted one whose owner died, and a launched one whose turn has passed.
/// `pace` is `None` for a reader that only reaps the dead.
fn sweep(host: &dyn Host, pace: Option<Duration>, now: u64) -> Result<Vec<(PathBuf, Ticket)>> {
    let mut queue = Vec::new();
    for path in ticket_paths(&host.queue_dir())? {
        let ticket = match read_ticket(&path) {
            Ok(Some(ticket)) => ticket,
            Ok(None) => continue,
            Err(err @ ThrottleError::CorruptTicket { .. }) => {
                tracing::warn!(error = %err, "removing corrupt loop throttle ticket");
                remove_ticket(&path);
                continue;
            }
            Err(err) => return Err(err),
        };
        let over = match &ticket.state {
            TicketState::Waiting { .. } | TicketState::Admitted => {
                !host.owner_is_live(&ticket.owner)
            }
            TicketState::Launched {
                at_ms,
                workspace,
                agent,
            } => pace.is_some_and(|pace| {
                now.saturating_sub(*at_ms) >= duration_ms(pace)
                    || host
                        .agents(workspace)
                        .is_ok_and(|agents| provider_reported(&agents, agent))
            }),
        };
        if over {
            remove_ticket(&path);
        } else {
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

/// An agent that is working now: mid-turn, not ended, not inside a bounded
/// compaction window, and its owner not gone.
fn working(agent: &AgentState, now: Timestamp) -> bool {
    agent.ended_at.is_none()
        && agent.effective_status() == AgentStatus::Running
        && !agent.is_compacting(now)
        && crate::store::runtime::agent_liveness(agent)
            != crate::store::runtime::AgentLiveness::Dead
}

/// The working agents task `task` started: stamped with it, or matched by one
/// of its open runs.
fn task_working<'a>(
    agents: &'a [AgentState],
    runs: &[RunRecord],
    task: &str,
    now: Timestamp,
) -> Vec<&'a AgentState> {
    agents
        .iter()
        .filter(|agent| working(agent, now))
        .filter(|agent| {
            agent.loop_task.as_deref() == Some(task)
                || runs.iter().any(|run| run.matches_agent(agent))
        })
        .collect()
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

fn cap_hold(
    kind: HoldKind,
    label: &str,
    cap: u32,
    count: usize,
    mut names: Vec<String>,
) -> Option<Hold> {
    if count < cap as usize {
        return None;
    }
    if count > names.len() {
        names.push(format!("+{}", count - names.len()));
    }
    Some(Hold {
        kind,
        text: format!("{label} {count} >= {cap} ({})", names.join(", ")),
    })
}

/// The first configured cap or limit that is not clear for `run`, sampled now.
/// A reading that cannot be taken is an error; a store that cannot be read is
/// a hold, never a short count.
fn limit_hold(host: &dyn Host, config: &ThrottleConfig, run: &Run) -> Result<Option<Hold>> {
    let now = host_time(host);
    let store_hold = |text: String| {
        Ok(Some(Hold {
            kind: HoldKind::Store,
            text: format!("cannot count active agents: {text}"),
        }))
    };
    if let Some(cap) = config.max_active_per_task {
        let read = host.agents(&run.workspace).and_then(|agents| {
            let runs = host.task_runs(&run.workspace, &run.task)?;
            Ok((agents, runs))
        });
        let (agents, runs) = match read {
            Ok(read) => read,
            Err(text) => return store_hold(text),
        };
        let counted = task_working(&agents, &runs, &run.task, now);
        let mut names = Vec::new();
        name_counted(&mut names, &agents, &counted);
        if let Some(hold) = cap_hold(
            HoldKind::TaskCap,
            "task's active agents",
            cap,
            counted.len(),
            names,
        ) {
            return Ok(Some(hold));
        }
    }
    if let Some(cap) = config.max_active {
        let workspaces = match host.workspaces() {
            Ok(workspaces) => workspaces,
            Err(text) => return store_hold(text),
        };
        let mut count = 0;
        let mut names = Vec::new();
        for workspace in workspaces {
            let agents = match host.agents(&workspace) {
                Ok(agents) => agents,
                Err(text) => return store_hold(text),
            };
            let counted: Vec<&AgentState> =
                agents.iter().filter(|agent| working(agent, now)).collect();
            name_counted(&mut names, &agents, &counted);
            count += counted.len();
        }
        if let Some(hold) = cap_hold(HoldKind::MachineCap, "active agents", cap, count, names) {
            return Ok(Some(hold));
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
                text: format!("{} pressure {worst:.0}% >= {limit}%", resource.name()),
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
                text: format!(
                    "available memory {} < {}",
                    decimal_bytes(available),
                    decimal_bytes(floor)
                ),
            }));
        }
    }
    if let Some(floor) = config.min_disk_bytes() {
        let free = host
            .disk_free(&run.disk)
            .map_err(|detail| ThrottleError::Reading {
                reading: "free disk",
                detail,
            })?;
        if free < floor {
            return Ok(Some(Hold {
                kind: HoldKind::Disk,
                text: format!(
                    "free disk {} < {} at {}",
                    decimal_bytes(free),
                    decimal_bytes(floor),
                    run.disk.display()
                ),
            }));
        }
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
    /// The blocking reason, once the run's first pass recorded one.
    pub reason: Option<String>,
    /// 1-based place among the runs waiting or holding the turn.
    pub position: usize,
}

/// The waiting runs of task `name` under `root`, in queue order. Read-only
/// and lock-free: a ticket is replaced by atomic rename, so every read is whole.
pub fn held(name: &str, root: &Path) -> Vec<Held> {
    held_in(&SystemHost, name, root)
}

fn held_in(host: &dyn Host, name: &str, root: &Path) -> Vec<Held> {
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
            (ticket.task == name && ticket.root == root && host.owner_is_live(&ticket.owner)).then(
                || Held {
                    pid: ticket.owner.pid,
                    checkout: ticket.checkout.clone(),
                    since_ms: ticket.enqueued_ms,
                    reason: reason.as_ref().map(|hold| hold.text.clone()),
                    position: index + 1,
                },
            )
        })
        .collect()
}

/// Current pressure pairs in avg10/avg60 order, omitting unavailable resources.
pub fn compact_load() -> Option<String> {
    compact_load_in(&SystemHost)
}

fn compact_load_in(host: &dyn Host) -> Option<String> {
    let readings = Resource::ALL
        .into_iter()
        .filter_map(|resource| {
            let pressure = host.pressure(resource).ok()?;
            Some(format!(
                "{} {:.0}%/{:.0}%",
                resource.name(),
                pressure.avg10,
                pressure.avg60
            ))
        })
        .collect::<Vec<_>>();
    (!readings.is_empty()).then(|| format!("{} (avg10/avg60)", readings.join(" · ")))
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
        task: name.to_owned(),
        root,
        disk: disk_at(entry, &config.agents.worktree, checkout.clone()),
        checkout,
        workspace: state.workspace_id,
    };
    readings_in(&SystemHost, &config.r#loop.throttle, &run)
}

/// Where a launch of `entry` into `checkout` writes, for the free-disk floor: a
/// fresh worktree's target, at its nearest existing ancestor, else `checkout`.
pub(super) fn disk_at(entry: &TaskEntry, worktrees: &WorktreeConfig, checkout: PathBuf) -> PathBuf {
    let Some(name) = entry.worktree.as_deref().map(str::trim) else {
        return checkout;
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
    let now = host_time(host);
    let mut rows = Vec::new();
    for resource in Resource::ALL {
        if let Ok(pressure) = host.pressure(resource) {
            rows.push((
                resource.label(),
                limited(
                    format!(
                        "avg10 {:.0}% · avg60 {:.0}%",
                        pressure.avg10, pressure.avg60
                    ),
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
    let active = host.workspaces().and_then(|workspaces| {
        workspaces.iter().try_fold(0, |count, workspace| {
            Ok(count
                + host
                    .agents(workspace)?
                    .iter()
                    .filter(|agent| working(agent, now))
                    .count())
        })
    });
    if let Ok(active) = active {
        rows.push((
            "active agents",
            limited(
                active.to_string(),
                config.max_active.map(|cap| format!("max {cap}")),
            ),
        ));
    }
    if let Some(cap) = config.max_active_per_task {
        let counted = host.agents(&run.workspace).and_then(|agents| {
            let runs = host.task_runs(&run.workspace, &run.task)?;
            Ok(task_working(&agents, &runs, &run.task, now).len())
        });
        if let Ok(counted) = counted {
            rows.push((
                "task's active agents",
                limited(counted.to_string(), Some(format!("max {cap}"))),
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

/// The host's clock as a timestamp, for the windows agent rows carry.
fn host_time(host: &dyn Host) -> Timestamp {
    i64::try_from(host.now_ms())
        .ok()
        .and_then(|ms| Timestamp::from_millisecond(ms).ok())
        .unwrap_or(Timestamp::MAX)
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
pub(super) mod tests;
