//! Durable run records: schema, codec, and the terminal wake sender.
//!
//! Run records are cold-path durable state: a waiting CLI may exit, a user may
//! inspect the result later with `rimz agents show`, and the final assistant text
//! is the product output. Writes therefore use fsyncing temp-file-plus-rename,
//! unlike cache sidecars whose correctness rides the event log.
//! Listings share a per-thread sorted view, re-parsing only records whose full file stamp changed.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::os::unix::net::UnixDatagram as StdUnixDatagram;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::agents::PermissionMode;
use crate::disk::atomic::write_temp_then_rename;
use crate::disk::parse_cache::FileStamp;
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunLifecycle {
    #[default]
    Turn,
    Session,
}

impl RunLifecycle {
    fn is_turn(&self) -> bool {
        *self == Self::Turn
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

    pub const fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::VerifyFailed => "verify failed",
            Self::TimedOut => "timed out",
            Self::BudgetExceeded => "budget exceeded",
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

/// Who is told when a launched run settles. `Nobody` comes from `rimz agents --bg --detach`,
/// teams, or a legacy subagent record: its answer is never owed, so no digest lists it and no
/// launcher waits on it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportTo {
    #[default]
    Launcher,
    Nobody,
}

impl ReportTo {
    fn is_launcher(&self) -> bool {
        *self == Self::Launcher
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerRun {
    pub launch_id: AgentSessionId,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub opened_by: Vec<crate::ids::MessageId>,
}

/// The team-long run an agent-launched team keeps on its leader: it collects
/// the leader's final messages across turns and settles only when the team's
/// board flips to Done, which reports the leader to the launcher.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamRun {
    /// The leader's launch id; every conversation row of the leader shares it.
    pub launch_id: AgentSessionId,
    /// `<team>#<channel>`, the cohort whose board settles this run.
    pub instance: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FollowUpTurn {
    pub started_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EarlierAnswer {
    pub ordinal: u32,
    pub status: RunStatus,
    pub started_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_tail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_failure: Option<LimitFailure>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub opened_by: Vec<crate::ids::MessageId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_message_id: Option<crate::ids::MessageId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub joined_at: Option<Timestamp>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunSession {
    pub agent_id: AgentSessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoppedTurnDisposition {
    Failed,
    Canceled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoppedTurn {
    pub at: Timestamp,
    pub disposition: StoppedTurnDisposition,
}

/// A provider-limit park held open past the run's deadline until the reset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitHold {
    /// The child's `last_activity`, naming the park as `park_noticed_activity` does.
    pub activity: Timestamp,
    /// The limit marker's time, clamped into the current answer's span.
    pub since: Timestamp,
    /// The reset read when the park was classified; never re-read.
    pub until: Timestamp,
}

/// Why a launched child's run failed on a provider-limit park, kept typed so
/// the fleet report computes the refill's distance when it is composed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitFailure {
    pub cause: LimitFailureCause,
    /// The refill read for the park; set for `BeyondHoldLimit` and `RefillKnown` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refill_at: Option<Timestamp>,
    /// The provider's message, when the limit marker carried one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitFailureCause {
    /// The refill falls past the hold limit.
    BeyondHoldLimit,
    /// The refill is known and nothing continues the child at it.
    RefillKnown,
    /// The account read quota available again, with nothing to continue it.
    Refilled,
    /// No refill was published and no fresh reading showed quota.
    RefillUnknown,
    /// Auto-continue spent its retries on the park.
    ContinueFailed,
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
    /// Handle of the agent this run reports to (its launcher), whose `out/`
    /// directory receives the response; absent when no agent launched it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reader: Option<String>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub earlier_sessions: Vec<RunSession>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_tail: Option<String>,
    /// The typed cause of a provider-limit failure, written with `failure_tail`
    /// and moved into the earlier answer with it on a reopen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_failure: Option<LimitFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_of: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<crate::agents::WorkflowAttemptRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<RunVerify>,
    pub status: RunStatus,
    #[serde(default, skip_serializing_if = "RunLifecycle::is_turn")]
    pub lifecycle: RunLifecycle,
    pub permission_mode: PermissionMode,
    /// Never reclaim this run's pane automatically, including when its parent
    /// agent exits.
    #[serde(default, skip_serializing_if = "is_false")]
    pub keep: bool,
    /// Pane-backed child launched through `rimz subagents`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub subagent: bool,
    /// Report policy of the answer the launch prompt opened; absent means the launcher.
    #[serde(default, skip_serializing_if = "ReportTo::is_launcher")]
    pub report_to: ReportTo,
    /// Number of times this run reopened for a follow-up prompt.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub follow_ups: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up: Option<FollowUpTurn>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub opened_by: Vec<crate::ids::MessageId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub earlier_answers: Vec<EarlierAnswer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<PeerRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<TeamRun>,
    /// Time at which the caller claimed the current answer, either by printing
    /// it to a human shell or to its launcher during the launcher's open turn
    /// (`harness::run::report::may_claim`), or by discarding it through
    /// `rimz subagents stop`; joined answers are excluded from the next
    /// completion digest and let the joiner cancel a digest once every row it
    /// lists has been joined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub joined_at: Option<Timestamp>,
    /// Completion digest that listed the current answer.
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
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::utils::time::duration_serde::optional"
    )]
    pub timeout: Option<Duration>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::utils::time::duration_serde::optional"
    )]
    pub grace: Option<Duration>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        with = "crate::utils::time::duration_serde::list"
    )]
    pub warn: Vec<Duration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_notice_at: Option<Timestamp>,
    pub updated_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<Timestamp>,
    /// When a clean turn end left this run open because its session was still
    /// owed a harness wake. Cleared by the first in-turn signal and by every
    /// terminal write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parked_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_turn: Option<StoppedTurn>,
    /// The answer text through the most recent park, sealed when an in-turn
    /// signal proves work resumed; each later turn's final message joins after
    /// it in `last_message`. Kept by the terminal write, cleared by a reopen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parked_answer: Option<String>,
    /// The child's `last_activity` at the provider-limit park its parent was
    /// last told about. Activity is frozen while the turn is dead and advances
    /// on any resume, so a later park carries a later value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub park_noticed_activity: Option<Timestamp>,
    /// The provider-limit park this run is held through. Set with the deadline
    /// shift when the park is classified, cleared by the first in-turn signal
    /// (which pulls an early resume's unused hold back) and by a reopen; a
    /// terminal write keeps it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_hold: Option<LimitHold>,
    /// When this run's parent was told the launched child went silent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stall_noticed_at: Option<Timestamp>,
    /// The instant of the interrupt evidence (a hook's ingress stamp, or the
    /// child's settle marker) holding this subagent turn run open for a new
    /// turn. Open while set on a running record: a later `TurnStarted` clears
    /// it, the room host's expiry settles the run canceled, and a terminal
    /// write keeps it until a reopen clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupted_at: Option<Timestamp>,
    /// The hook ingress instant of the newest `TurnStarted` folded into this
    /// subagent turn run: interrupt evidence at or before it is an earlier
    /// turn's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_opened_at: Option<Timestamp>,
    /// The pane of the wrapper `rimz agents restart` took this run from. While
    /// set, that wrapper and every pane but the one bound since neither settle
    /// nor move the run ([`RunRecord::superseded_in`]). A later restart
    /// overwrites it; a failed split, or a new wrapper binding the stamped pane
    /// id again, clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handed_over_from: Option<PaneId>,
}

