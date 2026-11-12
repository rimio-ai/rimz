//! Durable workflow records and append-only history; transitions belong to the harness.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::disk::lock::WorkspaceLock;
use crate::disk::paths::{StatePaths, WorkflowPaths};
use crate::ids::{AgentKind, AgentSessionId, HoldKind, LaneName, MessageId, RunId, WorkflowRunId};

/// The workflow language version, which is also the run record's: a sealed definition means
/// what the binary of this version reads it to mean, so a change to what one means raises it,
/// and `load` refuses a record of any other version before decoding it.
pub const VERSION: u32 = 8;

#[derive(Debug, thiserror::Error)]
pub enum WorkflowStoreErr {
    #[error(transparent)]
    Atomic(#[from] crate::disk::atomic::AtomicErr),
    #[error(transparent)]
    Path(#[from] crate::disk::paths::PathErr),
    #[error("workflow I/O at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("workflow JSON at {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error(
        "workflow run {run} at {path} has record version {version}, written by another RimZ \
         version; this RimZ reads only version {current}",
        current = VERSION
    )]
    Version {
        run: String,
        path: PathBuf,
        version: u32,
    },
}

pub type Result<T> = std::result::Result<T, WorkflowStoreErr>;

impl WorkflowStoreErr {
    fn io(path: &std::path::Path) -> impl FnOnce(std::io::Error) -> Self + '_ {
        move |source| Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    Worktree,
    Project,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Scope {
    pub kind: ScopeKind,
    pub path: PathBuf,
}

/// A workflow name and the name of its recorded scope's own lane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunAddress {
    pub workflow: String,
    pub lane: LaneName,
}

#[derive(Debug, thiserror::Error)]
#[error("run address must be <workflow>#<lane> with nonempty parts and no `#` in the lane")]
pub struct RunAddressErr;

impl std::fmt::Display for RunAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}#{}", self.workflow, self.lane)
    }
}

impl std::str::FromStr for RunAddress {
    type Err = RunAddressErr;

