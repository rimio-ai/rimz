//! Durable per-run records for supervised `rimz agents -p` turns: schema, codec, and the terminal wake sender.
//!
//! Run records are cold-path durable state: a waiting CLI may exit, a user may
//! inspect the result later with `rimz agents show`, and the final assistant text
//! is the product output. Writes therefore use fsyncing temp-file-plus-rename,
//! unlike cache sidecars whose correctness rides the event log.

use std::fs;
use std::io;
use std::os::unix::net::UnixDatagram as StdUnixDatagram;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::agents::PermissionMode;
use crate::disk::atomic::write_temp_then_rename;
use crate::disk::paths::RuntimePaths;
use crate::ids::{AgentKind, AgentSessionId, PaneId, RunId, WorkspaceId};

#[derive(Debug, thiserror::Error)]
pub enum RunStoreErr {
    #[error("run {0} not found")]
    NotFound(RunId),
    #[error(transparent)]
    Atomic(#[from] crate::disk::atomic::AtomicErr),
    #[error(transparent)]
    Lock(#[from] crate::disk::lock::LockErr),
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("json parse error on {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("run {run_id} is {actual}; expected {expected}")]
    InvalidStatus {
        run_id: RunId,
        actual: &'static str,
        expected: &'static str,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Pending,
    Running,
    Completed,
    Failed,
    VerifyFailed,
    TimedOut,
    BudgetExceeded,
    Canceled,
}

impl RunStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::VerifyFailed => "verify_failed",
            Self::TimedOut => "timed_out",
            Self::BudgetExceeded => "budget_exceeded",
            Self::Canceled => "canceled",
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed
                | Self::Failed
                | Self::VerifyFailed
                | Self::TimedOut
                | Self::BudgetExceeded
                | Self::Canceled
        )
    }

    pub const fn exit_code(self) -> i32 {
        match self {
            Self::Completed => 0,
            Self::Failed => 1,
            Self::VerifyFailed => 123,
            Self::Canceled => 130,
            Self::BudgetExceeded => 125,
            Self::TimedOut | Self::Pending | Self::Running => 124,
        }
    }

    pub const fn is_retryable(self) -> bool {
        matches!(self, Self::Failed)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunVerify {
    pub cmd: String,
    pub attempts: u32,
    pub passed: bool,
    pub code: Option<i32>,
    pub timed_out: bool,
    pub output: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: RunId,
    pub workspace_id: WorkspaceId,
    pub kind: AgentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<AgentSessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<PaneId>,
    /// Spawned provider process owned by the in-pane wrapper.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_pid: Option<u32>,
    /// Process-start token paired with `provider_pid` to reject PID reuse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_process_start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_tail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_of: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<RunVerify>,
    pub status: RunStatus,
    pub permission_mode: PermissionMode,
    /// Never reclaim this run's pane automatically, including when its parent
    /// agent exits.
    #[serde(default, skip_serializing_if = "is_false")]
    pub keep: bool,
    /// Pane-backed child launched through `rimz subagents`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub subagent: bool,
    /// Time at which the caller claimed the settled result, either by printing
    /// it during an open agent turn (or to a human shell) or discarding it
    /// through `rimz subagents stop`; joined runs are
    /// excluded from the next completion digest and let the joiner cancel a
    /// digest once every row it lists has been joined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub joined_at: Option<Timestamp>,
    /// Completion digest that listed this run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_message_id: Option<crate::ids::MessageId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    pub prompt: String,
    pub worktree_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message: Option<String>,
    pub started_at: Timestamp,
    /// Producer-enforced wall-clock deadline for this supervised attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_at: Option<Timestamp>,
    pub updated_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<Timestamp>,
}

impl RunRecord {
    pub fn new(
        workspace_id: WorkspaceId,
        kind: AgentKind,
        permission_mode: PermissionMode,
        prompt: String,
        worktree_path: PathBuf,
    ) -> Self {
        let now = Timestamp::now();
        Self {
            run_id: RunId::new(),
            workspace_id,
            kind,
            agent_id: None,
            agent_name: None,
            pane_id: None,
            provider_pid: None,
            provider_process_start: None,
            transcript_path: None,
            failure_tail: None,
            retry_of: None,
            loop_task: None,
            verify: None,
            status: RunStatus::Pending,
            permission_mode,
            keep: false,
            subagent: false,
            joined_at: None,
            report_message_id: None,
            budget: None,
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            prompt,
            worktree_path,
            last_message: None,
            started_at: now,
            deadline_at: None,
            updated_at: now,
            completed_at: None,
        }
    }

    /// Terminal state is sticky; callers must supply a terminal status.
    pub(crate) fn mark_terminal(&mut self, status: RunStatus, now: Timestamp) -> bool {
        debug_assert!(status.is_terminal());
        if self.status.is_terminal() {
            return false;
        }
        self.status = status;
        self.completed_at = Some(now);
        self.updated_at = now;
        true
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Wakeup frame the store writer sends to a per-run socket when a supervised
/// `rimz agents -p` turn reaches a terminal state.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WakeupFrame {
    RunCompleted {
        workspace_id: WorkspaceId,
        run_id: RunId,
        status: RunStatus,
    },
}

pub(crate) fn run_socket_path(rt: &RuntimePaths, run_id: &RunId) -> PathBuf {
    rt.sock_dir.join(format!("run.{}.sock", run_id.short()))
}

/// Send a terminal datagram to the supervised-run waiter. Durable run state
/// remains authoritative; sender creation and per-target failures are absorbed.
pub fn wake_run(rt: &RuntimePaths, record: &RunRecord) {
    let target = run_socket_path(rt, &record.run_id);
    if !target.exists() {
        return;
    }
    // String ids and a unit status enum cannot fail JSON serialization.
    let payload = serde_json::to_vec(&WakeupFrame::RunCompleted {
        workspace_id: record.workspace_id.clone(),
        run_id: record.run_id.clone(),
        status: record.status,
    })
    .expect("run wake frame is JSON-serializable");
    let sender = match StdUnixDatagram::unbound() {
        Ok(sender) => sender,
        Err(error) => {
            debug!(%error, "run wake: creating sender socket failed");
            return;
        }
    };
    if let Err(error) = sender.set_nonblocking(true) {
        debug!(%error, "run wake: making sender socket non-blocking failed");
        return;
    }
    if let Err(error) = sender.send_to(&payload, &target) {
        debug!(?target, %error, "run wake: send_to failed (waiter may have exited)");
    }
}

type Result<T> = std::result::Result<T, RunStoreErr>;

fn run_path(runs_dir: &Path, run_id: &RunId) -> PathBuf {
    runs_dir.join(format!("{run_id}.json"))
}

#[must_use = "durability barrier; check the result"]
pub(crate) fn write(runs_dir: &Path, record: &RunRecord) -> Result<()> {
    write_temp_then_rename(&run_path(runs_dir, &record.run_id), record)?;
    Ok(())
}

pub(crate) fn load(runs_dir: &Path, run_id: &RunId) -> Result<RunRecord> {
    let path = run_path(runs_dir, run_id);
    if !path.exists() {
        return Err(RunStoreErr::NotFound(run_id.clone()));
    }
    let bytes = fs::read(&path).map_err(|source| RunStoreErr::Io {
        path: path.clone(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| RunStoreErr::Json { path, source })
}

pub(crate) fn list(runs_dir: &Path) -> Result<Vec<RunRecord>> {
    if !runs_dir.exists() {
        return Ok(Vec::new());
    }
    let entries = fs::read_dir(runs_dir).map_err(|source| RunStoreErr::Io {
        path: runs_dir.to_path_buf(),
        source,
    })?;
    let mut records = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| RunStoreErr::Io {
            path: runs_dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let bytes = fs::read(&path).map_err(|source| RunStoreErr::Io {
            path: path.clone(),
            source,
        })?;
        records.push(
            serde_json::from_slice::<RunRecord>(&bytes).map_err(|source| RunStoreErr::Json {
                path: path.clone(),
                source,
            })?,
        );
    }
    records.sort_by_key(|record| std::cmp::Reverse(record.updated_at));
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::PermissionMode;
    use crate::ids::{AgentKind, WorkspaceId};
    use tempfile::tempdir;

    #[test]
    fn write_load_and_list_runs() {
        let dir = tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let mut first = RunRecord::new(
            workspace_id.clone(),
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "first".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        let mut second = RunRecord::new(
            workspace_id,
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "second".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        first.status = RunStatus::Completed;
        second.updated_at = first.updated_at + std::time::Duration::from_secs(1);

        write(dir.path(), &first).unwrap();
        write(dir.path(), &second).unwrap();

        let loaded = load(dir.path(), &first.run_id).unwrap();
        assert_eq!(loaded.prompt, "first");
        let listed = list(dir.path()).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].run_id, second.run_id);
    }
    #[test]
    fn retention_and_report_fields_default_for_old_run_records() {
        let record = RunRecord::new(
            WorkspaceId::from_project_root(Path::new("/tmp/rimz-run")),
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        let mut old_json = serde_json::to_value(&record).expect("serialize run");
        old_json.as_object_mut().expect("run object").remove("keep");
        old_json
            .as_object_mut()
            .expect("run object")
            .remove("subagent");
        old_json
            .as_object_mut()
            .expect("run object")
            .remove("joined_at");
        old_json
            .as_object_mut()
            .expect("run object")
            .remove("report_message_id");
        old_json
            .as_object_mut()
            .expect("run object")
            .remove("provider_pid");
        old_json
            .as_object_mut()
            .expect("run object")
            .remove("provider_process_start");

        let old: RunRecord = serde_json::from_value(old_json).expect("deserialize old run");

        assert!(!old.keep);
        assert!(!old.subagent);
        assert_eq!(old.joined_at, None);
        assert_eq!(old.report_message_id, None);
        assert_eq!(old.provider_pid, None);
        assert_eq!(old.provider_process_start, None);
    }

    #[test]
    fn retry_link_round_trips_and_defaults_when_absent() {
        let mut record = RunRecord::new(
            WorkspaceId::from_project_root(Path::new("/tmp/rimz-run")),
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        let prior = RunId::new();
        record.retry_of = Some(prior.clone());

        let mut value = serde_json::to_value(&record).unwrap();
        let decoded: RunRecord = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(decoded.retry_of.as_ref(), Some(&prior));

        value.as_object_mut().unwrap().remove("retry_of");
        let decoded: RunRecord = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.retry_of, None);
    }
}