/// Who authored a run's first prompt: its launching agent, RimZ's loop, or a person.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunPromptOrigin {
    Parent,
    Harness,
    Human,
}

pub struct AnswerClaim<'a> {
    pub ordinal: u32,
    pub report: Option<&'a crate::ids::MessageId>,
    pub joined: Option<Timestamp>,
    pub owed: bool,
    pub earlier: Option<&'a EarlierAnswer>,
}

impl RunRecord {
    /// Whether a wrapper or hook in `pane` no longer speaks for this run: a restart
    /// handed the run over from `pane`, or, once any restart has, another pane
    /// bound it since. Gating the second test on the stamp keeps a run never
    /// handed over, whose later panes need not bind it, as it was.
    pub fn superseded_in(&self, pane: &PaneId) -> bool {
        self.handed_over_from.as_ref().is_some_and(|from| {
            from == pane || self.pane_id.as_ref().is_some_and(|bound| bound != pane)
        })
    }

    /// Earlier answers first, then the settled current answer.
    pub fn answer_claims(&self) -> impl Iterator<Item = AnswerClaim<'_>> {
        self.earlier_answers
            .iter()
            .map(|answer| {
                (
                    answer.ordinal,
                    answer.report_message_id.as_ref(),
                    answer.joined_at,
                    Some(answer),
                )
            })
            .chain(self.status.is_terminal().then_some((
                self.follow_ups + 1,
                self.report_message_id.as_ref(),
                self.joined_at,
                None,
            )))
            .map(|(ordinal, report, joined, earlier)| AnswerClaim {
                ordinal,
                report,
                joined,
                owed: self.report_to == ReportTo::Launcher && report.is_none() && joined.is_none(),
                earlier,
            })
    }

    pub fn owes_report(&self) -> bool {
        self.status.is_terminal() && self.answer_claims().any(|claim| claim.owed)
    }

    /// The start of the answer this run is on now: a follow-up reopens the record without moving `started_at`.
    pub fn answer_started_at(&self) -> Timestamp {
        self.follow_up
            .as_ref()
            .map_or(self.started_at, |turn| turn.started_at)
    }

    pub fn prompt_origin(&self) -> RunPromptOrigin {
        match (
            self.subagent || self.peer.is_some() || self.team.is_some(),
            self.loop_task.as_ref(),
        ) {
            (true, _) => RunPromptOrigin::Parent,
            (false, Some(_)) => RunPromptOrigin::Harness,
            (false, None) => RunPromptOrigin::Human,
        }
    }

    // `harness::fleet::FleetRunIndex` mirrors these branches as `RunMatchKey`
    // hash keys; a new matching branch here changes that index too.
    pub fn matches_agent(&self, agent: &crate::agents::AgentState) -> bool {
        if self.kind != agent.kind {
            return false;
        }
        if let Some(peer) = &self.peer {
            return agent.launch_id.as_ref() == Some(&peer.launch_id)
                || agent.agent_id == peer.launch_id
                || self.agent_id.as_ref() == Some(&agent.agent_id);
        }
        if let Some(team) = &self.team {
            return agent.launch_id.as_ref() == Some(&team.launch_id)
                || agent.agent_id == team.launch_id;
        }
        self.agent_id.as_ref() == Some(&agent.agent_id)
            || agent
                .name
                .as_ref()
                .is_some_and(|name| self.agent_name.as_ref() == Some(name))
    }

    pub fn matches_stopped_agent(&self, agent: &crate::agents::AgentState) -> bool {
        self.matches_agent(agent)
            && (self.agent_id.as_ref() == Some(&agent.agent_id)
                || self.completed_at.is_none_or(|completed_at| {
                    agent
                        .registered_at
                        .is_some_and(|registered_at| registered_at <= completed_at)
                }))
    }

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
            reader: None,
            pane_id: None,
            provider_pid: None,
            provider_process_start: None,
            transcript_path: None,
            earlier_sessions: Vec::new(),
            failure_tail: None,
            limit_failure: None,
            retry_of: None,
            loop_task: None,
            workflow: None,
            verify: None,
            status: RunStatus::Pending,
            lifecycle: RunLifecycle::Turn,
            permission_mode,
            keep: false,
            subagent: false,
            report_to: ReportTo::Launcher,
            follow_ups: 0,
            follow_up: None,
            opened_by: Vec::new(),
            earlier_answers: Vec::new(),
            peer: None,
            team: None,
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
            timeout: None,
            grace: None,
            warn: Vec::new(),
            deadline_notice_at: None,
            updated_at: now,
            completed_at: None,
            parked_at: None,
            stopped_turn: None,
            parked_answer: None,
            park_noticed_activity: None,
            limit_hold: None,
            stall_noticed_at: None,
            interrupted_at: None,
            turn_opened_at: None,
            handed_over_from: None,
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
        self.parked_at = None;
        self.updated_at = now;
        true
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero(value: &u32) -> bool {
    *value == 0
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

/// A live waiter owns verification and pane cleanup; a stale socket does not.
pub fn run_waiter_is_live(rt: &RuntimePaths, run_id: &RunId) -> std::io::Result<bool> {
    let probe = StdUnixDatagram::unbound()?;
    match probe.connect(run_socket_path(rt, run_id)) {
        Ok(()) => Ok(true),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
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

#[derive(Clone)]
pub(crate) struct RunListing {
    records: Arc<[RunRecord]>,
    pub(crate) generation: u64,
}

impl std::ops::Deref for RunListing {
    type Target = [RunRecord];

    fn deref(&self) -> &Self::Target {
        &self.records
    }
}

/// `files` maps each record file to its stamp and its index in `listing`;
/// the two are replaced together, so every index is in bounds.
#[derive(Default)]
struct RunListingCache {
    files: HashMap<PathBuf, (FileStamp, usize)>,
    listing: Option<RunListing>,
    directory: Option<(FileStamp, SystemTime)>,
}

enum ListedRecord {
    Cached(usize),
    Parsed(Box<RunRecord>),
}

thread_local! {
    static RUN_LISTINGS: RefCell<HashMap<PathBuf, RunListingCache>> = RefCell::new(HashMap::new());
}

pub(super) fn run_path(runs_dir: &Path, run_id: &RunId) -> PathBuf {
    runs_dir.join(format!("{run_id}.json"))
}

// `RunListingCache::refresh` reuses a listing while the directory stamp is
// unchanged, which is safe only because every runs-dir writer publishes by
// sibling rename (or deletes). An in-place writer would serve stale listings.
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
    Ok(listing(runs_dir)?.to_vec())
}

pub(crate) fn listing(runs_dir: &Path) -> Result<RunListing> {
    RUN_LISTINGS.with(|caches| {
        let mut caches = caches.borrow_mut();
        let cache = caches.entry(runs_dir.to_path_buf()).or_default();
        cache.refresh(runs_dir)
    })
}

impl RunListingCache {
    fn refresh(&mut self, runs_dir: &Path) -> Result<RunListing> {
        let scan_started = SystemTime::now();
        let directory = match fs::metadata(runs_dir) {
            Ok(metadata) => Some(FileStamp::from_metadata(&metadata)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(source) => {
                return Err(RunStoreErr::Io {
                    path: runs_dir.to_path_buf(),
                    source,
                });
            }
        };
        if let Some((stamp, previous_scan)) = self.directory
            && directory == Some(stamp)
            && previous_scan
                .duration_since(
                    UNIX_EPOCH + Duration::new(stamp.modified_secs, stamp.modified_nanos),
                )
                .is_ok_and(|age| age >= Duration::from_secs(2))
            && let Some(listing) = &self.listing
        {
            return Ok(listing.clone());
        }
        #[cfg(test)]
        super::sidecar::testkit::record_enumeration();
        let entries = match fs::read_dir(runs_dir) {
            Ok(entries) => Some(entries),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(source) => {
                return Err(RunStoreErr::Io {
                    path: runs_dir.to_path_buf(),
                    source,
                });
            }
        };
        let mut found = Vec::new();
        let mut changed = false;
        for entry in entries.into_iter().flatten() {
            let entry = entry.map_err(|source| RunStoreErr::Io {
                path: runs_dir.to_path_buf(),
                source,
            })?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            #[cfg(test)]
            super::sidecar::testkit::record_stat();
            let metadata = match fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => return Err(RunStoreErr::Io { path, source }),
            };
            let stamp = FileStamp::from_metadata(&metadata);
            if let Some((old_stamp, index)) = self.files.get(&path)
                && *old_stamp == stamp
            {
                found.push((path, stamp, ListedRecord::Cached(*index)));
                continue;
            }
            let bytes = match fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => return Err(RunStoreErr::Io { path, source }),
            };
            #[cfg(test)]
            super::sidecar::testkit::record_parse_read();
            let record = serde_json::from_slice::<RunRecord>(&bytes).map_err(|source| {
                RunStoreErr::Json {
                    path: path.clone(),
                    source,
                }
            })?;
            found.push((path, stamp, ListedRecord::Parsed(Box::new(record))));
            changed = true;
        }
        changed |= found.len() != self.files.len();
        self.directory = directory.map(|stamp| (stamp, scan_started));
        if !changed && let Some(listing) = &self.listing {
            return Ok(listing.clone());
        }
        let previous = self.listing.as_deref().unwrap_or_default();
        let mut rows: Vec<_> = found
            .into_iter()
            .map(|(path, stamp, listed)| {
                let record = match listed {
                    ListedRecord::Cached(index) => previous[index].clone(),
                    ListedRecord::Parsed(record) => *record,
                };
                (path, stamp, record)
            })
            .collect();
        rows.sort_by_key(|(_, _, record)| std::cmp::Reverse(record.updated_at));
        let mut files = HashMap::with_capacity(rows.len());
        let records: Vec<_> = rows
            .into_iter()
            .enumerate()
            .map(|(index, (path, stamp, record))| {
                files.insert(path, (stamp, index));
                record
            })
            .collect();
        let listing = RunListing {
            records: records.into(),
            generation: self
                .listing
                .as_ref()
                .map_or(1, |listing| listing.generation + 1),
        };
        self.files = files;
        self.listing = Some(listing.clone());
        Ok(listing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::PermissionMode;
    use crate::ids::{AgentKind, WorkspaceId};
    use tempfile::tempdir;

    fn listing_record() -> RunRecord {
        RunRecord::new(
            WorkspaceId::from_project_root(Path::new("/repo")),
            AgentKind::new_unchecked("codex"),
            PermissionMode::Auto,
            "task".into(),
            "/repo".into(),
        )
    }

    fn set_directory_mtime(path: &Path, modified: std::time::SystemTime) {
        fs::File::open(path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
    }

    #[test]
    fn run_listing_aged_directory_skips_record_stats_and_enumeration() {
        let dir = tempdir().unwrap();
        write(dir.path(), &listing_record()).unwrap();
        set_directory_mtime(dir.path(), std::time::SystemTime::UNIX_EPOCH);
        let cold = listing(dir.path()).unwrap();
        let before = super::super::sidecar::testkit::read_counts();
        let warm = listing(dir.path()).unwrap();
        assert_eq!(super::super::sidecar::testkit::read_counts(), before);
        assert_eq!(warm.generation, cold.generation);
        assert!(Arc::ptr_eq(&cold.records, &warm.records));
    }

    #[test]
    fn run_listing_sees_write_after_aged_reuse() {
        let dir = tempdir().unwrap();
        let mut record = listing_record();
        write(dir.path(), &record).unwrap();
        set_directory_mtime(dir.path(), std::time::SystemTime::UNIX_EPOCH);
        let cold = listing(dir.path()).unwrap();
        assert_eq!(listing(dir.path()).unwrap().generation, cold.generation);
        record.prompt = "new task".into();
        write(dir.path(), &record).unwrap();
        let changed = listing(dir.path()).unwrap();
        assert_eq!(changed[0].prompt, "new task");
        assert_eq!(changed.generation, cold.generation + 1);
    }

    #[test]
    fn run_listing_sees_same_tick_write_inside_margin() {
        let dir = tempdir().unwrap();
        let mut record = listing_record();
        write(dir.path(), &record).unwrap();
        let modified = std::time::SystemTime::now();
        set_directory_mtime(dir.path(), modified);
        let cold = listing(dir.path()).unwrap();
        record.prompt = "new task".into();
        write(dir.path(), &record).unwrap();
        set_directory_mtime(dir.path(), modified);
        let changed = listing(dir.path()).unwrap();
        assert_eq!(changed[0].prompt, "new task");
        assert_eq!(changed.generation, cold.generation + 1);
    }

    #[test]
    fn run_listing_reuses_unchanged_records_per_directory() {
        let first = tempdir().unwrap();
        let second = tempdir().unwrap();
        write(first.path(), &listing_record()).unwrap();
        let cold = listing(first.path()).unwrap();
        listing(second.path()).unwrap();
        super::super::sidecar::testkit::reset_parse_reads();
        let warm = listing(first.path()).unwrap();
        assert_eq!(super::super::sidecar::testkit::parse_reads(), 0);
        assert_eq!(cold.generation, 1);
        assert_eq!(warm.generation, cold.generation);
        assert!(std::sync::Arc::ptr_eq(&cold.records, &warm.records));
    }

    #[test]
    fn run_listing_reloads_reopened_terminal_record() {
        let dir = tempdir().unwrap();
        let mut record = listing_record();
        record.status = RunStatus::Completed;
        write(dir.path(), &record).unwrap();
        let cold = listing(dir.path()).unwrap();
        record.status = RunStatus::Running;
        write(dir.path(), &record).unwrap();
        super::super::sidecar::testkit::reset_parse_reads();
        let reopened = listing(dir.path()).unwrap();
        assert_eq!(reopened.generation, cold.generation + 1);
        assert_eq!(reopened[0].status, RunStatus::Running);
        assert_eq!(super::super::sidecar::testkit::parse_reads(), 1);
    }

    #[test]
    fn run_listing_detects_equal_length_and_timestamp_replacement() {
        let dir = tempdir().unwrap();
        let mut record = listing_record();
        write(dir.path(), &record).unwrap();
        let path = run_path(dir.path(), &record.run_id);
        let metadata = fs::metadata(&path).unwrap();
        let cold = listing(dir.path()).unwrap();
        record.prompt = "work".into();
        write(dir.path(), &record).unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(metadata.modified().unwrap()))
            .unwrap();
        assert_eq!(fs::metadata(&path).unwrap().len(), metadata.len());
        let changed = listing(dir.path()).unwrap();
        assert_eq!(changed.generation, cold.generation + 1);
        assert_eq!(changed[0].prompt, "work");
    }

    #[test]
    fn run_listing_drops_deleted_records_and_ignores_temps() {
        let dir = tempdir().unwrap();
        let record = listing_record();
        write(dir.path(), &record).unwrap();
        let cold = listing(dir.path()).unwrap();
        fs::write(dir.path().join("run.json.tmp.1.2"), "invalid").unwrap();
        assert_eq!(listing(dir.path()).unwrap().generation, cold.generation);
        fs::remove_file(run_path(dir.path(), &record.run_id)).unwrap();
        let empty = listing(dir.path()).unwrap();
        assert_eq!(empty.generation, cold.generation + 1);
        assert!(empty.is_empty());
    }

    #[test]
    fn status_label_is_the_wire_form_with_spaces() {
        for status in [
            RunStatus::Pending,
            RunStatus::Running,
            RunStatus::Completed,
            RunStatus::Failed,
            RunStatus::VerifyFailed,
            RunStatus::TimedOut,
            RunStatus::BudgetExceeded,
            RunStatus::Canceled,
        ] {
            assert_eq!(status.label(), status.as_str().replace('_', " "));
        }
    }

    #[test]
    fn peer_run_codec_and_prompt_origin() {
        let record = RunRecord::new(
            WorkspaceId::from_project_root(Path::new("/repo")),
            AgentKind::new_unchecked("codex"),
            PermissionMode::Auto,
            "task".into(),
            "/repo".into(),
        );
        let mut value = serde_json::to_value(&record).unwrap();
        assert!(value.get("peer").is_none());
        assert!(value.get("follow_ups").is_none());
        let old: RunRecord = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(old.follow_ups, 0);
        assert_eq!(old.prompt_origin(), RunPromptOrigin::Human);
        for field in ["follow_up", "opened_by", "earlier_answers", "report_to"] {
            assert!(value.get(field).is_none());
        }
        assert_eq!(old.report_to, ReportTo::Launcher);
        let mut detached = value.clone();
        detached["report_to"] = serde_json::json!("nobody");
        let decoded: RunRecord = serde_json::from_value(detached.clone()).unwrap();
        assert_eq!(decoded.report_to, ReportTo::Nobody);
        assert_eq!(serde_json::to_value(decoded).unwrap(), detached);
        let mut answers = value.clone();
        answers["follow_up"] =
            serde_json::json!({"started_at": record.started_at, "prompt": "follow up"});
        answers["opened_by"] = serde_json::json!([crate::ids::MessageId::new()]);
        answers["earlier_answers"] = serde_json::json!([{
            "ordinal": 1, "status": "completed", "started_at": record.started_at,
            "completed_at": record.started_at, "report_message_id": crate::ids::MessageId::new()
        }]);
        let decoded: RunRecord = serde_json::from_value(answers.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), answers);
        value["peer"] = serde_json::json!({"launch_id": "peer-launch"});
        let peer: RunRecord = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(peer.prompt_origin(), RunPromptOrigin::Parent);
        assert_eq!(serde_json::to_value(&peer).unwrap(), value);
        value["peer"]["opened_by"] = serde_json::json!([crate::ids::MessageId::new()]);
        let peer: RunRecord = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(peer).unwrap(), value);
    }

    #[test]
    fn waiter_liveness_distinguishes_bound_stale_and_absent_socket() {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let id = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(id, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let run_id = RunId::new();
        assert!(!run_waiter_is_live(&runtime, &run_id).unwrap());
        let path = run_socket_path(&runtime, &run_id);
        let waiter = StdUnixDatagram::bind(&path).unwrap();
        assert!(run_waiter_is_live(&runtime, &run_id).unwrap());
        drop(waiter);
        assert!(path.exists());
        assert!(!run_waiter_is_live(&runtime, &run_id).unwrap());
    }

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
        old_json
            .as_object_mut()
            .expect("run object")
            .remove("parked_at");

        let old: RunRecord = serde_json::from_value(old_json).expect("deserialize old run");

        assert!(!old.keep);
        assert!(!old.subagent);
        assert_eq!(old.joined_at, None);
        assert_eq!(old.report_message_id, None);
        assert_eq!(old.provider_pid, None);
        assert_eq!(old.provider_process_start, None);
        assert_eq!(old.parked_at, None);
        assert_eq!(old.timeout, None);
        assert_eq!(old.grace, None);
        assert!(old.warn.is_empty());
        assert_eq!(old.deadline_notice_at, None);
    }

    #[test]
    fn a_detached_run_owes_no_answer_but_keeps_it_readable() {
        let mut record = RunRecord::new(
            WorkspaceId::from_project_root(Path::new("/tmp/rimz-run")),
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        record.status = RunStatus::Completed;
        assert!(record.owes_report());
        record.keep = true;
        assert!(record.owes_report());
        record.keep = false;
        record.report_to = ReportTo::Nobody;
        assert!(!record.owes_report());
        let claims = record.answer_claims().collect::<Vec<_>>();
        assert_eq!(claims.len(), 1, "the answer is still there to join");
        assert!(!claims[0].owed);
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

    #[test]
    fn limit_hold_and_handover_round_trip_and_default_when_absent() {
        let mut record = RunRecord::new(
            WorkspaceId::from_project_root(Path::new("/tmp/rimz-run")),
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        let unset = serde_json::to_value(&record).unwrap();
        for key in [
            "limit_hold",
            "handed_over_from",
            "interrupted_at",
            "turn_opened_at",
        ] {
            assert!(unset.get(key).is_none(), "an unset {key} is omitted");
        }
        let pane = PaneId::parse("tmux:%7").unwrap();
        record.handed_over_from = Some(pane.clone());
        let at: Timestamp = "2026-01-01T01:00:00Z".parse().unwrap();
        record.interrupted_at = Some(at + Duration::from_secs(5));
        record.turn_opened_at = Some(at + Duration::from_secs(2));
        let hold = LimitHold {
            activity: at,
            since: at + Duration::from_secs(1),
            until: at + Duration::from_secs(3600),
        };
        record.limit_hold = Some(hold);

        let mut value = serde_json::to_value(&record).unwrap();
        let decoded: RunRecord = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(decoded.limit_hold, Some(hold));
        assert_eq!(decoded.handed_over_from, Some(pane));
        assert_eq!(decoded.interrupted_at, record.interrupted_at);
        assert_eq!(decoded.turn_opened_at, record.turn_opened_at);

        for key in [
            "limit_hold",
            "handed_over_from",
            "interrupted_at",
            "turn_opened_at",
        ] {
            value.as_object_mut().unwrap().remove(key);
        }
        let decoded: RunRecord = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.limit_hold, None);
        assert_eq!(decoded.handed_over_from, None);
        assert_eq!(decoded.interrupted_at, None);
        assert_eq!(decoded.turn_opened_at, None);
    }

    #[test]
    fn limit_failure_round_trips_on_both_records_and_defaults_when_absent() {
        let mut record = RunRecord::new(
            WorkspaceId::from_project_root(Path::new("/tmp/rimz-run")),
            AgentKind::new_unchecked("codex"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        assert!(
            serde_json::to_value(&record)
                .unwrap()
                .get("limit_failure")
                .is_none()
        );
        let failure = LimitFailure {
            cause: LimitFailureCause::RefillKnown,
            refill_at: Some("2026-11-11T20:00:00Z".parse().unwrap()),
            label: Some("Usage limit reached".to_owned()),
        };
        record.limit_failure = Some(failure.clone());
        record.earlier_answers.push(EarlierAnswer {
            ordinal: 1,
            status: RunStatus::Failed,
            started_at: record.started_at,
            completed_at: None,
            prompt: None,
            failure_tail: None,
            limit_failure: Some(LimitFailure {
                cause: LimitFailureCause::ContinueFailed,
                refill_at: None,
                label: None,
            }),
            opened_by: Vec::new(),
            report_message_id: None,
            joined_at: None,
        });

        let mut value = serde_json::to_value(&record).unwrap();
        assert_eq!(
            value["limit_failure"],
            serde_json::json!({
                "cause": "refill_known",
                "refill_at": "2026-11-11T20:00:00Z",
                "label": "Usage limit reached",
            })
        );
        assert_eq!(
            value["earlier_answers"][0]["limit_failure"],
            serde_json::json!({ "cause": "continue_failed" })
        );
        let decoded: RunRecord = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(decoded, record);

        value.as_object_mut().unwrap().remove("limit_failure");
        value["earlier_answers"][0]
            .as_object_mut()
            .unwrap()
            .remove("limit_failure");
        let decoded: RunRecord = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.limit_failure, None);
        assert_eq!(decoded.earlier_answers[0].limit_failure, None);
    }

    #[test]
    fn stopped_agent_match_bounds_names_but_not_session_ids() {
        let mut record = RunRecord::new(
            WorkspaceId::from_project_root(Path::new("/repo")),
            AgentKind::new_unchecked("codex"),
            PermissionMode::Auto,
            "task".into(),
            "/repo".into(),
        );
        record.agent_id = Some("old".into());
        record.agent_name = Some("otter".to_owned());
        for (completed, registered, id, name, kind, expected) in [
            (Some(2000), None, "new", "otter", "codex", false),
            (Some(2000), Some(3000), "new", "otter", "codex", false),
            (Some(2000), Some(1000), "new", "otter", "codex", true),
            (Some(2000), Some(2000), "new", "otter", "codex", true),
            (None, None, "new", "otter", "codex", true),
            (None, Some(3000), "new", "otter", "codex", true),
            (Some(2000), None, "old", "renamed", "codex", true),
            (Some(2000), Some(3000), "old", "renamed", "codex", true),
            (Some(2000), Some(1000), "new", "other", "codex", false),
            (Some(2000), Some(1000), "old", "otter", "claude", false),
        ] {
            record.completed_at = completed.map(|seconds| Timestamp::from_second(seconds).unwrap());
            let mut agent =
                crate::agents::AgentState::stub(kind, id, crate::agents::AgentStatus::Idle);
            agent.name = Some(name.to_owned());
            agent.registered_at =
                registered.map(|seconds| Timestamp::from_second(seconds).unwrap());
            assert_eq!(
                record.matches_stopped_agent(&agent),
                expected,
                "completed={completed:?}, registered={registered:?}, id={id}, name={name}, kind={kind}"
            );
        }
    }
}
