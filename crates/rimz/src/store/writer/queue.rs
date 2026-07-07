use std::time::Duration;

use jiff::Timestamp;

use crate::ids::{AgentKind, AgentSessionId, MessageId};
use crate::message::{
    AutoCompact, DeliveryGate, MAX_DELIVERY_ATTEMPTS, MessageBody, MessageRecord, MessageStatus,
    claim_expired, queue_head_for_message,
};
use crate::store::event::{EventEnvelope, MessageEventMethod};

use super::super::{Result, Store, UnresolvedMessage, message_store};
use super::{PublishPolicy, Txn};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub requeued: usize,
    pub timed_out: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MessageEdit {
    pub text: Option<String>,
    pub gate: Option<DeliveryGate>,
    pub not_before: Option<Option<Timestamp>>,
    pub force: Option<bool>,
    pub enter: Option<bool>,
    pub auto_compact: Option<Option<AutoCompact>>,
}

impl MessageEdit {
    pub fn changed_fields(&self) -> Vec<&'static str> {
        let mut fields = Vec::new();
        if self.text.is_some() {
            fields.push("text");
        }
        if self.gate.is_some() {
            fields.push("gate");
        }
        if self.not_before.is_some() {
            fields.push("schedule");
        }
        if self.force.is_some() {
            fields.push("force");
        }
        if self.enter.is_some() {
            fields.push("enter");
        }
        if self.auto_compact.is_some() {
            fields.push("smart_compact");
        }
        fields
    }

    pub fn is_empty(&self) -> bool {
        self.changed_fields().is_empty()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum EditOutcome {
    Edited(Box<MessageRecord>),
    NotOpen(MessageStatus),
    NotFound,
}

enum MessageUpdate {
    Keep,
    Rewrite {
        method: MessageEventMethod,
        reason: Option<String>,
    },
    Finalize {
        status: MessageStatus,
        reason: Option<String>,
    },
}

fn claim_message_locked(
    txn: &mut Txn<'_>,
    message: &MessageRecord,
    now: Timestamp,
) -> Result<Option<MessageRecord>> {
    let mut claimed = message.clone();
    claimed.status = MessageStatus::Claimed;
    claimed.attempts = claimed.attempts.saturating_add(1);
    claimed.last_attempt_at = Some(now);
    claimed.last_error = None;
    claimed.retry_after = None;
    claimed.updated_at = now;
    message_store::write(&txn.paths.messages_dir, &claimed)?;
    Ok(Some(claimed))
}

impl Store {
    pub fn list_messages(&self) -> Result<Vec<MessageRecord>> {
        Ok(message_store::list(&self.inner.paths.messages_dir)?)
    }

    pub fn list_message_history(&self) -> Result<Vec<MessageRecord>> {
        Ok(message_store::list_history(&self.inner.paths.messages_dir)?)
    }

    pub fn list_pending_messages(&self) -> Result<Vec<MessageRecord>> {
        Ok(message_store::list_pending(&self.inner.paths.messages_dir)?)
    }

    fn finalize_message_locked(
        &self,
        txn: &mut Txn<'_>,
        mut message: MessageRecord,
        status: MessageStatus,
        session_name: &str,
        reason: Option<&str>,
        now: Timestamp,
    ) -> Result<(MessageRecord, EventEnvelope)> {
        debug_assert!(status.is_terminal());
        message.status = status;
        message.updated_at = now;
        if status == MessageStatus::Delivered {
            message.delivered_at = Some(now);
        }
        let method = MessageEventMethod::for_terminal_status(status)
            .expect("finalize_message_locked only accepts terminal message statuses");
        let event = EventEnvelope::message_event(&message, session_name, method, reason);
        message_store::append_history(&txn.paths.messages_dir, &message)?;
        message_store::remove(&txn.paths.messages_dir, &message.message_id)?;
        txn.append(&event)?;
        Ok((message, event))
    }

    fn update_messages_locked(
        &self,
        txn: &mut Txn<'_>,
        session_name: &str,
        now: Timestamp,
        mut update: impl FnMut(&mut MessageRecord) -> MessageUpdate,
    ) -> Result<Vec<MessageRecord>> {
        let mut messages = message_store::list(&txn.paths.messages_dir)?;
        let mut removed_ids = std::collections::BTreeSet::new();
        let mut updated = Vec::new();
        let mut history = Vec::new();
        let mut events = Vec::new();
        for message in &mut messages {
            match update(message) {
                MessageUpdate::Keep => {}
                MessageUpdate::Rewrite { method, reason } => {
                    updated.push(message.clone());
                    events.push(EventEnvelope::message_event(
                        message,
                        session_name,
                        method,
                        reason.as_deref(),
                    ));
                }
                MessageUpdate::Finalize { status, reason } => {
                    debug_assert!(status.is_terminal());
                    message.status = status;
                    message.updated_at = now;
                    if status == MessageStatus::Delivered {
                        message.delivered_at = Some(now);
                    }
                    let method = MessageEventMethod::for_terminal_status(status)
                        .expect("MessageUpdate::Finalize only accepts terminal statuses");
                    removed_ids.insert(message.message_id.to_string());
                    updated.push(message.clone());
                    history.push(message.clone());
                    events.push(EventEnvelope::message_event(
                        message,
                        session_name,
                        method,
                        reason.as_deref(),
                    ));
                }
            }
        }
        if updated.is_empty() {
            return Ok(updated);
        }
        for message in &history {
            message_store::append_history(&txn.paths.messages_dir, message)?;
        }
        messages.retain(|message| !removed_ids.contains(message.message_id.as_str()));
        message_store::replace_all(&txn.paths.messages_dir, &messages)?;
        for event in &events {
            txn.append(event)?;
        }
        Ok(updated)
    }

    fn finalize_matching_messages_locked(
        &self,
        txn: &mut Txn<'_>,
        status: MessageStatus,
        session_name: &str,
        reason: &str,
        mut select: impl FnMut(&MessageRecord) -> bool,
        mut update: impl FnMut(&mut MessageRecord),
    ) -> Result<Vec<MessageRecord>> {
        debug_assert!(status.is_terminal());
        let now = Timestamp::now();
        self.update_messages_locked(txn, session_name, now, |message| {
            if !select(message) {
                return MessageUpdate::Keep;
            }
            update(message);
            MessageUpdate::Finalize {
                status,
                reason: Some(reason.to_owned()),
            }
        })
    }

    #[must_use = "durability barrier; check the result"]
    pub fn queue_message(&self, message: &MessageRecord, session_name: &str) -> Result<()> {
        let event =
            EventEnvelope::message_event(message, session_name, MessageEventMethod::Queued, None);
        self.commit(PublishPolicy::Skip, |txn| {
            message_store::write(&txn.paths.messages_dir, message)?;
            txn.append(&event)
        })
    }

    #[must_use = "durability barrier; check the result"]
    pub fn edit_message(
        &self,
        message_id: &MessageId,
        edit: MessageEdit,
        session_name: &str,
    ) -> Result<EditOutcome> {
        self.commit(PublishPolicy::Skip, |txn| {
            let mut message = match message_store::load(&txn.paths.messages_dir, message_id) {
                Ok(message) => message,
                Err(message_store::MessageStoreErr::NotFound(_)) => {
                    return Ok(message_store::list_history(&txn.paths.messages_dir)?
                        .into_iter()
                        .find(|message| message.message_id == *message_id)
                        .map_or(EditOutcome::NotFound, |message| {
                            EditOutcome::NotOpen(message.status)
                        }));
                }
                Err(err) => return Err(err.into()),
            };
            if message.status != MessageStatus::Queued {
                return Ok(EditOutcome::NotOpen(message.status));
            }
            let fields = edit.changed_fields();
            if let Some(text) = edit.text {
                message.text = text;
            }
            if let Some(gate) = edit.gate {
                message.gate = gate;
            }
            if let Some(not_before) = edit.not_before {
                message.not_before = not_before;
            }
            if let Some(force) = edit.force {
                message.force = force;
            }
            if let Some(enter) = edit.enter {
                message.enter = enter;
            }
            if let Some(auto_compact) = edit.auto_compact {
                message.auto_compact = auto_compact;
                message.compacted_context_tokens = None;
            }
            message.retry_after = None;
            message.updated_at = Timestamp::now();
            message_store::write(&txn.paths.messages_dir, &message)?;
            let reason = fields.join(", ");
            let event = EventEnvelope::message_event(
                &message,
                session_name,
                MessageEventMethod::Edited,
                Some(&reason),
            );
            txn.append(&event)?;
            Ok(EditOutcome::Edited(Box::new(message)))
        })
    }

    #[must_use = "durability barrier; check the result"]
    pub fn record_sent_message(
        &self,
        message: &MessageRecord,
        session_name: &str,
    ) -> Result<Option<MessageRecord>> {
        self.commit(PublishPolicy::Skip, |txn| {
            let mut message =
                match message_store::load(&txn.paths.messages_dir, &message.message_id) {
                    Ok(existing)
                        if matches!(
                            existing.status,
                            MessageStatus::Queued | MessageStatus::Claimed | MessageStatus::Sent
                        ) =>
                    {
                        let mut existing = existing;
                        existing.pane_id = message.pane_id.clone();
                        existing.batch_id = message.batch_id.clone();
                        existing
                    }
                    Ok(_) => return Ok(None),
                    Err(message_store::MessageStoreErr::NotFound(_)) => message.clone(),
                    Err(err) => return Err(err.into()),
                };
            let now = Timestamp::now();
            message.status = MessageStatus::Sent;
            message.updated_at = now;
            message.last_error = None;
            message_store::write(&txn.paths.messages_dir, &message)?;
            let event = EventEnvelope::message_event(
                &message,
                session_name,
                MessageEventMethod::Sent,
                None,
            );
            txn.append(&event)?;
            Ok(Some(message))
        })
    }

    #[must_use = "durability barrier; check the result"]
    pub fn claim_message_for_delivery(
        &self,
        message_id: &MessageId,
        now: Timestamp,
    ) -> Result<Option<MessageRecord>> {
        self.commit(PublishPolicy::Skip, |txn| {
            let queued = message_store::list_pending(&txn.paths.messages_dir)?;
            let Some(message) = queued
                .iter()
                .find(|message| message.message_id == *message_id)
            else {
                return Ok(None);
            };
            if !claim_expired(message.last_attempt_at, now) {
                return Ok(None);
            }
            let Some(head) = queue_head_for_message(queued.iter(), message, now) else {
                return Ok(None);
            };
            if head.message_id != *message_id {
                return Ok(None);
            }
            claim_message_locked(txn, message, now)
        })
    }

    #[must_use = "durability barrier; check the result"]
    pub fn claim_message_for_steer(
        &self,
        message_id: &MessageId,
        now: Timestamp,
    ) -> Result<Option<MessageRecord>> {
        self.commit(PublishPolicy::Skip, |txn| {
            let queued = message_store::list_pending(&txn.paths.messages_dir)?;
            let Some(message) = queued
                .iter()
                .find(|message| message.message_id == *message_id)
            else {
                return Ok(None);
            };
            if !claim_expired(message.last_attempt_at, now) {
                return Ok(None);
            }
            claim_message_locked(txn, message, now)
        })
    }

    #[must_use = "durability barrier; check the result"]
    pub fn defer_message_wake(&self, message_id: &MessageId, until: Timestamp) -> Result<()> {
        self.commit(PublishPolicy::Skip, |txn| {
            let mut message = match message_store::load(&txn.paths.messages_dir, message_id) {
                Ok(message) if message.status == MessageStatus::Queued => message,
                Ok(_) | Err(message_store::MessageStoreErr::NotFound(_)) => return Ok(()),
                Err(err) => return Err(err.into()),
            };
            message.retry_after = Some(until);
            message_store::write(&txn.paths.messages_dir, &message)?;
            Ok(())
        })
    }

    #[must_use = "durability barrier; check the result"]
    pub fn settle_message(
        &self,
        message_id: &MessageId,
        status: MessageStatus,
        session_name: &str,
        reason: Option<&str>,
    ) -> Result<Option<MessageRecord>> {
        debug_assert!(status.is_terminal());
        self.commit(PublishPolicy::Skip, |txn| {
            let message = match message_store::load(&txn.paths.messages_dir, message_id) {
                Ok(message)
                    if matches!(
                        message.status,
                        MessageStatus::Queued | MessageStatus::Claimed | MessageStatus::Sent
                    ) =>
                {
                    message
                }
                Ok(_) | Err(message_store::MessageStoreErr::NotFound(_)) => return Ok(None),
                Err(err) => return Err(err.into()),
            };
            let now = Timestamp::now();
            let (settled, _event) =
                self.finalize_message_locked(txn, message, status, session_name, reason, now)?;
            Ok(Some(settled))
        })
    }

    #[must_use = "durability barrier; check the result"]
    pub fn confirm_delivered_for_card(
        &self,
        kind: &AgentKind,
        agent_id: &AgentSessionId,
        agent_name: Option<&str>,
        body: MessageBody,
        session_name: &str,
    ) -> Result<Option<MessageRecord>> {
        self.commit(PublishPolicy::Skip, |txn| {
            let messages = message_store::list(&txn.paths.messages_dir)?;
            let Some(oldest) = messages
                .iter()
                .filter(|message| {
                    message.status == MessageStatus::Sent
                        && message.body == body
                        && message.same_card(kind, agent_id, agent_name)
                })
                .min_by(|a, b| a.message_id.as_str().cmp(b.message_id.as_str()))
                .cloned()
            else {
                return Ok(None);
            };
            let now = Timestamp::now();
            let delivered = self.update_messages_locked(txn, session_name, now, |message| {
                let selected = if message.message_id == oldest.message_id {
                    true
                } else {
                    oldest.batch_id.is_some()
                        && message.status == MessageStatus::Sent
                        && message.body == body
                        && message.same_card(kind, agent_id, agent_name)
                        && message.batch_id == oldest.batch_id
                };
                if !selected {
                    return MessageUpdate::Keep;
                }
                MessageUpdate::Finalize {
                    status: MessageStatus::Delivered,
                    reason: None,
                }
            })?;
            Ok(delivered
                .into_iter()
                .find(|message| message.message_id == oldest.message_id))
        })
    }

    #[must_use = "durability barrier; check the result"]
    pub fn mark_message_timed_out(
        &self,
        message_id: &MessageId,
        session_name: &str,
        reason: Option<&str>,
    ) -> Result<Option<MessageRecord>> {
        self.commit(PublishPolicy::Skip, |txn| {
            let mut message = match message_store::load(&txn.paths.messages_dir, message_id) {
                Ok(message) if message.status == MessageStatus::Sent => message,
                Ok(_) | Err(message_store::MessageStoreErr::NotFound(_)) => return Ok(None),
                Err(err) => return Err(err.into()),
            };
            let now = Timestamp::now();
            let reason = reason.unwrap_or("delivery window elapsed");
            message.last_error = Some(reason.to_owned());
            let (message, _event) = self.finalize_message_locked(
                txn,
                message,
                MessageStatus::TimedOut,
                session_name,
                Some(reason),
                now,
            )?;
            Ok(Some(message))
        })
    }

    #[must_use = "durability barrier; check the result"]
    pub fn reconcile_stale_sent_messages(
        &self,
        session_name: &str,
        now: Timestamp,
        window: Duration,
        max_attempts: u32,
    ) -> Result<ReconcileReport> {
        self.commit(PublishPolicy::Skip, |txn| {
            let mut report = ReconcileReport::default();
            let updated = self.update_messages_locked(txn, session_name, now, |message| {
                if message.status != MessageStatus::Sent {
                    return MessageUpdate::Keep;
                }
                let age = now.duration_since(message.updated_at);
                if !age.is_negative() && age.as_millis() < window.as_millis() as i128 {
                    return MessageUpdate::Keep;
                }
                if message.unconfirmed_sends < max_attempts {
                    message.status = MessageStatus::Queued;
                    message.pane_id = None;
                    message.batch_id = None;
                    message.unconfirmed_sends = message.unconfirmed_sends.saturating_add(1);
                    message.last_attempt_at = None;
                    message.retry_after = None;
                    message.last_error = Some("delivery unconfirmed; re-queued".to_owned());
                    report.requeued += 1;
                    message.updated_at = now;
                    MessageUpdate::Rewrite {
                        method: MessageEventMethod::Queued,
                        reason: Some("reconcile".to_owned()),
                    }
                } else {
                    message.last_error = Some(format!(
                        "delivery unconfirmed after {max_attempts} unconfirmed sends"
                    ));
                    report.timed_out += 1;
                    MessageUpdate::Finalize {
                        status: MessageStatus::TimedOut,
                        reason: Some("reconcile".to_owned()),
                    }
                }
            })?;
            if !updated.is_empty() {
                txn.set_publish(PublishPolicy::Forced);
            }
            Ok(report)
        })
    }

    pub fn earliest_message_wake(
        &self,
        now: Timestamp,
        window: Duration,
    ) -> Result<Option<Timestamp>> {
        let next = message_store::list(&self.inner.paths.messages_dir)?
            .into_iter()
            .filter_map(|message| message.wake_deadline(now, window))
            .min();
        Ok(next)
    }

    #[must_use = "durability barrier; check the result"]
    pub fn record_send_error(
        &self,
        message: &MessageRecord,
        error: &str,
        session_name: &str,
    ) -> Result<Option<MessageRecord>> {
        self.commit(PublishPolicy::Skip, |txn| {
            let mut message =
                match message_store::load(&txn.paths.messages_dir, &message.message_id) {
                    Ok(existing)
                        if matches!(
                            existing.status,
                            MessageStatus::Queued | MessageStatus::Claimed
                        ) =>
                    {
                        let mut existing = existing;
                        existing.pane_id = message.pane_id.clone();
                        existing
                    }
                    Ok(existing) if existing.status == MessageStatus::Sent => return Ok(None),
                    Ok(_) => return Ok(None),
                    Err(message_store::MessageStoreErr::NotFound(_)) => message.clone(),
                    Err(err) => return Err(err.into()),
                };
            message.last_error = Some(error.to_owned());
            let (errored, _event) = self.finalize_message_locked(
                txn,
                message,
                MessageStatus::Errored,
                session_name,
                Some(error),
                Timestamp::now(),
            )?;
            Ok(Some(errored))
        })
    }

    #[must_use = "durability barrier; check the result"]
    pub fn record_message_delivery_failure(
        &self,
        message_id: &MessageId,
        error: &str,
        session_name: &str,
    ) -> Result<Option<MessageRecord>> {
        self.commit(PublishPolicy::Skip, |txn| {
            let mut message = match message_store::load(&txn.paths.messages_dir, message_id) {
                Ok(message)
                    if matches!(
                        message.status,
                        MessageStatus::Queued | MessageStatus::Claimed
                    ) =>
                {
                    message
                }
                Ok(_) | Err(message_store::MessageStoreErr::NotFound(_)) => return Ok(None),
                Err(err) => return Err(err.into()),
            };
            message.last_error = Some(error.to_owned());
            message.pane_id = None;
            message.batch_id = None;
            message.updated_at = Timestamp::now();
            if message.attempts >= MAX_DELIVERY_ATTEMPTS {
                let (message, _event) = self.finalize_message_locked(
                    txn,
                    message,
                    MessageStatus::Abandoned,
                    session_name,
                    Some(error),
                    Timestamp::now(),
                )?;
                Ok(Some(message))
            } else {
                message.status = MessageStatus::Queued;
                message_store::write(&txn.paths.messages_dir, &message)?;
                Ok(Some(message))
            }
        })
    }

    #[must_use = "durability barrier; check the result"]
    pub fn record_unresolved_message(&self, bounce: UnresolvedMessage<'_>) -> Result<MessageId> {
        let event = EventEnvelope::unresolved_message_event(
            bounce.workspace_id,
            bounce.session_name,
            bounce.address.to_owned(),
            bounce.channel.map(ToOwned::to_owned),
            bounce.sender.clone(),
            bounce.text_len,
            bounce.reason.to_owned(),
        );
        let message_id = match event.kind() {
            crate::store::event::EventKind::Message { payload, .. } => payload.message_id,
            _ => unreachable!("unresolved_message_event is a message event"),
        };
        self.commit(PublishPolicy::Skip, |txn| txn.append(&event))?;
        Ok(message_id)
    }

    #[must_use = "durability barrier; check the result"]
    pub fn remove_message(
        &self,
        message_id: &MessageId,
        session_name: &str,
        reason: &str,
    ) -> Result<bool> {
        Ok(self
            .settle_message(
                message_id,
                MessageStatus::Removed,
                session_name,
                Some(reason),
            )?
            .is_some())
    }

    #[must_use = "durability barrier; check the result"]
    pub fn clear_messages_for(
        &self,
        kind: &AgentKind,
        agent_id: &AgentSessionId,
        agent_name: Option<&str>,
        session_name: &str,
    ) -> Result<Vec<MessageRecord>> {
        self.commit(PublishPolicy::Skip, |txn| {
            self.finalize_matching_messages_locked(
                txn,
                MessageStatus::Removed,
                session_name,
                "clear",
                |message| message.status.is_open() && message.same_card(kind, agent_id, agent_name),
                |_| {},
            )
        })
    }

    #[must_use = "durability barrier; check the result"]
    pub fn clear_channel_messages(
        &self,
        channel: &str,
        session_name: &str,
    ) -> Result<Vec<MessageRecord>> {
        self.commit(PublishPolicy::Skip, |txn| {
            self.finalize_matching_messages_locked(
                txn,
                MessageStatus::Removed,
                session_name,
                "clear",
                |message| message.status.is_open() && message.channel.as_deref() == Some(channel),
                |_| {},
            )
        })
    }

    #[must_use = "durability barrier; check the result"]
    pub fn archive_orphan_messages(&self, session_name: &str) -> Result<usize> {
        let snapshot = self.snapshot()?;
        let live_agents = snapshot.agents;
        let archived = self.commit(PublishPolicy::Skip, |txn| {
            let archived = self.finalize_matching_messages_locked(
                txn,
                MessageStatus::Archived,
                session_name,
                "receiver ended",
                |message| {
                    message.status.is_open()
                        && !live_agents
                            .iter()
                            .any(|agent| message.same_agent_card(agent))
                },
                |message| message.last_error = Some("receiver ended".to_owned()),
            )?;
            if !archived.is_empty() {
                txn.set_publish(PublishPolicy::Forced);
            }
            Ok(archived)
        })?;
        Ok(archived.len())
    }

    #[must_use = "durability barrier; check the result"]
    pub fn archive_messages_for_card(
        &self,
        kind: &AgentKind,
        agent_id: &AgentSessionId,
        agent_name: Option<&str>,
        reason: &str,
        session_name: &str,
    ) -> Result<usize> {
        let archived = self.commit(PublishPolicy::Skip, |txn| {
            self.finalize_matching_messages_locked(
                txn,
                MessageStatus::Archived,
                session_name,
                reason,
                |message| message.status.is_open() && message.same_card(kind, agent_id, agent_name),
                |message| message.last_error = Some(reason.to_owned()),
            )
        })?;
        Ok(archived.len())
    }

    #[must_use = "durability barrier; check the result"]
    pub fn archive_channel_messages(
        &self,
        channel: &str,
        reason: &str,
        session_name: &str,
    ) -> Result<usize> {
        let archived = self.commit(PublishPolicy::Skip, |txn| {
            self.finalize_matching_messages_locked(
                txn,
                MessageStatus::Archived,
                session_name,
                reason,
                |message| message.status.is_open() && message.channel.as_deref() == Some(channel),
                |message| message.last_error = Some(reason.to_owned()),
            )
        })?;
        Ok(archived.len())
    }
}

#[cfg(test)]
mod tests;
