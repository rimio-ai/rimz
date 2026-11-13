//! The shared turn reader behind `rimz agents wait` and owed wakes and prompts.
//!
//! [`TurnWaitView`] reads the rollup, the message queue and the loop catalog
//! once per poll and answers whether an agent's turn has completed, and if not,
//! why.

use jiff::Timestamp;

use crate::Store;
use crate::agents::{AgentState, AgentStatus, TurnCompletion};
use crate::harness::auto_continue::CardStatus;
use crate::store::message::{
    HarnessNotice, MessageBody, MessageRecord, MessageSender, SoftCompactStep,
};
use crate::store::snapshot::SidebarSnapshot;

/// One wait poll's read of whether agents have finished a turn.
///
/// A wake publishes its message record before its loop-catalog row is
/// consumed, and a provider's turn start lands before the delivery ack settles
/// that record. Reading the catalog, then the queue, then the rollup therefore
/// catches every wake in flight in at least one of the three reads.
pub struct TurnWaitView {
    pub snapshot: SidebarSnapshot,
    messages: Vec<MessageRecord>,
    now: Timestamp,
    card_status: CardStatus,
}

/// Why a turn has not completed, including wakes not yet reflected on its card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnWaitReason {
    WaitingOnPerson(Option<crate::agents::OpenAsk>),
    Sleeping(Vec<crate::agents::PendingWait>),
    WakeInFlight,
    MessageQueued,
    Paused,
    NoTurnStarted,
    Running,
}

impl TurnWaitView {
    /// The blocker from the same wake-aware view used to decide completion.
    pub fn reason(&self, agent: &AgentState) -> Option<TurnWaitReason> {
        if self.completion(agent) != TurnCompletion::Open {
            return None;
        }
        Some(match self.status(agent) {
            AgentStatus::Waiting => {
                TurnWaitReason::WaitingOnPerson(agent.actionable_asks().next().map(|(ask, _)| ask))
            }
            AgentStatus::Sleeping if self.wake_in_flight(agent, false) => {
                TurnWaitReason::WakeInFlight
            }
            AgentStatus::Sleeping if self.rest_held_by_message(agent) => {
                TurnWaitReason::MessageQueued
            }
            AgentStatus::Sleeping => TurnWaitReason::Sleeping(agent.pending_waits.clone()),
            AgentStatus::Paused => TurnWaitReason::Paused,
            AgentStatus::Idle | AgentStatus::Success => TurnWaitReason::NoTurnStarted,
            _ => TurnWaitReason::Running,
        })
    }

    pub fn load(store: &Store) -> Result<Self, crate::store::StoreErr> {
        let waits = crate::harness::schedule::pending::SessionWaits::load(store.paths());
        let messages = store.list_messages()?;
        let now = Timestamp::now();
        let mut snapshot = store.snapshot_cached()?;
        waits.attach(&mut snapshot);
        Ok(Self {
            snapshot,
            messages,
            now,
            card_status: CardStatus::new(store),
        })
    }

    /// Effective status, held at `sleeping` while a wake for the agent is
    /// still between its consumed catalog row and its delivered turn, or while
    /// a person's or an agent's prompt is on its way to it, so that rest is not
    /// its finish. A displayed provider pause the card counts as spent reads as
    /// `failed`, unless a budget park takes precedence.
    fn status(&self, agent: &AgentState) -> AgentStatus {
        let status = self
            .card_status
            .status(&self.snapshot, agent, self.snapshot.now);
        let held = self.wake_in_flight(agent, false) || self.message_in_flight(agent);
        if held && Self::rested(status) {
            return AgentStatus::Sleeping;
        }
        status
    }

    fn rested(status: AgentStatus) -> bool {
        matches!(status, AgentStatus::Idle | AgentStatus::Success)
    }

    /// Whether a prompt on its way, not the card's own status, holds it asleep.
    fn rest_held_by_message(&self, agent: &AgentState) -> bool {
        Self::rested(
            self.card_status
                .status(&self.snapshot, agent, self.snapshot.now),
        ) && self.message_in_flight(agent)
    }

    pub fn completion(&self, agent: &AgentState) -> TurnCompletion {
        TurnCompletion::of(self.status(agent), agent.turn_started_at)
    }

    /// A harness wake between its publication and the turn it opens. Every
    /// reader counts `Wait` and `Signal`; a supervised run also counts the
    /// fleet digest or team report a launcher is owed. A `Sent` wake remains in
    /// flight throughout its receiver's open compaction bracket, regardless of
    /// its deadline or the bounded display/delivery window.
    pub(crate) fn wake_in_flight(&self, agent: &AgentState, digest: bool) -> bool {
        self.messages.iter().any(|message| {
            let wake = match &message.sender {
                MessageSender::Harness {
                    notice: HarnessNotice::Wait | HarnessNotice::Signal,
                } => true,
                MessageSender::Harness { notice } => {
                    digest
                        && (notice.is_fleet_digest() || matches!(notice, HarnessNotice::TeamReport))
                }
                _ => false,
            };
            wake && !message.status.is_terminal()
                && (!message.sent_hold_expired(self.now) || message.held_by_compaction(agent))
                && message.same_agent_card(agent)
        })
    }

    /// A prompt a person or an agent sent this card is on its way: queued and
    /// deliverable now, or sent and inside its hold.
    /// A `Sent` prompt counts inside its hold because the receiving turn's own
    /// Stop can read the record already claimed and sent. Commands open no turn
    /// and `System` text is RimZ's own, so neither counts.
    pub(crate) fn message_in_flight(&self, agent: &AgentState) -> bool {
        self.messages.iter().any(|message| {
            message.sender.is_conversation()
                && message.body == MessageBody::Prompt
                && !message.status.is_terminal()
                && message.is_deliverable(self.now)
                && (!message.sent_hold_expired(self.now) || message.held_by_compaction(agent))
                && message.same_agent_card(agent)
        })
    }

    /// A non-terminal soft continuation still owed to this live card.
    pub(crate) fn soft_compact_in_flight(&self, agent: &AgentState) -> bool {
        agent.ended_at.is_none()
            && self.messages.iter().any(|message| {
                message.soft_compact == Some(SoftCompactStep::Continue)
                    && !message.status.is_terminal()
                    && message.same_agent_card(agent)
            })
    }
}

#[cfg(test)]
mod tests;
