//! A blocking wait's check for prompts queued for the agent running it.
//!
//! An agent blocked in a wait is idle, so a person's or an agent's prompt
//! queued for it would sit until the wait returns. [`WaitCaller::queued`]
//! lets each wait loop stop instead: it finds those prompts, stamps each one
//! `announced_at` once, and leaves the text to the turn-end paste.

use jiff::Timestamp;

use crate::Store;
use crate::agents::AgentState;
use crate::harness::ancestry::CallerIdentity;
use crate::store::message::{MessageRecord, MessageSender};

/// The caller of a blocking wait, and the instant that wait started.
pub struct WaitCaller {
    since: Timestamp,
    caller: Caller,
}

/// Who runs the wait, as far as a snapshot has told.
enum Caller {
    /// No snapshot has been read yet; the next poll tries again.
    Unresolved,
    /// A person's shell, which has no identity: its wait reads nothing it
    /// did not read before.
    Person,
    Agent(CallerIdentity),
}

/// Prompts queued for a wait's caller: how many, and their distinct senders
/// in queue order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Announcement {
    pub count: usize,
    pub senders: Vec<MessageSender>,
}

impl WaitCaller {
    /// Stamp the wait's start; call it at the wait's entry, before any target
    /// or queue read, so a prompt queued during those reads still counts. The
    /// caller itself resolves at the first poll whose snapshot read succeeds.
    pub fn starting_now() -> Self {
        Self {
            since: Timestamp::now(),
            caller: Caller::Unresolved,
        }
    }

    /// The prompts a person or an agent queued for the caller's live card that
    /// this wait announces: those `MessageRecord::announces_to` the card,
    /// not announced before this wait started. Stamps the unannounced ones
    /// under the queue lock; writes nothing when there are none or all are
    /// already stamped. A read or write error skips this poll's check, since
    /// the prompts still arrive at the caller's turn end.
    pub fn queued(&mut self, store: &Store, session_name: &str) -> Option<Announcement> {
        let skip = |error: &dyn std::fmt::Display| {
            tracing::debug!(%error, "skipped the wait's queued-message check");
        };
        if matches!(self.caller, Caller::Person) {
            return None;
        }
        let snapshot = store.snapshot_cached().inspect_err(|e| skip(e)).ok()?;
        if matches!(self.caller, Caller::Unresolved) {
            self.caller = crate::harness::ancestry::resolve_caller(&snapshot.agents)
                .map_or(Caller::Person, Caller::Agent);
        }
        let Caller::Agent(caller) = &self.caller else {
            return None;
        };
        let card = crate::harness::ancestry::resolve_launch_caller(&snapshot.agents, caller)
            .ok()
            .filter(|card| card.ended_at.is_none())?;
        let now = Timestamp::now();
        let candidates = store
            .list_messages()
            .inspect_err(|e| skip(e))
            .ok()?
            .into_iter()
            .filter(|message| self.announces(message, card, now))
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return None;
        }
        let announced = if candidates
            .iter()
            .any(|message| message.announced_at.is_none())
        {
            store
                .queue_announced(card, self.since, session_name)
                .inspect_err(|e| skip(e))
                .ok()?
        } else {
            candidates
        };
        Announcement::of(&announced)
    }

    fn announces(&self, message: &MessageRecord, card: &AgentState, now: Timestamp) -> bool {
        message.announces_to(card, now) && message.announced_at.is_none_or(|at| at >= self.since)
    }
}

impl Announcement {
    fn of(messages: &[MessageRecord]) -> Option<Self> {
        let mut senders = Vec::new();
        for message in messages {
            if !senders.contains(&message.sender) {
                senders.push(message.sender.clone());
            }
        }
        (!messages.is_empty()).then_some(Self {
            count: messages.len(),
            senders,
        })
    }
}
