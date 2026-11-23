//! The steer-only smart-compaction reminder.

use crate::Store;
use crate::ids::PaneId;
use crate::store::message::MessageRecord;
use crate::store::writer::DeliveryFailureDisposition;
use crate::workspace::ResolvedWorkspace;

use super::deliver::{self, DeliveryPolicy, DeliveryReport, Result};

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
