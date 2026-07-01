use std::time::Duration;

use jiff::Timestamp;

use crate::ids::{AgentKind, AgentSessionId, MessageId};
use crate::message::{
    MAX_DELIVERY_ATTEMPTS, MessageBody, MessageRecord, MessageStatus, claim_expired,
    queue_head_for_message,
};
use crate::schema::event::{EventEnvelope, MessageEventMethod};

use super::super::{Ledger, Result, event_log, lock, message_store};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub requeued: usize,
    pub timed_out: usize,
}

impl Ledger {
    pub fn list_messages(&self) -> Result<Vec<MessageRecord>> {
        Ok(message_store::list(&self.inner.paths.messages_dir)?)
    }

    pub fn list_pending_messages(&self) -> Result<Vec<MessageRecord>> {
        Ok(message_store::list_pending(&self.inner.paths.messages_dir)?)
    }

    fn finalize_message_locked(
        &self,
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
        message_store::remove(&self.inner.paths.messages_dir, &message.message_id)?;
        event_log::append(&self.inner.paths.events_log, &event)?;
        Ok((message, event))
    }

    fn finalize_matching_messages_locked(
        &self,
        status: MessageStatus,
        session_name: &str,
        reason: &str,
        mut select: impl FnMut(&MessageRecord) -> bool,
        mut update: impl FnMut(&mut MessageRecord),
    ) -> Result<(Vec<MessageRecord>, Vec<EventEnvelope>)> {
        debug_assert!(status.is_terminal());
        let now = Timestamp::now();
        let method = MessageEventMethod::for_terminal_status(status)
            .expect("finalize_matching_messages_locked only accepts terminal message statuses");
        let mut messages = message_store::list(&self.inner.paths.messages_dir)?;
        let mut removed_ids = std::collections::BTreeSet::new();
        let mut finalized = Vec::new();
        let mut events = Vec::new();
        for message in &mut messages {
            if !select(message) {
                continue;
            }
            update(message);
            message.status = status;
            message.updated_at = now;
            if status == MessageStatus::Delivered {
                message.delivered_at = Some(now);
            }
            removed_ids.insert(message.message_id.to_string());
            finalized.push(message.clone());
            events.push(EventEnvelope::message_event(
                message,
                session_name,
                method,
                Some(reason),
            ));
        }
        if finalized.is_empty() {
            return Ok((finalized, events));
        }
        messages.retain(|message| !removed_ids.contains(message.message_id.as_str()));
        message_store::replace_all(&self.inner.paths.messages_dir, &messages)?;
        for event in &events {
            event_log::append(&self.inner.paths.events_log, event)?;
        }
        Ok((finalized, events))
    }

    #[must_use = "durability barrier; check the result"]
    pub fn queue_message(&self, message: &MessageRecord, session_name: &str) -> Result<()> {
        let event =
            EventEnvelope::message_event(message, session_name, MessageEventMethod::Queued, None);
        {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            message_store::write(&self.inner.paths.messages_dir, message)?;
            event_log::append(&self.inner.paths.events_log, &event)?;
        }
        self.wake_sidebars_for_event_best_effort(&event);
        self.publish_snapshot_best_effort();
        Ok(())
    }

