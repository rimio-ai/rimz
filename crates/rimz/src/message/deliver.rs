//! Receiver readiness, shared delivery-attempt recovery, queued sweeps, and wake maintenance.

use std::fs::File;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::Duration;

use jiff::Timestamp;
use serde::Serialize;

use crate::agents::{AgentState, AgentStatus};
use crate::ids::{MessageId, MuxName, PaneId};
use crate::message::{
    AfterCondition, DeliveryGate, MessageRecord, MessageSender, MessageStatus, WhenCondition,
    command_submit_delay_from_env, delivery_window_from_env, gate_open_for_agent,
    max_delivery_attempts_from_env, message_interval_from_env, older_ready_blocker, queue_head,
};
use crate::store::snapshot::{PaneAgent, SidebarSnapshot};
use crate::workspace::ResolvedWorkspace;
use crate::{RuntimePaths, Store};

use super::send;

pub type Result<T> = std::result::Result<T, DeliverErr>;

#[derive(Debug, thiserror::Error)]
pub enum DeliverErr {
    #[error(transparent)]
    Store(#[from] crate::store::StoreErr),
    #[error(transparent)]
    Path(#[from] crate::store::paths::PathErr),
    #[error(transparent)]
    Atomic(#[from] crate::store::atomic::AtomicErr),
    #[error(transparent)]
    Produce(#[from] crate::sidebar::produce::ProduceErr),
    #[error(transparent)]
    Send(#[from] send::SendErr),
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryPolicy {
    Boundary,
    Steer { force: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReceiverReadiness {
    pub status: AgentStatus,
    pub compacting: bool,
    pub gate_open: bool,
    pub waiting: bool,
}

impl ReceiverReadiness {
    pub fn accepts_prompt(self) -> bool {
        self.gate_open && !self.waiting
    }
}

pub(crate) fn receiver_readiness(
    agent: &crate::agents::AgentState,
    gate: DeliveryGate,
    force: bool,
    now: Timestamp,
) -> ReceiverReadiness {
    ReceiverReadiness {
        status: agent.effective_status(),
        compacting: agent.is_compacting(now),
        gate_open: gate_open_for_agent(gate, agent, force, now),
        waiting: !force && agent.is_awaiting_input(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttemptSource {
    Fresh { durable_receiver: bool },
    Claimed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttemptOutcome {
    Sent { compacted: bool },
    Queued,
    CompactionPending,
    SkippedWaiting,
}

pub(crate) struct Attempt<'a> {
    pub workspace: &'a ResolvedWorkspace,
    pub store: &'a Store,
    pub snapshot: &'a SidebarSnapshot,
    pub target: &'a PaneAgent,
    pub bound: Option<&'a crate::agents::AgentState>,
    pub records: &'a [MessageRecord],
    pub source: AttemptSource,
    pub policy: DeliveryPolicy,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DeliveryVerdict {
    Scheduled {
        not_before: Option<Timestamp>,
    },
    WaitingOnAfter {
        address: String,
        agent_present: bool,
    },
    WaitingOnWhen {
        address: String,
        expected: AgentStatus,
        current: Option<AgentStatus>,
        dwell_secs: u64,
        dwell_so_far_secs: Option<u64>,
    },
    BehindFifo {
        blocker: Option<MessageId>,
    },
    ReceiverGone,
    Compacting,
    GateClosed {
        gate: DeliveryGate,
        status: Option<AgentStatus>,
    },
    ResumeUnrecovered,
    AskWaiting,
    NoPane {
        pinned_pane_id: Option<PaneId>,
    },
    Ready,
}

/// The record a system-initiated nudge queues against an agent card: budget
/// continues, auto-continue resumes, and supervised verify re-prompts. A nudge
/// carries the agent's channel, always submits, and stays verbatim as system
/// text; its gate marks the automation.
fn synthetic_record(
    workspace_id: crate::ids::WorkspaceId,
    agent: &crate::agents::AgentState,
    sender: MessageSender,
    text: String,
    gate: DeliveryGate,
    pane_id: Option<&PaneId>,
) -> MessageRecord {
    let record = MessageRecord::new(workspace_id, agent, text, true, gate)
        .with_channel(crate::harness::target::agent_channel(agent))
        .with_sender(sender);
    match pane_id {
        Some(pane_id) => record.with_pane_id(pane_id.clone()),
        None => record,
    }
}

/// Queue synthetic text against an agent card.
pub fn queue_synthetic(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &crate::agents::AgentState,
    sender: MessageSender,
    text: String,
    gate: DeliveryGate,
    pane_id: Option<&PaneId>,
) -> Result<MessageId> {
    let message = synthetic_record(
        workspace.workspace_id.clone(),
        agent,
        sender,
        text,
        gate,
        pane_id,
    );
    store.queue_message(&message, &workspace.session_name)?;
    Ok(message.message_id)
}

/// Queue a nudge against a known pane and attempt boundary delivery in the same
/// pass. Returns the record id and whether the attempt delivered.
pub fn nudge_now(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &crate::agents::AgentState,
    text: String,
    gate: DeliveryGate,
    pane_id: &PaneId,
) -> Result<(MessageId, bool)> {
    let message_id = queue_synthetic(
        workspace,
        store,
        agent,
        MessageSender::System,
        text,
        gate,
        Some(pane_id),
    )?;
    let delivered = deliver_one(
        workspace,
        store,
        &message_id,
        Duration::ZERO,
        Some(pane_id.mux()),
        DeliveryPolicy::Boundary,
    )?;
    Ok((message_id, delivered))
}

pub fn deliver_one(
    workspace: &ResolvedWorkspace,
    store: &Store,
    message_id: &MessageId,
    settle: Duration,
    mux: Option<MuxName>,
    policy: DeliveryPolicy,
) -> Result<bool> {
    if !settle.is_zero() {
        std::thread::sleep(settle);
    }
    let pending = store.list_pending_messages()?;
    let mut snapshot = crate::sidebar::produce::resolution_snapshot(workspace, store, mux)?;
    if let Ok(runtime) = RuntimePaths::for_workspace(workspace.workspace_id.clone()) {
        snapshot = snapshot.with_agent_context(crate::store::agent_context::read_all(&runtime));
    }
    attempt_delivery(workspace, store, message_id, policy, &pending, &snapshot)
}

fn attempt_delivery(
    workspace: &ResolvedWorkspace,
    store: &Store,
    message_id: &MessageId,
    policy: DeliveryPolicy,
    pending: &[MessageRecord],
    snapshot: &SidebarSnapshot,
) -> Result<bool> {
    let now = Timestamp::now();
    let Some(candidate) = delivery_candidate(pending, snapshot, message_id, policy, now) else {
        return Ok(false);
    };
    let claimed = match policy {
        DeliveryPolicy::Boundary => {
            store.claim_delivery_batch(&candidate.message.message_id, candidate.status, now)?
        }
        DeliveryPolicy::Steer { .. } => store
            .claim_message_for_steer(&candidate.message.message_id, now)?
            .map(|message| vec![message]),
    };
    let Some(claimed) = claimed else {
        return Ok(false);
    };
    // Hook delivery handles one claimed batch; settle above owns any
    // pre-delivery spacing, so this pacer's first tick stays a no-op.
    let mut live_send = send::LiveSend {
        force: claimed[0].force || matches!(policy, DeliveryPolicy::Steer { force: true }),
        steer: matches!(policy, DeliveryPolicy::Steer { .. }),
        pacer: send::Pacer::new(message_interval_from_env()),
        command_submit_delay: command_submit_delay_from_env(),
    };
    let send_messages: Vec<MessageRecord> = claimed
        .iter()
        .cloned()
        .map(|message| message.with_pane_id(candidate.target.pane_id.clone()))
        .collect();
    let outcome = execute_attempt(
        Attempt {
            workspace,
            store,
            snapshot: candidate.snapshot,
            target: candidate.target,
            bound: candidate.bound,
            records: &send_messages,
            source: AttemptSource::Claimed,
            policy,
        },
        &mut live_send,
    )?;
    register_message_wake(workspace, store)?;
    Ok(matches!(outcome, AttemptOutcome::Sent { .. }))
}

pub(crate) fn execute_attempt(
    attempt: Attempt<'_>,
    live_send: &mut send::LiveSend,
) -> Result<AttemptOutcome> {
    let Attempt {
        workspace,
        store,
        snapshot,
        target,
        bound,
        records,
        source,
        policy,
    } = attempt;
    let head = records
        .first()
        .expect("delivery attempts require at least one message");
    let ids = records
        .iter()
        .map(|message| message.message_id.clone())
        .collect::<Vec<_>>();
    match send::send_batch_to_live_pane(
        workspace, store, snapshot, target, bound, records, live_send,
    ) {
        Ok(send::Receipt::Sent { compacted }) => Ok(AttemptOutcome::Sent { compacted }),
        Ok(send::Receipt::SkippedWaiting) => {
            const WAITING: &str = "agent is waiting on input in its pane";
            if matches!(source, AttemptSource::Fresh { .. })
                && matches!(policy, DeliveryPolicy::Steer { .. })
            {
                store.record_send_error(head, WAITING, &workspace.session_name)?;
                return Ok(AttemptOutcome::SkippedWaiting);
            }
            store.record_message_delivery_failures(
                &ids,
                records.first(),
                crate::store::writer::DeliveryFailureDisposition::Retry,
                WAITING,
                &workspace.session_name,
            )?;
            Ok(AttemptOutcome::Queued)
        }
        Ok(send::Receipt::CompactionPending) => {
            store.release_message_claims(
                &ids,
                "parked: waiting for compaction to finish",
                &workspace.session_name,
            )?;
            Ok(AttemptOutcome::CompactionPending)
        }
        Err(err) => {
            let durable_receiver = matches!(
                source,
                AttemptSource::Claimed
                    | AttemptSource::Fresh {
                        durable_receiver: true
                    }
            );
            let failure = store.record_message_delivery_failures(
                &ids,
                durable_receiver.then_some(head),
                if durable_receiver {
                    crate::store::writer::DeliveryFailureDisposition::Retry
                } else {
                    crate::store::writer::DeliveryFailureDisposition::Terminal
                },
                &err.to_string(),
                &workspace.session_name,
            )?;
            if failure.head_sent {
                return Ok(AttemptOutcome::Sent { compacted: false });
            }
            if durable_receiver {
                return Ok(AttemptOutcome::Queued);
            }
            Err(err.into())
        }
    }
}

pub fn sweep(workspace: &ResolvedWorkspace, store: &Store, mux: Option<MuxName>) -> Result<()> {
    let runtime = RuntimePaths::for_workspace(workspace.workspace_id.clone())?;
    let Some(_guard) = try_start_sweep(&runtime)? else {
        return Ok(());
    };
    let now = Timestamp::now();
    let delivery_window = delivery_window_from_env();
    let live = store.list_messages()?;
    let needs_snapshot = live
        .iter()
        .any(|message| matches!(message.status, MessageStatus::Sent | MessageStatus::Queued));
    let snapshot = if needs_snapshot {
        let snapshot = crate::sidebar::produce::resolution_snapshot(workspace, store, mux)?;
        Some(snapshot.with_agent_context(crate::store::agent_context::read_all(&runtime)))
    } else {
        None
    };
    store.reconcile_stale_sent_messages(
        &workspace.session_name,
        now,
        max_delivery_attempts_from_env(),
        |message| {
            snapshot.as_ref().is_some_and(|snapshot| {
                snapshot
                    .agents
                    .iter()
                    .any(|agent| message.same_agent_card(agent) && agent.is_compacting(now))
            })
        },
    )?;
    let live = store.list_messages()?;
    if live
        .iter()
        .any(|message| message.status == MessageStatus::Queued && !message.conditions_met())
    {
        let snapshot = snapshot
            .as_ref()
            .expect("unmet delivery conditions require a resolution snapshot");
        evaluate_delivery_conditions(workspace, store, snapshot, &live, now, delivery_window)?;
    }
    let pending = store.list_pending_messages()?;
    let snapshot = snapshot.as_ref();
    let mut heads_seen = std::collections::BTreeSet::new();
    for message in pending.iter().filter(|message| message.is_deliverable(now)) {
        let Some(head) = queue_head(
            pending.iter(),
            &message.kind,
            &message.agent_id,
            message.agent_name.as_deref(),
            now,
        ) else {
            continue;
        };
        if heads_seen.insert(head.message_id.to_string()) {
            let delivered = attempt_delivery(
                workspace,
                store,
                &head.message_id,
                DeliveryPolicy::Boundary,
                &pending,
                snapshot.expect("queued delivery requires a resolution snapshot"),
            )?;
            if !delivered {
                store.defer_message_wake(&head.message_id, now + delivery_window)?;
            }
        }
    }
    register_message_wake(workspace, store)?;
    Ok(())
}

fn evaluate_delivery_conditions(
    workspace: &ResolvedWorkspace,
    store: &Store,
    snapshot: &SidebarSnapshot,
    pending: &[MessageRecord],
    now: Timestamp,
    delivery_window: Duration,
) -> Result<()> {
    let mut updates = Vec::new();
    for message in pending
        .iter()
        .filter(|message| message.status == MessageStatus::Queued && !message.conditions_met())
    {
        let evaluation = evaluate_delivery(message, pending, snapshot, now, delivery_window);
        updates.push(crate::store::writer::DeliverySweepUpdate {
            message_id: message.message_id.clone(),
            after_indices: evaluation.after_stamps,
            when_indices: evaluation.when_stamps,
            retry_after: evaluation.retry_at,
            archive_reason: evaluation.archive_reason,
        });
    }
    if !updates.is_empty() {
        store.apply_delivery_sweep(&updates, now, &workspace.session_name)?;
    }
    Ok(())
}

struct SweepRunGuard {
    file: File,
}

impl Drop for SweepRunGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn try_start_sweep(runtime: &RuntimePaths) -> Result<Option<SweepRunGuard>> {
    let path = runtime.root.join("message-sweep.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|source| DeliverErr::Io {
            path: path.clone(),
            source,
        })?;
    match file.try_lock() {
        Ok(()) => Ok(Some(SweepRunGuard { file })),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(source)) => Err(DeliverErr::Io { path, source }),
    }
}

struct DeliveryCandidate<'a> {
    message: MessageRecord,
    status: AgentStatus,
    snapshot: &'a SidebarSnapshot,
    target: &'a PaneAgent,
    bound: Option<&'a AgentState>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DeliveryCheck {
    pub schedule: ScheduleCheck,
    pub after: Vec<AfterConditionCheck>,
    pub when: Vec<WhenConditionCheck>,
    pub fifo: FifoCheck,
    pub agent: AgentCheck,
    pub gate: GateCheck,
    pub ask: AskCheck,
    pub pane: PaneCheck,
}

impl DeliveryCheck {
    pub fn gate_ready(&self) -> bool {
        self.gate.open && self.gate.resume_recovered != Some(false)
    }

    /// Derived from [`Self::verdict`] so the gate ordering has one home.
    pub fn passes(&self) -> bool {
        self.verdict() == DeliveryVerdict::Ready
    }

    pub fn verdict(&self) -> DeliveryVerdict {
        if !self.schedule.ready {
            return DeliveryVerdict::Scheduled {
                not_before: self.schedule.not_before,
            };
        }
        if let Some(condition) = self.after.iter().find(|condition| !condition.met) {
            return DeliveryVerdict::WaitingOnAfter {
                address: condition.address.clone(),
                agent_present: condition.agent_present,
            };
        }
        if let Some(condition) = self.when.iter().find(|condition| !condition.met) {
            return DeliveryVerdict::WaitingOnWhen {
                address: condition.address.clone(),
                expected: condition.expected,
                current: condition.status,
                dwell_secs: condition.dwell_secs,
                dwell_so_far_secs: condition.dwell_so_far_secs,
            };
        }
        if !self.fifo.head {
            return DeliveryVerdict::BehindFifo {
                blocker: self.fifo.blocker.clone(),
            };
        }
        if !self.agent.present {
            return DeliveryVerdict::ReceiverGone;
        }
        if self.gate.compacting {
            return DeliveryVerdict::Compacting;
        }
        if !self.gate.open {
            return DeliveryVerdict::GateClosed {
                gate: self.gate.gate,
                status: self.gate.status,
            };
        }
        if self.gate.resume_recovered == Some(false) {
            return DeliveryVerdict::ResumeUnrecovered;
        }
        if self.ask.waiting {
            return DeliveryVerdict::AskWaiting;
        }
        if !self.pane.present {
            return DeliveryVerdict::NoPane {
                pinned_pane_id: self.pane.pinned_pane_id.clone(),
            };
        }
        DeliveryVerdict::Ready
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AfterConditionCheck {
    pub address: String,
    pub met: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub met_at: Option<Timestamp>,
    pub agent_present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<AgentStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WhenConditionCheck {
    pub address: String,
    pub expected: AgentStatus,
    pub dwell_secs: u64,
    pub met: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub met_at: Option<Timestamp>,
    pub agent_present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<AgentStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dwell_so_far_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trip_at: Option<Timestamp>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ScheduleCheck {
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_before: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<Timestamp>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FifoCheck {
    pub head: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocker: Option<MessageId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AgentCheck {
    pub present: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GateCheck {
    pub gate: DeliveryGate,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<AgentStatus>,
    pub compacting: bool,
    pub open: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume_recovered: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AskCheck {
    pub waiting: bool,
    pub force: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PaneCheck {
    pub present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<PaneId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned_pane_id: Option<PaneId>,
}

pub(crate) struct AfterEvaluation {
    pub check: AfterConditionCheck,
    pub stamp_needed: bool,
}

pub(crate) struct WhenEvaluation {
    pub check: WhenConditionCheck,
    pub stamp_needed: bool,
    retry_at: Option<Timestamp>,
    pub(crate) archive_reason: Option<String>,
}

struct DeliveryEvaluation<'a> {
    check: DeliveryCheck,
    agent: Option<&'a crate::agents::AgentState>,
    binding: Option<crate::harness::target::PaneBinding<'a, 'a>>,
    after_stamps: Vec<usize>,
    when_stamps: Vec<usize>,
    retry_at: Option<Timestamp>,
    archive_reason: Option<String>,
}

pub fn explain(
    message: &MessageRecord,
    pending: &[MessageRecord],
    snapshot: &SidebarSnapshot,
    now: Timestamp,
) -> DeliveryCheck {
    evaluate_delivery(message, pending, snapshot, now, delivery_window_from_env()).check
}

fn evaluate_delivery<'a>(
    message: &MessageRecord,
    pending: &[MessageRecord],
    snapshot: &'a SidebarSnapshot,
    now: Timestamp,
    delivery_window: Duration,
) -> DeliveryEvaluation<'a> {
    let schedule = ScheduleCheck {
        ready: message.is_ready(now),
        not_before: message.not_before,
        retry_after: message.retry_after,
    };
    let after = message
        .after
        .iter()
        .map(|condition| evaluate_after_condition(condition, message.gate, pending, snapshot, now))
        .collect::<Vec<_>>();
    let after_ready = after.iter().all(|condition| condition.check.met);
    let when = message
        .when
        .iter()
        .map(|condition| evaluate_when_condition(condition, snapshot, now, delivery_window))
        .collect::<Vec<_>>();
    let when_ready = when.iter().all(|condition| condition.check.met);
    let fifo =
        if message.status == MessageStatus::Queued && schedule.ready && after_ready && when_ready {
            match older_ready_blocker(pending, message, |pending| pending.is_deliverable(now)) {
                Some(head) => FifoCheck {
                    head: false,
                    blocker: Some(head.message_id.clone()),
                },
                None => FifoCheck {
                    head: true,
                    blocker: None,
                },
            }
        } else {
            FifoCheck {
                head: true,
                blocker: None,
            }
        };
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| message.same_agent_card(agent));
    let readiness = agent.map(|agent| receiver_readiness(agent, message.gate, message.force, now));
    let status = readiness.map(|readiness| readiness.status);
    let compacting = readiness.is_some_and(|readiness| readiness.compacting);
    let open = readiness.is_some_and(|readiness| readiness.gate_open);
    let resume_recovered = match (message.gate, agent, open) {
        (DeliveryGate::Resume, Some(agent), true) => {
            let runtime = RuntimePaths::for_workspace(message.workspace_id.clone()).ok();
            Some(runtime.as_ref().is_some_and(|runtime| {
                crate::harness::auto_continue::resume_gate_recovered(runtime, agent, now)
            }))
        }
        _ => None,
    };
    let waiting = readiness.is_some_and(|readiness| readiness.waiting);
    let binding = agent.and_then(|agent| {
        crate::harness::target::bind_agent(snapshot, agent, message.pane_id.as_ref())
    });
    let retry_at = after
        .iter()
        .filter_map(|evaluation| (!evaluation.check.met).then_some(now + delivery_window))
        .chain(when.iter().filter_map(|evaluation| evaluation.retry_at))
        .min();
    let archive_reason = when
        .iter()
        .find_map(|evaluation| evaluation.archive_reason.clone());
    let after_stamps = after
        .iter()
        .enumerate()
        .filter_map(|(index, evaluation)| evaluation.stamp_needed.then_some(index))
        .collect();
    let when_stamps = when
        .iter()
        .enumerate()
        .filter_map(|(index, evaluation)| evaluation.stamp_needed.then_some(index))
        .collect();
    let check = DeliveryCheck {
        schedule,
        after: after
            .into_iter()
            .map(|evaluation| evaluation.check)
            .collect(),
        when: when
            .into_iter()
            .map(|evaluation| evaluation.check)
            .collect(),
        fifo,
        agent: AgentCheck {
            present: agent.is_some(),
        },
        gate: GateCheck {
            gate: message.gate,
            status,
            compacting,
            open,
            resume_recovered,
        },
        ask: AskCheck {
            waiting,
            force: message.force,
        },
        pane: PaneCheck {
            present: binding.is_some(),
            pane_id: binding.map(|binding| binding.pane.pane_id.clone()),
            pinned_pane_id: message.pane_id.clone(),
        },
    };
    DeliveryEvaluation {
        check,
        agent,
        binding,
        after_stamps,
        when_stamps,
        retry_at,
        archive_reason,
    }
}

pub(crate) fn evaluate_when_condition(
    condition: &WhenCondition,
    snapshot: &SidebarSnapshot,
    now: Timestamp,
    delivery_window: Duration,
) -> WhenEvaluation {
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| condition.card_ref().matches(agent.card_ref()));
    let status = agent.map(|agent| agent.status);
    let matching = agent.filter(|agent| agent.status == condition.status);
    let base = matching.map(|agent| {
        match condition.status {
            AgentStatus::Running => agent.turn_started_at,
            AgentStatus::Waiting => agent.waiting_since,
            AgentStatus::Idle | AgentStatus::Success | AgentStatus::Failed => None,
            AgentStatus::Paused => None,
        }
        .unwrap_or(agent.last_activity)
    });
    let dwell_so_far_secs = base.map(|base| now.duration_since(base).as_secs().max(0) as u64);
    let trip_at = base.and_then(|base| {
        base.checked_add(Duration::from_secs(condition.dwell_secs))
            .ok()
    });
    let met = condition.met_at.is_some()
        || dwell_so_far_secs.is_some_and(|elapsed| elapsed >= condition.dwell_secs);
    let agent_gone = condition.met_at.is_none() && agent.is_none();
    WhenEvaluation {
        check: WhenConditionCheck {
            address: condition.address.clone(),
            expected: condition.status,
            dwell_secs: condition.dwell_secs,
            met,
            met_at: condition.met_at,
            agent_present: agent.is_some(),
            status,
            dwell_so_far_secs,
            trip_at,
        },
        stamp_needed: condition.met_at.is_none() && met,
        retry_at: (!met && !agent_gone).then(|| trip_at.unwrap_or(now + delivery_window)),
        archive_reason: agent_gone.then(|| condition.expiry_reason()),
    }
}

pub(crate) fn evaluate_after_condition(
    condition: &AfterCondition,
    gate: DeliveryGate,
    pending: &[MessageRecord],
    snapshot: &SidebarSnapshot,
    now: Timestamp,
) -> AfterEvaluation {
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| condition.card_ref().matches(agent.card_ref()));
    let met = condition.met_at.is_some()
        || agent.is_some_and(|agent| {
            gate_open_for_agent(gate, agent, false, now)
                && !pending.iter().any(|message| {
                    !message.status.is_terminal()
                        && message.is_ready(now)
                        && message.same_card(condition.card_ref())
                })
        });
    AfterEvaluation {
        check: AfterConditionCheck {
            address: condition.address.clone(),
            met,
            met_at: condition.met_at,
            agent_present: agent.is_some(),
            status: agent.map(crate::agents::AgentState::effective_status),
        },
        stamp_needed: condition.met_at.is_none() && met,
    }
}

fn delivery_candidate<'a>(
    pending: &[MessageRecord],
    snapshot: &'a SidebarSnapshot,
    message_id: &MessageId,
    policy: DeliveryPolicy,
    now: Timestamp,
) -> Option<DeliveryCandidate<'a>> {
    let message = pending
        .iter()
        .find(|message| message.message_id == *message_id)
        .cloned()?;
    let evaluation =
        evaluate_delivery(&message, pending, snapshot, now, delivery_window_from_env());
    let check = &evaluation.check;
    if matches!(policy, DeliveryPolicy::Boundary)
        && (!message.is_deliverable(now) || !check.fifo.head || !check.gate_ready())
    {
        return None;
    }
    if check.ask.waiting && !matches!(policy, DeliveryPolicy::Steer { force: true }) {
        return None;
    }
    let agent = evaluation.agent?;
    let status = agent.effective_status();
    let target = evaluation.binding.map(|binding| binding.pane)?;
    let bound = crate::harness::target::pane_binding(snapshot, target, None)
        .and_then(|binding| binding.exact_agent);
    Some(DeliveryCandidate {
        message,
        status,
        snapshot,
        target,
        bound,
    })
}

pub fn register_message_wake(workspace: &ResolvedWorkspace, store: &Store) -> Result<()> {
    let runtime = RuntimePaths::for_workspace(workspace.workspace_id.clone())?;
    refresh_wake_stamp(&runtime, store, Timestamp::now())
}

fn refresh_wake_stamp(runtime: &RuntimePaths, store: &Store, now: Timestamp) -> Result<()> {
    let path = wake_stamp_path(runtime);
    let next = store.earliest_message_wake(now)?;
    match next {
        Some(not_before) => {
            crate::store::atomic::write_temp_then_rename_cache(&path, &Some(not_before))?;
        }
        None => match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => {
                return Err(DeliverErr::Io { path, source: err });
            }
        },
    }
    Ok(())
}

pub(crate) fn wake_stamp_path(runtime: &RuntimePaths) -> PathBuf {
    runtime.root.join(crate::message::MESSAGE_WAKE_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::agents::AgentState;
    use crate::ids::WorkspaceId;
    use crate::store::snapshot::PaneAgent;

    #[test]
    fn receiver_readiness_unifies_gate_waiting_and_force() {
        let now = Timestamp::now();
        let idle = agent("sess-idle", AgentStatus::Idle);
        assert!(receiver_readiness(&idle, DeliveryGate::Done, false, now).accepts_prompt());

        let running = agent("sess-running", AgentStatus::Running);
        assert!(!receiver_readiness(&running, DeliveryGate::Done, false, now).accepts_prompt());

        let mut waiting = agent("sess-waiting", AgentStatus::Waiting);
        waiting.waiting_since = Some(waiting.last_activity);
        assert!(!receiver_readiness(&waiting, DeliveryGate::Done, false, now).accepts_prompt());
        assert!(receiver_readiness(&waiting, DeliveryGate::Done, true, now).accepts_prompt());
    }

    #[test]
    fn nudge_record_rides_a_system_sender_and_carries_the_agents_channel() {
        let mut agent = agent("sess-parked", AgentStatus::Paused);
        agent.channel = Some("auth".to_owned());
        let pane_id = PaneId::from_parts(MuxName::Zellij, "terminal_4");

        let resume = synthetic_record(
            workspace_id(),
            &agent,
            MessageSender::System,
            "continue".to_owned(),
            DeliveryGate::Resume,
            Some(&pane_id),
        );
        // A resume nudge is system-initiated and stays verbatim; its gate keeps
        // it out of `is_user_input`.
        assert_eq!(resume.sender, crate::message::MessageSender::System);
        assert!(!resume.is_user_input());
        assert!(resume.enter, "a nudge always submits");
        assert_eq!(resume.gate, DeliveryGate::Resume);
        assert_eq!(resume.channel.as_deref(), Some("auth"));
        assert_eq!(resume.pane_id.as_ref(), Some(&pane_id));
        assert_eq!(resume.status, MessageStatus::Queued);

        let parked = synthetic_record(
            workspace_id(),
            &agent,
            MessageSender::System,
            "continue".to_owned(),
            DeliveryGate::Done,
            None,
        );
        assert_eq!(parked.pane_id, None);
        assert_eq!(parked.gate, DeliveryGate::Done);
    }

    #[test]
    fn sweep_guard_is_single_flight() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = RuntimePaths::under(
            crate::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/message-sweep")),
            dir.path(),
        )
        .expect("runtime");
        runtime.ensure_dirs().expect("runtime dirs");

        let first = try_start_sweep(&runtime)
            .expect("first guard")
            .expect("first guard starts");
        assert!(
            try_start_sweep(&runtime).expect("second guard").is_none(),
            "a running sweep keeps later helpers from entering delivery"
        );
        drop(first);
        assert!(
            try_start_sweep(&runtime).expect("third guard").is_some(),
            "the sweep lock releases when the helper exits"
        );
    }

    #[test]
    fn explain_reports_actionable_delivery_blockers() {
        let now = Timestamp::now();
        let receiver = agent("sess-receiver", AgentStatus::Idle);
        let live = snapshot(receiver.clone(), true, now);

        let older = message(&receiver, 1, "first");
        let newer = message(&receiver, 2, "second");
        let check = explain(&newer, &[newer.clone(), older.clone()], &live, now);
        assert!(!check.fifo.head);
        assert_eq!(check.fifo.blocker, Some(older.message_id));

        let mut running = receiver.clone();
        running.status = AgentStatus::Running;
        let running = snapshot(running, true, now);
        let candidate = message(&receiver, 1, "wait");
        let check = explain(&candidate, std::slice::from_ref(&candidate), &running, now);
        assert_eq!(check.gate.status, Some(AgentStatus::Running));
        assert!(!check.gate.open);

        let mut parked = receiver.clone();
        parked.status = AgentStatus::Running;
        parked.phase = crate::agents::TurnPhase::Parked;
        let parked = snapshot(parked, true, now);
        let check = explain(&candidate, std::slice::from_ref(&candidate), &parked, now);
        assert_eq!(check.gate.status, Some(AgentStatus::Success));
        assert!(check.gate.open);

        let mut waiting = receiver.clone();
        waiting.status = AgentStatus::Waiting;
        waiting.waiting_since = Some(waiting.last_activity);
        let waiting = snapshot(waiting, true, now);
        let check = explain(&candidate, std::slice::from_ref(&candidate), &waiting, now);
        assert!(check.ask.waiting);

        let no_pane = snapshot(receiver.clone(), false, now);
        let check = explain(&candidate, std::slice::from_ref(&candidate), &no_pane, now);
        assert!(!check.pane.present);

        let not_before = now + jiff::SignedDuration::from_secs(60);
        let scheduled = message(&receiver, 1, "later").with_not_before(Some(not_before));
        let check = explain(&scheduled, std::slice::from_ref(&scheduled), &live, now);
        assert!(!check.schedule.ready);
        assert_eq!(check.schedule.not_before, Some(not_before));

        let mut upstream = agent("sess-planner", AgentStatus::Running);
        upstream.role = Some("planner".to_owned());
        let mut dependency = live;
        dependency.agents.push(upstream.clone());
        let after = message(&receiver, 1, "wait for plan").with_after(vec![AfterCondition {
            kind: upstream.kind.clone(),
            agent_id: upstream.agent_id.clone(),
            agent_name: upstream.name.clone(),
            address: "@planner".to_owned(),
            met_at: None,
        }]);
        let check = explain(&after, std::slice::from_ref(&after), &dependency, now);
        assert_eq!(check.after[0].address, "@planner");
        assert!(!check.after[0].met);
        assert!(check.after[0].agent_present);
        assert_eq!(check.after[0].status, Some(AgentStatus::Running));
        assert_eq!(
            check.verdict(),
            DeliveryVerdict::WaitingOnAfter {
                address: "@planner".to_owned(),
                agent_present: true,
            }
        );

        dependency.agents[1].status = AgentStatus::Idle;
        let check = explain(&after, std::slice::from_ref(&after), &dependency, now);
        assert!(check.after[0].met);
    }

    #[test]
    fn delivery_check_reports_first_blocker_and_passes_only_when_ready() {
        let mut check = ready_check();
        assert!(check.passes());
        assert_eq!(check.verdict(), DeliveryVerdict::Ready);

        let not_before = Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_secs(60);
        check.schedule.ready = false;
        check.schedule.not_before = Some(not_before);
        check.after.push(AfterConditionCheck {
            address: "@planner".to_owned(),
            met: false,
            met_at: None,
            agent_present: false,
            status: None,
        });
        assert_eq!(
            check.verdict(),
            DeliveryVerdict::Scheduled {
                not_before: Some(not_before)
            }
        );

        check.schedule.ready = true;
        assert_eq!(
            check.verdict(),
            DeliveryVerdict::WaitingOnAfter {
                address: "@planner".to_owned(),
                agent_present: false,
            }
        );
        check.after[0].met = true;
        check.fifo.head = false;
        check.fifo.blocker = Some(message_id(7));
        assert_eq!(
            check.verdict(),
            DeliveryVerdict::BehindFifo {
                blocker: Some(message_id(7))
            }
        );
        check.fifo.head = true;
        check.agent.present = false;
        assert_eq!(check.verdict(), DeliveryVerdict::ReceiverGone);
        check.agent.present = true;
        check.gate.open = false;
        assert_eq!(
            check.verdict(),
            DeliveryVerdict::GateClosed {
                gate: DeliveryGate::Done,
                status: Some(AgentStatus::Idle),
            }
        );
        check.gate.open = true;
        check.gate.resume_recovered = Some(false);
        assert_eq!(check.verdict(), DeliveryVerdict::ResumeUnrecovered);
        check.gate.resume_recovered = None;
        check.ask.waiting = true;
        assert_eq!(check.verdict(), DeliveryVerdict::AskWaiting);
        check.ask.waiting = false;
        check.pane.present = false;
        check.pane.pinned_pane_id = Some(PaneId::from_parts(MuxName::Zellij, "terminal_9"));
        assert_eq!(
            check.verdict(),
            DeliveryVerdict::NoPane {
                pinned_pane_id: Some(PaneId::from_parts(MuxName::Zellij, "terminal_9"))
            }
        );
        assert!(!check.passes());
    }

    #[test]
    fn candidate_uses_shared_evaluation_but_requires_durable_condition_stamp() {
        let now = Timestamp::from_second(10_000).unwrap();
        let receiver = agent("sess-receiver", AgentStatus::Idle);
        let upstream = agent("sess-upstream", AgentStatus::Idle);
        let mut live = snapshot(receiver.clone(), true, now);
        live.agents.push(upstream.clone());
        let mut candidate =
            message(&receiver, 1, "after upstream").with_after(vec![AfterCondition {
                kind: upstream.kind.clone(),
                agent_id: upstream.agent_id.clone(),
                agent_name: upstream.name.clone(),
                address: "@upstream".to_owned(),
                met_at: None,
            }]);
        let pending = vec![candidate.clone()];

        assert_eq!(
            explain(&candidate, &pending, &live, now).verdict(),
            DeliveryVerdict::Ready
        );
        assert!(
            delivery_candidate(
                &pending,
                &live,
                &candidate.message_id,
                DeliveryPolicy::Boundary,
                now,
            )
            .is_none(),
            "dynamic truth explains readiness but cannot cross claim boundary"
        );

        candidate.after[0].met_at = Some(now);
        let pending = vec![candidate.clone()];
        assert!(
            delivery_candidate(
                &pending,
                &live,
                &candidate.message_id,
                DeliveryPolicy::Boundary,
                now,
            )
            .is_some()
        );
    }

    #[test]
    fn when_evaluation_handles_exact_boundary_clock_skew_and_expiry() {
        let now = Timestamp::from_second(10_000).unwrap();
        let mut watched = agent("sess-watched", AgentStatus::Running);
        watched.turn_started_at = Some(Timestamp::from_second(9_940).unwrap());
        let condition = WhenCondition {
            kind: watched.kind.clone(),
            agent_id: watched.agent_id.clone(),
            agent_name: watched.name.clone(),
            address: "@watched".to_owned(),
            status: AgentStatus::Running,
            dwell_secs: 60,
            met_at: None,
        };
        let live = snapshot(watched.clone(), false, now);
        let exact = evaluate_when_condition(&condition, &live, now, Duration::from_secs(30));
        assert!(exact.check.met);
        assert!(exact.stamp_needed);

        watched.turn_started_at = Some(Timestamp::from_second(10_010).unwrap());
        let skewed = snapshot(watched, false, now);
        let skewed = evaluate_when_condition(&condition, &skewed, now, Duration::from_secs(30));
        assert!(!skewed.check.met);
        assert_eq!(skewed.check.dwell_so_far_secs, Some(0));
        assert_eq!(
            skewed.check.trip_at,
            Some(Timestamp::from_second(10_070).unwrap())
        );

        let gone = snapshot(agent("other", AgentStatus::Idle), false, now);
        let gone = evaluate_when_condition(&condition, &gone, now, Duration::from_secs(30));
        assert_eq!(gone.archive_reason, Some(condition.expiry_reason()));
    }

    fn ready_check() -> DeliveryCheck {
        DeliveryCheck {
            schedule: ScheduleCheck {
                ready: true,
                not_before: None,
                retry_after: None,
            },
            after: Vec::new(),
            when: Vec::new(),
            fifo: FifoCheck {
                head: true,
                blocker: None,
            },
            agent: AgentCheck { present: true },
            gate: GateCheck {
                gate: DeliveryGate::Done,
                status: Some(AgentStatus::Idle),
                compacting: false,
                open: true,
                resume_recovered: None,
            },
            ask: AskCheck {
                waiting: false,
                force: false,
            },
            pane: PaneCheck {
                present: true,
                pane_id: Some(PaneId::from_parts(MuxName::Zellij, "terminal_3")),
                pinned_pane_id: None,
            },
        }
    }

    fn snapshot(agent: AgentState, with_pane: bool, now: Timestamp) -> SidebarSnapshot {
        let mut snapshot = SidebarSnapshot::build_with_agents(workspace_id(), vec![agent], now);
        if with_pane {
            let agent = &snapshot.agents[0];
            snapshot.agent_panes = vec![PaneAgent {
                kind: agent.kind.clone(),
                kind_ordinal: agent.kind_ordinal,
                name: agent.name.clone(),
                name_explicit: agent.name_explicit,
                profile: agent.profile.clone(),
                role: agent.role.clone(),
                channel: agent.channel.clone(),
                agent_id: Some(agent.agent_id.clone()),
                pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_3"),
                pane_pid: None,
                worktree_path: agent.worktree_path.clone(),
                worktree_branch: agent.worktree_branch.clone(),
            }];
        }
        snapshot
    }

    fn message(agent: &AgentState, id: u64, text: &str) -> MessageRecord {
        let mut message = MessageRecord::new(
            workspace_id(),
            agent,
            text.to_owned(),
            true,
            DeliveryGate::Done,
        );
        message.message_id = message_id(id);
        message
    }

    fn message_id(value: u64) -> MessageId {
        MessageId::parse(&format!("msg_{value:016}")).unwrap()
    }

    fn workspace_id() -> WorkspaceId {
        WorkspaceId::parse("ws_000000000000000000000000").unwrap()
    }

    fn agent(id: &str, status: AgentStatus) -> AgentState {
        AgentState::stub("claude", id, status)
    }
}
