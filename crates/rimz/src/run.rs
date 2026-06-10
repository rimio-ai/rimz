//! Supervised interactive-agent runs.

use std::path::PathBuf;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::{AgentLifecycleObservation, LifecycleSignal, TurnPhase};
use crate::feed::{AgentState, AgentStatus, FeedItem, Surface};
use crate::ids::{AgentKind, AgentSessionId, PaneId, RequestId, RunId, WorkspaceId};
use crate::ledger::lock::WorkspaceLock;
use crate::ledger::run_store::{self, RunStoreErr};
use crate::ledger::{SidebarSnapshot, StatePaths};

pub const ENV_RUN_ID: &str = "RIMZ_RUN_ID";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    Auto,
    Ask,
    Yolo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Pending,
    Running,
    Completed,
    Failed,
    TimedOut,
    Canceled,
}

impl RunStatus {
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::TimedOut | Self::Canceled
        )
    }

    pub const fn exit_code(self) -> i32 {
        match self {
            Self::Completed => 0,
            Self::Failed => 1,
            Self::Canceled => 130,
            Self::TimedOut | Self::Pending | Self::Running => 124,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: RunId,
    pub workspace_id: WorkspaceId,
    pub kind: AgentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<AgentSessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<PaneId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    pub status: RunStatus,
    pub permission_mode: PermissionMode,
    pub prompt: String,
    pub worktree_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message: Option<String>,
    pub started_at: Timestamp,
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
            pane_id: None,
            transcript_path: None,
            status: RunStatus::Pending,
            permission_mode,
            prompt,
            worktree_path,
            last_message: None,
            started_at: now,
            updated_at: now,
            completed_at: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RunPendingAsk {
    pub request_id: RequestId,
    pub surface: Surface,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RunLiveStatus {
    pub agent_status: AgentStatus,
    pub phase: TurnPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<PaneId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_pct: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_ask: Option<RunPendingAsk>,
}

pub fn create(paths: &StatePaths, record: &RunRecord) -> Result<()> {
    let _guard = WorkspaceLock::acquire(&paths.workspace_lock)?;
    run_store::write(&paths.runs_dir, record)
}

pub fn load(paths: &StatePaths, run_id: &RunId) -> Result<RunRecord> {
    run_store::load(&paths.runs_dir, run_id)
}

pub fn list(paths: &StatePaths) -> Result<Vec<RunRecord>> {
    run_store::list(&paths.runs_dir)
}

pub fn record_pane(paths: &StatePaths, run_id: &RunId, pane_id: PaneId) -> Result<RunRecord> {
    let _guard = WorkspaceLock::acquire(&paths.workspace_lock)?;
    let mut record = load(paths, run_id)?;
    if record.pane_id.as_ref() == Some(&pane_id) {
        return Ok(record);
    }
    record.pane_id = Some(pane_id);
    record.updated_at = Timestamp::now();
    run_store::write(&paths.runs_dir, &record)?;
    Ok(record)
}

pub fn timeout(paths: &StatePaths, run_id: &RunId) -> Result<RunRecord> {
    mark_terminal(paths, run_id, RunStatus::TimedOut).map(|(record, _wrote)| record)
}

pub fn cancel(paths: &StatePaths, run_id: &RunId) -> Result<(RunRecord, bool)> {
    mark_terminal(paths, run_id, RunStatus::Canceled)
}

pub fn fail(paths: &StatePaths, run_id: &RunId) -> Result<RunRecord> {
    mark_terminal(paths, run_id, RunStatus::Failed).map(|(record, _wrote)| record)
}

pub fn fail_if_nonterminal(paths: &StatePaths, run_id: &RunId) -> Result<Option<RunRecord>> {
    let (record, wrote) = mark_terminal(paths, run_id, RunStatus::Failed)?;
    Ok(wrote.then_some(record))
}

fn mark_terminal(
    paths: &StatePaths,
    run_id: &RunId,
    status: RunStatus,
) -> Result<(RunRecord, bool)> {
    let _guard = WorkspaceLock::acquire(&paths.workspace_lock)?;
    let mut record = load(paths, run_id)?;
    if record.status.is_terminal() {
        return Ok((record, false));
    }
    let now = Timestamp::now();
    record.status = status;
    record.updated_at = now;
    record.completed_at = Some(now);
    run_store::write(&paths.runs_dir, &record)?;
    Ok((record, true))
}

/// Fold one lifecycle observation into an optional run record update.
///
/// Returns `Some(record)` only when this observation newly makes the run
/// terminal, so callers can send exactly one wakeup datagram.
pub fn record_lifecycle(
    paths: &StatePaths,
    run_id: &RunId,
    kind: &str,
    observation: &AgentLifecycleObservation,
    last_message: Option<String>,
) -> Result<Option<RunRecord>> {
    let _guard = WorkspaceLock::acquire(&paths.workspace_lock)?;
    if observation.parent_agent_id.is_some()
        || matches!(
            observation.signal,
            LifecycleSignal::SubagentStarted | LifecycleSignal::SubagentStopped { .. }
        )
    {
        return Ok(None);
    }

    let mut record = load(paths, run_id)?;
    if record.kind.as_str() != kind || record.status.is_terminal() {
        return Ok(None);
    }

    match (&record.agent_id, &observation.agent_id) {
        (Some(bound), Some(observed)) if observed != bound => return Ok(None),
        (Some(_), None) => return Ok(None),
        (None, Some(observed)) => record.agent_id = Some(observed.clone()),
        (None, None) | (Some(_), Some(_)) => {}
    }

    let completion = terminal_status_for_signal(&observation.signal);
    let now = Timestamp::now();
    if let Some(status) = completion {
        record.status = status;
        if let Some(path) = observation.transcript_path.as_ref() {
            record.transcript_path = Some(path.clone());
        }
        record.last_message = last_message.or(record.last_message);
        record.updated_at = now;
        record.completed_at = Some(now);
        run_store::write(&paths.runs_dir, &record)?;
        Ok(Some(record))
    } else {
        let first_transcript_path =
            record.transcript_path.is_none() && observation.transcript_path.is_some();
        if record.status == RunStatus::Pending || first_transcript_path {
            record.status = RunStatus::Running;
            if record.transcript_path.is_none()
                && let Some(path) = observation.transcript_path.as_ref()
            {
                record.transcript_path = Some(path.clone());
            }
            record.updated_at = now;
            run_store::write(&paths.runs_dir, &record)?;
        }
        Ok(None)
    }
}

pub fn live_status(record: &RunRecord, snapshot: &SidebarSnapshot) -> Option<RunLiveStatus> {
    if record.status.is_terminal() {
        return None;
    }
    let agent_id = record.agent_id.as_ref()?;
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.kind == record.kind && &agent.agent_id == agent_id)?;
    Some(RunLiveStatus {
        agent_status: agent.status,
        phase: agent.phase,
        pane_id: agent
            .pane
            .as_ref()
            .map(|pane| pane.pane_id.clone())
            .or_else(|| record.pane_id.clone()),
        context_pct: agent_context_pct(agent),
        pending_ask: pending_ask_for(agent, snapshot),
    })
}

fn agent_context_pct(agent: &AgentState) -> Option<u8> {
    agent
        .context
        .as_ref()
        .and_then(|context| context.tokens.as_ref())
        .and_then(|tokens| tokens.used_percentage)
        .or(agent.context_pct)
}

fn pending_ask_for(agent: &AgentState, snapshot: &SidebarSnapshot) -> Option<RunPendingAsk> {
    snapshot
        .needs_attention
        .iter()
        .chain(snapshot.resolver_working.iter())
        .find(|item| item_matches_agent(item, agent))
        .map(|item| RunPendingAsk {
            request_id: item.request_id.clone(),
            surface: item.surface,
        })
}

fn item_matches_agent(item: &FeedItem, agent: &AgentState) -> bool {
    item.source_kind == "agent-hook"
        && item.source == agent.kind.as_str()
        && item.agent_session_id() == Some(agent.agent_id.as_str())
}

/// Terminal run status produced by one agent lifecycle signal.
///
/// Hook ingestion uses this same predicate before extracting the final
/// assistant message, so the deliverable and status transition stay in lockstep.
pub fn terminal_status_for_signal(signal: &LifecycleSignal) -> Option<RunStatus> {
    match signal {
        LifecycleSignal::TurnEnded {
            errored,
            parked_on_background,
        } if !parked_on_background => Some(if *errored {
            RunStatus::Failed
        } else {
            RunStatus::Completed
        }),
        LifecycleSignal::Ended => Some(RunStatus::Failed),
        _ => None,
    }
}

pub type Result<T> = std::result::Result<T, RunStoreErr>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use crate::agents::LifecycleSignal;
    use crate::feed::{AgentState, FeedKind, PaneRef};
    use crate::ids::MuxName;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn terminal_status_maps_to_exit_code() {
        assert_eq!(RunStatus::Completed.exit_code(), 0);
        assert_eq!(RunStatus::Failed.exit_code(), 1);
        assert_eq!(RunStatus::TimedOut.exit_code(), 124);
        assert_eq!(RunStatus::Canceled.exit_code(), 130);
        assert!(RunStatus::Canceled.is_terminal());
    }

    #[test]
    fn lifecycle_completion_writes_terminal_record_once() {
        let dir = tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        let record = RunRecord::new(
            workspace_id,
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        create(&paths, &record).unwrap();
        let observation = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("sess-1")),
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
        );
        let completed = record_lifecycle(
            &paths,
            &record.run_id,
            "claude",
            &observation,
            Some("done".to_owned()),
        )
        .unwrap()
        .expect("terminal update");
        assert_eq!(completed.status, RunStatus::Completed);
        assert_eq!(completed.last_message.as_deref(), Some("done"));
        assert_eq!(completed.agent_id.as_deref(), Some("sess-1"));

        let repeated = record_lifecycle(
            &paths,
            &record.run_id,
            "claude",
            &observation,
            Some("done".to_owned()),
        )
        .unwrap();
        assert!(repeated.is_none());
    }

    #[test]
    fn subagent_observation_does_not_complete_parent_run() {
        let dir = tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        let record = RunRecord::new(
            workspace_id,
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        create(&paths, &record).unwrap();
        let mut observation = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("child-1")),
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
        );
        observation.parent_agent_id = Some(AgentSessionId::from("sess-parent"));

        let update = record_lifecycle(
            &paths,
            &record.run_id,
            "claude",
            &observation,
            Some("child done".to_owned()),
        )
        .unwrap();
        assert!(update.is_none());
        let after = load(&paths, &record.run_id).unwrap();
        assert_eq!(after.status, RunStatus::Pending);
        assert_eq!(after.last_message, None);
    }

    #[test]
    fn same_kind_child_process_does_not_complete_bound_parent_run() {
        let dir = tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        let record = RunRecord::new(
            workspace_id,
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        create(&paths, &record).unwrap();
        let parent = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("sess-parent")),
            LifecycleSignal::TurnStarted,
        );
        record_lifecycle(&paths, &record.run_id, "claude", &parent, None).unwrap();

        let child = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("sess-child")),
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
        );
        let update = record_lifecycle(
            &paths,
            &record.run_id,
            "claude",
            &child,
            Some("child done".to_owned()),
        )
        .unwrap();

        assert!(update.is_none());
        let after = load(&paths, &record.run_id).unwrap();
        assert_eq!(after.status, RunStatus::Running);
        assert_eq!(after.agent_id.as_deref(), Some("sess-parent"));
        assert_eq!(after.last_message, None);
    }

    #[test]
    fn timeout_marks_nonterminal_run_and_preserves_terminal_run() {
        let dir = tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        let record = RunRecord::new(
            workspace_id,
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        create(&paths, &record).unwrap();

        let timed_out = timeout(&paths, &record.run_id).unwrap();
        assert_eq!(timed_out.status, RunStatus::TimedOut);
        assert!(timed_out.completed_at.is_some());

        let still_timed_out = fail(&paths, &record.run_id).unwrap();
        assert_eq!(still_timed_out.status, RunStatus::TimedOut);
    }

    #[test]
    fn cancel_marks_nonterminal_run_and_preserves_terminal_run() {
        let dir = tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        let record = RunRecord::new(
            workspace_id,
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        create(&paths, &record).unwrap();

        let (canceled, wrote) = cancel(&paths, &record.run_id).unwrap();
        assert!(wrote);
        assert_eq!(canceled.status, RunStatus::Canceled);
        assert!(canceled.completed_at.is_some());

        let (still_canceled, wrote) = cancel(&paths, &record.run_id).unwrap();
        assert!(!wrote);
        assert_eq!(still_canceled.status, RunStatus::Canceled);
    }

    #[test]
    fn record_lifecycle_folds_transcript_path_on_run_writes() {
        let dir = tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        let record = RunRecord::new(
            workspace_id,
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        create(&paths, &record).unwrap();

        let mut started = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("sess-1")),
            LifecycleSignal::TurnStarted,
        );
        started.transcript_path = Some("/tmp/first.jsonl".to_owned());
        assert!(
            record_lifecycle(&paths, &record.run_id, "claude", &started, None)
                .unwrap()
                .is_none()
        );
        let running = load(&paths, &record.run_id).unwrap();
        assert_eq!(running.status, RunStatus::Running);
        assert_eq!(running.transcript_path.as_deref(), Some("/tmp/first.jsonl"));

        let mut tool = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("sess-1")),
            LifecycleSignal::ToolUsed {
                mutates: true,
                edits: true,
            },
        );
        tool.transcript_path = Some("/tmp/second.jsonl".to_owned());
        record_lifecycle(&paths, &record.run_id, "claude", &tool, None).unwrap();
        assert_eq!(
            load(&paths, &record.run_id)
                .unwrap()
                .transcript_path
                .as_deref(),
            Some("/tmp/first.jsonl"),
            "a non-terminal running observation does not add a run-store write"
        );

        let mut stopped = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("sess-1")),
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
        );
        stopped.transcript_path = Some("/tmp/second.jsonl".to_owned());
        record_lifecycle(
            &paths,
            &record.run_id,
            "claude",
            &stopped,
            Some("done".to_owned()),
        )
        .unwrap();
        assert_eq!(
            load(&paths, &record.run_id)
                .unwrap()
                .transcript_path
                .as_deref(),
            Some("/tmp/second.jsonl")
        );
    }

    #[test]
    fn record_lifecycle_folds_first_late_transcript_path() {
        let dir = tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        let record = RunRecord::new(
            workspace_id,
            AgentKind::new_unchecked("codex"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        create(&paths, &record).unwrap();

        let started = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("sess-1")),
            LifecycleSignal::TurnStarted,
        );
        record_lifecycle(&paths, &record.run_id, "codex", &started, None).unwrap();
        let running = load(&paths, &record.run_id).unwrap();
        assert_eq!(running.status, RunStatus::Running);
        assert_eq!(running.transcript_path, None);

        let mut tool = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("sess-1")),
            LifecycleSignal::ToolUsed {
                mutates: true,
                edits: true,
            },
        );
        tool.transcript_path = Some("/tmp/late.jsonl".to_owned());
        record_lifecycle(&paths, &record.run_id, "codex", &tool, None).unwrap();
        assert_eq!(
            load(&paths, &record.run_id)
                .unwrap()
                .transcript_path
                .as_deref(),
            Some("/tmp/late.jsonl")
        );
    }

    #[test]
    fn live_status_joins_agent_state_and_pending_ask() {
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let mut record = RunRecord::new(
            workspace_id.clone(),
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        record.status = RunStatus::Running;
        record.agent_id = Some(AgentSessionId::from("sess-1"));
        let pane_id = PaneId::from_parts(MuxName::Tmux, "%7");
        let mut pane = PaneRef::from_id(pane_id.clone());
        pane.session_name = "rimz-test".to_owned();
        let mut agent = agent_state("claude", "sess-1", AgentStatus::Running);
        agent.phase = TurnPhase::Reasoning;
        agent.pane = Some(pane);
        agent.context_pct = Some(42);
        let mut ask = FeedItem::new(
            workspace_id.clone(),
            Surface::NativeUi,
            FeedKind::Permission,
            "Approve?",
            "claude",
            "agent-hook",
        );
        ask.payload = json!({ "session_id": "sess-1" });
        let request_id = ask.request_id.clone();
        let snapshot = SidebarSnapshot::build_with_agents(
            workspace_id,
            vec![ask],
            vec![agent],
            Timestamp::UNIX_EPOCH,
        );

        let live = live_status(&record, &snapshot).expect("live status");
        assert_eq!(live.agent_status, AgentStatus::Running);
        assert_eq!(live.phase, TurnPhase::Reasoning);
        assert_eq!(live.pane_id.as_ref(), Some(&pane_id));
        assert_eq!(live.context_pct, Some(42));
        assert_eq!(live.pending_ask.unwrap().request_id, request_id);
    }

    #[test]
    fn live_status_is_absent_for_unbound_or_terminal_runs() {
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let mut record = RunRecord::new(
            workspace_id.clone(),
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        record.status = RunStatus::Running;
        let snapshot = SidebarSnapshot::build_with_agents(
            workspace_id,
            vec![],
            vec![agent_state("claude", "sess-1", AgentStatus::Running)],
            Timestamp::UNIX_EPOCH,
        );
        assert!(live_status(&record, &snapshot).is_none());

        record.agent_id = Some(AgentSessionId::from("sess-1"));
        record.status = RunStatus::Completed;
        assert!(live_status(&record, &snapshot).is_none());
    }

    #[test]
    fn record_pane_persists_launch_pane_id() {
        let dir = tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        let record = RunRecord::new(
            workspace_id,
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        create(&paths, &record).unwrap();
        let pane_id = PaneId::from_parts(MuxName::Tmux, "%7");

        let updated = record_pane(&paths, &record.run_id, pane_id.clone()).unwrap();
        assert_eq!(updated.pane_id.as_ref(), Some(&pane_id));
        assert_eq!(
            load(&paths, &record.run_id).unwrap().pane_id.as_ref(),
            Some(&pane_id)
        );
    }

    fn agent_state(kind: &str, id: &str, status: AgentStatus) -> AgentState {
        AgentState {
            agent_id: AgentSessionId::from(id),
            kind: AgentKind::new_unchecked(kind),
            status,
            phase: TurnPhase::Idle,
            pane: None,
            agent_pid: None,
            agent_process_start: None,
            runtime_owner: None,
            parent_agent_id: None,
            worktree_path: None,
            worktree_branch: None,
            task: None,
            prompt: None,
            transcript_path: None,
            recent_prompts: Vec::new(),
            model: None,
            effort: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            cache_read_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            todo_done: None,
            todo_total: None,
            context: None,
            subagent_description: None,
            subagent_started_at: None,
            turn_started_at: None,
            compacting_since: None,
            compaction_count: 0,
            last_seen: Timestamp::UNIX_EPOCH,
            last_activity: Timestamp::UNIX_EPOCH,
            registered_at: Some(Timestamp::UNIX_EPOCH),
        }
    }
}
