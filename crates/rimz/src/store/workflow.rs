//! Durable workflow records and append-only history; transitions belong to the harness.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::disk::lock::WorkspaceLock;
use crate::disk::paths::{StatePaths, WorkflowPaths};
use crate::ids::{AgentKind, AgentSessionId, HoldKind, LaneName, MessageId, RunId, WorkflowRunId};

pub const VERSION: u32 = 5;

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
         version; this RimZ reads only version {current}. Clear it with \
         `rimz workflow cancel {run}`, then start the workflow again",
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
    /// Whether this is `load`'s refusal of another version's record: a run every background
    /// reader treats as absent, and only `rimz workflow cancel` removes, or GC past the owned
    /// grace when the record is an older version's.
    pub fn is_refused(&self) -> bool {
        matches!(self, Self::Version { .. })
    }

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

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StepVisits {
    pub total: u64,
    pub outcomes: Vec<OutcomeVisits>,
}

impl StepVisits {
    /// Visits in this outcome/key bucket since the last reset; 0 before its first visit.
    pub fn for_bucket(&self, bucket: (Option<&str>, Option<&Key>)) -> u64 {
        self.bucket_index(bucket)
            .map_or(0, |index| self.outcomes[index].visits)
    }

    /// Count one visit in this bucket and in the total since the last full reset.
    pub fn bump(&mut self, bucket: (Option<String>, Option<Key>)) {
        self.total += 1;
        let (outcome, key) = bucket;
        if let Some(index) = self.bucket_index((outcome.as_deref(), key.as_ref())) {
            self.outcomes[index].visits += 1;
        } else {
            self.outcomes.push(OutcomeVisits {
                outcome,
                key,
                visits: 1,
            });
        }
    }

    fn bucket_index(&self, bucket: (Option<&str>, Option<&Key>)) -> Option<usize> {
        self.outcomes
            .iter()
            .position(|row| (row.outcome.as_deref(), row.key.as_ref()) == bucket)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutcomeVisits {
    pub outcome: Option<String>,
    pub key: Option<Key>,
    pub visits: u64,
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
    Stay,
    Until,
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
    /// When a loop fire's default capacity wait gives up: a second before the task's next clock
    /// fire, never past its deadline. An agent step that sets no `capacity-wait` holds until
    /// then, however late in the run its first hold comes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_until: Option<Timestamp>,
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
    pub visits: BTreeMap<String, StepVisits>,
    pub visit_grant: u64,
    pub deadline: Option<Timestamp>,
    pub deadline_fired: bool,
    pub next_seq: u64,
    pub attempt: Option<LiveAttempt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<HeldSession>,
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
    /// The agent session still owned by this run, including an until session under a check.
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

    /// The routed outcome and key, preserving a capped node's bucket across resume.
    pub fn visit_bucket(&self) -> (Option<&str>, Option<&Key>) {
        (
            self.input.as_ref().and_then(|input| {
                let field = if input["outcome"] == "cap" && input["step"] == self.node {
                    "routed_by"
                } else {
                    "outcome"
                };
                input[field].as_str()
            }),
            self.key.as_ref(),
        )
    }

    /// Derive the lane without resolving or reading the checkout, which may be gone.
    pub fn address(&self) -> RunAddress {
        let home = match self.scope.kind {
            ScopeKind::Worktree => crate::ids::LaneHome::Worktree,
            ScopeKind::Project => crate::ids::LaneHome::Trunk,
        };
        let lane = crate::ids::Lane::own(home, self.scope.path.to_string_lossy());
        RunAddress {
            workflow: self.workflow.clone(),
            lane: lane.routing_name(),
        }
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
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        all: bool,
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
    /// A signal a staying step subscribes to, queued for its leader as `message`.
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

/// Remove a run home whose record `load` refuses as another version's, without reading it
/// further; false, touching nothing, when the record now loads or is gone.
pub fn remove_refused(paths: &WorkflowPaths, _guard: &WorkspaceLock) -> Result<bool> {
    if !load(paths).is_err_and(|error| error.is_refused()) {
        return Ok(false);
    }
    std::fs::remove_dir_all(&paths.home).map_err(WorkflowStoreErr::io(&paths.home))?;
    Ok(true)
}

/// Write an inline definition's text and assets into a definition root: a run's own, before
/// `initialize` publishes the record, or an ejected task's staging directory. Each asset name
/// is a relative path inside that root.
pub fn seal_definition(root: &Path, text: &str, assets: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    std::fs::create_dir_all(root).map_err(WorkflowStoreErr::io(root))?;
    crate::disk::atomic::write_bytes_atomically(
        &crate::disk::paths::workflow_spec_file(root),
        text.as_bytes(),
    )?;
    for (name, bytes) in assets {
        let relative = Path::new(name);
        if !relative
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
        {
            return Err(WorkflowStoreErr::Io {
                path: root.join(relative),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "a definition asset must be a relative path inside its root",
                ),
            });
        }
        let file = root.join(relative);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).map_err(WorkflowStoreErr::io(parent))?;
        }
        crate::disk::atomic::write_bytes_atomically(&file, bytes)?;
        // Publish each new subdirectory entry down from the root.
        for dir in file
            .ancestors()
            .skip(2)
            .take_while(|dir| dir.starts_with(root))
        {
            crate::disk::atomic::sync_dir(dir)?;
        }
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

pub fn list(state: &StatePaths) -> Result<Vec<RunRecord>> {
    let mut records = Vec::new();
    let mut refused = 0;
    for paths in state.workflow_runs()? {
        match load(&paths) {
            Ok(record) => records.push(record),
            Err(WorkflowStoreErr::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) if err.is_refused() => refused += 1,
            Err(err) => {
                tracing::warn!(path = %paths.record.display(), error = %err, "skipping workflow with unreadable record");
            }
        }
    }
    warn_refused(refused);
    Ok(records)
}

/// One warning for every run a listing skipped because another RimZ version wrote its record,
/// so a room holding thousands of old runs does not print a line for each.
fn warn_refused(count: usize) {
    if count > 0 {
        tracing::warn!(
            count,
            "skipping workflow runs whose record another RimZ version wrote; \
             `rimz workflow list` names each, and `rimz workflow cancel <run-id>` clears one"
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunRead {
    Strict,
    BestEffort,
}

/// Read unfinished runs in every recorded room without resolving their checkout paths.
pub fn unfinished_under(home: &std::path::Path, mode: RunRead) -> Result<Vec<RunRecord>> {
    let homes = match mode {
        RunRead::Strict => crate::disk::paths::workflow_runs_under_strict(home)?,
        RunRead::BestEffort => crate::disk::paths::workflow_runs_under(home)?,
    };
    let mut records = Vec::new();
    let mut refused = 0;
    for (_, paths) in homes {
        match load(&paths) {
            Ok(record) if !record.state.is_terminal() => records.push(record),
            Ok(_) => {}
            Err(WorkflowStoreErr::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) if error.is_refused() => refused += 1,
            Err(error) if mode == RunRead::BestEffort => {
                tracing::warn!(path = %paths.record.display(), %error,
                    "skipping workflow with unreadable record");
            }
            Err(error) => return Err(error),
        }
    }
    warn_refused(refused);
    Ok(records)
}

#[cfg(test)]
mod tests;
