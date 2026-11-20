//! Durable workflow records and append-only history; transitions belong to the harness.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agents::TurnErrorClass;
use crate::disk::lock::WorkspaceLock;
use crate::disk::paths::{StatePaths, WorkflowPaths};
use crate::ids::{AgentKind, AgentSessionId, HoldKind, LaneName, MessageId, RunId, WorkflowRunId};

/// The run record's own shape version, raised when a binary on either side of a change would
/// misread the other's record; `load` refuses a record of any other version before decoding it.
/// A change to what a definition means raises nothing: every run reads its definition as it is.
pub const VERSION: u32 = 18;

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
#[error("run address must be <workflow>#<checkout> with nonempty parts and no `#` in the checkout")]
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

/// One item key's visits to a step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyVisits {
    pub key: Key,
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

impl AttemptKind {
    /// Whether an attempt of this kind owns an agent run, which ends through the stop protocol:
    /// an agent step's or a judge's.
    pub fn owns_agent(self) -> bool {
        matches!(self, Self::Agent | Self::Judge)
    }
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
    Restart {
        continued: bool,
    },
    /// `workflow resume --at` moved the run off the attempt's node.
    Resume,
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
    /// A judge's answer, accepted from `rimz workflow submit` and normalized against its node;
    /// the engine routes on it alone, never on the judge's last message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<Value>,
    /// A judge that rested without an answer was reminded once.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reminded: bool,
    /// A judge's ending evidence, staged with its ending for the settlement after the stop: its
    /// failure's detail, beside a `reason` that names the provider's error or what the judge was
    /// stopped on as its timeout passed. A judge's ending never touches `prev_attempt`, which
    /// stays the last step's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Value>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub woken: bool,
    /// The blocking ask the attempt is stopped on, a judge's included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asking: Option<Asking>,
    /// When the attempt's current hold began. It pauses an agent step's timeout; a judge's
    /// timeout runs through it, so on a judge it only shows the stop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_since: Option<Timestamp>,
    /// The stop the attempt is held on, written and cleared with `held_since`; none while it
    /// is held only on an ask.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_on: Option<Hold>,
    /// The held time an agent step's timeout credits; a judge's timeout reads none of it.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub asked_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judged: Option<TurnEnd>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<PushBack>,
    /// A message step's notice id, minted before the queue so a retry never queues twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<MessageId>,
    /// A `runs` step's children, reserved before any of them exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs: Option<StartedRuns>,
}

/// The runs a `runs` step starts: the commit every child's worktree is cut from, resolved once,
/// and one run per listed workflow in the step's order, each under an id reserved here before
/// the run exists, so a retried start rejoins its children by id and never starts one twice.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StartedRuns {
    pub base: String,
    pub runs: Vec<StartedRun>,
}

/// One run a `runs` step started, and how it ended once its report stored that here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StartedRun {
    pub id: WorkflowRunId,
    pub workflow: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<JoinedEnding>,
}

/// A started run's ending as its report carried it to the run that started it: the parent reads
/// it here, never from the child's home, which moves once the report is sent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JoinedEnding {
    pub ending: RunEnding,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<PathBuf>,
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
    Errored,
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
            Self::Errored => "errored",
            Self::Canceled => "canceled",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Asked {
    pub text: String,
    pub kind: String,
    pub handle: String,
}

/// A leader turn that ended on an unrecoverable provider error: the provider's text (the class
/// in words when it gave none), the class, and the agent's handle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Errored {
    pub text: String,
    pub class: TurnErrorClass,
    pub handle: String,
}

/// What holds a live agent attempt besides an ask: a stopped turn of one of its agents.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hold {
    pub since: Timestamp,
    /// The agent the stop came from; none when only the run's record shows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    #[serde(flatten)]
    pub stop: HoldStop,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "stop", rename_all = "snake_case")]
pub enum HoldStop {
    /// A turn stopped on a provider error or limit, by its class when the row shows one, with
    /// the provider's text.
    Error {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        class: Option<TurnErrorClass>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    Budget,
}

impl Hold {
    /// Whether the hold waits on the user rather than on a provider or budget window: an error
    /// with no class, or one no wait recovers from.
    pub fn needs_you(&self) -> bool {
        match &self.stop {
            HoldStop::Error { class, .. } => class.is_none_or(|class| !class.pauses_turn()),
            HoldStop::Budget => false,
        }
    }

