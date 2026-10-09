//! Sender resolution and prose for undelivered and long-queued messages.

use std::time::Duration;

use super::{DeliveryGate, HarnessNotice, MessageRecord, MessageSender};
use crate::agents::AgentState;
use crate::ids::WorkspaceId;

pub(crate) fn resolve_sender<'a>(
    sender: &MessageSender,
    agents: &'a [AgentState],
) -> Option<&'a AgentState> {
    let MessageSender::Agent {
        kind,
        agent_id,
        name,
        channel,
        ..
    } = sender
    else {
        return None;
    };
    let mut cards = agents.iter().filter(|agent| {
        agent.ended_at.is_none()
            && &agent.kind == kind
            && match agent_id {
                Some(id) => &agent.agent_id == id || agent.launch_id.as_ref() == Some(id),
                None => name.is_some() && &agent.name == name,
            }
    });
    let first = cards.next()?;
    let Some(second) = cards.next() else {
        return Some(first);
    };
    // Team roles repeat a name across lanes, so only the recorded channel tells these apart.
    let channel = channel.as_ref()?;
    [first, second]
        .into_iter()
        .chain(cards)
        .find(|agent| agent.channel().as_ref() == Some(channel))
}

fn receiver_label(record: &MessageRecord) -> String {
    record.address.clone().unwrap_or_else(|| {
        record
            .agent_name
            .as_ref()
            .map_or_else(|| record.agent_id.to_string(), |name| format!("@{name}"))
    })
}

/// `close_reason` is the reason the closing transition gave; an `Archived` or `Expired` record
/// already carries it as `last_error`.
pub(crate) fn undelivered(record: &MessageRecord, close_reason: Option<&str>) -> String {
    let receiver = receiver_label(record);
    let status = if record.status == super::MessageStatus::TimedOut {
        "timed out"
    } else {
        record.status.as_str()
    };
    let reason = match record.status {
        super::MessageStatus::Archived | super::MessageStatus::Expired => {
            record.last_error.as_deref()
        }
        _ => close_reason,
    }
    .map_or_else(String::new, |reason| format!("Reason: {reason}\n"));
    let last_write = record
        .last_sent_at
        .map_or_else(String::new, |at| format!("; last write {at}"));
    let preview: String = record
        .text
        .chars()
        .take(80)
        .map(|ch| if ch.is_whitespace() { ' ' } else { ch })
        .collect();
    format!(
        "Message {} to {receiver} ended {status}.\n{reason}Tried: {} claims; {} unconfirmed writes{last_write}.\nText: {preview}\nThe text was not delivered. Check the receiver with `rimz agents show {receiver}` and decide whether to resend.",
        record.message_id, record.attempts, record.unconfirmed_sends,
    )
}

pub(crate) fn still_queued(record: &MessageRecord, cause: &str, waited: Duration) -> String {
    let receiver = receiver_label(record);
    format!(
        "Message {} to {receiver} has waited {}: {cause}.\nIt stays queued and will deliver at the receiver's next boundary. Withdraw it with `rimz message cancel {}`.",
        record.message_id,
        super::format_dwell(waited.as_secs()),
        record.message_id,
    )
}

pub(crate) fn compose(
    workspace_id: WorkspaceId,
    sender_card: &AgentState,
    notice: HarnessNotice,
    text: String,
) -> MessageRecord {
    MessageRecord::new(workspace_id, sender_card, text, DeliveryGate::Done)
        .with_channel(sender_card.channel())
        .with_sender(MessageSender::Harness { notice })
}
