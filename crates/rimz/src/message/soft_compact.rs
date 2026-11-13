//! Steer-only wrap-ups and queue-tail continues for a soft-compaction cycle.

use crate::Store;
use crate::ids::PaneId;
use crate::store::message::MessageRecord;
use crate::store::writer::DeliveryFailureDisposition;
use crate::workspace::ResolvedWorkspace;

use super::deliver::{self, DeliveryPolicy, DeliveryReport, Result};
use super::synthetic;

#[derive(Debug, PartialEq, Eq)]
pub enum WrapUpOutcome {
    Sent,
    Missed(String),
}

/// Queue and steer the wrap-up on the multiplexer of `pane`, the card's pane as the caller read it.
pub fn send_wrap_up(
    workspace: &ResolvedWorkspace,
    store: &Store,
    message: &MessageRecord,
    pane: &PaneId,
) -> Result<WrapUpOutcome> {
    store.queue_message(message, &workspace.session_name)?;
    let outcome = deliver::deliver_one_report(
        workspace,
        store,
        &message.message_id,
        Some(pane.mux()),
        DeliveryPolicy::Steer { force: false },
    );
    let reason = match &outcome {
        Ok(DeliveryReport::Sent { .. }) => return Ok(WrapUpOutcome::Sent),
        Ok(DeliveryReport::Refused(verdict)) => verdict.reason(),
        Ok(DeliveryReport::Stopped) => store
            .list_messages()?
            .into_iter()
            .find(|live| live.message_id == message.message_id)
            .and_then(|live| live.last_error)
            .unwrap_or_else(|| "soft-compaction steer was not sent".to_owned()),
        Err(error) => error.to_string(),
    };
    let result = store.record_unheld_delivery_miss(
        &message.message_id,
        DeliveryFailureDisposition::Terminal,
        &reason,
        &workspace.session_name,
    )?;
    if result.head_sent {
        return Ok(WrapUpOutcome::Sent);
    }
    outcome.map(|_| WrapUpOutcome::Missed(reason))
}

/// Queue the continue at the card's tail and attempt it on the multiplexer of `pane`.
pub fn send_continue(
    workspace: &ResolvedWorkspace,
    store: &Store,
    message: &MessageRecord,
    pane: &PaneId,
) -> Result<()> {
    let queued = store.queue_tail_message(message, &workspace.session_name)?;
    synthetic::attempt_now(
        workspace,
        store,
        &queued.message_id,
        Some(pane),
        DeliveryFailureDisposition::Retry,
        "soft-compaction continue delivery gate closed",
    )?;
    Ok(())
}