    /// What holds the attempt, in words, with the provider's text for an errored turn.
    pub fn label(&self) -> String {
        match &self.stop {
            HoldStop::Budget => "budget park".into(),
            HoldStop::Error {
                class: Some(class), ..
            } if class.pauses_turn() => class.words().into(),
            HoldStop::Error { class, text } => {
                let words = class.map_or(String::new(), |class| format!(" ({})", class.words()));
                let text = text
                    .as_ref()
                    .map_or(String::new(), |text| format!(": {text}"));
                format!("errored turn{words}{text}")
            }
        }
    }
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
    pub asked: Option<Asked>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errored: Option<Errored>,
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

/// Whether a run still owes its recipient the report of how it ended: its loop task, or the
/// agent that started it ([`RunStarter::Agent`]) without `--detach`. The start sets it owed, and
/// only the collector marks it sent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportMark {
    /// Nothing is owed: no task or agent is told how the run ended, the start was detached, or
    /// the run was recorded before reports existed.
    #[default]
    None,
    /// The run's report has not fully landed: for a loop task, the history row, the strike
    /// update, or both are missing. On a terminal record the run awaits its collection, which
    /// the command that ended it runs next, and which the scheduler's tick retries.
    Owed,
    /// The terminal outcome reached its recipient: for a loop task, the loop history and, while
    /// the task's row existed, the strike count.
    Sent,
}

/// What started a run, for whom its report is meant and where it is drawn; a run a person or a
/// loop task started has none.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "by")]
pub enum RunStarter {
    /// An agent, by its launch: `launch` is the launch id, matched to a living row as a team's
    /// launcher is.
    Agent {
        kind: AgentKind,
        launch: AgentSessionId,
    },
    /// Another run, from its `runs` step `node`, which joins the run through its report, or from
    /// a command, script or check at `node`, owed no report.
    Run { run: WorkflowRunId, node: String },
}

impl RunStarter {
    /// Whether this starter is the agent `agent`, by its launch, as a team's launcher is matched.
    pub fn is_agent(&self, agent: &crate::agents::AgentState) -> bool {
        match self {
            Self::Agent { kind, launch } => {
                agent.launch_id.as_ref() == Some(launch)
                    || (kind == &agent.kind && agent.answers_to(launch))
            }
            Self::Run { .. } => false,
        }
    }
}

/// How a run ended, in the engine's terms: written in the record write that makes the run
/// terminal, so a reader of the record learns the end without the journal or the definition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunOutcome {
    pub ending: RunEnding,
    /// The summed cost of the run's attempts and evaluations; none when none recorded a cost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// Whether the run entered an agent, message or runs step.
    pub acted: bool,
    /// Why a failed run's last attempt ended: the newest reason an attempt or a budget route
    /// recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The run ended done without starting an agent, a judge or a message: its record said
    /// `engaged = false` at the terminal write. Its loop collection forgets it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub quiet: bool,
}

/// The decision that ended a run; a failure the run's last decision explains names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum RunEnding {
    Done,
    Failed,
    Canceled,
    /// A try (`rimz workflow run --once`) ended done once its one node settled; the route it did
    /// not take is on the journal's `Terminal` entry.
    Tried,
    /// The run's last decision was the `capacity` route out of an agent step, after waiting
    /// `waited_ms` for the capacity named by `reason`.
    CapacityGaveUp {
        waited_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

/// The loop fire that started a run, as the run's briefs name it. `evidence` is the fire's one
/// line naming what fired it (a signal's subject and fields, a condition's readings), which the
/// run's commands read as `RIMZ_LOOP_TRIGGER` and its agent steps get after the run's prompt;
/// a clock fire, or a manual one without `--signal`, has none. `booked_by` is the
/// fired task row's booking chain as the fire read it, which every agent the run launches
/// carries; a fire of a row with no chain, or of a record written before the field, has none.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum RunFire {
    /// `rimz loop fire <task>` started the run by hand.
    Manual {
        task: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        evidence: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        booked_by: Vec<crate::agents::LaunchedBy>,
    },
    /// The task's trigger started the run; `trigger` words it as the brief does
    /// (``on the cron schedule `0 * * * *` ``), and a watch trigger has none.
    Scheduled {
        task: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trigger: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        evidence: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        booked_by: Vec<crate::agents::LaunchedBy>,
    },
}

