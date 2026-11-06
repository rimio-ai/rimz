//! Durable workflow records and append-only history; transitions belong to the harness.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::disk::lock::WorkspaceLock;
use crate::disk::paths::{StatePaths, WorkflowPaths};
use crate::ids::{AgentKind, AgentSessionId, HoldKind, RunId, WorkflowRunId};

pub const VERSION: u32 = 3;

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
    #[error("workflow record at {path} has unsupported version {version}")]
    Version { path: PathBuf, version: u32 },
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

/// A workflow name and the basename of its recorded scope path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunAddress {
    pub workflow: String,
    pub lane: String,
}

#[derive(Debug, thiserror::Error)]
#[error("run address must be <workflow>#<lane> with nonempty parts")]
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
            .filter(|(workflow, lane)| !workflow.is_empty() && !lane.is_empty())
            .ok_or(RunAddressErr)?;
        Ok(Self {
            workflow: workflow.into(),
            lane: lane.into(),
        })
    }
}

impl RunAddress {
    /// Match a requested address against a recorded one, accepting dashed worktree names.
    pub fn matches(&self, address: &Self) -> bool {
        self.workflow == address.workflow
            && (self.lane == address.lane || self.lane.replace('/', "-") == address.lane)
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
    #[serde(alias = "keys")]
    pub outcomes: Vec<OutcomeVisits>,
}

impl StepVisits {
    /// Visits in this outcome/key bucket since the last reset; 0 before its first visit.
    pub fn for_bucket(&self, bucket: (Option<&str>, Option<&Key>)) -> u64 {
        self.bucket_index(bucket)
            .map_or(0, |index| self.outcomes[index].visits)
    }

    /// Count one visit in this bucket and in the never-reset total.
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
    #[serde(default)]
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
    #[serde(default, skip_serializing_if = "is_zero")]
    pub asked_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judged: Option<TurnEnd>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Timestamp>,
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
    pub channel: String,
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub version: u32,
    #[serde(default = "turn_lifecycle")]
    pub default_lifecycle: StepLifecycle,
    pub id: WorkflowRunId,
    pub workflow: String,
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub manual: bool,
    pub spec_dir: PathBuf,
    pub spec_digest: String,
    pub prompt: Option<String>,
    pub scope: Scope,
    pub project_root: PathBuf,
    pub created_at: Timestamp,
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
    pub prev_attempt: Option<PreviousAttempt>,
    pub terminal_at: Option<Timestamp>,
    #[serde(default)]
    pub check_retry: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_retry_context: Option<Value>,
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
        RunAddress {
            workflow: self.workflow.clone(),
            lane: self
                .scope
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        }
    }

    /// The shared predicate for a live run of a loop task.
    pub fn started_by(&self, task: &str) -> bool {
        !self.state.is_terminal() && self.task.as_deref() == Some(task)
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
    },
    VisitsReset {
        step: String,
        outcome: String,
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
}

pub fn load(paths: &WorkflowPaths) -> Result<RunRecord> {
    let bytes = std::fs::read(&paths.record).map_err(WorkflowStoreErr::io(&paths.record))?;
    let record: RunRecord =
        serde_json::from_slice(&bytes).map_err(|source| WorkflowStoreErr::Json {
            path: paths.record.clone(),
            source,
        })?;
    if !matches!(record.version, 1 | 2 | VERSION) {
        return Err(WorkflowStoreErr::Version {
            path: paths.record.clone(),
            version: record.version,
        });
    }
    Ok(record)
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
    for dir in [paths.notes(), paths.signs(None)] {
        std::fs::create_dir_all(&dir).map_err(WorkflowStoreErr::io(&dir))?;
    }
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
    for paths in state.workflow_runs()? {
        match load(&paths) {
            Ok(record) => records.push(record),
            Err(WorkflowStoreErr::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                tracing::warn!(path = %paths.record.display(), error = %err, "skipping workflow with unreadable record");
            }
        }
    }
    Ok(records)
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
    for (_, paths) in homes {
        match load(&paths) {
            Ok(record) if !record.state.is_terminal() => records.push(record),
            Ok(_) => {}
            Err(WorkflowStoreErr::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) if mode == RunRead::BestEffort => {
                tracing::warn!(path = %paths.record.display(), %error,
                    "skipping workflow with unreadable record");
            }
            Err(error) => return Err(error),
        }
    }
    Ok(records)
}

#[cfg(test)]
mod tests;