    #[must_use = "durability barrier; check the result"]
    pub fn record_sent_message(
        &self,
        message: &MessageRecord,
        session_name: &str,
    ) -> Result<Option<MessageRecord>> {
        let (sent, event) = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            let mut message =
                match message_store::load(&self.inner.paths.messages_dir, &message.message_id) {
                    Ok(existing)
                        if matches!(
                            existing.status,
                            MessageStatus::Created
                                | MessageStatus::Queued
                                | MessageStatus::Claimed
                                | MessageStatus::Sent
                        ) =>
                    {
                        let mut existing = existing;
                        existing.pane_id = message.pane_id.clone();
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
            message_store::write(&self.inner.paths.messages_dir, &message)?;
            let event = EventEnvelope::message_event(
                &message,
                session_name,
                MessageEventMethod::Sent,
                None,
            );
            event_log::append(&self.inner.paths.events_log, &event)?;
            (message, event)
        };
        self.wake_sidebars_for_event_best_effort(&event);
        self.publish_snapshot_best_effort();
        Ok(Some(sent))
    }

    #[must_use = "durability barrier; check the result"]
    pub fn claim_message_for_delivery(
        &self,
        message_id: &MessageId,
        now: Timestamp,
    ) -> Result<Option<MessageRecord>> {
        let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
        let queued = message_store::list_pending(&self.inner.paths.messages_dir)?;
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
        let mut claimed = message.clone();
        claimed.status = MessageStatus::Claimed;
        claimed.attempts = claimed.attempts.saturating_add(1);
        claimed.last_attempt_at = Some(now);
        claimed.last_error = None;
        claimed.retry_after = None;
        claimed.updated_at = now;
        message_store::write(&self.inner.paths.messages_dir, &claimed)?;
        Ok(Some(claimed))
    }

    #[must_use = "durability barrier; check the result"]
    pub fn defer_message_wake(&self, message_id: &MessageId, until: Timestamp) -> Result<()> {
        let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
        let mut message = match message_store::load(&self.inner.paths.messages_dir, message_id) {
            Ok(message) if message.status == MessageStatus::Queued => message,
            Ok(_) | Err(message_store::MessageStoreErr::NotFound(_)) => return Ok(()),
            Err(err) => return Err(err.into()),
        };
        message.retry_after = Some(until);
        message_store::write(&self.inner.paths.messages_dir, &message)?;
        Ok(())
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
        let (settled, event) = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            let message = match message_store::load(&self.inner.paths.messages_dir, message_id) {
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
            self.finalize_message_locked(message, status, session_name, reason, now)?
        };
        self.wake_sidebars_for_event_best_effort(&event);
        self.publish_snapshot_best_effort();
        Ok(Some(settled))
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
        let outcome = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            let Some(message) = message_store::list(&self.inner.paths.messages_dir)?
                .into_iter()
                .filter(|message| {
                    message.status == MessageStatus::Sent
                        && message.body == body
                        && message.same_card(kind, agent_id, agent_name)
                })
                .min_by(|a, b| a.message_id.as_str().cmp(b.message_id.as_str()))
            else {
                return Ok(None);
            };
            let now = Timestamp::now();
            Some(self.finalize_message_locked(
                message,
                MessageStatus::Delivered,
                session_name,
                None,
                now,
            )?)
        };
        let Some((message, event)) = outcome else {
            return Ok(None);
        };
        self.wake_sidebars_for_event_best_effort(&event);
        self.publish_snapshot_best_effort();
        Ok(Some(message))
    }