impl RunFire {
    /// The fire's evidence line, when it carried one.
    pub fn evidence(&self) -> Option<&str> {
        match self {
            Self::Manual { evidence, .. } | Self::Scheduled { evidence, .. } => evidence.as_deref(),
        }
    }

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
    pub id: WorkflowRunId,
    #[serde(default)]
    pub origin: RunOrigin,
    pub workflow: String,
    /// The loop task's name frozen at the fire, for briefs and display; identity is
    /// `loop_fire`.
    #[serde(default)]
    pub task: Option<String>,
    /// The loop fire whose environment or runner started the run. With `task` set the run is
    /// that task's own ([`RunRecord::ran_for`]); without, a fire's script started it and it
    /// reports to nobody.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_fire: Option<crate::ids::FireId>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub manual: bool,
    /// The run is a try (`rimz workflow run --once`): it ends done at its start node's first
    /// route that is not an `unknown` wait. Set at creation, never changed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub once: bool,
    /// The run's one report mark, for its loop task or its agent starter. The key keeps its
    /// older name, which [`RunHeader`] reads from a record of any version.
    #[serde(default, rename = "loop_report")]
    pub report: ReportMark,
    /// What started the run, set at creation and never changed; none for a person, a loop task,
    /// or a loop shell task's command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starter: Option<RunStarter>,
    /// The id of the report queued for an agent starter, minted before the report is queued so
    /// a collection that dies after queueing it queues the same message again, which the queue
    /// takes once ([`stamp_report_message`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_message: Option<crate::ids::MessageId>,
    /// How many runs sit on the run's launch path, itself included: 1 for a run a person, a loop
    /// task or an agent outside any workflow started, one more than the run, or the step's run,
    /// that started it. Set at creation; `[agents] max-workflow-depth` bounds it.
    #[serde(default = "first_depth")]
    pub depth: u32,
    /// A deployed run's definition directory, canonical at the start and read live; an inline
    /// run has none and reads its own `definition/`, through [`RunRecord::definition_dir`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_dir: Option<PathBuf>,
    pub prompt: Option<String>,
    pub scope: Scope,
    /// The run's give-up instant, a start argument: a capacity wait at an agent step that sets
    /// no `admission.give-up` gives up then, however late in the run its first hold comes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub give_up: Option<Timestamp>,
    /// Whether the run's starts wait at a capacity hold or launch past it, a start argument.
    #[serde(default, skip_serializing_if = "StartCapacity::is_wait")]
    pub capacity: StartCapacity,
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
    /// The share of each step's visits charged under each item key, in first-charge order.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub key_visits: BTreeMap<String, Vec<KeyVisits>>,
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
    /// The runs the last `runs` step the run left had started, with the endings their reports
    /// stored, for the steps after it; the step's next visit replaces them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub joined: Vec<StartedRun>,
    /// Judges whose evaluation an interrupt of their held session abandoned while their agents
    /// ran, owed a stop: the next engine pass stops them off the lock, and a closing run stops
    /// them as it ends.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub abandoned: Vec<AttemptRun>,
    /// The current step entry spent its one retry of a launch whose eviction failed before it
    /// started anything; cleared wherever the run enters a step afresh.
    #[serde(default)]
    pub launch_retry: bool,
    /// Whether the run has started anything but a script or command: set `false` at creation
    /// and latched `true` by the record write that publishes its first agent, judge, team or
    /// message attempt, never cleared. Absent means a RimZ without the field wrote or rewrote
    /// the record, which is not known to be quiet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engaged: Option<bool>,
    /// What the loop fire that started the run would have logged at launch, sealed at creation
    /// for its one report row; opaque to the store and the engine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fire_facts: Option<Value>,
    /// This RimZ found the run unfinished in a record it could not continue and ended it as
    /// failed in place ([`end_stranded`]); its loop report moves no strike.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stranded: bool,
}

