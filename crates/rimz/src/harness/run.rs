//! Supervised-run requests, records, transitions, and cancellation.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::{AgentLifecycleObservation, LifecycleSignal, TurnPhase};
use crate::agents::{AgentState, AgentStatus};
use crate::harness::schedule::runner::tail_output;
use crate::ids::{AgentKind, AgentSessionId, PaneId, RunId, WorkspaceId};
use crate::store::lock::WorkspaceLock;
use crate::store::run_store::{self, RunStoreErr};
use crate::store::{SidebarSnapshot, StatePaths, Store};

pub const ENV_RUN_ID: &str = "RIMZ_RUN_ID";
/// The launched adapter kind (`claude`, `codex`, ...). Its presence marks the
/// process as a Rimz-launched agent for peer-message attribution.
pub const ENV_AGENT_KIND: &str = "RIMZ_AGENT_KIND";
pub const ENV_AGENT_NAME: &str = "RIMZ_AGENT_NAME";
/// The `[agents.profiles]` profile name an agent launched as, so it answers to
/// `@<profile>`. Set by the launch wrapper; read into the lifecycle observation.
pub const ENV_AGENT_PROFILE: &str = "RIMZ_AGENT_PROFILE";
/// The `[agents.teams]` role name an agent launched as, so it answers to
/// `@<role>`. Set by the launch wrapper; read into the lifecycle observation.
pub const ENV_AGENT_ROLE: &str = "RIMZ_AGENT_ROLE";
/// The `[agents.teams]` team name an agent launched under. Set by the launch
/// wrapper; read by member CLI calls so in-place teams scope to their channel.
pub const ENV_TEAM: &str = "RIMZ_TEAM";
/// The inline multi-agent launch cohort this agent belongs to. Team launches
/// use [`ENV_TEAM`] as their cohort key; inline layouts use this generated id.
pub const ENV_LAUNCH_GROUP: &str = "RIMZ_LAUNCH_GROUP";
/// The agent's order inside its launch cohort: team role-list index or inline
/// agent-cell index. Set by the wrapper; read into lifecycle observations.
pub const ENV_LAUNCH_ORDINAL: &str = "RIMZ_LAUNCH_ORDINAL";
/// Named cooperation lane an agent launched under. Set by the launch wrapper;
/// read by lifecycle hooks and peer-message commands as the routing channel.
pub const ENV_CHANNEL: &str = "RIMZ_CHANNEL";
/// The cwd backing a launched pane. Set with the room pin so split panes can
/// still report the worktree path they were opened for.
pub const ENV_WORKTREE_PATH: &str = "RIMZ_WORKTREE_PATH";
/// The model selected by launch flags or profile presets. Set by the launch
/// wrapper; read into the lifecycle observation as card identity fallback.
pub const ENV_AGENT_MODEL: &str = "RIMZ_AGENT_MODEL";
/// The reasoning effort selected by launch flags or profile presets. Set by
/// the launch wrapper; read into the lifecycle observation as card identity fallback.
pub const ENV_AGENT_EFFORT: &str = "RIMZ_AGENT_EFFORT";
/// The canonical dollar cap selected by launch flags, profiles, or roles.
/// Set by the launch wrapper and read into lifecycle observations.
pub const ENV_AGENT_BUDGET: &str = "RIMZ_AGENT_BUDGET";
/// The configured `[harness] rtk` mode (`auto`/`on`/`off`), exported to every
/// agent launch so `cargo xtask` can route recognized cargo commands through
/// `rtk`. Read by xtask, never by rimz itself.
pub const ENV_RTK: &str = "RIMZ_RTK";
const FAILURE_TAIL_CAP: usize = 4 * 1024;

/// Typed cancellation signal shared between CLI signal handlers and the
/// supervised-run waiter.
#[derive(Clone, Debug, Default)]
pub struct RunCancellation {
    requested: Arc<AtomicBool>,
}

