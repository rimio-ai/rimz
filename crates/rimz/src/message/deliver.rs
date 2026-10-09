//! Receiver readiness, shared delivery-attempt recovery, queued sweeps, and wake maintenance.

use std::fs::File;
use std::path::PathBuf;
use std::time::Duration;

use super::DeliveryKind;
use jiff::Timestamp;
use serde::Serialize;

use crate::agents::{AgentState, AgentStatus};
use crate::ids::{MessageId, MuxName, PaneId};
use crate::message::{gate_open_for_agent, max_delivery_attempts_from_env};
use crate::store::message::{
    AfterCondition, DeliveryGate, HarnessNotice, MessageBody, MessageRecord, MessageSender,
    MessageStatus, QUEUED_NOTICE_BUSY_DELAY, QUEUED_NOTICE_BUSY_ENV, QUEUED_NOTICE_IDLE_DELAY,
    QUEUED_NOTICE_IDLE_ENV, WhenCondition, env_ms, older_ready_blocker, queue_head, sender_notice,
};
use crate::store::snapshot::{PaneAgent, SidebarSnapshot};
use crate::store::writer::BlockerUpdate;
use crate::workspace::ResolvedWorkspace;
use crate::{RuntimePaths, Store};

use super::send;

pub(super) type Result<T> = std::result::Result<T, DeliverErr>;