impl RunRecord {
    /// The directory holding the run's definition: a deployed run's recorded directory, as it is
    /// now, or an inline run's own `definition/` under `paths`, the home the caller was handed,
    /// live or finished.
    pub fn definition_dir(&self, paths: &WorkflowPaths) -> PathBuf {
        self.spec_dir
            .clone()
            .unwrap_or_else(|| paths.definition.clone())
    }

    /// The run's live agent attempts: the current one, a judge's included, then a session held
    /// under its rest check. A run that is not active has none.
    pub fn live_attempts(&self) -> impl Iterator<Item = &LiveAttempt> {
        let active = self.state == RunState::Active;
        self.attempt
            .iter()
            .chain(self.held.as_ref().map(|held| &held.attempt))
            .filter(move |_| active)
    }

    /// Whether the active run waits on the user: an agent asking, or held on an error no wait
    /// recovers from.
    pub fn calls_you(&self) -> bool {
        self.live_attempts().any(|attempt| attempt.asking.is_some())
            || self
                .live_attempts()
                .find_map(|attempt| attempt.held_on.as_ref())
                .is_some_and(Hold::needs_you)
    }

    /// Whether the run waits on a person: parked, an agent asking, held on an error no wait
    /// recovers from, or a capacity hold that needs a fix. `workflow list` puts such a run under
    /// NEEDS YOU, `workflow wait` returns on it, and a run that starts to fires
    /// `workflow.waiting`.
    pub fn needs_you(&self) -> bool {
        self.state == RunState::Parked
            || self.calls_you()
            || self.wait.as_ref().is_some_and(|wait| {
                self.state == RunState::Active
                    && wait.outcome == "capacity"
                    && wait.hold.is_some_and(crate::ids::HoldKind::needs_fix)
            })
    }

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

