//! The steer-only smart-compaction reminder and RimZ-enforced auto-compact ceiling steer.

use crate::Store;
use crate::agents::AgentState;
use crate::ids::{MessageId, PaneId};
use crate::store::message::{
    DeliveryGate, MessageBody, MessageRecord, MessageSender, MessageStatus, Premise,
};
use crate::store::writer::DeliveryFailureDisposition;
use crate::workspace::ResolvedWorkspace;

use super::deliver::{self, DeliveryPolicy, DeliveryReport, Result};
use super::synthetic::SyntheticMessage;

#[derive(Debug, PartialEq, Eq)]
pub enum ReminderOutcome {
    Sent,
    Missed(String),
}

/// Queue and steer the reminder on the multiplexer of `pane`, the card's pane as the caller
/// read it; a miss cancels it rather than leaving it for a boundary.
pub fn send_reminder(
    workspace: &ResolvedWorkspace,
    store: &Store,
    message: &MessageRecord,
    pane: &PaneId,
) -> Result<ReminderOutcome> {
    store.queue_message(message, &workspace.session_name)?;
    let outcome = deliver::deliver_one_report(
        workspace,
        store,
        &message.message_id,
        Some(pane.mux()),
        DeliveryPolicy::Steer { force: false },
    );
    let reason = match &outcome {
        Ok(DeliveryReport::Sent { .. }) => return Ok(ReminderOutcome::Sent),
        Ok(DeliveryReport::Refused(verdict)) => verdict.reason(),
        Ok(DeliveryReport::Stopped | DeliveryReport::InterruptWithheld { .. }) => store
            .list_messages()?
            .into_iter()
            .find(|live| live.message_id == message.message_id)
            .and_then(|live| live.last_error)
            .unwrap_or_else(|| "smart-compaction reminder was not sent".to_owned()),
        Err(error) => error.to_string(),
    };
    let result = store.record_unheld_delivery_miss(
        &message.message_id,
        DeliveryFailureDisposition::Terminal,
        &reason,
        &workspace.session_name,
    )?;
    if result.head_sent {
        return Ok(ReminderOutcome::Sent);
    }
    outcome.map(|_| ReminderOutcome::Missed(reason))
}

/// Queue a RimZ-enforced ceiling compaction command (carrying `Premise::OverCeiling`) and its
/// continue tail in one commit, then steer the command once into the open turn. Any miss
/// withdraws both records while open so neither is ever left `Queued`.
#[expect(
    clippy::too_many_arguments,
    reason = "enforced ceiling steer takes the target card, texts, threshold, and pane explicitly"
)]
pub fn send_ceiling_compact(
    workspace: &ResolvedWorkspace,
    store: &Store,
    message_id: MessageId,
    agent: &AgentState,
    command_text: String,
    continue_text: String,
    threshold: u64,
    pane: &PaneId,
) -> Result<ReminderOutcome> {
    let mut command = SyntheticMessage {
        agent,
        text: command_text,
        sender: MessageSender::System,
        gate: DeliveryGate::Done,
        premises: vec![Premise::OverCeiling { threshold }],
    }
    .record(workspace)
    .with_automated(true)
    .with_body(MessageBody::Command);
    command.message_id = message_id;
    command.compacted_context_tokens = agent.occupied_context_tokens();
    let tail =
        super::compact::continue_record(workspace, agent, &command.message_id, continue_text);
    let tail = store.queue_message_with_tail(&command, &tail, &workspace.session_name)?;
    let outcome = deliver::deliver_one_report(
        workspace,
        store,
        &command.message_id,
        Some(pane.mux()),
        DeliveryPolicy::Steer { force: false },
    );
    let reason = match &outcome {
        Ok(DeliveryReport::Sent { .. }) => return Ok(ReminderOutcome::Sent),
        Ok(DeliveryReport::Refused(verdict)) => verdict.reason(),
        Ok(DeliveryReport::Stopped | DeliveryReport::InterruptWithheld { .. }) => {
            let (live, history) = store.list_messages_and_history()?;
            let record = live
                .iter()
                .chain(&history)
                .find(|record| record.message_id == command.message_id);
            if record.is_some_and(|record| {
                matches!(
                    record.status,
                    MessageStatus::Sent | MessageStatus::Delivered
                )
            }) {
                return Ok(ReminderOutcome::Sent);
            }
            record
                .and_then(|record| record.last_error.clone())
                .unwrap_or_else(|| "auto-compact ceiling steer was not sent".to_owned())
        }
        Err(error) => error.to_string(),
    };
    let sent = store
        .list_messages()?
        .iter()
        .any(|live| live.message_id == command.message_id && live.status == MessageStatus::Sent);
    if sent {
        return Ok(ReminderOutcome::Sent);
    }
    store.withdraw_messages(&workspace.session_name, |live| {
        (live.message_id == command.message_id || live.message_id == tail.message_id)
            .then(|| reason.clone())
    })?;
    outcome.map(|_| ReminderOutcome::Missed(reason))
}
