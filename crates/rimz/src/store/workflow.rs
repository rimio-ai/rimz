//! Durable workflow records and append-only history; transitions belong to the harness.

use std::collections::BTreeMap;
use std::path::PathBuf;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::disk::lock::WorkspaceLock;
use crate::disk::paths::{StatePaths, WorkflowPaths};
use crate::ids::{AgentKind, RunId, WorkflowRunId};

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Key {
    String(String),
    Integer(i64),
    Unsigned(u64),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StepVisits {
    pub total: u64,
    pub keys: Vec<KeyVisits>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeyVisits {
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
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LiveAttempt {
    pub kind: AttemptKind,
    pub seq: u64,
    pub node: String,
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
    pub started_at: Timestamp,
    pub ended_at: Timestamp,
    pub handle: Option<String>,
    pub run_id: Option<RunId>,
    pub claim_path: PathBuf,
    pub head_start: Option<String>,
    pub head_end: Option<String>,
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
    #[serde(default)]
    pub detail: Option<String>,
    pub first: Timestamp,
    pub last: Timestamp,
    pub count: u64,
    pub recheck_at: Option<Timestamp>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub version: u32,
    pub id: WorkflowRunId,
    pub workflow: String,
    #[serde(default)]
    pub task: Option<String>,
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
    pub prev_attempt: Option<PreviousAttempt>,
    pub terminal_at: Option<Timestamp>,
    #[serde(default)]
    pub check_retry: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_retry_context: Option<Value>,
}

impl RunRecord {
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
#[serde(tag = "event", rename_all = "snake_case")]
pub enum JournalEvent {
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
    Wait {
        outcome: String,
        #[serde(default)]
        detail: Option<String>,
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
    Cancel,
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
    if record.version != 1 {
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
    use std::io::{BufRead, BufReader};
    let file = match std::fs::File::open(&paths.journal) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(WorkflowStoreErr::Io {
                path: paths.journal.clone(),
                source,
            });
        }
    };
    let mut entries = Vec::new();
    for line in BufReader::new(file).split(b'\n') {
        let line = line.map_err(WorkflowStoreErr::io(&paths.journal))?;
        match serde_json::from_slice(&line) {
            Ok(entry) => entries.push(entry),
            Err(err) => {
                tracing::debug!(path = %paths.journal.display(), %err, "skipping unreadable workflow history row")
            }
        }
    }
    Ok(entries)
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

#[cfg(test)]
mod tests;