    /// The outcome that routed the run into its node: a failure route's cause, else the answer.
    pub fn routed_by(&self) -> Option<&str> {
        self.input.as_ref().and_then(|input| {
            let field = if input["outcome"] == "failed" {
                "cause"
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
    pub fn started_by(&self, task: &crate::ids::TaskId) -> bool {
        !self.state.is_terminal() && self.ran_for(task)
    }

    /// Whether this run is loop task `task`'s own, live or finished: a fire of the task started
    /// it as the task's run, so a rename leaves it the task's.
    pub fn ran_for(&self, task: &crate::ids::TaskId) -> bool {
        self.task.is_some()
            && self
                .loop_fire
                .as_ref()
                .is_some_and(|fire| fire.task() == *task)
    }

    /// The agent this run owes its report, while it owes it: that agent waits on the run, so
    /// the run's launches run beside it rather than waiting on it or stopping it.
    pub fn owed_starter(&self) -> Option<&RunStarter> {
        self.starter
            .as_ref()
            .filter(|starter| matches!(starter, RunStarter::Agent { .. }))
            .filter(|_| self.report == ReportMark::Owed)
    }

    /// A finished run whose loop task, agent starter or `runs` step has not yet heard how it
    /// ended: the one debt that keeps the run's home live and its paced advance due.
    pub fn owes_report(&self) -> bool {
        self.state.is_terminal() && self.report == ReportMark::Owed && self.has_recipient()
    }

    /// A finished run whose loop task has not yet heard how it ended.
    pub fn owes_loop_report(&self) -> bool {
        self.owes_report() && self.task.is_some()
    }

    fn has_recipient(&self) -> bool {
        self.task.is_some() || self.starter.is_some()
    }
}

const fn first_depth() -> u32 {
    1
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub seq: Option<u64>,
    pub at: Timestamp,
    pub node: String,
    #[serde(flatten)]
    pub event: JournalEvent,
}

impl JournalEntry {
    /// Point every log this entry names inside its run's home at the same file under `home`:
    /// a run's home moves to the finished directory with its files in place, while its entries
    /// keep the paths they were recorded with. A path outside the run's homes stays as recorded.
    pub fn relocate(&mut self, home: &WorkflowPaths) {
        match &mut self.event {
            JournalEvent::Attempt { log: Some(log), .. } => *log = home.relocate(log),
            JournalEvent::Evaluation { object, .. } => relocate_logs(object, home),
            JournalEvent::Route { input, .. } => relocate_logs(input, home),
            _ => {}
        }
    }
}

/// An evaluation's answer or a step's input names its own log, and a failure input its
/// evidence's in `detail`.
fn relocate_logs(object: &mut Value, home: &WorkflowPaths) {
    relocate_log(object.get_mut("log"), home);
    relocate_log(
        object
            .get_mut("detail")
            .and_then(|detail| detail.get_mut("log")),
        home,
    );
}

fn relocate_log(log: Option<&mut Value>, home: &WorkflowPaths) {
    let Some(log) = log else {
        return;
    };
    let Some(recorded) = log.as_str() else {
        return;
    };
    let moved = home.relocate(Path::new(recorded));
    *log = Value::from(moved.to_string_lossy().into_owned());
}

/// The logs a run record names, read from `home`, as [`JournalEntry::relocate`] reads its
/// journal's.
fn relocate_record(record: &mut RunRecord, home: &WorkflowPaths) {
    if let Some(input) = &mut record.input {
        relocate_logs(input, home);
    }
    if let Some(log) = record
        .prev_attempt
        .as_mut()
        .and_then(|previous| previous.log.as_mut())
    {
        *log = home.relocate(log);
    }
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
        asked: Option<Asked>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        errored: Option<Errored>,
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
        /// A try's end: the target of the route it did not take, in `Target`'s display form.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tried: Option<String>,
    },
    Cancel {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// A start placed by hand (`rimz workflow run --at` or `--input`), the journal's first entry,
    /// at the start node, with the input given to it.
    Start {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<Value>,
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

/// Read a run record, refusing one of any other version before its body is decoded. The logs
/// it names are read under `paths`' home ([`JournalEntry::relocate`]).
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
    let mut record = serde_json::from_slice(&bytes).map_err(json)?;
    relocate_record(&mut record, paths);
    Ok(record)
}

/// Write a definition's text and assets into a definition root: an inline run's own, sealed
/// before `initialize` publishes the record, or an ejected task's staging directory. Each asset
/// name is a relative path inside that root.
pub fn seal_definition(root: &Path, text: &str, assets: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    std::fs::create_dir_all(root).map_err(WorkflowStoreErr::io(root))?;
    crate::disk::atomic::write_bytes_atomically(
        &crate::disk::paths::workflow_spec_file(root),
        text.as_bytes(),
    )?;
    for (name, bytes) in assets {
        let file = sealed_entry(root, name)?;
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).map_err(WorkflowStoreErr::io(parent))?;
        }
        crate::disk::atomic::write_bytes_atomically(&file, bytes)?;
        sync_up(root, file.parent().and_then(Path::parent))?;
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
pub fn initialize(state: &StatePaths, record: &RunRecord, guard: &WorkspaceLock) -> Result<()> {
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

/// Read a run's journal, its logs under `paths`' home ([`JournalEntry::relocate`]).
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
    let mut entries = parse_journal(&bytes, &paths.journal);
    for entry in &mut entries {
        entry.relocate(paths);
    }
    Ok(entries)
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
    pub loop_fire: Option<crate::ids::FireId>,
    pub manual: bool,
    pub project_root: Option<PathBuf>,
    pub scope_kind: Option<ScopeKind>,
    pub scope_path: Option<PathBuf>,
    pub created_at: Option<Timestamp>,
    pub entered_at: Option<Timestamp>,
    pub state: Option<RunState>,
    pub node: Option<String>,
    pub terminal_at: Option<Timestamp>,
    pub report: Option<ReportMark>,
    pub starter: Option<RunStarter>,
    /// Where the run's definition came from; a record naming none was deployed, as its own
    /// decode reads it.
    pub origin: RunOrigin,
    /// Whether the run is a try (`rimz workflow run --once`).
    pub once: bool,
    pub ending: Option<RunEnding>,
    pub reason: Option<String>,
    pub acted: Option<bool>,
    pub notes: Option<PathBuf>,
    pub fire_facts: Option<Value>,
    /// Why this RimZ could not continue the run, when the record was ended in place as stranded
    /// ([`end_stranded`]): read again from the version and the reason that ending kept.
    pub stranded: Option<StrandedCause>,
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
            loop_fire: field(record.get("loop_fire")),
            manual: field(record.get("manual")).unwrap_or(false),
            project_root: field(record.get("project_root")),
            scope_kind: field(record.get("scope").and_then(|scope| scope.get("kind"))),
            scope_path: field(record.get("scope").and_then(|scope| scope.get("path"))),
            created_at: field(record.get("created_at")),
            entered_at: field(record.get("entered_at")),
            state: field(record.get("state")),
            node: field(record.get("node")),
            terminal_at: field(record.get("terminal_at")),
            report: field(record.get("loop_report")),
            starter: field(record.get("starter")),
            origin: field(record.get("origin")).unwrap_or_default(),
            once: field(record.get("once")).unwrap_or(false),
            ending: field(outcome.and_then(|outcome| outcome.get("ending"))),
            reason: field(outcome.and_then(|outcome| outcome.get("reason"))),
            acted: field(outcome.and_then(|outcome| outcome.get("acted"))),
            notes: field(record.get("notes")),
            fire_facts: record.get("fire_facts").cloned(),
            stranded: field(record.get("stranded")).unwrap_or(false).then(|| {
                let reason = field::<String>(outcome.and_then(|outcome| outcome.get("reason")));
                StrandedCause::kept(record.get("version"), reason.as_deref())
            }),
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

    /// Whether this run is loop task `task`'s own, as [`RunRecord::ran_for`] reads it.
    pub fn ran_for(&self, task: &crate::ids::TaskId) -> bool {
        self.task.is_some()
            && self
                .loop_fire
                .as_ref()
                .is_some_and(|fire| fire.task() == *task)
    }

    /// A finished run that owes its report, as [`RunRecord::owes_report`] reads it.
    pub fn owes_report(&self) -> bool {
        self.is_terminal()
            && self.report == Some(ReportMark::Owed)
            && (self.task.is_some() || self.starter.is_some())
    }

    /// A finished run whose loop task has not yet heard how it ended.
    pub fn owes_loop_report(&self) -> bool {
        self.owes_report() && self.task.is_some()
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

impl StrandedCause {
    /// The cause of a record ended as stranded, from what the ending kept: the record's version
    /// member and the reason [`end_stranded`] wrote, whose text carries a decode error.
    fn kept(version: Option<&Value>, reason: Option<&str>) -> Self {
        let Some(version) = version else {
            return Self::MissingVersion;
        };
        match version.as_u64() {
            None => Self::InvalidVersion,
            Some(version) if version != u64::from(VERSION) => Self::Older { version },
            Some(_) => {
                let empty = Self::Undecodable {
                    error: String::new(),
                };
                let undecodable = format!("{STRANDED}{empty}");
                let error = reason.map(|reason| {
                    reason
                        .strip_prefix(&undecodable)
                        .and_then(|error| error.strip_suffix(&stranded_tail()))
                        .unwrap_or(reason)
                });
                Self::Undecodable {
                    error: error.unwrap_or_default().into(),
                }
            }
        }
    }
}

/// How the reason [`end_stranded`] writes begins: the cause follows, then [`stranded_tail`].
const STRANDED: &str = "this RimZ cannot continue the run: ";

fn stranded_tail() -> String {
    format!("; it reads only record version {VERSION}")
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
    /// Whether the room's workflow pass has work in a live home of this class: anything but a
    /// run this RimZ continues, a newer RimZ's run, and a home without a record. A finished run
    /// this RimZ reads that still owes its report is left to its paced advance.
    pub fn awaits_pass(&self) -> bool {
        match self {
            Self::Current(record) => record.state.is_terminal() && !record.owes_report(),
            Self::Stranded { .. } | Self::Ended(_) | Self::NotObject => true,
            Self::Newer { .. } | Self::Recordless => false,
        }
    }

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
            loop_fire: self.loop_fire.clone(),
            manual: self.manual,
            project_root: Some(self.project_root.clone()),
            scope_kind: Some(self.scope.kind),
            scope_path: Some(self.scope.path.clone()),
            created_at: Some(self.created_at),
            entered_at: Some(self.entered_at),
            state: Some(self.state),
            node: Some(self.node.clone()),
            terminal_at: self.terminal_at,
            report: Some(self.report),
            starter: self.starter.clone(),
            origin: self.origin,
            once: self.once,
            ending: self.outcome.as_ref().map(|outcome| outcome.ending.clone()),
            reason: self
                .outcome
                .as_ref()
                .and_then(|outcome| outcome.reason.clone()),
            acted: self.outcome.as_ref().map(|outcome| outcome.acted),
            notes: self.notes.clone(),
            fire_facts: self.fire_facts.clone(),
            stranded: self.stranded.then(|| {
                let reason = self
                    .outcome
                    .as_ref()
                    .and_then(|outcome| outcome.reason.as_deref());
                StrandedCause::kept(Some(&Value::from(self.version)), reason)
            }),
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
    if let Some(Ok(mut decoded)) = decoded {
        relocate_record(&mut decoded, paths);
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

/// End a stranded run in its record, in place: the record names the run failed with why this
/// RimZ cannot continue it, owes its loop task the report when it names one (an agent starter's
/// owed mark stays as written, so that starter is still told), and keeps every other member and
/// its version, so a RimZ that reads that version still reads the body. A first `terminal_at`
/// and a recorded `acted` are kept. Returns the header read before the ending when this call
/// ended the run; a record that is no longer stranded is left as it is.
#[must_use = "durability barrier; check the result"]
pub fn end_stranded(paths: &WorkflowPaths, _guard: &WorkspaceLock) -> Result<Option<RunHeader>> {
    let RunClass::Stranded { header, cause } = classify(paths)? else {
        return Ok(None);
    };
    let mut record = read_object(paths)?;
    let reason = format!("{STRANDED}{cause}{}", stranded_tail());
    record.insert("state".into(), serde_json::json!(RunState::Failed));
    if header.terminal_at.is_none() {
        record.insert("terminal_at".into(), serde_json::json!(Timestamp::now()));
    }
    let mut outcome = match record.remove("outcome") {
        Some(Value::Object(outcome)) => outcome,
        _ => serde_json::Map::new(),
    };
    outcome.insert("ending".into(), serde_json::json!(RunEnding::Failed));
    outcome.insert("reason".into(), serde_json::json!(reason));
    outcome.entry("acted").or_insert(serde_json::json!(false));
    record.insert("outcome".into(), Value::Object(outcome));
    if header.task.is_some() {
        record.insert("loop_report".into(), serde_json::json!(ReportMark::Owed));
    }
    record.insert("stranded".into(), serde_json::json!(true));
    crate::disk::atomic::write_temp_then_rename(&paths.record, &Value::Object(record))?;
    Ok(Some(*header))
}

/// Mark the report of a run as sent, in place, leaving every other member of its record as it
/// is, whatever version wrote it.
#[must_use = "durability barrier; check the result"]
pub fn mark_header_report_sent(paths: &WorkflowPaths, _guard: &WorkspaceLock) -> Result<()> {
    let mut record = read_object(paths)?;
    record.insert("loop_report".into(), serde_json::json!(ReportMark::Sent));
    Ok(crate::disk::atomic::write_temp_then_rename(
        &paths.record,
        &Value::Object(record),
    )?)
}

/// The id of the report an agent starter is owed, minted and written into the record, in place,
/// on the first call and read back on every later one, whatever version wrote the record.
#[must_use = "durability barrier; check the result"]
pub fn stamp_report_message(
    paths: &WorkflowPaths,
    _guard: &WorkspaceLock,
) -> Result<crate::ids::MessageId> {
    let mut record = read_object(paths)?;
    if let Some(id) = record
        .get("report_message")
        .and_then(|id| crate::ids::MessageId::deserialize(id).ok())
    {
        return Ok(id);
    }
    let id = crate::ids::MessageId::new();
    record.insert("report_message".into(), serde_json::json!(id));
    crate::disk::atomic::write_temp_then_rename(&paths.record, &Value::Object(record))?;
    Ok(id)
}

fn read_object(paths: &WorkflowPaths) -> Result<serde_json::Map<String, Value>> {
    let bytes = std::fs::read(&paths.record).map_err(WorkflowStoreErr::io(&paths.record))?;
    match serde_json::from_slice(&bytes) {
        Ok(Value::Object(record)) => Ok(record),
        Ok(_) => Err(WorkflowStoreErr::Io {
            path: paths.record.clone(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the record holds no JSON object",
            ),
        }),
        Err(source) => Err(WorkflowStoreErr::Json {
            path: paths.record.clone(),
            source,
        }),
    }
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
            .is_some_and(|header| header.is_terminal() && !header.owes_report()),
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