    fn from_str(raw: &str) -> std::result::Result<Self, Self::Err> {
        let (workflow, lane) = raw
            .split_once('#')
            .filter(|(workflow, lane)| {
                !workflow.is_empty() && !lane.is_empty() && !lane.contains('#')
            })
            .ok_or(RunAddressErr)?;
        Ok(Self {
            workflow: workflow.into(),
            lane: LaneName::dashed(lane),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Active,
    Parked,
    Done,
    Failed,
    Canceled,
}

impl RunState {
    /// The state's wire word, as every surface prints it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Parked => "parked",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Canceled)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParkReason {
    User,
    Cap,
    Deadline,
    Timeout,
    Invalid,
    Asked,
    Check,
}

impl ParkReason {
    /// The reason's wire word, as every surface prints it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Cap => "cap",
            Self::Deadline => "deadline",
            Self::Timeout => "timeout",
            Self::Invalid => "invalid",
            Self::Asked => "asked",
            Self::Check => "check",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Park {
    pub reason: ParkReason,
    pub text: Option<String>,
    pub at: Timestamp,
}

impl Park {
    /// `<reason>[: <text>]`, the park as one phrase.
    pub fn label(&self) -> String {
        match &self.text {
            Some(text) => format!("{}: {text}", self.reason.as_str()),
            None => self.reason.as_str().to_owned(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum Key {
    String(String),
    Integer(i64),
    Unsigned(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptKind {
    Script,
    Agent,
    Judge,
    Team,
    Workflow,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Host {
    pub pid: u32,
    pub start: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HostChild {
    pub pid: u32,
    pub start: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttemptRun {
    pub run_id: RunId,
    pub handle: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndCause {
    Lifecycle,
    Timeout,
    Deadline,
    Cancel,
    Restart { continued: bool },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Restart {
    pub resume: Option<crate::ids::AgentSessionId>,
    /// A previous attempt being stopped again, whose end and cost are already journaled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ending {
    pub outcome: AttemptOutcome,
    pub cause: EndCause,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<Verdict>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LiveAttempt {
    pub kind: AttemptKind,
    #[serde(
        default = "turn_lifecycle",
        skip_serializing_if = "StepLifecycle::is_turn"
    )]
    pub lifecycle: StepLifecycle,
    pub seq: u64,
    pub node: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_past_limit: Option<String>,
    pub started_at: Timestamp,
    pub host: Host,
    #[serde(default)]
    pub child: Option<HostChild>,
    #[serde(default)]
    pub run: Option<AttemptRun>,
    #[serde(default)]
    pub head_start: Option<String>,
    #[serde(default)]
    pub digests: BTreeMap<String, String>,
    #[serde(default)]
    pub ending: Option<Ending>,
    #[serde(default)]
    pub retry: Option<Value>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub woken: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asking: Option<Asking>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_since: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub asked_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judged: Option<TurnEnd>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<PushBack>,
    /// A message step's notice id, minted before the queue so a retry never queues twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<MessageId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushBack {
    pub message: MessageId,
    pub queued: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnEnd {
    pub agent_id: AgentSessionId,
    pub started_at: Timestamp,
    pub ended_at: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    pub node: String,
    pub entered_at: Timestamp,
    pub at: Timestamp,
    pub target: String,
    pub object: Value,
    pub reason: ParkReason,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HeldSession {
    pub attempt: LiveAttempt,
    pub entered_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnEndAction {
    Kept,
    Pushed,
    Closed,
    Discarded,
    Interrupted,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StepLifecycle {
    Turn,
    Session,
}

fn turn_lifecycle() -> StepLifecycle {
    StepLifecycle::Turn
}

impl StepLifecycle {
    pub fn is_turn(&self) -> bool {
        matches!(self, Self::Turn)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    Ended,
    Died,
    Timeout,
    Asked,
    Limited,
    Canceled,
}

impl AttemptOutcome {
    /// The attempt's wire word, as every surface prints it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ended => "ended",
            Self::Died => "died",
            Self::Timeout => "timeout",
            Self::Asked => "asked",
            Self::Limited => "limited",
            Self::Canceled => "canceled",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Limited {
    pub reset_at: Option<Timestamp>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Asked {
    pub text: String,
    pub kind: String,
    pub handle: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Asking {
    pub since: Timestamp,
    #[serde(flatten)]
    pub ask: Asked,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TeamRef {
    pub name: String,
    /// The cohort's lane; none is the `external` instance.
    #[serde(
        serialize_with = "crate::ids::TeamInstanceId::serialize_lane",
        deserialize_with = "crate::ids::TeamInstanceId::deserialize_lane"
    )]
    pub channel: Option<LaneName>,
    pub stage: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChildRef {
    pub handle: String,
    pub kind: AgentKind,
    pub team: Option<TeamRef>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Children {
    pub stopped: Vec<ChildRef>,
    pub live: Vec<ChildRef>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PreviousAttempt {
    pub step: String,
    pub outcome: AttemptOutcome,
    pub seq: u64,
    pub visit: u64,
    pub key: Option<Key>,
    #[serde(default)]
    pub routed_by: Option<String>,
    pub started_at: Timestamp,
    pub ended_at: Timestamp,
    pub handle: Option<String>,
    pub run_id: Option<RunId>,
    pub claim_path: PathBuf,
    pub head_start: Option<String>,
    pub head_end: Option<String>,
    #[serde(default)]
    pub code: Option<i32>,
    #[serde(default)]
    pub log: Option<PathBuf>,
    pub children: Option<Children>,
    #[serde(default)]
    pub limited: Option<Limited>,
    #[serde(default)]
    pub asked: Option<Asked>,
    #[serde(default)]
    pub cost_usd: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Wait {
    pub node: String,
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub case: Option<u32>,
    #[serde(default)]
    pub detail: Option<String>,
    /// A hold's last answer while it was unknown: the answer's reason, or empty when it gave
    /// none. The hold keeps its start; readers describe the node as unknown until a true
    /// answer of the held case clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "HoldKind::deserialize_optional"
    )]
    pub hold: Option<HoldKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<AgentKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gives_up_at: Option<Timestamp>,
    pub first: Timestamp,
    pub last: Timestamp,
    pub count: u64,
    pub recheck_at: Option<Timestamp>,
}

impl Wait {
    pub(crate) fn is_hold(&self) -> bool {
        self.outcome == "hold"
    }
}

/// What a run's agent starts do at a capacity hold: wait in front of the node, or launch past
/// the hold and record it on the attempt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartCapacity {
    #[default]
    Wait,
    Skip,
}

impl StartCapacity {
    pub fn is_wait(&self) -> bool {
        *self == Self::Wait
    }
}

/// Where a run's definition came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOrigin {
    /// A workflow directory under the machine home; `workflow` is its name.
    #[default]
    Deployed,
    /// A loop task's compiled definition, sealed in the run's own root; `workflow` is the task.
    Inline,
}

/// Whether a run still owes its loop task the report of how it ended. The fire's task sets it
/// owed at creation, and only the scheduler's collector marks it sent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopReport {
    /// Nothing is owed: no task started the run, or it was recorded before reports existed.
    #[default]
    None,
    /// A loop task started the run; its report has not fully landed: the history row, the
    /// strike update, or both are missing. On a terminal record the run awaits its collection,
    /// which the command that ended it runs next, and which the scheduler's tick retries.
    Owed,
    /// The terminal outcome reached the loop history and, while the task's row existed, the
    /// strike count.
    Sent,
}

/// How a run ended, in the engine's terms: written in the record write that makes the run
/// terminal, so a reader of the record learns the end without the journal or the sealed graph.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunOutcome {
    pub ending: RunEnding,
    /// The summed cost of the run's attempts and evaluations; none when none recorded a cost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// Whether the run entered an agent or message step.
    pub acted: bool,
    /// Why a failed run's last attempt ended: the newest reason an attempt or a budget route
    /// recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The decision that ended a run; a failure the run's last decision explains names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum RunEnding {
    Done,
    Failed,
    Canceled,
    /// The run's last decision was the `capacity` route out of an agent step, after waiting
    /// `waited_ms` for the capacity named by `reason`.
    CapacityGaveUp {
        waited_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// The last attempt ended at its budget; `turned` says whether an agent turn ran, which a
    /// refused launch or judge never did.
    Budget {
        turned: bool,
    },
}

/// The loop fire that started a run, as the run's briefs name it. `booked_by` is the
/// fired task row's booking chain as the fire read it, which every agent the run launches
/// carries; a fire of a row with no chain, or of a record written before the field, has none.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum RunFire {
    /// `rimz loop fire <task>` started the run by hand.
    Manual {
        task: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        booked_by: Vec<crate::agents::LaunchedBy>,
    },
    /// The task's trigger started the run; `trigger` words it as the brief does
    /// (``on the cron schedule `0 * * * *` ``), and a watch trigger has none.
    Scheduled {
        task: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trigger: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        booked_by: Vec<crate::agents::LaunchedBy>,
    },
}

impl RunFire {
    /// The fired task row's booking chain, nearest booker first.
    pub fn booked_by(&self) -> &[crate::agents::LaunchedBy] {
        match self {
            Self::Manual { booked_by, .. } | Self::Scheduled { booked_by, .. } => booked_by,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub break_agents: Vec<String>,
    pub id: WorkflowRunId,
    #[serde(default)]
    pub origin: RunOrigin,
    pub workflow: String,
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub manual: bool,
    #[serde(default)]
    pub loop_report: LoopReport,
    pub spec_dir: PathBuf,
    pub spec_digest: String,
    pub prompt: Option<String>,
    pub scope: Scope,
    /// A loop fire cut `scope` as its own fresh worktree, so loop keys the run on
    /// `project_root`, through [`RunRecord::firing_checkout`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fire_tree: bool,
    /// The run's give-up instant, a start argument: a capacity wait at an agent step that sets
    /// no `admission.give-up` gives up then, however late in the run its first hold comes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub give_up: Option<Timestamp>,
    /// Whether the run's starts wait at a capacity hold or launch past it, a start argument.
    #[serde(default, skip_serializing_if = "StartCapacity::is_wait")]
    pub capacity: StartCapacity,
    /// The loop task row's `surplus` value as written when its fire started the run. The run's
    /// judges gate on it and on `surplus_after`, never on the live row; a record with neither
    /// applies no surplus gate to its judges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surplus: Option<String>,
    /// The loop task row's `surplus-after` value as written when its fire started the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surplus_after: Option<String>,
    /// What the loop fire that started the run tells the run's briefs, frozen at creation, so an
    /// edit, rename or removal of the task mid-run leaves every brief as it was; a run
    /// `rimz workflow run` started carries none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fire: Option<RunFire>,
    pub project_root: PathBuf,
    pub created_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<PathBuf>,
    pub state: RunState,
    pub park: Option<Park>,
    pub node: String,
    pub entered_at: Timestamp,
    pub input: Option<Value>,
    pub key: Option<Key>,
    pub recheck_at: Option<Timestamp>,
    pub wait: Option<Wait>,
    /// Visits to each step since its last full reset.
    pub visits: BTreeMap<String, u64>,
    pub visit_grant: u64,
    pub deadline: Option<Timestamp>,
    pub deadline_fired: bool,
    pub next_seq: u64,
    pub attempt: Option<LiveAttempt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<HeldSession>,
    /// A manual restart of the current step, pending until its replacement launch is published.
    /// A route to another node, a park, a cancel, or a terminal state drops it, so no other step
    /// launches with its resume session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<Restart>,
    pub prev_attempt: Option<PreviousAttempt>,
    pub terminal_at: Option<Timestamp>,
    /// How the run ended, written with the terminal state; none while the run is live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<RunOutcome>,
    #[serde(default)]
    pub check_retry: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_retry_context: Option<Value>,
    /// The current step entry spent its one retry of a launch whose eviction failed before it
    /// started anything; cleared wherever the run enters a step afresh.
    #[serde(default)]
    pub launch_retry: bool,
}

impl RunRecord {
    /// The agent session still owned by this run, including a session held under its rest check.
    pub fn session_attempt(&self) -> Option<&LiveAttempt> {
        self.held
            .as_ref()
            .map(|held| &held.attempt)
            .or(self.attempt.as_ref())
            .filter(|attempt| attempt.kind == AttemptKind::Agent)
    }

    /// Resolve the current step's root among the caller's addressable sessions.
    pub fn step_agent<'a>(
        &self,
        agents: &[&'a crate::agents::AgentState],
    ) -> Option<&'a crate::agents::AgentState> {
        let attempt = self.session_attempt()?;
        let leader = attempt
            .run
            .as_ref()
            .and_then(|run| run.handle.strip_prefix('@'));
        agents
            .iter()
            .copied()
            .filter(|agent| agent.parent_agent_id.is_none())
            .filter(|agent| {
                agent.workflow.as_ref().is_some_and(|pin| {
                    pin.run == self.id
                        && pin.seq == attempt.seq
                        && pin.role == crate::agents::WorkflowAttemptRole::Step
                })
            })
            .min_by_key(|agent| leader.is_none_or(|name| agent.name.as_deref() != Some(name)))
    }

    /// The outcome that routed the run into its node, kept across a cap at that node.
    pub fn routed_by(&self) -> Option<&str> {
        self.input.as_ref().and_then(|input| {
            let field = if input["outcome"] == "cap" && input["step"] == self.node {
                "routed_by"
            } else {
                "outcome"
            };
            input[field].as_str()
        })
    }

    /// Visits to `node` since its last full reset.
    pub fn visits_to(&self, node: &str) -> u64 {
        self.visits.get(node).copied().unwrap_or_default()
    }

    /// Derive the lane without resolving or reading the checkout, which may be gone:
    /// a run scoped to the project root, per project or per worktree, is trunk's.
    pub fn address(&self) -> RunAddress {
        run_address(
            self.workflow.clone(),
            self.scope.kind,
            &self.scope.path,
            Some(&self.project_root),
        )
    }

    /// The shared predicate for a live run of a loop task.
    pub fn started_by(&self, task: &str) -> bool {
        !self.state.is_terminal() && self.ran_for(task)
    }

    /// The checkout a loop task fired from: the project root for a run in its fire's own tree,
    /// else the run's scope. Overlap, the backlog and the task's history key on it.
    pub fn firing_checkout(&self) -> &Path {
        if self.fire_tree {
            &self.project_root
        } else {
            &self.scope.path
        }
    }

    /// Whether loop task `task` started this run, live or finished.
    pub fn ran_for(&self, task: &str) -> bool {
        self.task.as_deref() == Some(task)
    }

    /// A finished run whose loop task has not yet heard how it ended.
    pub fn owes_loop_report(&self) -> bool {
        self.state.is_terminal() && self.task.is_some() && self.loop_report == LoopReport::Owed
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub seq: Option<u64>,
    pub at: Timestamp,
    pub node: String,
    #[serde(flatten)]
    pub event: JournalEvent,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationOrigin {
    #[default]
    Check,
    Engine,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[expect(
    clippy::large_enum_variant,
    reason = "journal evidence stays inline without an allocation or a public variant change"
)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum JournalEvent {
    TurnEnd {
        action: TurnEndAction,
        turn: TurnEnd,
        check: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// The visit closed by this push-back, numbered within its admitted outcome/key bucket.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        visit: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<MessageId>,
    },
    Evaluation {
        #[serde(default)]
        origin: EvaluationOrigin,
        outcome: String,
        object: Value,
        digests: BTreeMap<String, String>,
        started_at: Timestamp,
        ended_at: Timestamp,
        #[serde(skip_serializing_if = "Option::is_none")]
        stderr_tail: Option<String>,
        #[serde(default)]
        cost_usd: Option<f64>,
        #[serde(default)]
        retry: Option<Value>,
    },
    Attempt {
        outcome: AttemptOutcome,
        visit: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_past_limit: Option<String>,
        key: Option<Key>,
        #[serde(default)]
        routed_by: Option<String>,
        digests: BTreeMap<String, String>,
        started_at: Timestamp,
        ended_at: Timestamp,
        #[serde(default)]
        handle: Option<String>,
        #[serde(default)]
        run_id: Option<RunId>,
        #[serde(default)]
        head_start: Option<String>,
        #[serde(default)]
        head_end: Option<String>,
        #[serde(default)]
        code: Option<i32>,
        #[serde(default)]
        log: Option<PathBuf>,
        #[serde(default)]
        cost_usd: Option<f64>,
        #[serde(default)]
        children: Option<Children>,
        #[serde(default)]
        limited: Option<Limited>,
        #[serde(default)]
        asked: Option<Asked>,
        #[serde(default)]
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<MessageId>,
    },
    VisitsReset {
        step: String,
        outcome: String,
    },
    Restart {
        step: String,
        continued: bool,
    },
    Wait {
        outcome: String,
        #[serde(default)]
        detail: Option<String>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "HoldKind::deserialize_optional"
        )]
        hold: Option<HoldKind>,
        first: Timestamp,
        last: Timestamp,
        count: u64,
        recheck_at: Option<Timestamp>,
    },
    Route {
        outcome: String,
        input: Value,
    },
    Park {
        reason: ParkReason,
        text: Option<String>,
    },
    Terminal {
        state: RunState,
    },
    Cancel {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Written by workflow resume for both parked-run resumption and active-run `--extend`.
    Resume {
        visit_grant: u64,
        deadline: Option<Timestamp>,
    },
    Wake {
        signal: String,
    },
    /// A signal a session step subscribes to, queued for its leader as `message`.
    Signal {
        signal: String,
        message: MessageId,
    },
}

/// Read a run record, refusing one of any other version before its body is decoded.
pub fn load(paths: &WorkflowPaths) -> Result<RunRecord> {
    #[derive(Deserialize)]
    struct Probe {
        version: u32,
    }
    let bytes = std::fs::read(&paths.record).map_err(WorkflowStoreErr::io(&paths.record))?;
    let json = |source| WorkflowStoreErr::Json {
        path: paths.record.clone(),
        source,
    };
    let Probe { version } = serde_json::from_slice(&bytes).map_err(json)?;
    if version != VERSION {
        return Err(WorkflowStoreErr::Version {
            run: paths.home.file_name().map_or_else(
                || paths.home.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            ),
            path: paths.record.clone(),
            version,
        });
    }
    serde_json::from_slice(&bytes).map_err(json)
}

/// Write a definition's text and assets into a definition root: a run's own, sealed before
/// `initialize` publishes the record, or an ejected task's staging directory. Each asset,
/// directory and link name is a relative path inside that root. An asset named in `modes` is
/// written with those permission bits, as a deployed directory's scripts keep their executable
/// bit; the rest take the default mode. Each of `dirs` is created even when no asset sits under
/// it, and each of `links` is written as a symlink to its target as given, which a deployed
/// directory's caller makes relative to the link's own directory so the link resolves inside
/// the root.
pub fn seal_definition(
    root: &Path,
    text: &str,
    assets: &BTreeMap<String, Vec<u8>>,
    modes: &BTreeMap<String, u32>,
    dirs: &[String],
    links: &BTreeMap<String, PathBuf>,
) -> Result<()> {
    std::fs::create_dir_all(root).map_err(WorkflowStoreErr::io(root))?;
    crate::disk::atomic::write_bytes_atomically(
        &crate::disk::paths::workflow_spec_file(root),
        text.as_bytes(),
    )?;
    for name in dirs {
        let dir = sealed_entry(root, name)?;
        std::fs::create_dir_all(&dir).map_err(WorkflowStoreErr::io(&dir))?;
        sync_up(root, dir.parent())?;
    }
    for (name, bytes) in assets {
        let file = sealed_entry(root, name)?;
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).map_err(WorkflowStoreErr::io(parent))?;
        }
        match modes.get(name) {
            Some(&mode) => crate::disk::atomic::write_bytes_atomically_mode(&file, bytes, mode)?,
            None => crate::disk::atomic::write_bytes_atomically(&file, bytes)?,
        }
        sync_up(root, file.parent().and_then(Path::parent))?;
    }
    for (name, target) in links {
        let link = sealed_entry(root, name)?;
        if let Some(parent) = link.parent() {
            std::fs::create_dir_all(parent).map_err(WorkflowStoreErr::io(parent))?;
        }
        crate::disk::atomic::symlink_atomically(target, &link)?;
        sync_up(root, link.parent().and_then(Path::parent))?;
    }
    Ok(())
}

/// Entry `name` of a definition root, refusing a name that leaves it.
fn sealed_entry(root: &Path, name: &str) -> Result<PathBuf> {
    let relative = Path::new(name);
    if relative
        .components()
        .all(|part| matches!(part, std::path::Component::Normal(_)))
    {
        return Ok(root.join(relative));
    }
    Err(WorkflowStoreErr::Io {
        path: root.join(relative),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a definition asset must be a relative path inside its root",
        ),
    })
}

/// Publish each new subdirectory entry down from the root, syncing `from` and every directory
/// above it inside the root.
fn sync_up(root: &Path, from: Option<&Path>) -> Result<()> {
    let Some(from) = from else {
        return Ok(());
    };
    for dir in from.ancestors().take_while(|dir| dir.starts_with(root)) {
        crate::disk::atomic::sync_dir(dir)?;
    }
    Ok(())
}

#[must_use = "durability barrier; check the result"]
pub fn initialize(
    state: &StatePaths,
    record: &RunRecord,
    spec: &[u8],
    guard: &WorkspaceLock,
) -> Result<()> {
    let paths = state.workflow(&record.id);
    std::fs::create_dir_all(&paths.work).map_err(WorkflowStoreErr::io(&paths.work))?;
    match &record.notes {
        Some(relative) => {
            let run = state.notes_run(crate::disk::paths::Class::AutomationNotes, relative);
            let run = run.canonicalize().map_err(WorkflowStoreErr::io(&run))?;
            crate::disk::atomic::symlink_atomically(&run, &paths.notes())?;
        }
        None => {
            std::fs::create_dir_all(paths.notes()).map_err(WorkflowStoreErr::io(&paths.notes()))?
        }
    }
    std::fs::create_dir_all(paths.signs(None)).map_err(WorkflowStoreErr::io(&paths.signs(None)))?;
    // Publish every new directory entry up to the already-existing room root.
    for dir in paths.work.ancestors().take_while(|dir| *dir != state.root) {
        crate::disk::atomic::sync_dir(dir)?;
    }
    crate::disk::atomic::sync_dir(&state.root)?;
    crate::disk::atomic::write_bytes_atomically(&paths.spec, spec)?;
    write(&paths, record, guard)
}

#[must_use = "durability barrier; check the result"]
pub fn write(paths: &WorkflowPaths, record: &RunRecord, _guard: &WorkspaceLock) -> Result<()> {
    Ok(crate::disk::atomic::write_temp_then_rename(
        &paths.record,
        record,
    )?)
}

#[must_use = "durability barrier; check the result"]
pub fn append(paths: &WorkflowPaths, entry: &JournalEntry, _guard: &WorkspaceLock) -> Result<()> {
    use std::io::{Read, Seek, SeekFrom};
    let mut bytes = Vec::new();
    match std::fs::File::open(&paths.journal) {
        Ok(mut file) => {
            if file
                .metadata()
                .map_err(WorkflowStoreErr::io(&paths.journal))?
                .len()
                > 0
            {
                file.seek(SeekFrom::End(-1))
                    .map_err(WorkflowStoreErr::io(&paths.journal))?;
                let mut last = [0];
                file.read_exact(&mut last)
                    .map_err(WorkflowStoreErr::io(&paths.journal))?;
                if last[0] != b'\n' {
                    bytes.push(b'\n');
                }
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(WorkflowStoreErr::Io {
                path: paths.journal.clone(),
                source,
            });
        }
    }
    serde_json::to_writer(&mut bytes, entry).map_err(|source| WorkflowStoreErr::Json {
        path: paths.journal.clone(),
        source,
    })?;
    bytes.push(b'\n');
    crate::disk::atomic::append_record_bytes(&paths.journal, &bytes)?;
    Ok(crate::disk::atomic::sync_file_data(&paths.journal)?)
}

pub fn journal(paths: &WorkflowPaths) -> Result<Vec<JournalEntry>> {
    let bytes = match std::fs::read(&paths.journal) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(WorkflowStoreErr::Io {
                path: paths.journal.clone(),
                source,
            });
        }
    };
    Ok(parse_journal(&bytes, &paths.journal))
}

/// Decode readable rows, ignoring torn or unknown entries as the file reader does.
pub(crate) fn parse_journal(bytes: &[u8], path: &Path) -> Vec<JournalEntry> {
    let mut entries = Vec::new();
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        match serde_json::from_slice(line) {
            Ok(entry) => entries.push(entry),
            Err(err) => {
                tracing::debug!(
                    path = %path.display(), %err, "skipping unreadable workflow history row"
                )
            }
        }
    }
    entries
}

fn run_address(
    workflow: String,
    kind: ScopeKind,
    path: &Path,
    project_root: Option<&Path>,
) -> RunAddress {
    let trunk = kind == ScopeKind::Project || project_root == Some(path);
    let home = if trunk {
        crate::ids::LaneHome::Trunk
    } else {
        crate::ids::LaneHome::Worktree
    };
    let lane = crate::ids::Lane::own(home, path.to_string_lossy());
    RunAddress {
        workflow,
        lane: lane.routing_name(),
    }
}

/// What a field-by-field read of `record.json` finds, whatever version wrote it: each field is
/// decoded on its own, so a missing or malformed field leaves only that field unknown.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunHeader {
    pub version: Option<u64>,
    pub workflow: Option<String>,
    pub task: Option<String>,
    pub manual: bool,
    pub project_root: Option<PathBuf>,
    pub scope_kind: Option<ScopeKind>,
    pub scope_path: Option<PathBuf>,
    pub fire_tree: bool,
    pub created_at: Option<Timestamp>,
    pub state: Option<RunState>,
    pub node: Option<String>,
    pub terminal_at: Option<Timestamp>,
    pub loop_report: Option<LoopReport>,
    pub ending: Option<RunEnding>,
    pub reason: Option<String>,
    pub acted: Option<bool>,
    pub notes: Option<PathBuf>,
}

impl RunHeader {
    fn read(record: &serde_json::Map<String, Value>) -> Self {
        fn field<T: serde::de::DeserializeOwned>(value: Option<&Value>) -> Option<T> {
            value.and_then(|value| T::deserialize(value).ok())
        }
        let outcome = record.get("outcome");
        Self {
            version: field(record.get("version")),
            workflow: field(record.get("workflow")),
            task: field(record.get("task")),
            manual: field(record.get("manual")).unwrap_or(false),
            project_root: field(record.get("project_root")),
            scope_kind: field(record.get("scope").and_then(|scope| scope.get("kind"))),
            scope_path: field(record.get("scope").and_then(|scope| scope.get("path"))),
            fire_tree: field(record.get("fire_tree")).unwrap_or(false),
            created_at: field(record.get("created_at")),
            state: field(record.get("state")),
            node: field(record.get("node")),
            terminal_at: field(record.get("terminal_at")),
            loop_report: field(record.get("loop_report")),
            ending: field(outcome.and_then(|outcome| outcome.get("ending"))),
            reason: field(outcome.and_then(|outcome| outcome.get("reason"))),
            acted: field(outcome.and_then(|outcome| outcome.get("acted"))),
            notes: field(record.get("notes")),
        }
    }

    /// Whether the record names a state that ends a run.
    pub fn is_terminal(&self) -> bool {
        self.state.is_some_and(RunState::is_terminal)
    }

    /// The run's address, derived as [`RunRecord::address`] derives it; a record naming no
    /// scope kind reads as a worktree run.
    pub fn address(&self) -> Option<RunAddress> {
        Some(run_address(
            self.workflow.clone()?,
            self.scope_kind.unwrap_or(ScopeKind::Worktree),
            self.scope_path.as_deref()?,
            self.project_root.as_deref(),
        ))
    }

    /// A finished run whose loop task has not yet heard how it ended.
    pub fn owes_loop_report(&self) -> bool {
        self.is_terminal() && self.task.is_some() && self.loop_report == Some(LoopReport::Owed)
    }
}

/// Why this RimZ cannot continue a run whose record it finds unfinished.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StrandedCause {
    /// An older RimZ wrote the record.
    Older { version: u64 },
    /// The record names no version.
    MissingVersion,
    /// The record's version is not a version number.
    InvalidVersion,
    /// The record names this version but does not decode.
    Undecodable { error: String },
}

impl std::fmt::Display for StrandedCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Older { version } => {
                write!(f, "an older RimZ wrote it (record version {version})")
            }
            Self::MissingVersion => f.write_str("its record names no version"),
            Self::InvalidVersion => f.write_str("its record's version is not a number"),
            Self::Undecodable { error } => {
                write!(f, "its version {VERSION} record does not decode: {error}")
            }
        }
    }
}

/// What a run home's record is to this RimZ.
#[derive(Clone, Debug, PartialEq)]
pub enum RunClass {
    /// A record this RimZ reads, whatever its state.
    Current(Box<RunRecord>),
    /// An unfinished run this RimZ cannot continue.
    Stranded {
        header: Box<RunHeader>,
        cause: StrandedCause,
    },
    /// A record that ends its run but that this RimZ does not decode.
    Ended(Box<RunHeader>),
    /// A newer RimZ wrote the record; nothing else of it is read.
    Newer { version: u64 },
    /// `record.json` holds no JSON object.
    NotObject,
    /// The home has no `record.json`.
    Recordless,
}

impl RunClass {
    /// The record's header, where this RimZ read one.
    pub fn header(&self) -> Option<std::borrow::Cow<'_, RunHeader>> {
        match self {
            Self::Current(record) => Some(std::borrow::Cow::Owned(record.header())),
            Self::Stranded { header, .. } | Self::Ended(header) => {
                Some(std::borrow::Cow::Borrowed(header))
            }
            Self::Newer { .. } | Self::NotObject | Self::Recordless => None,
        }
    }
}

impl RunRecord {
    /// The record's header, as a field-by-field read finds it.
    pub fn header(&self) -> RunHeader {
        RunHeader {
            version: Some(self.version.into()),
            workflow: Some(self.workflow.clone()),
            task: self.task.clone(),
            manual: self.manual,
            project_root: Some(self.project_root.clone()),
            scope_kind: Some(self.scope.kind),
            scope_path: Some(self.scope.path.clone()),
            fire_tree: self.fire_tree,
            created_at: Some(self.created_at),
            state: Some(self.state),
            node: Some(self.node.clone()),
            terminal_at: self.terminal_at,
            loop_report: Some(self.loop_report),
            ending: self.outcome.as_ref().map(|outcome| outcome.ending.clone()),
            reason: self
                .outcome
                .as_ref()
                .and_then(|outcome| outcome.reason.clone()),
            acted: self.outcome.as_ref().map(|outcome| outcome.acted),
            notes: self.notes.clone(),
        }
    }
}

/// Classify a run home by its record, reading a newer RimZ's record no further than its
/// version. A read error other than a missing record is returned.
pub fn classify(paths: &WorkflowPaths) -> Result<RunClass> {
    let bytes = match std::fs::read(&paths.record) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RunClass::Recordless);
        }
        Err(source) => {
            return Err(WorkflowStoreErr::Io {
                path: paths.record.clone(),
                source,
            });
        }
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(RunClass::NotObject);
    };
    let Some(record) = value.as_object() else {
        return Ok(RunClass::NotObject);
    };
    let version = record.get("version");
    if let Some(version) = version
        .and_then(Value::as_u64)
        .filter(|version| *version > u64::from(VERSION))
    {
        return Ok(RunClass::Newer { version });
    }
    let decoded = (version.and_then(Value::as_u64) == Some(VERSION.into()))
        .then(|| RunRecord::deserialize(&value));
    if let Some(Ok(decoded)) = decoded {
        return Ok(RunClass::Current(Box::new(decoded)));
    }
    let header = Box::new(RunHeader::read(record));
    if header.is_terminal() {
        return Ok(RunClass::Ended(header));
    }
    let cause = match (version, decoded) {
        (_, Some(Err(error))) => StrandedCause::Undecodable {
            error: error.to_string(),
        },
        (None, _) => StrandedCause::MissingVersion,
        (Some(version), _) => match version.as_u64() {
            Some(version) => StrandedCause::Older { version },
            None => StrandedCause::InvalidVersion,
        },
    };
    Ok(RunClass::Stranded { header, cause })
}

/// One home of a room's live directory.
#[derive(Clone, Debug)]
pub struct LiveRun {
    pub paths: WorkflowPaths,
    pub class: RunClass,
}

/// Every home of the room's live directory that this RimZ has a part in: a run it runs, or one it
/// must end or move. A newer RimZ's run, a home without a record, and a home whose record cannot
/// be read are skipped without a warning; only an unreadable directory fails the listing.
pub fn live(state: &StatePaths) -> Result<Vec<LiveRun>> {
    let mut runs = Vec::new();
    for paths in state.workflow_runs()? {
        match classify(&paths) {
            Ok(RunClass::Newer { .. } | RunClass::Recordless) => {}
            Ok(class) => runs.push(LiveRun { paths, class }),
            Err(error) => {
                tracing::debug!(path = %paths.record.display(), %error,
                    "skipping workflow home that cannot be read");
            }
        }
    }
    Ok(runs)
}

/// Every record in the room's live directory that this RimZ runs, in any state.
pub fn live_runs(state: &StatePaths) -> Result<Vec<RunRecord>> {
    Ok(live(state)?
        .into_iter()
        .filter_map(|run| match run.class {
            RunClass::Current(record) => Some(*record),
            _ => None,
        })
        .collect())
}

/// A run found by id, in the live directory or the finished one.
#[derive(Clone, Debug)]
pub struct FoundRun {
    pub paths: WorkflowPaths,
    /// The home is in the finished directory: the run is over, whatever its record says.
    pub finished: bool,
    pub class: RunClass,
}

/// Find a run of this room by id: its live home first, then its finished one. None when neither
/// holds it, or the live home has no record and no finished home exists.
pub fn find(state: &StatePaths, id: &WorkflowRunId) -> Result<Option<FoundRun>> {
    let live = state.workflow(id);
    match classify(&live)? {
        RunClass::Recordless => {}
        class => {
            #[cfg(feature = "testkit")]
            crate::testkit::rendezvous("RIMZ_TEST_WORKFLOW_AFTER_FIND");
            return Ok(Some(FoundRun {
                paths: live,
                finished: false,
                class,
            }));
        }
    }
    let finished = state.finished_workflow(id);
    if !finished
        .home
        .try_exists()
        .map_err(WorkflowStoreErr::io(&finished.home))?
    {
        return Ok(None);
    }
    Ok(Some(FoundRun {
        class: classify(&finished)?,
        paths: finished,
        finished: true,
    }))
}

/// Find a run by id in every recorded room under `home`, probing each room's two homes for it.
/// A room that fails the probe is skipped, unless one of its homes for the run exists: then the
/// run is there and cannot be read, and that error is returned.
pub fn find_under(home: &Path, id: &WorkflowRunId) -> Result<Option<(StatePaths, FoundRun)>> {
    for state in crate::disk::paths::recorded_rooms_under(home)? {
        match find(&state, id) {
            Ok(Some(found)) => return Ok(Some((state, found))),
            Ok(None) => {}
            Err(error)
                if state.workflow(id).home.is_dir()
                    || state.finished_workflow(id).home.is_dir() =>
            {
                return Err(error);
            }
            Err(error) => {
                tracing::debug!(path = %state.root.display(), %error,
                    "skipping room whose workflow homes cannot be probed");
            }
        }
    }
    Ok(None)
}

/// What [`finish`] did with a run's live home.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Finished {
    /// The home moved to the finished directory.
    Moved,
    /// The home was already in the finished directory; the move's barriers were completed.
    Already,
    /// Neither directory holds the run.
    Gone,
    /// The run may not leave the live directory: it is unfinished, still owes its loop report,
    /// or a newer RimZ wrote it. Nothing changed.
    Kept,
    /// The finished directory already holds a home of that id, which was left alone. The live
    /// home keeps its `history.txt`.
    Collision,
}

/// Move a run's home from the live directory to the finished one, once its record ends the run
/// and owes no loop report, or holds no JSON object at all. `history`, when given, is written to
/// the home's `history.txt` first. The rename never replaces a destination, and a retry after a
/// crash completes the barriers of a move already made.
#[must_use = "durability barrier; check the result"]
pub fn finish(
    state: &StatePaths,
    id: &WorkflowRunId,
    history: Option<&[u8]>,
    _guard: &WorkspaceLock,
) -> Result<Finished> {
    let live = state.workflow(id);
    let finished = state.finished_workflow(id);
    let movable = match classify(&live)? {
        RunClass::Recordless => {
            if !finished
                .home
                .try_exists()
                .map_err(WorkflowStoreErr::io(&finished.home))?
            {
                return Ok(Finished::Gone);
            }
            sync_parents(&live, &finished)?;
            return Ok(Finished::Already);
        }
        RunClass::NotObject => true,
        class => class
            .header()
            .is_some_and(|header| header.is_terminal() && !header.owes_loop_report()),
    };
    if !movable {
        return Ok(Finished::Kept);
    }
    if let Some(history) = history {
        crate::disk::atomic::write_bytes_atomically(&live.history(), history)?;
    }
    let parent = parent_dir(&finished.home)?;
    std::fs::create_dir_all(parent).map_err(WorkflowStoreErr::io(parent))?;
    crate::disk::atomic::sync_dir(parent_dir(parent)?)?;
    match crate::disk::atomic::publish_staged_no_replace(&live.home, &finished.home) {
        Ok(()) => {}
        Err(crate::disk::atomic::AtomicErr::Exists { .. }) => return Ok(Finished::Collision),
        Err(error) => return Err(error.into()),
    }
    crate::disk::atomic::sync_dir(parent_dir(&live.home)?)?;
    Ok(Finished::Moved)
}

fn sync_parents(live: &WorkflowPaths, finished: &WorkflowPaths) -> Result<()> {
    crate::disk::atomic::sync_dir(parent_dir(&finished.home)?)?;
    Ok(crate::disk::atomic::sync_dir(parent_dir(&live.home)?)?)
}

fn parent_dir(path: &Path) -> Result<&Path> {
    path.parent().ok_or_else(|| WorkflowStoreErr::Io {
        path: path.to_path_buf(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "no parent directory"),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunRead {
    Strict,
    BestEffort,
}

/// Read unfinished runs this RimZ runs in every recorded room's live directory, without
/// resolving their checkout paths. Every other home is skipped without a warning: a run this
/// RimZ cannot continue protects nothing until it is ended, and a newer RimZ's run is its own.
/// Strict fails on a room or a home it cannot read; best effort skips it.
pub fn unfinished_under(home: &std::path::Path, mode: RunRead) -> Result<Vec<RunRecord>> {
    let homes = match mode {
        RunRead::Strict => crate::disk::paths::workflow_runs_under_strict(home)?,
        RunRead::BestEffort => crate::disk::paths::workflow_runs_under(home)?,
    };
    let mut records = Vec::new();
    for (_, paths) in homes {
        match classify(&paths) {
            Ok(RunClass::Current(record)) if !record.state.is_terminal() => records.push(*record),
            Ok(_) => {}
            Err(error) if mode == RunRead::BestEffort => {
                tracing::debug!(path = %paths.record.display(), %error,
                    "skipping workflow home that cannot be read");
            }
            Err(error) => return Err(error),
        }
    }
    Ok(records)
}
#[cfg(test)]
mod tests;
