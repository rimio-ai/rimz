//! Receiver readiness, shared delivery-attempt recovery, queued sweeps, and wake maintenance.

use std::fs::File;
use std::path::PathBuf;
use std::time::Duration;

use super::{DeliveryKind, InterruptConfirmation};
use jiff::Timestamp;
use serde::Serialize;

use crate::agents::{AgentState, AgentStatus, ModelHold};
use crate::harness::auto_continue::CardStatus;
use crate::ids::{MessageId, MuxName, PaneId};
use crate::message::{gate_open_for_agent, max_delivery_attempts_from_env};
use crate::store::message::{
    AfterCondition, DeliveryGate, HarnessNotice, MessageBody, MessageRecord, MessageSender,
    MessageStatus, QUEUED_NOTICE_BUSY_DELAY, QUEUED_NOTICE_BUSY_ENV, QUEUED_NOTICE_IDLE_DELAY,
    QUEUED_NOTICE_IDLE_ENV, TypingHold, TypingHoldReplay, WhenCondition, env_ms,
    older_ready_blocker, queue_head, sender_notice,
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
    fn from_typing_hold(hold: TypingHold) -> Option<Self> {
        Some(match hold.replay? {
            TypingHoldReplay::Steer => Self::Steer { force: hold.force },
            TypingHoldReplay::Interrupt => Self::Interrupt { force: hold.force },
        })
    }

    fn kind(self) -> DeliveryKind {
        match self {
            Self::Boundary => DeliveryKind::Boundary,
            Self::Steer { .. } => DeliveryKind::Steer,
            Self::Interrupt { .. } => DeliveryKind::Interrupt,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ReceiverReadiness {
    pub status: AgentStatus,
    pub compacting: bool,
    pub gate_open: bool,
    pub waiting: bool,
    /// Why no message of any kind or gate, forced or not, goes in.
    pub held: Option<ModelHold>,
}

impl ReceiverReadiness {
    pub fn accepts_prompt(&self) -> bool {
        self.gate_open && !self.waiting && self.held.is_none()
    }
}

pub(super) fn receiver_readiness(
    agent: &crate::agents::AgentState,
    gate: DeliveryGate,
    force: bool,
    now: Timestamp,
    snapshot: &SidebarSnapshot,
    card_status: &CardStatus,
) -> ReceiverReadiness {
    ReceiverReadiness {
        status: super::gate_status(gate, agent, now, snapshot, card_status),
        compacting: agent.is_compacting(now),
        gate_open: gate_open_for_agent(gate, agent, force, now, snapshot, card_status),
        waiting: !force && agent.is_awaiting_input(),
        held: card_status.held(snapshot, agent, now),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AttemptSource {
    Fresh { durable_receiver: bool },
    Claimed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum AttemptOutcome {
    Sent {
        compacted: bool,
        interrupt: InterruptConfirmation,
    },
    Queued,
    CompactionPending,
    SkippedWaiting(super::InputWait),
    TypingHeld {
        until: Timestamp,
    },
    /// An interrupt wrote no text: its receiver left the claim's pane, before the key or after
    /// it (`pressed`). The records are queued.
    InterruptWithheld {
        cause: &'static str,
        pressed: bool,
    },
    Deferred(send::Deferral),
}

pub(super) struct Attempt<'a> {
    pub workspace: &'a ResolvedWorkspace,
    pub store: &'a Store,
    pub snapshot: &'a SidebarSnapshot,
    pub target: &'a PaneAgent,
    pub bound: Option<&'a crate::agents::AgentState>,
    pub card_status: &'a CardStatus,
    pub records: &'a [MessageRecord],
    pub source: AttemptSource,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DeliveryVerdict {
    Expired {
        expires_at: Timestamp,
    },
    TypingHeld {
        until: Timestamp,
    },
    WaitingForRegistration {
        register_by: Timestamp,
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
    ModelHeld(ModelHold),
    Compacting,
    GateClosed {
        gate: DeliveryGate,
        status: Option<AgentStatus>,
    },
    ResumeUnrecovered,
    ProviderStarting,
    AskWaiting,
    PaneHeld,
    NoPane,
    /// The hosting lookup answers another pane for the receiver than the one it is bound to,
    /// or cannot be read; `blocker` says which, and waits as [`Self::NoPane`] does.
    NotHosted {
        blocker: String,
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
            | Self::Compacting
            | Self::TypingHeld { .. } => Some(StallClass::Busy),
            Self::NoPane
            | Self::NotHosted { .. }
            | Self::ProviderStarting
            | Self::ResumeUnrecovered
            | Self::AskWaiting
            | Self::PaneHeld
            | Self::ReceiverGone
            | Self::ReceiverEnded
            | Self::ModelHeld(_)
            | Self::GateClosed { .. } => Some(StallClass::NotWorking),
            Self::Scheduled { .. }
            | Self::WaitingOnAfter { .. }
            | Self::WaitingOnWhen { .. }
            | Self::BehindFifo { .. }
            | Self::WaitingForRegistration { .. }
            | Self::Expired { .. }
            | Self::Ready => None,
        }
    }

    pub(super) fn reason(&self) -> String {
        match self {
            Self::TypingHeld { until } => {
                format!("someone is using the receiver's pane; retry at {until}")
            }
            Self::NoPane => NO_LIVE_PANE.to_owned(),
            Self::NotHosted { blocker } => blocker.clone(),
            Self::ProviderStarting => "provider is starting".to_owned(),
            Self::ResumeUnrecovered => "resuming".to_owned(),
            Self::AskWaiting => "waiting on input in its pane".to_owned(),
            Self::PaneHeld => "pane held after a provider error ended its turn".to_owned(),
            Self::ReceiverGone => "receiver has no current card".to_owned(),
            Self::ReceiverEnded => "receiver ended".to_owned(),
            Self::ModelHeld(hold) => hold.to_string(),
            Self::WaitingForRegistration { register_by } => {
                format!("waiting for registration after resume until {register_by}")
            }
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
            Self::Scheduled {
                not_before: Some(not_before),
            } => format!("scheduled for {not_before}"),
            Self::Scheduled { not_before: None } => "scheduled delivery is not due".to_owned(),
            Self::WaitingOnAfter { address, .. } => format!("waiting for {address} to finish"),
            Self::WaitingOnWhen {
                address,
                expected,
                dwell_secs,
                ..
            } => format!(
                "waiting for {address} to be {} for {}",
                expected.as_str(),
                crate::store::message::format_dwell(*dwell_secs),
            ),
            Self::BehindFifo { blocker } => blocker
                .as_ref()
                .map(|blocker| format!("blocked: behind {blocker}"))
                .unwrap_or_else(|| "blocked: FIFO head unavailable".to_owned()),
            Self::Expired { .. } => "automatic command expired".to_owned(),
            Self::Ready => {
                "delivery conditions pass but the durable condition stamp is not written yet"
                    .to_owned()
            }
        }
    }
}

/// The blocker sentence for a `NoPane` verdict, without the receiver; the sweep records it as `last_error` and the CLI appends the target.
pub const NO_LIVE_PANE: &str = "stuck: no live pane";
/// The blocker a record pinned to its enqueue-time pane once recorded; records on disk may still
/// carry it.
const PINNED_PANE_PREFIX: &str = "stuck: pinned pane ";
const PINNED_PANE_SUFFIX: &str = " is not live";
/// The start of a `NotHosted` blocker, which the sweep clears as it clears its no-pane one.
const NOT_HOSTED_PREFIX: &str = "stuck: receiver ";

/// Whether a recorded `last_error` is one this sweep wrote, so a defer on any other verdict clears it and leaves a real send failure alone.
fn is_no_pane_blocker(blocker: &str) -> bool {
    blocker == NO_LIVE_PANE
        || blocker.starts_with(NOT_HOSTED_PREFIX)
        || (blocker.starts_with(PINNED_PANE_PREFIX) && blocker.ends_with(PINNED_PANE_SUFFIX))
}

/// The wait delivery puts a record bound to `target` in before its claim, when the hosting
/// lookup does not answer that pane for the receiver; `None` when it does, or has no answer
/// for a sessionless pane. Delivery, the fresh send, and `message show` all ask here.
pub(super) fn hosting_wait(
    workspace: &ResolvedWorkspace,
    store: &Store,
    target: &PaneAgent,
    bound: Option<&AgentState>,
) -> Option<DeliveryVerdict> {
    let miss = send::lookup_miss(workspace, store, target, bound)?;
    let bound = &target.pane_id;
    Some(match miss {
        send::LookupMiss::Nowhere => DeliveryVerdict::NoPane,
        send::LookupMiss::Elsewhere(found) => DeliveryVerdict::NotHosted {
            blocker: format!("{NOT_HOSTED_PREFIX}in pane {found}, not its bound pane {bound}"),
        },
        send::LookupMiss::Unread(error) => DeliveryVerdict::NotHosted {
            blocker: format!("{NOT_HOSTED_PREFIX}pane unreadable: {error}"),
        },
    })
}

pub enum DeliveryReport {
    Sent {
        interrupt: InterruptConfirmation,
    },
    Refused(DeliveryVerdict),
    /// An interrupt wrote no text: its receiver left the claim's pane, before the key or after
    /// it (`pressed`), and the record is queued.
    InterruptWithheld {
        cause: &'static str,
        pressed: bool,
    },
    Stopped,
}

pub fn deliver_one(
    workspace: &ResolvedWorkspace,
    store: &Store,
    message_id: &MessageId,
    mux: Option<MuxName>,
    policy: DeliveryPolicy,
) -> Result<bool> {
    Ok(matches!(
        deliver_one_report(workspace, store, message_id, mux, policy)?,
        DeliveryReport::Sent { .. }
    ))
}

pub fn deliver_one_report(
    workspace: &ResolvedWorkspace,
    store: &Store,
    message_id: &MessageId,
    mux: Option<MuxName>,
    policy: DeliveryPolicy,
) -> Result<DeliveryReport> {
    let live = store.list_messages()?;
    let mut snapshot = crate::sidebar::produce::resolution_snapshot(workspace, store, mux)?;
    if let Ok(runtime) = RuntimePaths::for_project_root(&workspace.project_root) {
        snapshot = snapshot.with_agent_context(crate::store::agent_context::read_all(&runtime));
    }
    if !live
        .iter()
        .any(|message| &message.message_id == message_id && message.status == MessageStatus::Queued)
    {
        return Ok(DeliveryReport::Stopped);
    }
    attempt_delivery(
        workspace,
        store,
        message_id,
        policy,
        &live,
        &snapshot,
        &CardStatus::new(store),
    )
}

fn attempt_delivery(
    workspace: &ResolvedWorkspace,
    store: &Store,
    message_id: &MessageId,
    policy: DeliveryPolicy,
    pending: &[MessageRecord],
    snapshot: &SidebarSnapshot,
    card_status: &CardStatus,
) -> Result<DeliveryReport> {
    if let Some(message) = pending
        .iter()
        .find(|message| &message.message_id == message_id)
    {
        match super::premise::withdraw_before_candidacy(workspace, store, message) {
            Ok(false) => {}
            Ok(true) => return Ok(DeliveryReport::Stopped),
            Err(error) => {
                tracing::warn!(
                    %message_id,
                    %error,
                    "deferring message: cannot check whether its premises hold"
                );
                return Ok(DeliveryReport::Stopped);
            }
        }
    }
    let now = Timestamp::now();
    let candidate =
        match delivery_candidate(pending, snapshot, message_id, policy, now, card_status) {
            Candidacy::Ready(candidate) => candidate,
            Candidacy::Refused(verdict) => {
                let replay = match (&verdict, policy) {
                    (DeliveryVerdict::TypingHeld { .. }, DeliveryPolicy::Steer { force }) => {
                        Some((TypingHoldReplay::Steer, force))
                    }
                    (DeliveryVerdict::TypingHeld { .. }, DeliveryPolicy::Interrupt { force }) => {
                        Some((TypingHoldReplay::Interrupt, force))
                    }
                    _ => None,
                };
                if let Some((replay, force)) = replay {
                    store.record_typing_hold_replay(
                        message_id,
                        replay,
                        force,
                        &workspace.session_name,
                    )?;
                }
                return Ok(DeliveryReport::Refused(verdict));
            }
            Candidacy::Gone => return Ok(DeliveryReport::Stopped),
        };
    // Read before the claim, which a wait would only release again: a record waits here, with
    // no claim and no commit, until the lookup answers the pane it is bound to.
    if let Some(wait) = hosting_wait(workspace, store, candidate.target, candidate.bound) {
        return Ok(DeliveryReport::Refused(wait));
    }
    #[cfg(feature = "testkit")]
    crate::testkit::rendezvous("RIMZ_TEST_DELIVERY_BEFORE_CLAIM");
    let claimed = match policy {
        DeliveryPolicy::Boundary => {
            store.claim_delivery_batch(&candidate.message.message_id, candidate.status, now)?
        }
        // A retry is a boundary send, whose keys could answer a prompt the steer's chord spares.
        DeliveryPolicy::Steer { .. }
            if super::prompt_chord(crate::agents::message_keys(
                candidate
                    .bound
                    .map_or(&candidate.message.kind, |row| &row.kind),
            ))
            .is_some() =>
        {
            store
                .claim_message_spending_force(&candidate.message.message_id, now)?
                .map(|message| vec![message])
        }
        DeliveryPolicy::Steer { .. } | DeliveryPolicy::Interrupt { .. } => store
            .claim_message_for_steer(&candidate.message.message_id, now)?
            .map(|message| vec![message]),
    };
    let Some(claimed) = claimed else {
        return Ok(DeliveryReport::Stopped);
    };
    #[cfg(feature = "testkit")]
    crate::testkit::rendezvous("RIMZ_TEST_DELIVERY_AFTER_CLAIM");
    let mut send_messages = claimed
        .iter()
        .map(|message| {
            message
                .clone()
                .with_pane_id(candidate.target.pane_id.clone())
        })
        .collect::<Vec<_>>();
    let Some(first) = send_messages.first() else {
        return Ok(DeliveryReport::Stopped);
    };
    // Hook delivery handles one claimed batch; the caller's settle owns any pre-delivery spacing, so this pacer's first tick stays a no-op.
    let mut live_send = send::LiveSend::new(
        first.force
            || matches!(
                policy,
                DeliveryPolicy::Steer { force: true } | DeliveryPolicy::Interrupt { force: true }
            ),
        policy.kind(),
    );
    live_send.past_typing_hold = first.forced_keepalive;
    let batch_id = (send_messages.len() > 1).then(|| first.message_id.clone());
    for message in &mut send_messages {
        message.batch_id = batch_id.clone();
    }
    let outcome = execute_attempt(
        Attempt {
            workspace,
            store,
            snapshot: candidate.snapshot,
            target: candidate.target,
            bound: candidate.bound,
            card_status,
            records: &send_messages,
            source: AttemptSource::Claimed,
        },
        &mut live_send,
    )?;
    Ok(match outcome {
        AttemptOutcome::Sent { interrupt, .. } => DeliveryReport::Sent { interrupt },
        AttemptOutcome::InterruptWithheld { cause, pressed } => {
            DeliveryReport::InterruptWithheld { cause, pressed }
        }
        AttemptOutcome::TypingHeld { until } => {
            DeliveryReport::Refused(DeliveryVerdict::TypingHeld { until })
        }
        _ => DeliveryReport::Stopped,
    })
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
        card_status,
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
                    false,
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
    let receipt = if let Some(until) =
        live_send.typing_hold_until(workspace, store.runtime_paths(), &target.pane_id)
    {
        Ok(send::Receipt::TypingHeld {
            until,
            compacted: false,
        })
    } else {
        send::send_batch_to_live_pane(
            workspace,
            store,
            snapshot,
            target,
            bound,
            card_status,
            records,
            live_send,
        )
    };
    let receipt = receipt.and_then(|receipt| {
        let released: Option<(std::borrow::Cow<'static, str>, bool)> = match receipt {
            send::Receipt::ClaimLost { compacted } => Some((
                "pane write stopped: message claim no longer held".into(),
                compacted,
            )),
            send::Receipt::NotHosted { compacted, reason } => Some((reason.into(), compacted)),
            send::Receipt::InterruptWithheld {
                compacted, cause, ..
            } => Some((format!("interrupt withheld: {cause}").into(), compacted)),
            _ => None,
        };
        if let Some((reason, compacted)) = released {
            store.release_message_claims(records, &reason, compacted, &workspace.session_name)?;
        }
        Ok(receipt)
    });
    match receipt {
        Ok(send::Receipt::Sent {
            compacted,
            interrupt,
        }) => Ok(AttemptOutcome::Sent {
            compacted,
            interrupt,
        }),
        Ok(
            send::Receipt::ClaimLost { .. }
            | send::Receipt::NotHosted { .. }
            | send::Receipt::Withdrawn { .. },
        ) => Ok(AttemptOutcome::Queued),
        Ok(send::Receipt::InterruptWithheld { cause, pressed, .. }) => {
            Ok(AttemptOutcome::InterruptWithheld { cause, pressed })
        }
        Ok(send::Receipt::CacheCold) => {
            // The cold check reads the ping at the head; whatever was batched behind it waits for
            // the next attempt rather than failing with the ping.
            let (pings, batched): (Vec<_>, Vec<_>) = records.iter().cloned().partition(|record| {
                matches!(
                    record.sender,
                    MessageSender::Harness {
                        notice: HarnessNotice::CacheKeepalive
                    }
                )
            });
            store.record_message_delivery_failures(
                &pings,
                crate::store::writer::DeliveryFailureDisposition::Terminal,
                "the receiver's prompt cache went cold before the ping was written",
                &workspace.session_name,
            )?;
            if !batched.is_empty() {
                store.release_message_claims(
                    &batched,
                    "deferred: the cache keepalive ahead of it went cold before the paste",
                    false,
                    &workspace.session_name,
                )?;
            }
            Ok(AttemptOutcome::Queued)
        }
        Ok(send::Receipt::TypingHeld { until, compacted }) => {
            if matches!(
                source,
                AttemptSource::Fresh {
                    durable_receiver: false
                }
            ) {
                let error = send::SendErr::TypingHeld { until };
                store.record_message_delivery_failures(
                    records,
                    crate::store::writer::DeliveryFailureDisposition::Terminal,
                    &error.to_string(),
                    &workspace.session_name,
                )?;
                return Err(error.into());
            }
            let replay = match live_send.kind {
                DeliveryKind::Boundary => None,
                DeliveryKind::Steer => Some(TypingHoldReplay::Steer),
                DeliveryKind::Interrupt => Some(TypingHoldReplay::Interrupt),
            };
            store.hold_message_claims(
                records,
                TypingHold {
                    until,
                    replay,
                    force: live_send.force,
                },
                compacted,
                &workspace.session_name,
            )?;
            Ok(AttemptOutcome::TypingHeld { until })
        }
        Ok(send::Receipt::SkippedWaiting(reason)) => {
            const WAITING: &str = "agent is waiting on input in its pane";
            if matches!(source, AttemptSource::Fresh { .. })
                && live_send.kind != DeliveryKind::Boundary
                && !matches!(
                    head.sender,
                    MessageSender::Harness {
                        notice: HarnessNotice::Wait
                    }
                )
            {
                store.record_send_error(head, WAITING, &workspace.session_name)?;
                return Ok(AttemptOutcome::SkippedWaiting(reason));
            }
            store.record_message_delivery_failures(
                records,
                crate::store::writer::DeliveryFailureDisposition::Retry,
                WAITING,
                &workspace.session_name,
            )?;
            Ok(AttemptOutcome::Queued)
        }
        Ok(send::Receipt::Deferred(deferral)) => {
            if matches!(
                source,
                AttemptSource::Fresh {
                    durable_receiver: false
                }
            ) {
                let error = send::SendErr::Deferred {
                    reason: deferral.to_string(),
                };
                store.record_message_delivery_failures(
                    records,
                    crate::store::writer::DeliveryFailureDisposition::Terminal,
                    &error.to_string(),
                    &workspace.session_name,
                )?;
                return Err(error.into());
            }
            // A busy receiver is no failure: the release nets out this attempt.
            store.release_message_claims(
                records,
                &format!("deferred: {deferral}"),
                false,
                &workspace.session_name,
            )?;
            Ok(AttemptOutcome::Deferred(deferral))
        }
        Ok(send::Receipt::CompactionPending) => {
            store.release_message_claims(
                records,
                "parked: waiting for compaction to finish",
                true,
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
                return Ok(AttemptOutcome::Sent {
                    compacted: false,
                    interrupt: InterruptConfirmation::NotChecked,
                });
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
fn ended_receiver_reason(
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

/// Archive the queue's debt to the agent whose session `(kind, agent_id)` is,
/// once that agent has ended: the conditions watching it and the messages
/// parked for it, archived with the ended-receiver reason. An agent still live
/// in another session archives nothing. Returns whether it archived.
pub fn archive_for_ended_agent(
    store: &Store,
    session_name: &str,
    kind: &crate::ids::AgentKind,
    agent_id: &crate::ids::AgentSessionId,
    agent_name: Option<&str>,
) -> crate::store::Result<bool> {
    let rows = store.audit_rows()?;
    if crate::agents::state::agent_end(rows.iter(), kind, agent_id)
        == crate::agents::state::AgentEnd::Live
    {
        return Ok(false);
    }
    let receiver = rows.find(kind, agent_id);
    // The row's launch reaches the records of every session of the agent, of any kind.
    let card = receiver.map_or_else(
        || crate::agents::AgentCardRef::new(kind, agent_id, agent_name),
        AgentState::card_ref,
    );
    store.archive_messages_watching_card(card, None, session_name)?;
    let reason = receiver.map_or_else(
        || "receiver ended".to_owned(),
        |receiver| {
            let peers: Vec<_> = rows
                .iter()
                .filter(|agent| !agent.is_provider_subagent())
                .collect();
            ended_receiver_reason(receiver, None, &peers)
        },
    );
    store.archive_messages_for_card(card, None, &reason, session_name)?;
    Ok(true)
}

/// Archive the follow-ups of a resumed child whose provider exited before it took
/// `follow_up`, the message its resume was opened for, saying `what` happened, so the
/// sender is told to launch a fresh child rather than resend into the same failed resume.
/// Returns how many it archived.
pub fn archive_failed_resume(
    store: &Store,
    session_name: &str,
    kind: &crate::ids::AgentKind,
    agent_id: &crate::ids::AgentSessionId,
    agent_name: Option<&str>,
    follow_up: &MessageId,
    what: &str,
) -> crate::store::Result<usize> {
    let rows = store.audit_rows()?;
    let card = rows.find(kind, agent_id).map_or_else(
        || crate::agents::AgentCardRef::new(kind, agent_id, agent_name),
        AgentState::card_ref,
    );
    store.archive_failed_resume(card, kind.as_str(), follow_up, what, session_name)
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
    let cause = verdict.reason();
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
            vec![crate::store::message::Premise::StillQueued {
                message_id: record.message_id.clone(),
            }],
        );
        store.queue_still_queued_notice(&record.message_id, &notice, &workspace.session_name)?;
    }
    Ok(())
}

/// Run a sweep, asking the caller to wake hook processing immediately after any ingress hold commits.
pub fn sweep(
    workspace: &ResolvedWorkspace,
    store: &Store,
    mux: Option<MuxName>,
    on_ingress_hold: impl FnOnce(),
) -> Result<()> {
    let runtime = RuntimePaths::for_project_root(&workspace.project_root)?;
    let Some(_guard) = try_start_sweep(&runtime)? else {
        return Ok(());
    };
    let now = Timestamp::now();
    let delivery_window = MessageBody::Prompt.delivery_window();
    let reconcile = store.reconcile_stale_messages(
        &workspace.session_name,
        now,
        max_delivery_attempts_from_env(),
    )?;
    if reconcile.held_on_ingress > 0 {
        on_ingress_hold();
    }
    // An acknowledgement after this queue read must not free a card in a snapshot that still has it idle.
    let live = store.list_messages()?;
    #[cfg(feature = "testkit")]
    crate::testkit::rendezvous("RIMZ_TEST_SWEEP_AFTER_SNAPSHOT");
    // Built on first need, after the first queue read: a record enqueued between
    // the two reads can be a deliverable head the first read never showed.
    let mut built = None;
    let card_status = CardStatus::new(store);
    if live
        .iter()
        .any(|message| message.status == MessageStatus::Queued && !message.conditions_met())
    {
        let snapshot = sweep_snapshot(&mut built, workspace, store, mux, &runtime)?;
        evaluate_delivery_conditions(workspace, store, snapshot, &live, now, &card_status)?;
    }
    let pending = store.list_pending_messages()?;
    let mut heads_seen = std::collections::BTreeSet::new();
    for message in pending.iter().filter(|message| {
        message.is_deliverable(now)
            || (message.status == MessageStatus::Queued
                && message
                    .typing_hold
                    .is_some_and(|hold| hold.until <= now && hold.replay.is_some()))
    }) {
        let expired_hold = message.typing_hold.filter(|hold| hold.until <= now);
        let (head, policy) = match expired_hold.and_then(DeliveryPolicy::from_typing_hold) {
            Some(policy) => (message, policy),
            None if message.gate == DeliveryGate::Resume && expired_hold.is_some() => {
                (message, DeliveryPolicy::Boundary)
            }
            None => {
                let Some(head) = queue_head(pending.iter(), message.card_ref(), now) else {
                    continue;
                };
                (head, DeliveryPolicy::Boundary)
            }
        };
        if heads_seen.insert(head.message_id.to_string()) {
            let snapshot = sweep_snapshot(&mut built, workspace, store, mux, &runtime)?;
            let report = if policy == DeliveryPolicy::Boundary
                && !head.typing_hold.is_some_and(|hold| hold.until > now)
                && older_ready_blocker(live.iter(), head, now, |message| {
                    message.is_deliverable(now)
                })
                .is_some()
            {
                DeliveryReport::Stopped
            } else {
                attempt_delivery(
                    workspace,
                    store,
                    &head.message_id,
                    policy,
                    &pending,
                    snapshot,
                    &card_status,
                )?
            };
            let verdict = match report {
                DeliveryReport::Sent { .. } => continue,
                DeliveryReport::Refused(verdict) => Some(verdict),
                DeliveryReport::Stopped | DeliveryReport::InterruptWithheld { .. } => None,
            };
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
                    receiver.card_ref(),
                    receiver.ended_at,
                    &workspace.session_name,
                )?;
                store.archive_messages_for_card(
                    receiver.card_ref(),
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
            let held_until = match &verdict {
                Some(DeliveryVerdict::TypingHeld { until }) => Some(*until),
                _ => None,
            };
            let blocker = match verdict {
                Some(DeliveryVerdict::NoPane) => BlockerUpdate::Set(NO_LIVE_PANE),
                Some(DeliveryVerdict::NotHosted { ref blocker }) => BlockerUpdate::Set(blocker),
                _ => BlockerUpdate::ClearOwn(is_no_pane_blocker),
            };
            let retry_at = held_until.unwrap_or_else(|| {
                head.register_by
                    .filter(|deadline| *deadline > now)
                    .map_or(now + delivery_window, |deadline| {
                        deadline.min(now + delivery_window)
                    })
            });
            store.defer_message_wake(&head.message_id, retry_at, blocker)?;
        }
    }
    store.rearm_message_wake(Timestamp::now());
    Ok(())
}

fn sweep_snapshot<'a>(
    built: &'a mut Option<SidebarSnapshot>,
    workspace: &ResolvedWorkspace,
    store: &Store,
    mux: Option<MuxName>,
    runtime: &RuntimePaths,
) -> Result<&'a SidebarSnapshot> {
    if built.is_none() {
        let snapshot = crate::sidebar::produce::resolution_snapshot(workspace, store, mux)?;
        *built = Some(snapshot.with_agent_context(crate::store::agent_context::read_all(runtime)));
    }
    // Set just above when it was empty.
    Ok(built.as_ref().expect("sweep snapshot is built"))
}

fn evaluate_delivery_conditions(
    workspace: &ResolvedWorkspace,
    store: &Store,
    snapshot: &SidebarSnapshot,
    pending: &[MessageRecord],
    now: Timestamp,
    card_status: &CardStatus,
) -> Result<()> {
    let mut updates = Vec::new();
    for message in pending
        .iter()
        .filter(|message| message.status == MessageStatus::Queued && !message.conditions_met())
    {
        let evaluation = evaluate_delivery(message, pending, snapshot, now, card_status);
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub typing_hold: Option<TypingHold>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub register_by: Option<Timestamp>,
    pub schedule: ScheduleCheck,
    pub after: Vec<AfterConditionCheck>,
    pub when: Vec<WhenConditionCheck>,
    pub fifo: FifoCheck,
    pub agent: AgentCheck,
    pub gate: GateCheck,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hold: Option<ModelHold>,
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
        if let Some(hold) = self.typing_hold {
            return DeliveryVerdict::TypingHeld { until: hold.until };
        }
        if let Some(register_by) = self.register_by {
            return DeliveryVerdict::WaitingForRegistration { register_by };
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
        if let Some(hold) = &self.hold {
            return DeliveryVerdict::ModelHeld(hold.clone());
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
            return if self.ask.turn_ended {
                DeliveryVerdict::PaneHeld
            } else {
                DeliveryVerdict::AskWaiting
            };
        }
        if !self.pane.present {
            return DeliveryVerdict::NoPane;
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
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub permission_review: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub turn_ended: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PaneCheck {
    pub present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<PaneId>,
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
    card_status: &CardStatus,
) -> DeliveryCheck {
    evaluate_delivery(message, pending, snapshot, now, card_status).check
}

/// [`explain`], with the verdict delivery reaches before its claim: when the snapshot's
/// conditions pass, the hosting lookup is asked once, as delivery asks it.
pub fn explain_before_claim(
    workspace: &ResolvedWorkspace,
    store: &Store,
    message: &MessageRecord,
    pending: &[MessageRecord],
    snapshot: &SidebarSnapshot,
    now: Timestamp,
    card_status: &CardStatus,
) -> (DeliveryCheck, DeliveryVerdict) {
    let evaluation = evaluate_delivery(message, pending, snapshot, now, card_status);
    let verdict = evaluation.check.verdict();
    let hosted = match (&verdict, &evaluation.binding) {
        (DeliveryVerdict::Ready, Some(binding)) => {
            hosting_wait(workspace, store, binding.pane, binding.exact_agent)
        }
        _ => None,
    };
    (evaluation.check, hosted.unwrap_or(verdict))
}

fn evaluate_delivery<'a>(
    message: &MessageRecord,
    pending: &[MessageRecord],
    snapshot: &'a SidebarSnapshot,
    now: Timestamp,
    card_status: &CardStatus,
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
        .map(|condition| {
            evaluate_after_condition(condition, message.gate, pending, snapshot, now, card_status)
        })
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
    let readiness = agent.map(|agent| {
        receiver_readiness(
            agent,
            message.gate,
            message.force,
            now,
            snapshot,
            card_status,
        )
    });
    let status = readiness.as_ref().map(|readiness| readiness.status);
    let compacting = readiness
        .as_ref()
        .is_some_and(|readiness| readiness.compacting);
    let open = readiness
        .as_ref()
        .is_some_and(|readiness| readiness.gate_open);
    let resume_recovered = match (message.gate, agent, open) {
        (DeliveryGate::Resume, Some(agent), true) => {
            let runtime = RuntimePaths::for_workspace(message.workspace_id.clone()).ok();
            Some(runtime.as_ref().is_some_and(|runtime| {
                crate::harness::auto_continue::resume_gate_recovered(runtime, agent, now)
            }))
        }
        _ => None,
    };
    let waiting = readiness
        .as_ref()
        .is_some_and(|readiness| readiness.waiting);
    let hold = readiness.and_then(|readiness| readiness.held);
    let binding = agent.and_then(|agent| crate::address::bind_agent(snapshot, agent));
    let retry_at = message
        .typing_hold
        .filter(|hold| hold.until > now)
        .map(|hold| hold.until)
        .or_else(|| {
            after
                .iter()
                .filter_map(|evaluation| (!evaluation.check.met).then_some(now + delivery_window))
                .chain(when.iter().filter_map(|evaluation| evaluation.retry_at))
                .min()
        });
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
        typing_hold: message.typing_hold.filter(|hold| hold.until > now),
        register_by: message
            .register_by
            .filter(|_| message.awaiting_registration(&snapshot.agents)),
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
            provider_start_pending: agent.is_some_and(|agent| {
                super::provider_start_pending(agent, message.register_by.is_some(), now)
            }),
            gate: message.gate,
            status,
            compacting,
            open,
            resume_recovered,
        },
        hold,
        ask: AskCheck {
            waiting,
            force: message.force,
            permission_review: agent.is_some_and(|agent| agent.is_reviewing_permission(now)),
            turn_ended: waiting
                && agent.is_some_and(|agent| {
                    super::InputWait::for_agent(agent, now) == super::InputWait::TurnEnded
                }),
        },
        pane: PaneCheck {
            present: binding.is_some(),
            pane_id: binding.map(|binding| binding.pane.pane_id.clone()),
        },
    };
    DeliveryEvaluation {
        check,
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
    let status = agent.map(|agent| {
        if agent.is_reviewing_permission(now) || agent.permission_settled() {
            AgentStatus::Running
        } else {
            agent.status
        }
    });
    let matching = agent.filter(|_| status == Some(condition.status));
    let base = matching.map(|agent| {
        match condition.status {
            AgentStatus::Running => agent.turn_started_at,
            AgentStatus::Waiting => agent.permission_review_until().or(agent.waiting_since),
            AgentStatus::Idle
            | AgentStatus::Success
            | AgentStatus::Sleeping
            | AgentStatus::Failed => None,
            AgentStatus::Paused => None,
        }
        .unwrap_or(agent.last_activity)
    });
    let dwell_so_far_secs = base.map(|base| now.duration_since(base).as_secs().max(0) as u64);
    let trip_at = base
        .or_else(|| {
            agent
                .filter(|agent| {
                    condition.status == AgentStatus::Waiting && agent.is_reviewing_permission(now)
                })
                .and_then(AgentState::permission_review_until)
        })
        .and_then(|base| {
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
    card_status: &CardStatus,
) -> AfterEvaluation {
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| condition.card_ref().matches(agent.card_ref()));
    let met = condition.met_at.is_some()
        || agent.is_some_and(|agent| {
            gate_open_for_agent(gate, agent, false, now, snapshot, card_status)
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
            status: agent.map(|agent| super::gate_status(gate, agent, now, snapshot, card_status)),
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
    card_status: &CardStatus,
) -> Candidacy<'a> {
    let Some(message) = pending
        .iter()
        .find(|message| message.message_id == *message_id)
        .cloned()
    else {
        return Candidacy::Gone;
    };
    let mut evaluation = evaluate_delivery(&message, pending, snapshot, now, card_status);
    if matches!(policy, DeliveryPolicy::Interrupt { force: true }) || message.forced_keepalive {
        evaluation.check.typing_hold = None;
    }
    let check = &evaluation.check;
    if check.typing_hold.is_some() {
        return Candidacy::Refused(check.verdict());
    }
    if message.expired(now)
        || (matches!(policy, DeliveryPolicy::Boundary)
            && (!message.is_deliverable(now) || !check.passes()))
    {
        return Candidacy::Refused(check.verdict());
    }
    if (check.ask.waiting || check.register_by.is_some())
        && !matches!(
            policy,
            DeliveryPolicy::Steer { force: true } | DeliveryPolicy::Interrupt { force: true }
        )
    {
        return Candidacy::Refused(check.verdict());
    }
    let (Some(status), Some(binding)) = (check.gate.status, evaluation.binding) else {
        return Candidacy::Refused(check.verdict());
    };
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

#[cfg(test)]
mod tests;