#[derive(Debug, thiserror::Error)]
pub enum DeliverErr {
    #[error(transparent)]
    Store(#[from] crate::store::StoreErr),
    #[error(transparent)]
    Path(#[from] crate::disk::paths::PathErr),
    #[error(transparent)]
    Atomic(#[from] crate::disk::atomic::AtomicErr),
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
    Interrupt { force: bool },
}

impl DeliveryPolicy {
    fn kind(self) -> DeliveryKind {
        match self {
            Self::Boundary => DeliveryKind::Boundary,
            Self::Steer { .. } => DeliveryKind::Steer,
            Self::Interrupt { .. } => DeliveryKind::Interrupt,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ReceiverReadiness {
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

pub(super) fn receiver_readiness(
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
pub(super) enum AttemptSource {
    Fresh { durable_receiver: bool },
    Claimed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AttemptOutcome {
    Sent { compacted: bool },
    Queued,
    CompactionPending,
    SkippedWaiting,
}

pub(super) struct Attempt<'a> {
    pub workspace: &'a ResolvedWorkspace,
    pub store: &'a Store,
    pub snapshot: &'a SidebarSnapshot,
    pub target: &'a PaneAgent,
    pub bound: Option<&'a crate::agents::AgentState>,
    pub records: &'a [MessageRecord],
    pub source: AttemptSource,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DeliveryVerdict {
    Expired {
        expires_at: Timestamp,
    },
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
    ReceiverEnded,
    Compacting,
    GateClosed {
        gate: DeliveryGate,
        status: Option<AgentStatus>,
    },
    ResumeUnrecovered,
    ProviderStarting,
    AskWaiting,
    NoPane {
        pinned_pane_id: Option<PaneId>,
    },
    Ready,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StallClass {
    NotWorking,
    Busy,
}

impl StallClass {
    fn delay(self) -> Duration {
        match self {
            Self::NotWorking => env_ms(QUEUED_NOTICE_IDLE_ENV).unwrap_or(QUEUED_NOTICE_IDLE_DELAY),
            Self::Busy => env_ms(QUEUED_NOTICE_BUSY_ENV).unwrap_or(QUEUED_NOTICE_BUSY_DELAY),
        }
    }
}

impl DeliveryVerdict {
    fn stall_class(&self) -> Option<StallClass> {
        match self {
            Self::GateClosed {
                status: Some(AgentStatus::Running),
                ..
            }
            | Self::Compacting => Some(StallClass::Busy),
            Self::NoPane { .. }
            | Self::ProviderStarting
            | Self::ResumeUnrecovered
            | Self::AskWaiting
            | Self::ReceiverGone
            | Self::ReceiverEnded
            | Self::GateClosed { .. } => Some(StallClass::NotWorking),
            Self::Scheduled { .. }
            | Self::WaitingOnAfter { .. }
            | Self::WaitingOnWhen { .. }
            | Self::BehindFifo { .. }
            | Self::Expired { .. }
            | Self::Ready => None,
        }
    }

    fn stall_cause(&self) -> String {
        match self {
            Self::NoPane { pinned_pane_id } => no_pane_blocker(pinned_pane_id.as_ref()),
            Self::ProviderStarting => "provider is starting".to_owned(),
            Self::ResumeUnrecovered => "resuming".to_owned(),
            Self::AskWaiting => "waiting on input in its pane".to_owned(),
            Self::ReceiverGone => "receiver has no current card".to_owned(),
            Self::ReceiverEnded => "receiver ended".to_owned(),
            Self::Compacting => "compacting".to_owned(),
            Self::GateClosed {
                status: Some(AgentStatus::Running),
                ..
            } => "busy in a turn".to_owned(),
            Self::GateClosed {
                status: Some(status),
                ..
            } => format!("gate closed while {}", status.as_str()),
            Self::GateClosed { status: None, .. } => {
                "gate closed while status is unknown".to_owned()
            }
            Self::Scheduled { .. }
            | Self::WaitingOnAfter { .. }
            | Self::WaitingOnWhen { .. }
            | Self::BehindFifo { .. }
            | Self::Expired { .. }
            | Self::Ready => String::new(),
        }
    }
}

const NO_LIVE_PANE: &str = "stuck: no live pane";
const PINNED_PANE_PREFIX: &str = "stuck: pinned pane ";
const PINNED_PANE_SUFFIX: &str = " is not live";

/// The blocker sentence for a `NoPane` verdict, without the receiver; the sweep records it as `last_error` and the CLI appends the target.
pub fn no_pane_blocker(pinned_pane_id: Option<&PaneId>) -> String {
    match pinned_pane_id {
        Some(pane_id) => format!("{PINNED_PANE_PREFIX}{pane_id}{PINNED_PANE_SUFFIX}"),
        None => NO_LIVE_PANE.to_owned(),
    }
}

/// Whether a recorded `last_error` is one this sweep wrote, so a defer on any other verdict clears it and leaves a real send failure alone.
fn is_no_pane_blocker(blocker: &str) -> bool {
    blocker == NO_LIVE_PANE
        || (blocker.starts_with(PINNED_PANE_PREFIX) && blocker.ends_with(PINNED_PANE_SUFFIX))
}

enum DeliveryReport {
    Sent,
    Stopped(Option<DeliveryVerdict>),
}

pub fn deliver_one(
    workspace: &ResolvedWorkspace,
    store: &Store,
    message_id: &MessageId,
    mux: Option<MuxName>,
    policy: DeliveryPolicy,
) -> Result<bool> {
    let pending = store.list_pending_messages()?;
    let mut snapshot = crate::sidebar::produce::resolution_snapshot(workspace, store, mux)?;
    if let Ok(runtime) = RuntimePaths::for_project_root(&workspace.project_root) {
        snapshot = snapshot.with_agent_context(crate::store::agent_context::read_all(&runtime));
    }
    if matches!(policy, DeliveryPolicy::Interrupt { .. })
        && let Some(message) = pending
            .iter()
            .find(|message| &message.message_id == message_id)
    {
        send::interrupt_key(
            &message.kind,
            &format!(
                "@{}",
                message
                    .agent_name
                    .as_deref()
                    .unwrap_or(message.kind.as_str())
            ),
        )?;
    }
    Ok(matches!(
        attempt_delivery(workspace, store, message_id, policy, &pending, &snapshot)?,
        DeliveryReport::Sent
    ))
}

fn attempt_delivery(
    workspace: &ResolvedWorkspace,
    store: &Store,
    message_id: &MessageId,
    policy: DeliveryPolicy,
    pending: &[MessageRecord],
    snapshot: &SidebarSnapshot,
) -> Result<DeliveryReport> {
    if let Some(message) = pending
        .iter()
        .find(|message| &message.message_id == message_id)
    {
        match cancel_joined_subagent_report(workspace, store, message) {
            Ok(false) => {}
            Ok(true) => {
                register_message_wake(workspace, store);
                return Ok(DeliveryReport::Stopped(None));
            }
            Err(error) => {
                tracing::warn!(%message_id, %error, "deferring subagent digest: cannot check or settle joined runs");
                return Ok(DeliveryReport::Stopped(None));
            }
        }
    }
    let now = Timestamp::now();
    let candidate = match delivery_candidate(pending, snapshot, message_id, policy, now) {
        Candidacy::Ready(candidate) => candidate,
        Candidacy::Refused(verdict) => return Ok(DeliveryReport::Stopped(Some(verdict))),
        Candidacy::Gone => return Ok(DeliveryReport::Stopped(None)),
    };
    #[cfg(feature = "testkit")]
    crate::testkit::rendezvous("RIMZ_TEST_DELIVERY_BEFORE_CLAIM");
    let claimed = match policy {
        DeliveryPolicy::Boundary => {
            store.claim_delivery_batch(&candidate.message.message_id, candidate.status, now)?
        }
        DeliveryPolicy::Steer { .. } | DeliveryPolicy::Interrupt { .. } => store
            .claim_message_for_steer(&candidate.message.message_id, now)?
            .map(|message| vec![message]),
    };
    let Some(claimed) = claimed else {
        return Ok(DeliveryReport::Stopped(None));
    };
    register_message_wake(workspace, store);
    #[cfg(feature = "testkit")]
    crate::testkit::rendezvous("RIMZ_TEST_DELIVERY_AFTER_CLAIM");
    match cancel_joined_subagent_report(workspace, store, &claimed[0]) {
        Ok(false) => {}
        Ok(true) => {
            register_message_wake(workspace, store);
            return Ok(DeliveryReport::Stopped(None));
        }
        Err(error) => {
            tracing::warn!(%message_id, %error, "deferring subagent digest: cannot check or settle joined runs");
            store.release_message_claims(
                &claimed,
                "deferred: cannot check or settle joined runs",
                &workspace.session_name,
            )?;
            register_message_wake(workspace, store);
            return Ok(DeliveryReport::Stopped(None));
        }
    }
    // Hook delivery handles one claimed batch; the caller's settle owns any pre-delivery spacing, so this pacer's first tick stays a no-op.
    let mut live_send = send::LiveSend::new(
        claimed[0].force
            || matches!(
                policy,
                DeliveryPolicy::Steer { force: true } | DeliveryPolicy::Interrupt { force: true }
            ),
        policy.kind(),
    );
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
        },
        &mut live_send,
    )?;
    register_message_wake(workspace, store);
    Ok(if matches!(outcome, AttemptOutcome::Sent { .. }) {
        DeliveryReport::Sent
    } else {
        DeliveryReport::Stopped(None)
    })
}

fn cancel_joined_subagent_report(
    workspace: &ResolvedWorkspace,
    store: &Store,
    message: &MessageRecord,
) -> crate::store::Result<bool> {
    if !matches!(
        &message.sender,
        MessageSender::Harness { notice } if notice.is_fleet_digest()
    ) || !crate::harness::run::report::digest_fully_joined(store.paths(), &message.message_id)?
    {
        return Ok(false);
    }
    store.cancel_message(
        &message.message_id,
        &workspace.session_name,
        "joined before delivery",
    )?;
    Ok(true)
}

pub(super) fn execute_attempt(
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
    } = attempt;
    let head = records
        .first()
        .expect("delivery attempts require at least one message");
    if head.body == MessageBody::Command {
        let current = store.runtime_projection(crate::RuntimeScope::Audit)?;
        let now = Timestamp::now();
        if let Some(agent) = current
            .agents
            .iter()
            .find(|agent| head.same_agent_card(agent) && agent.compaction_unprompted(now))
        {
            if agent.is_compacting(now) {
                store.release_message_claims(
                    records,
                    "parked: waiting for compaction to finish",
                    &workspace.session_name,
                )?;
                return Ok(AttemptOutcome::Queued);
            }
            store.record_message_delivery_failures(
                records,
                crate::store::writer::DeliveryFailureDisposition::Terminal,
                "a compaction never follows a compaction; the agent has not taken a turn since its last one",
                &workspace.session_name,
            )?;
            return Ok(AttemptOutcome::Queued);
        }
    }
    match send::send_batch_to_live_pane(
        workspace, store, snapshot, target, bound, records, live_send,
    ) {
        Ok(send::Receipt::Sent { compacted }) => Ok(AttemptOutcome::Sent { compacted }),
        Ok(send::Receipt::ClaimLost) => Ok(AttemptOutcome::Queued),
        Ok(send::Receipt::SkippedWaiting) => {
            const WAITING: &str = "agent is waiting on input in its pane";
            if matches!(source, AttemptSource::Fresh { .. })
                && live_send.kind != DeliveryKind::Boundary
            {
                store.record_send_error(head, WAITING, &workspace.session_name)?;
                return Ok(AttemptOutcome::SkippedWaiting);
            }
            store.record_message_delivery_failures(
                records,
                crate::store::writer::DeliveryFailureDisposition::Retry,
                WAITING,
                &workspace.session_name,
            )?;
            Ok(AttemptOutcome::Queued)
        }
        Ok(send::Receipt::CompactionPending) => {
            store.release_message_claims(
                records,
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
                records,
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

/// Archive reason for the open messages of an ended receiver: a launched
/// child's names the `rimz message` that resumes it, addressed as `address`
/// when the sender gave one.
pub fn ended_receiver_reason(
    receiver: &AgentState,
    address: Option<&str>,
    peers: &[&AgentState],
) -> String {
    if !receiver.is_launched_child() {
        return "receiver ended".to_owned();
    }
    let target = address.map_or_else(
        || crate::address::agent_handle(receiver, peers, true),
        str::to_owned,
    );
    format!("receiver ended; rimz message {target} resumes it")
}

/// Tells each agent sender, once per message, that its condition-met record on `head`'s card has
/// stayed queued past the delay of the class `verdict` puts the receiver in.
fn notify_long_queued(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agents: &[AgentState],
    pending: &[MessageRecord],
    head: &MessageRecord,
    verdict: &DeliveryVerdict,
    now: Timestamp,
) -> Result<()> {
    let Some(class) = verdict.stall_class() else {
        return Ok(());
    };
    let delay = class.delay();
    let cause = verdict.stall_cause();
    for record in pending.iter().filter(|record| {
        record.same_card(head.card_ref())
            && record.is_deliverable(now)
            && record.queued_notice_at.is_none()
            && record.wants_queued_notice()
    }) {
        let waited =
            Duration::from_millis(now.duration_since(record.enqueued_at).as_millis().max(0) as u64);
        if waited < delay {
            continue;
        }
        let Some(sender) = sender_notice::resolve_sender(&record.sender, agents) else {
            continue;
        };
        let notice = sender_notice::compose(
            workspace.workspace_id.clone(),
            sender,
            HarnessNotice::MessageQueued,
            sender_notice::still_queued(record, &cause, waited),
        );
        store.queue_still_queued_notice(&record.message_id, &notice, &workspace.session_name)?;
    }
    Ok(())
}

pub fn sweep(workspace: &ResolvedWorkspace, store: &Store, mux: Option<MuxName>) -> Result<()> {
    let runtime = RuntimePaths::for_project_root(&workspace.project_root)?;
    let Some(_guard) = try_start_sweep(&runtime)? else {
        return Ok(());
    };
    let now = Timestamp::now();
    let delivery_window = MessageBody::Prompt.delivery_window();
    store.reconcile_stale_messages(
        &workspace.session_name,
        now,
        max_delivery_attempts_from_env(),
    )?;
    // An acknowledgement after this queue read must not free a card in a snapshot that still has it idle.
    let live = store.list_messages()?;
    let needs_snapshot = live
        .iter()
        .any(|message| message.status.is_open() || message.status == MessageStatus::Sent);
    let snapshot = if needs_snapshot {
        let snapshot = crate::sidebar::produce::resolution_snapshot(workspace, store, mux)?;
        Some(snapshot.with_agent_context(crate::store::agent_context::read_all(&runtime)))
    } else {
        None
    };
    #[cfg(feature = "testkit")]
    crate::testkit::rendezvous("RIMZ_TEST_SWEEP_AFTER_SNAPSHOT");
    if live
        .iter()
        .any(|message| message.status == MessageStatus::Queued && !message.conditions_met())
    {
        let snapshot = snapshot
            .as_ref()
            .expect("unmet delivery conditions require a resolution snapshot");
        evaluate_delivery_conditions(workspace, store, snapshot, &live, now)?;
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
            let snapshot = snapshot.expect("queued delivery requires a resolution snapshot");
            let report = if older_ready_blocker(live.iter(), head, now, |message| {
                message.is_deliverable(now)
            })
            .is_some()
            {
                DeliveryReport::Stopped(None)
            } else {
                attempt_delivery(
                    workspace,
                    store,
                    &head.message_id,
                    DeliveryPolicy::Boundary,
                    &pending,
                    snapshot,
                )?
            };
            if let DeliveryReport::Stopped(verdict) = report {
                let ended_receiver = match &verdict {
                    Some(DeliveryVerdict::ReceiverEnded) => snapshot
                        .agents
                        .iter()
                        .find(|agent| head.same_agent_card(agent))
                        .cloned(),
                    Some(DeliveryVerdict::ReceiverGone) => store
                        .runtime_projection(crate::RuntimeScope::Audit)?
                        .agents
                        .into_iter()
                        .find(|agent| head.same_agent_card(agent))
                        .filter(|agent| agent.ended_at.is_some()),
                    _ => None,
                };
                if let Some(receiver) = ended_receiver {
                    let reason = ended_receiver_reason(
                        &receiver,
                        head.address.as_deref(),
                        &crate::address::addressable_agents(snapshot),
                    );
                    store.archive_messages_watching_card(
                        &receiver.kind,
                        &receiver.agent_id,
                        receiver.name.as_deref(),
                        receiver.ended_at,
                        &workspace.session_name,
                    )?;
                    store.archive_messages_for_card(
                        &receiver.kind,
                        &receiver.agent_id,
                        receiver.name.as_deref(),
                        receiver.ended_at,
                        &reason,
                        &workspace.session_name,
                    )?;
                    continue;
                }
                if let Some(verdict) = &verdict {
                    notify_long_queued(
                        workspace,
                        store,
                        &snapshot.agents,
                        &pending,
                        head,
                        verdict,
                        now,
                    )?;
                }
                let recorded = match verdict {
                    Some(DeliveryVerdict::NoPane { pinned_pane_id }) => {
                        Some(no_pane_blocker(pinned_pane_id.as_ref()))
                    }
                    _ => None,
                };
                let blocker = match recorded.as_deref() {
                    Some(sentence) => BlockerUpdate::Set(sentence),
                    None => BlockerUpdate::ClearOwn(is_no_pane_blocker),
                };
                store.defer_message_wake(&head.message_id, now + delivery_window, blocker)?;
            }
        }
    }
    register_message_wake(workspace, store);
    Ok(())
}

fn evaluate_delivery_conditions(
    workspace: &ResolvedWorkspace,
    store: &Store,
    snapshot: &SidebarSnapshot,
    pending: &[MessageRecord],
    now: Timestamp,
) -> Result<()> {
    let mut updates = Vec::new();
    for message in pending
        .iter()
        .filter(|message| message.status == MessageStatus::Queued && !message.conditions_met())
    {
        let evaluation = evaluate_delivery(message, pending, snapshot, now);
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
    let path = runtime.lock_path("message-sweep.lock");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|source| DeliverErr::Io {
            path: path.clone(),
            source,
        })?;
    match crate::disk::lock::try_lock_file(&mut file, &path) {
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
    pub expiry: ExpiryCheck,
    pub schedule: ScheduleCheck,
    pub after: Vec<AfterConditionCheck>,
    pub when: Vec<WhenConditionCheck>,
    pub fifo: FifoCheck,
    pub agent: AgentCheck,
    pub gate: GateCheck,
    pub ask: AskCheck,
    pub pane: PaneCheck,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExpiryCheck {
    pub expired: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Timestamp>,
}

impl DeliveryCheck {
    pub fn gate_ready(&self) -> bool {
        self.gate.open
            && !self.gate.provider_start_pending
            && self.gate.resume_recovered != Some(false)
    }

    /// Derived from [`Self::verdict`] so the gate ordering has one home.
    pub fn passes(&self) -> bool {
        self.verdict() == DeliveryVerdict::Ready
    }

    pub fn verdict(&self) -> DeliveryVerdict {
        if self.expiry.expired
            && let Some(expires_at) = self.expiry.expires_at
        {
            return DeliveryVerdict::Expired { expires_at };
        }
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
        if self.agent.ended {
            return DeliveryVerdict::ReceiverEnded;
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
        if self.gate.provider_start_pending {
            return DeliveryVerdict::ProviderStarting;
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
    pub ended: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GateCheck {
    pub provider_start_pending: bool,
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

pub(super) struct AfterEvaluation {
    pub check: AfterConditionCheck,
    pub stamp_needed: bool,
}

pub(super) struct WhenEvaluation {
    pub check: WhenConditionCheck,
    pub stamp_needed: bool,
    retry_at: Option<Timestamp>,
    pub(crate) archive_reason: Option<String>,
}

struct DeliveryEvaluation<'a> {
    check: DeliveryCheck,
    agent: Option<&'a crate::agents::AgentState>,
    binding: Option<crate::address::PaneBinding<'a, 'a>>,
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
    evaluate_delivery(message, pending, snapshot, now).check
}

fn evaluate_delivery<'a>(
    message: &MessageRecord,
    pending: &[MessageRecord],
    snapshot: &'a SidebarSnapshot,
    now: Timestamp,
) -> DeliveryEvaluation<'a> {
    let delivery_window = MessageBody::Prompt.delivery_window();
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
            match older_ready_blocker(pending, message, now, |pending| pending.is_deliverable(now))
            {
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
    let binding = agent
        .and_then(|agent| crate::address::bind_agent(snapshot, agent, message.pane_id.as_ref()));
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
        expiry: ExpiryCheck {
            expired: message.expired(now),
            expires_at: message.expires_at(),
        },
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
            ended: agent.is_some_and(|agent| agent.ended_at.is_some()),
        },
        gate: GateCheck {
            provider_start_pending: agent
                .is_some_and(|agent| super::provider_start_pending(agent, now)),
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

pub(super) fn evaluate_when_condition(
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
            AgentStatus::Idle
            | AgentStatus::Success
            | AgentStatus::Sleeping
            | AgentStatus::Failed => None,
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

pub(super) fn evaluate_after_condition(
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

enum Candidacy<'a> {
    Ready(Box<DeliveryCandidate<'a>>),
    Refused(DeliveryVerdict),
    Gone,
}

fn delivery_candidate<'a>(
    pending: &[MessageRecord],
    snapshot: &'a SidebarSnapshot,
    message_id: &MessageId,
    policy: DeliveryPolicy,
    now: Timestamp,
) -> Candidacy<'a> {
    let Some(message) = pending
        .iter()
        .find(|message| message.message_id == *message_id)
        .cloned()
    else {
        return Candidacy::Gone;
    };
    let evaluation = evaluate_delivery(&message, pending, snapshot, now);
    let check = &evaluation.check;
    if message.expired(now)
        || (matches!(policy, DeliveryPolicy::Boundary)
            && (!message.is_deliverable(now) || !check.passes()))
    {
        return Candidacy::Refused(check.verdict());
    }
    if check.ask.waiting
        && !matches!(
            policy,
            DeliveryPolicy::Steer { force: true } | DeliveryPolicy::Interrupt { force: true }
        )
    {
        return Candidacy::Refused(check.verdict());
    }
    let (Some(agent), Some(binding)) = (evaluation.agent, evaluation.binding) else {
        return Candidacy::Refused(check.verdict());
    };
    let status = agent.effective_status();
    let target = binding.pane;
    let bound = binding.exact_agent;
    Candidacy::Ready(Box::new(DeliveryCandidate {
        message,
        status,
        snapshot,
        target,
        bound,
    }))
}

/// Refreshes the elder's wake after any queue change: before a claim's pane write, so a sender
/// that dies mid-write is requeued after `CLAIM_TTL`, and after an attempt or a queue edit, so the
/// elder sees what remains. Wakes are latency, never truth: a stamp that cannot be written warns
/// and delays the elder's next look, and never changes the outcome of the change it follows.
pub fn register_message_wake(workspace: &ResolvedWorkspace, store: &Store) {
    let refresh = || -> Result<()> {
        let runtime = RuntimePaths::for_project_root(&workspace.project_root)?;
        Ok(send::refresh_wake_stamp(&runtime, store, Timestamp::now())?)
    };
    if let Err(error) = refresh() {
        tracing::warn!(
            %error,
            "cannot refresh the message wake stamp; the elder sweep may run late"
        );
    }
}

pub(super) fn wake_stamp_path(runtime: &RuntimePaths) -> PathBuf {
    runtime.lane_path(crate::message::MESSAGE_WAKE_FILE)
}

#[cfg(test)]
mod tests;