    #[must_use = "durability barrier; check the result"]
    pub fn mark_message_timed_out(
        &self,
        message_id: &MessageId,
        session_name: &str,
        reason: Option<&str>,
    ) -> Result<Option<MessageRecord>> {
        let outcome = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            let mut message = match message_store::load(&self.inner.paths.messages_dir, message_id)
            {
                Ok(message) if message.status == MessageStatus::Sent => message,
                Ok(_) | Err(message_store::MessageStoreErr::NotFound(_)) => return Ok(None),
                Err(err) => return Err(err.into()),
            };
            let now = Timestamp::now();
            let reason = reason.unwrap_or("delivery window elapsed");
            message.last_error = Some(reason.to_owned());
            Some(self.finalize_message_locked(
                message,
                MessageStatus::TimedOut,
                session_name,
                Some(reason),
                now,
            )?)
        };
        let Some((message, event)) = outcome else {
            return Ok(None);
        };
        self.wake_sidebars_for_event_best_effort(&event);
        self.publish_snapshot_best_effort();
        Ok(Some(message))
    }

    #[must_use = "durability barrier; check the result"]
    pub fn reconcile_stale_sent_messages(
        &self,
        session_name: &str,
        now: Timestamp,
        window: Duration,
        max_attempts: u32,
    ) -> Result<ReconcileReport> {
        let mut report = ReconcileReport::default();
        let mut events = Vec::new();
        {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            let mut messages = message_store::list(&self.inner.paths.messages_dir)?;
            let mut removed_ids = std::collections::BTreeSet::new();
            for message in &mut messages {
                if message.status != MessageStatus::Sent {
                    continue;
                }
                let age = now.duration_since(message.updated_at);
                if !age.is_negative() && age.as_millis() < window.as_millis() as i128 {
                    continue;
                }
                let (method, reason) = if message.unconfirmed_sends < max_attempts {
                    message.status = MessageStatus::Queued;
                    message.pane_id = None;
                    message.unconfirmed_sends = message.unconfirmed_sends.saturating_add(1);
                    message.last_attempt_at = None;
                    message.retry_after = None;
                    message.last_error = Some("delivery unconfirmed; re-queued".to_owned());
                    report.requeued += 1;
                    (MessageEventMethod::Queued, "reconcile")
                } else {
                    message.status = MessageStatus::TimedOut;
                    message.last_error = Some(format!(
                        "delivery unconfirmed after {max_attempts} unconfirmed sends"
                    ));
                    removed_ids.insert(message.message_id.to_string());
                    report.timed_out += 1;
                    (MessageEventMethod::TimedOut, "reconcile")
                };
                message.updated_at = now;
                let event =
                    EventEnvelope::message_event(message, session_name, method, Some(reason));
                events.push(event);
            }
            if !events.is_empty() {
                messages.retain(|message| !removed_ids.contains(message.message_id.as_str()));
                message_store::replace_all(&self.inner.paths.messages_dir, &messages)?;
                for event in &events {
                    event_log::append(&self.inner.paths.events_log, event)?;
                }
            }
        }
        for event in &events {
            self.wake_sidebars_for_event_best_effort(event);
        }
        if !events.is_empty() {
            self.publish_snapshot_forced();
        }
        Ok(report)
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
        let (errored, event) = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            let mut message =
                match message_store::load(&self.inner.paths.messages_dir, &message.message_id) {
                    Ok(existing)
                        if matches!(
                            existing.status,
                            MessageStatus::Created | MessageStatus::Queued | MessageStatus::Claimed
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
            self.finalize_message_locked(
                message,
                MessageStatus::Errored,
                session_name,
                Some(error),
                Timestamp::now(),
            )?
        };
        self.wake_sidebars_for_event_best_effort(&event);
        self.publish_snapshot_best_effort();
        Ok(Some(errored))
    }

    #[must_use = "durability barrier; check the result"]
    pub fn record_message_delivery_failure(
        &self,
        message_id: &MessageId,
        error: &str,
        session_name: &str,
    ) -> Result<Option<MessageRecord>> {
        let outcome = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            let mut message = match message_store::load(&self.inner.paths.messages_dir, message_id)
            {
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
            message.updated_at = Timestamp::now();
            if message.attempts >= MAX_DELIVERY_ATTEMPTS {
                let (message, event) = self.finalize_message_locked(
                    message,
                    MessageStatus::Abandoned,
                    session_name,
                    Some(error),
                    Timestamp::now(),
                )?;
                Some((message, Some(event)))
            } else {
                message.status = MessageStatus::Queued;
                message_store::write(&self.inner.paths.messages_dir, &message)?;
                Some((message, None))
            }
        };
        let Some((message, terminal)) = outcome else {
            return Ok(None);
        };
        if let Some(event) = terminal.as_ref() {
            self.wake_sidebars_for_event_best_effort(event);
            self.publish_snapshot_best_effort();
        }
        Ok(Some(message))
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
    ) -> Result<usize> {
        let (removed, events) = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            self.finalize_matching_messages_locked(
                MessageStatus::Removed,
                session_name,
                "clear",
                |message| message.status.is_open() && message.same_card(kind, agent_id, agent_name),
                |_| {},
            )?
        };
        for event in &events {
            self.wake_sidebars_for_event_best_effort(event);
        }
        if !removed.is_empty() {
            self.publish_snapshot_best_effort();
        }
        Ok(removed.len())
    }

    #[must_use = "durability barrier; check the result"]
    pub fn archive_orphan_messages(&self, session_name: &str) -> Result<usize> {
        let snapshot = self.snapshot()?;
        let live_agents = snapshot.agents;
        let (archived, events) = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            self.finalize_matching_messages_locked(
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
            )?
        };
        for event in &events {
            self.wake_sidebars_for_event_best_effort(event);
        }
        if !archived.is_empty() {
            self.publish_snapshot_forced();
        }
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
        let (archived, events) = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            self.finalize_matching_messages_locked(
                MessageStatus::Archived,
                session_name,
                reason,
                |message| message.status.is_open() && message.same_card(kind, agent_id, agent_name),
                |message| message.last_error = Some(reason.to_owned()),
            )?
        };
        for event in &events {
            self.wake_sidebars_for_event_best_effort(event);
        }
        if !archived.is_empty() {
            self.publish_snapshot_best_effort();
        }
        Ok(archived.len())
    }

    #[must_use = "durability barrier; check the result"]
    pub fn archive_channel_messages(
        &self,
        channel: &str,
        reason: &str,
        session_name: &str,
    ) -> Result<usize> {
        let (archived, events) = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            self.finalize_matching_messages_locked(
                MessageStatus::Archived,
                session_name,
                reason,
                |message| message.status.is_open() && message.channel.as_deref() == Some(channel),
                |message| message.last_error = Some(reason.to_owned()),
            )?
        };
        for event in &events {
            self.wake_sidebars_for_event_best_effort(event);
        }
        if !archived.is_empty() {
            self.publish_snapshot_best_effort();
        }
        Ok(archived.len())
    }
}

#[cfg(test)]
mod tests;