/// Command-neutral input for one supervised turn.
#[derive(Clone, Debug)]
pub struct SupervisedRunRequest {
    pub spec: String,
    pub prompt: String,
    pub description: Option<String>,
    pub worktree: Option<String>,
    pub from_pr: Option<crate::forge::PrTarget>,
    pub channel: Option<String>,
    pub name: Option<String>,
    pub background: bool,
    pub force_new_tab: bool,
    pub permission_mode: PermissionMode,
    pub model: Option<String>,
    pub system_prompt_file: Option<PathBuf>,
    pub append_system_prompt_file: Option<PathBuf>,
    pub effort: Option<String>,
    pub budget: Option<crate::harness::budget::BudgetSpec>,
    pub max_turns: Option<u32>,
    pub timeout: Option<std::time::Duration>,
    pub keep: bool,
    pub retries: u32,
    pub verify: Option<String>,
    pub max_attempts: Option<u32>,
    pub loop_zone: bool,
    pub loop_task: Option<String>,
    pub passthrough: Vec<String>,
}

/// Command-neutral result of attempting one supervised turn.
#[derive(Debug)]
pub enum SupervisedRunOutcome {
    Record(Box<RunRecord>),
    Background,
    BudgetExceeded { reason: String },
}

impl RunCancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn request(&self) {
        self.requested.store(true, Ordering::SeqCst);
    }

    pub fn reset(&self) {
        self.requested.store(false, Ordering::SeqCst);
    }

    pub fn is_requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }

    /// Shared flag registered by the CLI's OS signal effect.
    pub fn signal_flag(&self) -> Arc<AtomicBool> {
        self.requested.clone()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CancelRunErr {
    #[error(transparent)]
    Store(#[from] RunStoreErr),
    #[error(transparent)]
    Wake(#[from] crate::store::wakeup::WakeupErr),
}

/// Durably cancel a run and wake its waiter only for the newly-written
/// terminal transition.
pub fn cancel_and_wake(
    store: &Store,
    run_id: &RunId,
) -> std::result::Result<RunRecord, CancelRunErr> {
    let (record, wrote) = cancel(store.paths(), run_id)?;
    if wrote {
        crate::store::wakeup::wake_run(store.runtime_paths(), &record)?;
    }
    Ok(record)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    Auto,
    Ask,
    Yolo,
    Plan,
}

impl FromStr for PermissionMode {
    type Err = String;

    fn from_str(raw: &str) -> std::result::Result<Self, Self::Err> {
        match raw {
            "auto" => Ok(Self::Auto),
            "ask" => Ok(Self::Ask),
            "yolo" => Ok(Self::Yolo),
            "plan" => Ok(Self::Plan),
            _ => Err(format!(
                "unknown permission mode `{raw}`; expected auto, ask, plan, or yolo"
            )),
        }
    }
}

impl fmt::Display for PermissionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Auto => "auto",
            Self::Ask => "ask",
            Self::Yolo => "yolo",
            Self::Plan => "plan",
        })
    }
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
            transcript_path: None,
            failure_tail: None,
            retry_of: None,
            loop_task: None,
            verify: None,
            status: RunStatus::Pending,
            permission_mode,
            budget: None,
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            prompt,
            worktree_path,
            last_message: None,
            started_at: now,
            updated_at: now,
            completed_at: None,
        }
    }
}

pub fn retry_prompt(base: &str, failure_tail: Option<&str>) -> String {
    let failure = failure_tail.map_or_else(
        || "A previous attempt at this task failed (exit 1), but no terminal output was captured."
            .to_owned(),
        |tail| {
            format!(
                "A previous attempt at this task failed (exit 1). The tail of its terminal output:\n{tail}"
            )
        },
    );
    format!("{base}\n\n<previous-attempt-failure>\n{failure}\n</previous-attempt-failure>")
}

pub fn verify_reprompt(cmd: &str, code_label: &str, output: &str) -> String {
    let tail = tail_output(output.as_bytes(), FAILURE_TAIL_CAP);
    format!(
        "Verification failed — the task is not done yet. Fix the underlying problem in this same session until the verify command passes.\n\n--- verify `{cmd}` exited {code_label} ---\n{tail}"
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RunLiveStatus {
    pub agent_status: AgentStatus,
    pub phase: TurnPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<PaneId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_pct: Option<u8>,
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

pub fn record_failure_tail(paths: &StatePaths, run_id: &RunId, tail: &str) -> Result<RunRecord> {
    let tail = tail.trim_end();
    let _guard = WorkspaceLock::acquire(&paths.workspace_lock)?;
    let mut record = load(paths, run_id)?;
    if record.failure_tail.is_some() || tail.trim().is_empty() {
        return Ok(record);
    }
    record.failure_tail = Some(
        tail_output(tail.as_bytes(), FAILURE_TAIL_CAP)
            .trim_end()
            .to_owned(),
    );
    record.updated_at = Timestamp::now();
    run_store::write(&paths.runs_dir, &record)?;
    Ok(record)
}

pub fn timeout(paths: &StatePaths, run_id: &RunId) -> Result<RunRecord> {
    mark_terminal(paths, run_id, RunStatus::TimedOut).map(|(record, _wrote)| record)
}

pub fn budget_exceeded(
    paths: &StatePaths,
    run_id: &RunId,
    cost_usd: Option<f64>,
) -> Result<(RunRecord, bool)> {
    let _guard = WorkspaceLock::acquire(&paths.workspace_lock)?;
    let mut record = load(paths, run_id)?;
    if record.status.is_terminal() {
        return Ok((record, false));
    }
    let now = Timestamp::now();
    record.status = RunStatus::BudgetExceeded;
    record.cost_usd = cost_usd.filter(|cost| cost.is_finite() && *cost >= 0.0);
    record.updated_at = now;
    record.completed_at = Some(now);
    run_store::write(&paths.runs_dir, &record)?;
    Ok((record, true))
}

pub fn record_spend(
    paths: &StatePaths,
    run_id: &RunId,
    cost_usd: Option<f64>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
) -> Result<RunRecord> {
    let cost_usd = cost_usd.filter(|cost| cost.is_finite() && *cost >= 0.0);
    if cost_usd.is_none() && input_tokens.is_none() && output_tokens.is_none() {
        return load(paths, run_id);
    }
    let _guard = WorkspaceLock::acquire(&paths.workspace_lock)?;
    let mut record = load(paths, run_id)?;
    if let Some(cost_usd) = cost_usd {
        record.cost_usd = Some(cost_usd);
    }
    if let Some(input_tokens) = input_tokens {
        record.input_tokens = Some(input_tokens);
    }
    if let Some(output_tokens) = output_tokens {
        record.output_tokens = Some(output_tokens);
    }
    record.updated_at = Timestamp::now();
    run_store::write(&paths.runs_dir, &record)?;
    Ok(record)
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

pub fn reopen_for_verify(
    paths: &StatePaths,
    run_id: &RunId,
    verify: RunVerify,
) -> Result<RunRecord> {
    let _guard = WorkspaceLock::acquire(&paths.workspace_lock)?;
    let mut record = load(paths, run_id)?;
    require_completed(&record)?;
    record.status = RunStatus::Running;
    record.verify = Some(verify);
    record.updated_at = Timestamp::now();
    record.completed_at = None;
    run_store::write(&paths.runs_dir, &record)?;
    Ok(record)
}

pub fn verify_failed(paths: &StatePaths, run_id: &RunId, verify: RunVerify) -> Result<RunRecord> {
    let _guard = WorkspaceLock::acquire(&paths.workspace_lock)?;
    let mut record = load(paths, run_id)?;
    if record.status == RunStatus::VerifyFailed {
        return Ok(record);
    }
    require_completed(&record)?;
    let now = Timestamp::now();
    record.status = RunStatus::VerifyFailed;
    record.verify = Some(verify);
    record.updated_at = now;
    record.completed_at = Some(now);
    run_store::write(&paths.runs_dir, &record)?;
    Ok(record)
}

pub fn verify_passed(paths: &StatePaths, run_id: &RunId, verify: RunVerify) -> Result<RunRecord> {
    let _guard = WorkspaceLock::acquire(&paths.workspace_lock)?;
    let mut record = load(paths, run_id)?;
    require_completed(&record)?;
    record.verify = Some(verify);
    record.updated_at = Timestamp::now();
    run_store::write(&paths.runs_dir, &record)?;
    Ok(record)
}

fn require_completed(record: &RunRecord) -> Result<()> {
    if record.status != RunStatus::Completed {
        return Err(RunStoreErr::InvalidStatus {
            run_id: record.run_id.clone(),
            actual: run_status_name(record.status),
            expected: "completed",
        });
    }
    Ok(())
}

const fn run_status_name(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Pending => "pending",
        RunStatus::Running => "running",
        RunStatus::Completed => "completed",
        RunStatus::Failed => "failed",
        RunStatus::VerifyFailed => "verify_failed",
        RunStatus::TimedOut => "timed_out",
        RunStatus::BudgetExceeded => "budget_exceeded",
        RunStatus::Canceled => "canceled",
    }
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
        (None, Some(observed)) => {
            record.agent_id = Some(observed.clone());
            record.agent_name = observation.agent_name.clone().or(record.agent_name);
        }
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

/// Store provider-declared final visible output without ending the run.
pub fn record_assistant_message(
    paths: &StatePaths,
    run_id: &RunId,
    kind: &str,
    agent_id: &AgentSessionId,
    message: String,
) -> Result<()> {
    let _guard = WorkspaceLock::acquire(&paths.workspace_lock)?;
    let mut record = load(paths, run_id)?;
    if record.kind.as_str() != kind || record.status.is_terminal() {
        return Ok(());
    }
    match &record.agent_id {
        Some(bound) if bound != agent_id => return Ok(()),
        None => record.agent_id = Some(agent_id.clone()),
        Some(_) => {}
    }
    record.last_message = Some(message);
    record.updated_at = Timestamp::now();
    run_store::write(&paths.runs_dir, &record)
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
    })
}

fn agent_context_pct(agent: &AgentState) -> Option<u8> {
    agent
        .context
        .as_ref()
        .and_then(|context| context.tokens.as_ref())
        // Trust a statusline percentage only alongside its own window; without
        // one, the fold-derived scalar (tied to the resolved window) is the
        // consistent reading.
        .filter(|tokens| tokens.context_window_size.is_some())
        .and_then(|tokens| tokens.used_percentage)
        .or(agent.context_pct)
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
        LifecycleSignal::TurnInterrupted => Some(RunStatus::Canceled),
        LifecycleSignal::Ended => Some(RunStatus::Failed),
        _ => None,
    }
}

pub type Result<T> = std::result::Result<T, RunStoreErr>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use crate::agents::AgentState;
    use crate::agents::LifecycleSignal;
    use crate::ids::MuxName;
    use crate::pane::PaneRef;
    use tempfile::tempdir;

    fn setup() -> (tempfile::TempDir, StatePaths, RunRecord) {
        setup_for("claude")
    }

    fn setup_for(kind: &str) -> (tempfile::TempDir, StatePaths, RunRecord) {
        let dir = tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        let record = RunRecord::new(
            workspace_id,
            AgentKind::new_unchecked(kind),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        create(&paths, &record).unwrap();
        (dir, paths, record)
    }

    #[test]
    fn lifecycle_completion_writes_terminal_record_once() {
        let (_dir, paths, record) = setup();
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
        let (_dir, paths, record) = setup();
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
        let (_dir, paths, record) = setup();
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
    fn terminal_transitions_are_once_only_and_map_exit_codes() {
        let (_dir, paths, record) = setup();
        let timed_out = timeout(&paths, &record.run_id).unwrap();
        assert_eq!(timed_out.status, RunStatus::TimedOut);
        assert!(timed_out.completed_at.is_some());
        assert_eq!(timed_out.status.exit_code(), 124);

        let still_timed_out = fail(&paths, &record.run_id).unwrap();
        assert_eq!(still_timed_out.status, RunStatus::TimedOut);

        let (_dir, paths, record) = setup();
        let (canceled, wrote) = cancel(&paths, &record.run_id).unwrap();
        assert!(wrote);
        assert_eq!(canceled.status, RunStatus::Canceled);
        assert!(canceled.completed_at.is_some());
        assert_eq!(canceled.status.exit_code(), 130);

        let (still_canceled, wrote) = cancel(&paths, &record.run_id).unwrap();
        assert!(!wrote);
        assert_eq!(still_canceled.status, RunStatus::Canceled);

        let (_dir, paths, record) = setup();
        let (budgeted, wrote) = budget_exceeded(&paths, &record.run_id, Some(5.25)).unwrap();
        assert!(wrote);
        assert_eq!(budgeted.status, RunStatus::BudgetExceeded);
        assert_eq!(budgeted.cost_usd, Some(5.25));
        assert_eq!(budgeted.status.exit_code(), 125);

        assert_eq!(RunStatus::Completed.exit_code(), 0);
        assert_eq!(RunStatus::Failed.exit_code(), 1);
        assert_eq!(RunStatus::VerifyFailed.exit_code(), 123);
        assert_eq!(RunStatus::BudgetExceeded.exit_code(), 125);
        assert!(RunStatus::Failed.is_retryable());
        assert!(RunStatus::VerifyFailed.is_terminal());
        assert!(!RunStatus::VerifyFailed.is_retryable());
        assert!(!RunStatus::Completed.is_retryable());
        assert!(!RunStatus::TimedOut.is_retryable());
        assert!(!RunStatus::BudgetExceeded.is_retryable());
        assert!(!RunStatus::Canceled.is_retryable());
        assert!(RunStatus::BudgetExceeded.is_terminal());
        assert!(RunStatus::Canceled.is_terminal());
    }

    #[test]
    fn retry_link_round_trips_and_defaults_when_absent() {
        let (_dir, _paths, mut record) = setup();
        let prior = RunId::new();
        record.retry_of = Some(prior.clone());

        let mut value = serde_json::to_value(&record).unwrap();
        let decoded: RunRecord = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(decoded.retry_of.as_ref(), Some(&prior));

        value.as_object_mut().unwrap().remove("retry_of");
        let decoded: RunRecord = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.retry_of, None);
    }

    #[test]
    fn verify_transitions_reopen_completed_runs_and_finish_once() {
        let (_dir, paths, record) = setup();
        let completed = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("sess-1")),
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
        );
        record_lifecycle(&paths, &record.run_id, "claude", &completed, None)
            .unwrap()
            .expect("completed run");
        let first = RunVerify {
            cmd: "cargo xtask test run".to_owned(),
            attempts: 1,
            passed: false,
            code: Some(1),
            timed_out: false,
            output: "red".to_owned(),
        };

        let reopened = reopen_for_verify(&paths, &record.run_id, first.clone()).unwrap();
        assert_eq!(reopened.status, RunStatus::Running);
        assert_eq!(reopened.completed_at, None);
        assert_eq!(reopened.verify.as_ref(), Some(&first));
        assert!(reopen_for_verify(&paths, &record.run_id, first.clone()).is_err());

        record_lifecycle(&paths, &record.run_id, "claude", &completed, None)
            .unwrap()
            .expect("second completed turn");
        let second = RunVerify {
            attempts: 2,
            output: "still red".to_owned(),
            ..first
        };
        let failed = verify_failed(&paths, &record.run_id, second.clone()).unwrap();
        assert_eq!(failed.status, RunStatus::VerifyFailed);
        assert_eq!(failed.verify.as_ref(), Some(&second));
        let updated_at = failed.updated_at;

        let repeated = verify_failed(&paths, &record.run_id, second).unwrap();
        assert_eq!(repeated.updated_at, updated_at);
    }

    #[test]
    fn record_spend_persists_tokens_and_ignores_non_finite_cost() {
        let (_dir, paths, record) = setup();

        let updated = record_spend(
            &paths,
            &record.run_id,
            Some(f64::NAN),
            Some(1_200),
            Some(340),
        )
        .unwrap();

        assert_eq!(updated.cost_usd, None);
        assert_eq!(updated.input_tokens, Some(1_200));
        assert_eq!(updated.output_tokens, Some(340));
        let unchanged = record_spend(&paths, &record.run_id, None, None, None).unwrap();
        assert_eq!(unchanged, updated);
    }

    #[test]
    fn record_lifecycle_folds_transcript_path_on_run_writes() {
        let (_dir, paths, record) = setup();

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
        let (_dir, paths, record) = setup_for("codex");

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
    fn live_status_joins_agent_state() {
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
        let mut agent = agent_state("claude", "sess-1", AgentStatus::Waiting);
        agent.phase = TurnPhase::Idle;
        agent.pane = Some(pane);
        agent.context_pct = Some(42);
        agent.waiting_since = Some(Timestamp::UNIX_EPOCH);
        let snapshot =
            SidebarSnapshot::build_with_agents(workspace_id, vec![agent], Timestamp::UNIX_EPOCH);

        let live = live_status(&record, &snapshot).expect("live status");
        assert_eq!(live.agent_status, AgentStatus::Waiting);
        assert_eq!(live.phase, TurnPhase::Idle);
        assert_eq!(live.pane_id.as_ref(), Some(&pane_id));
        assert_eq!(live.context_pct, Some(42));
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
        let (_dir, paths, record) = setup();
        let pane_id = PaneId::from_parts(MuxName::Tmux, "%7");

        let updated = record_pane(&paths, &record.run_id, pane_id.clone()).unwrap();
        assert_eq!(updated.pane_id.as_ref(), Some(&pane_id));
        assert_eq!(
            load(&paths, &record.run_id).unwrap().pane_id.as_ref(),
            Some(&pane_id)
        );
    }

    #[test]
    fn record_failure_tail_persists_first_non_empty_tail() {
        let (_dir, paths, record) = setup();

        let stored = record_failure_tail(&paths, &record.run_id, "first\n\n").unwrap();
        assert_eq!(stored.failure_tail.as_deref(), Some("first"));

        let unchanged = record_failure_tail(&paths, &record.run_id, "second").unwrap();
        assert_eq!(unchanged.failure_tail.as_deref(), Some("first"));
        assert_eq!(
            load(&paths, &record.run_id)
                .unwrap()
                .failure_tail
                .as_deref(),
            Some("first")
        );
    }

    #[test]
    fn record_failure_tail_ignores_empty_tail() {
        let (_dir, paths, record) = setup();

        let stored = record_failure_tail(&paths, &record.run_id, " \n\t").unwrap();

        assert_eq!(stored.failure_tail, None);
        assert_eq!(load(&paths, &record.run_id).unwrap().failure_tail, None);
    }

    #[test]
    fn record_failure_tail_caps_stored_tail() {
        let (_dir, paths, record) = setup();
        let tail = format!("{}{}", "a".repeat(FAILURE_TAIL_CAP), "b".repeat(20));

        let stored = record_failure_tail(&paths, &record.run_id, &tail).unwrap();

        let stored = stored.failure_tail.expect("tail");
        assert_eq!(stored.len(), FAILURE_TAIL_CAP);
        assert!(stored.starts_with('a'));
        assert!(stored.ends_with('b'));
    }

    #[test]
    fn retry_prompt_includes_the_latest_failure_tail() {
        let prompt = retry_prompt("fix it", Some("error: broken\nlast line"));

        assert!(prompt.starts_with("fix it\n\n<previous-attempt-failure>"));
        assert!(prompt.contains("The tail of its terminal output:\nerror: broken\nlast line"));
        assert!(prompt.ends_with("</previous-attempt-failure>"));
    }

    #[test]
    fn retry_prompt_explains_when_no_tail_was_captured() {
        let prompt = retry_prompt("fix it", None);

        assert!(prompt.contains("no terminal output was captured"));
    }

    #[test]
    fn retry_prompt_recomposes_from_the_base_without_nesting() {
        let first = retry_prompt("fix it", Some("first failure"));
        let second = retry_prompt("fix it", Some("second failure"));

        assert!(first.contains("first failure"));
        assert!(!second.contains("first failure"));
        assert_eq!(second.matches("<previous-attempt-failure>").count(), 1);
    }

    #[test]
    fn verify_reprompt_formats_status_and_caps_the_output_tail() {
        let output = format!("old{}latest", "x".repeat(FAILURE_TAIL_CAP));

        let prompt = verify_reprompt("cargo xtask test auth", "1", &output);

        assert!(prompt.starts_with("Verification failed — the task is not done yet."));
        assert!(prompt.contains("--- verify `cargo xtask test auth` exited 1 ---"));
        assert!(!prompt.contains("old"));
        assert!(prompt.ends_with("latest"));
    }

    fn agent_state(kind: &str, id: &str, status: AgentStatus) -> AgentState {
        let mut agent = crate::sidebar::test_support::root_agent(kind, id, None);
        agent.name = None;
        agent.kind_ordinal = None;
        agent.status = status;
        agent.last_seen = Timestamp::UNIX_EPOCH;
        agent.last_activity = Timestamp::UNIX_EPOCH;
        agent.registered_at = Some(Timestamp::UNIX_EPOCH);
        agent
    }
}
