use std::collections::BTreeSet;
#[cfg(test)]
use std::time::Duration;

use jiff::Timestamp;

use crate::agents::{AgentCardRef, AgentStatus};
use crate::ids::{AgentKind, AgentSessionId, MessageId};
#[cfg(test)]
use crate::message::CLAIM_TTL;
use crate::message::{
    AutoCompact, DeliveryGate, MAX_DELIVERY_ATTEMPTS, MessageBody, MessageRecord, MessageStatus,
    claim_expired, delivery_batch_indices,
};
use crate::store::event::{EventEnvelope, MessageEventMethod};

use super::super::{Result, Store, message_store};
use super::Txn;
use super::UnresolvedMessage;

impl MessageRecord {
    fn requeue(&mut self, now: Timestamp, error: impl Into<String>) {
        self.status = MessageStatus::Queued;
        self.pane_id = None;
        self.batch_id = None;
        self.last_error = Some(error.into());
        self.updated_at = now;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub requeued: usize,
    pub timed_out: usize,
}

pub enum DeliveryAck<'a> {
    /// A turn started. `None` means the adapter reported no usable prompt text.
    TurnStarted {
        prompt: Option<&'a str>,
    },
    Compaction,
}

fn same_submitted_batch(first: &MessageRecord, candidate: &MessageRecord) -> bool {
    if first.message_id == candidate.message_id {
        return true;
    }
    if let Some(batch_id) = first.batch_id.as_ref() {
        return candidate.batch_id.as_ref() == Some(batch_id);
    }
    first.last_sent_at.is_some()
        && candidate.batch_id.is_none()
        && candidate.last_sent_at == first.last_sent_at
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliverySweepUpdate {
    pub message_id: MessageId,
    pub after_indices: Vec<usize>,
    pub when_indices: Vec<usize>,
    pub retry_after: Option<Timestamp>,
    pub archive_reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeliveryFailureResult {
    head_found: bool,
    pub head_sent: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryFailureDisposition {
    Retry,
    Terminal,
}

impl MessageEdit {
    /// Apply requested deltas to a record. Setting `auto_compact` resets the
    /// compaction baseline so the new threshold is re-evaluated at delivery.
    pub fn apply(self, record: &mut MessageRecord) {
        if let Some(text) = self.text {
            record.text = text;
        }
        if let Some(gate) = self.gate {
            record.gate = gate;
        }
        if let Some(not_before) = self.not_before {
            record.not_before = not_before;
        }
        if let Some(force) = self.force {
            record.force = force;
        }
        if let Some(enter) = self.enter {
            record.enter = enter;
        }
        if let Some(auto_compact) = self.auto_compact {
            record.auto_compact = auto_compact;
            record.compacted_context_tokens = None;
        }
    }

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
    SilentRewrite,
    Rewrite {
        method: MessageEventMethod,
        reason: Option<String>,
    },
    Finalize {
        status: MessageStatus,
        reason: Option<String>,
    },
}

struct QueueTxn<'txn, 'paths> {
    txn: &'txn mut Txn<'paths>,
    /// Message-id order comes from `message_store::list` and is restored by
    /// `replace_all` at the transaction boundary.
    live: Vec<MessageRecord>,
    history: Vec<MessageRecord>,
    events: Vec<EventEnvelope>,
    live_changed: bool,
}

impl<'txn, 'paths> QueueTxn<'txn, 'paths> {
    fn new(txn: &'txn mut Txn<'paths>) -> Result<Self> {
        let live = message_store::list(&txn.paths.messages_dir)?;
        debug_assert!(
            live.windows(2)
                .all(|pair| pair[0].message_id.as_str() <= pair[1].message_id.as_str())
        );
        Ok(Self {
            txn,
            live,
            history: Vec::new(),
            events: Vec::new(),
            live_changed: false,
        })
    }

    fn live(&self) -> &[MessageRecord] {
        &self.live
    }

    fn get(&self, message_id: &MessageId) -> Option<MessageRecord> {
        self.live
            .iter()
            .find(|message| message.message_id == *message_id)
            .cloned()
    }

    fn upsert(&mut self, message: MessageRecord) {
        match self
            .live
            .iter_mut()
            .find(|existing| existing.message_id == message.message_id)
        {
            Some(existing) => *existing = message,
            None => self.live.push(message),
        }
        self.live_changed = true;
    }

    fn stage_event(&mut self, event: EventEnvelope) {
        self.events.push(event);
    }

    fn force_publish(&mut self) {
        self.txn.force_publish();
    }

    fn terminalize(
        &mut self,
        message: MessageRecord,
        status: MessageStatus,
        session_name: &str,
        reason: Option<&str>,
        now: Timestamp,
    ) -> MessageRecord {
        let message = normalize_terminal(message, status, now, reason);
        let method = MessageEventMethod::for_terminal_status(status)
            .expect("terminal message statuses have an event method");
        if let Some(index) = self
            .live
            .iter()
            .position(|live| live.message_id == message.message_id)
        {
            self.live.remove(index);
            self.live_changed = true;
        }
        self.history.push(message.clone());
        self.events.push(EventEnvelope::message_event(
            &message,
            session_name,
            method,
            reason,
        ));
        message
    }

    fn apply_all(
        &mut self,
        session_name: &str,
        now: Timestamp,
        mut update: impl FnMut(&mut MessageRecord) -> MessageUpdate,
    ) -> Vec<MessageRecord> {
        let mut updated = Vec::new();
        let mut live = Vec::with_capacity(self.live.len());
        for mut message in std::mem::take(&mut self.live) {
            match update(&mut message) {
                MessageUpdate::Keep => live.push(message),
                MessageUpdate::SilentRewrite => {
                    self.live_changed = true;
                    updated.push(message.clone());
                    live.push(message);
                }
                MessageUpdate::Rewrite { method, reason } => {
                    self.live_changed = true;
                    self.events.push(EventEnvelope::message_event(
                        &message,
                        session_name,
                        method,
                        reason.as_deref(),
                    ));
                    updated.push(message.clone());
                    live.push(message);
                }
                MessageUpdate::Finalize { status, reason } => {
                    self.live_changed = true;
                    let message = normalize_terminal(message, status, now, reason.as_deref());
                    let method = MessageEventMethod::for_terminal_status(status)
                        .expect("terminal message statuses have an event method");
                    self.history.push(message.clone());
                    self.events.push(EventEnvelope::message_event(
                        &message,
                        session_name,
                        method,
                        reason.as_deref(),
                    ));
                    updated.push(message);
                }
            }
        }
        self.live = live;
        updated
    }

    fn finalize_matching(
        &mut self,
        status: MessageStatus,
        session_name: &str,
        reason: &str,
        mut select: impl FnMut(&MessageRecord) -> bool,
        mut update: impl FnMut(&mut MessageRecord),
    ) -> Vec<MessageRecord> {
        debug_assert!(status.is_terminal());
        self.apply_all(session_name, Timestamp::now(), |message| {
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

    fn claim_indices(&mut self, indices: &[usize], now: Timestamp) -> Vec<MessageRecord> {
        let mut claimed = Vec::with_capacity(indices.len());
        for index in indices {
            let message = &mut self.live[*index];
            message.status = MessageStatus::Claimed;
            message.attempts = message.attempts.saturating_add(1);
            message.last_attempt_at = Some(now);
            message.last_error = None;
            message.retry_after = None;
            message.updated_at = now;
            claimed.push(message.clone());
        }
        self.live_changed |= !claimed.is_empty();
        claimed
    }

    fn finish(self) -> Result<()> {
        message_store::append_history_many(&self.txn.paths.messages_dir, &self.history)?;
        if self.live_changed {
            message_store::replace_all(&self.txn.paths.messages_dir, &self.live)?;
        }
        for event in &self.events {
            self.txn.append(event)?;
        }
        Ok(())
    }
}

fn normalize_terminal(
    mut message: MessageRecord,
    status: MessageStatus,
    now: Timestamp,
    reason: Option<&str>,
) -> MessageRecord {
    debug_assert!(status.is_terminal());
    message.status = status;
    message.updated_at = now;
    if status == MessageStatus::Archived
        && let Some(reason) = reason
    {
        message.last_error = Some(reason.to_owned());
    }
    if status == MessageStatus::Delivered {
        message.delivered_at = Some(now);
    }
    message
}

fn apply_sweep_update(
    message: &mut MessageRecord,
    update: &DeliverySweepUpdate,
    now: Timestamp,
) -> MessageUpdate {
    if let Some(reason) = update.archive_reason.as_ref() {
        return MessageUpdate::Finalize {
            status: MessageStatus::Archived,
            reason: Some(reason.clone()),
        };
    }
    let after = stamp_after_conditions(message, &update.after_indices, now);
    let when = stamp_when_conditions(message, &update.when_indices, now);
    let retry_changed = update.retry_after.is_some() && message.retry_after != update.retry_after;
    if retry_changed {
        message.retry_after = update.retry_after;
    }
    if after.is_empty() && when.is_empty() {
        return if retry_changed {
            MessageUpdate::SilentRewrite
        } else {
            MessageUpdate::Keep
        };
    }
    message.updated_at = now;
    MessageUpdate::Rewrite {
        method: if when.is_empty() {
            MessageEventMethod::AfterMet
        } else {
            MessageEventMethod::WhenMet
        },
        reason: Some(condition_stamp_reason(&after, &when)),
    }
}

fn stamp_after_conditions(
    message: &mut MessageRecord,
    indices: &[usize],
    now: Timestamp,
) -> Vec<String> {
    indices
        .iter()
        .filter_map(|index| {
            let condition = message.after.get_mut(*index)?;
            if condition.met_at.is_some() {
                return None;
            }
            condition.met_at = Some(now);
            Some(condition.address.clone())
        })
        .collect()
}

fn stamp_when_conditions(
    message: &mut MessageRecord,
    indices: &[usize],
    now: Timestamp,
) -> Vec<String> {
    indices
        .iter()
        .filter_map(|index| {
            let condition = message.when.get_mut(*index)?;
            if condition.met_at.is_some() {
                return None;
            }
            condition.met_at = Some(now);
            Some(format!(
                "{} {} {}",
                condition.address,
                condition.status.as_str(),
                crate::message::format_dwell(condition.dwell_secs)
            ))
        })
        .collect()
}

fn condition_stamp_reason(after: &[String], when: &[String]) -> String {
    [
        (!after.is_empty()).then(|| format!("{} finished", after.join(", "))),
        (!when.is_empty()).then(|| format!("{} reached", when.join(", "))),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("; ")
}

impl Store {
    fn commit_queue<T>(&self, f: impl FnOnce(&mut QueueTxn<'_, '_>) -> Result<T>) -> Result<T> {
        self.commit(|txn| {
            let mut queue = QueueTxn::new(txn)?;
            let result = f(&mut queue)?;
            queue.finish()?;
            Ok(result)
        })
    }

    pub fn list_messages(&self) -> Result<Vec<MessageRecord>> {
        Ok(message_store::list(&self.inner.paths.messages_dir)?)
    }

    pub fn list_message_history(&self) -> Result<Vec<MessageRecord>> {
        Ok(message_store::list_history(&self.inner.paths.messages_dir)?)
    }

    pub fn list_pending_messages(&self) -> Result<Vec<MessageRecord>> {
        Ok(message_store::list_pending(&self.inner.paths.messages_dir)?)
    }

    #[must_use = "durability barrier; check the result"]
    pub fn queue_message(&self, message: &MessageRecord, session_name: &str) -> Result<()> {
        let event =
            EventEnvelope::message_event(message, session_name, MessageEventMethod::Queued, None);
        self.commit_queue(|queue| {
            queue.upsert(message.clone());
            queue.stage_event(event);
            Ok(())
        })
    }

    #[must_use = "durability barrier; check the result"]
    pub fn apply_delivery_sweep(
        &self,
        updates: &[DeliverySweepUpdate],
        now: Timestamp,
        session_name: &str,
    ) -> Result<()> {
        self.commit_queue(|queue| {
            let updates = updates
                .iter()
                .map(|update| (update.message_id.as_str(), update))
                .collect::<std::collections::BTreeMap<_, _>>();
            queue.apply_all(session_name, now, |message| {
                if message.status != MessageStatus::Queued {
                    return MessageUpdate::Keep;
                }
                let Some(update) = updates.get(message.message_id.as_str()) else {
                    return MessageUpdate::Keep;
                };
                apply_sweep_update(message, update, now)
            });
            Ok(())
        })
    }

    #[must_use = "durability barrier; check the result"]
    pub fn edit_message(
        &self,
        message_id: &MessageId,
        edit: MessageEdit,
        session_name: &str,
    ) -> Result<EditOutcome> {
        self.commit_queue(|queue| {
            let mut message = match queue.get(message_id) {
                Some(message) => message,
                None => {
                    return Ok(message_store::list_history(&queue.txn.paths.messages_dir)?
                        .into_iter()
                        .find(|message| message.message_id == *message_id)
                        .map_or(EditOutcome::NotFound, |message| {
                            EditOutcome::NotOpen(message.status)
                        }));
                }
            };
            if message.status != MessageStatus::Queued {
                return Ok(EditOutcome::NotOpen(message.status));
            }
            let fields = edit.changed_fields();
            edit.apply(&mut message);
            message.retry_after = None;
            message.updated_at = Timestamp::now();
            queue.upsert(message.clone());
            let reason = fields.join(", ");
            let event = EventEnvelope::message_event(
                &message,
                session_name,
                MessageEventMethod::Edited,
                Some(&reason),
            );
            queue.stage_event(event);
            Ok(EditOutcome::Edited(Box::new(message)))
        })
    }

    #[must_use = "durability barrier; check the result"]
    pub fn record_sent_batch(
        &self,
        messages: &[MessageRecord],
        session_name: &str,
    ) -> Result<Vec<MessageRecord>> {
        self.commit_queue(|queue| {
            let now = Timestamp::now();
            let mut sent = Vec::with_capacity(messages.len());
            for supplied in messages {
                let mut message = match queue.get(&supplied.message_id) {
                    Some(existing)
                        if matches!(
                            existing.status,
                            MessageStatus::Queued | MessageStatus::Claimed | MessageStatus::Sent
                        ) =>
                    {
                        let mut existing = existing;
                        existing.pane_id = supplied.pane_id.clone();
                        existing.batch_id = supplied.batch_id.clone();
                        existing
                    }
                    Some(_) => continue,
                    None => supplied.clone(),
                };
                message.status = MessageStatus::Sent;
                message.last_sent_at = Some(now);
                message.updated_at = now;
                message.last_error = None;
                queue.upsert(message.clone());
                queue.stage_event(EventEnvelope::message_event(
                    &message,
                    session_name,
                    MessageEventMethod::Sent,
                    None,
                ));
                sent.push(message);
            }
            Ok(sent)
        })
    }

    #[must_use = "durability barrier; check the result"]
    pub fn claim_delivery_batch(
        &self,
        message_id: &MessageId,
        status: AgentStatus,
        now: Timestamp,
    ) -> Result<Option<Vec<MessageRecord>>> {
        let Some(mut claimed) = self.commit_queue(|queue| {
            let Some(indices) = delivery_batch_indices(queue.live(), message_id, status, now)
            else {
                return Ok(None);
            };
            Ok(Some(queue.claim_indices(&indices, now)))
        })?
        else {
            return Ok(None);
        };
        if claimed.len() > 1 {
            let batch_id = claimed[0].message_id.clone();
            for message in &mut claimed {
                message.batch_id = Some(batch_id.clone());
            }
        }
        Ok(Some(claimed))
    }

    #[must_use = "durability barrier; check the result"]
    pub fn claim_message_for_steer(
        &self,
        message_id: &MessageId,
        now: Timestamp,
    ) -> Result<Option<MessageRecord>> {
        self.commit_queue(|queue| {
            let Some(index) = queue.live().iter().position(|message| {
                message.message_id == *message_id && message.status == MessageStatus::Queued
            }) else {
                return Ok(None);
            };
            if !claim_expired(queue.live()[index].last_attempt_at, now) {
                return Ok(None);
            }
            let claimed = queue
                .claim_indices(std::slice::from_ref(&index), now)
                .pop()
                .expect("selected claim index returns one message");
            Ok(Some(claimed))
        })
    }

    #[must_use = "durability barrier; check the result"]
    pub fn release_message_claims(
        &self,
        message_ids: &[MessageId],
        note: &str,
        session_name: &str,
    ) -> Result<Vec<MessageRecord>> {
        self.commit_queue(|queue| {
            let now = Timestamp::now();
            let message_ids = message_ids
                .iter()
                .map(MessageId::as_str)
                .collect::<BTreeSet<_>>();
            let updated = queue.apply_all(session_name, now, |message| {
                if !message_ids.contains(message.message_id.as_str())
                    || !matches!(
                        message.status,
                        MessageStatus::Queued | MessageStatus::Claimed
                    )
                {
                    return MessageUpdate::Keep;
                }
                message.attempts = message.attempts.saturating_sub(1);
                message.last_attempt_at = None;
                message.retry_after = None;
                // This claim was released because its compact command already
                // fired; the fresh-window delivery must not fire it again.
                message.auto_compact = None;
                message.requeue(now, note);
                MessageUpdate::Rewrite {
                    method: MessageEventMethod::Queued,
                    reason: Some(note.to_owned()),
                }
            });
            Ok(updated)
        })
    }

    #[must_use = "durability barrier; check the result"]
    pub fn defer_message_wake(&self, message_id: &MessageId, until: Timestamp) -> Result<()> {
        self.commit_queue(|queue| {
            let mut message = match queue.get(message_id) {
                Some(message) if message.status == MessageStatus::Queued => message,
                Some(_) | None => return Ok(()),
            };
            message.retry_after = Some(until);
            queue.upsert(message);
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
        self.commit_queue(|queue| {
            let message = match queue.get(message_id) {
                Some(message)
                    if matches!(
                        message.status,
                        MessageStatus::Queued | MessageStatus::Claimed | MessageStatus::Sent
                    ) =>
                {
                    message
                }
                Some(_) | None => return Ok(None),
            };
            let now = Timestamp::now();
            let settled = queue.terminalize(message, status, session_name, reason, now);
            Ok(Some(settled))
        })
    }

    #[must_use = "durability barrier; check the result"]
    pub fn confirm_delivered_for_card(
        &self,
        kind: &AgentKind,
        agent_id: &AgentSessionId,
        agent_name: Option<&str>,
        ack: DeliveryAck<'_>,
        session_name: &str,
    ) -> Result<Vec<MessageRecord>> {
        let card = AgentCardRef::new(kind, agent_id, agent_name);
        self.commit_queue(|queue| {
            let now = Timestamp::now();
            let oldest_sent_batch = |body| {
                let Some(oldest) = queue
                    .live()
                    .iter()
                    .filter(|message| {
                        message.status == MessageStatus::Sent
                            && message.body == body
                            && message.same_card(card)
                    })
                    .min_by(|a, b| a.message_id.as_str().cmp(b.message_id.as_str()))
                else {
                    return BTreeSet::new();
                };
                queue
                    .live()
                    .iter()
                    .filter(|message| {
                        message.message_id == oldest.message_id
                            || (oldest.batch_id.is_some()
                                && message.status == MessageStatus::Sent
                                && message.body == body
                                && message.same_card(card)
                                && message.batch_id == oldest.batch_id)
                    })
                    .map(|message| message.message_id.as_str().to_owned())
                    .collect()
            };
            let selected = match ack {
                DeliveryAck::TurnStarted {
                    prompt: Some(prompt),
                } if !prompt.trim().is_empty() => {
                    let mut confirmable = queue
                        .live()
                        .iter()
                        .filter(|message| {
                            message.body == MessageBody::Prompt
                                && message.same_card(card)
                                && (message.status == MessageStatus::Sent
                                    || message.awaiting_late_ack(now))
                        })
                        .collect::<Vec<_>>();
                    confirmable.sort_by(|a, b| a.message_id.as_str().cmp(b.message_id.as_str()));
                    let mut selected = BTreeSet::new();
                    for first in &confirmable {
                        let batch = confirmable
                            .iter()
                            .copied()
                            .filter(|message| same_submitted_batch(first, message))
                            .collect::<Vec<_>>();
                        if crate::harness::target::align_submitted_prompt(prompt, &batch).is_some()
                        {
                            selected.extend(
                                batch
                                    .iter()
                                    .map(|message| message.message_id.as_str().to_owned()),
                            );
                            break;
                        }
                    }
                    selected
                }
                DeliveryAck::TurnStarted { .. } => oldest_sent_batch(MessageBody::Prompt),
                DeliveryAck::Compaction => oldest_sent_batch(MessageBody::Command),
            };
            if selected.is_empty() {
                return Ok(Vec::new());
            }
            let delivered = queue.apply_all(session_name, now, |message| {
                if !selected.contains(message.message_id.as_str()) {
                    return MessageUpdate::Keep;
                }
                MessageUpdate::Finalize {
                    status: MessageStatus::Delivered,
                    reason: None,
                }
            });
            Ok(delivered)
        })
    }

    #[must_use = "durability barrier; check the result"]
    pub fn mark_message_timed_out(
        &self,
        message_id: &MessageId,
        session_name: &str,
        reason: Option<&str>,
    ) -> Result<Option<MessageRecord>> {
        self.commit_queue(|queue| {
            let mut message = match queue.get(message_id) {
                Some(message) if message.status == MessageStatus::Sent => message,
                Some(_) | None => return Ok(None),
            };
            let now = Timestamp::now();
            let reason = reason.unwrap_or("delivery window elapsed");
            message.last_error = Some(reason.to_owned());
            let message = queue.terminalize(
                message,
                MessageStatus::TimedOut,
                session_name,
                Some(reason),
                now,
            );
            Ok(Some(message))
        })
    }

    #[must_use = "durability barrier; check the result"]
    pub fn reconcile_stale_sent_messages(
        &self,
        session_name: &str,
        now: Timestamp,
        max_attempts: u32,
        defer: impl Fn(&MessageRecord) -> bool,
    ) -> Result<ReconcileReport> {
        self.commit_queue(|queue| {
            let mut report = ReconcileReport::default();
            let updated = queue.apply_all(session_name, now, |message| {
                if message.status != MessageStatus::Sent {
                    return MessageUpdate::Keep;
                }
                let Some(deadline) = message.sent_reconcile_deadline() else {
                    return MessageUpdate::Keep;
                };
                if now < deadline {
                    return MessageUpdate::Keep;
                }
                if defer(message) {
                    message.retry_after = Some(now + message.body.delivery_window());
                    return MessageUpdate::SilentRewrite;
                }
                if !message.body.resends_unconfirmed() {
                    message.last_error =
                        Some("delivery unconfirmed; command not resent".to_owned());
                    report.timed_out += 1;
                    MessageUpdate::Finalize {
                        status: MessageStatus::TimedOut,
                        reason: Some("reconcile".to_owned()),
                    }
                } else if message.unconfirmed_sends < max_attempts {
                    message.unconfirmed_sends = message.unconfirmed_sends.saturating_add(1);
                    message.last_attempt_at = None;
                    message.retry_after = None;
                    message.requeue(now, "delivery unconfirmed; re-queued");
                    report.requeued += 1;
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
            });
            if !updated.is_empty() {
                queue.force_publish();
            }
            Ok(report)
        })
    }

    pub fn earliest_message_wake(&self, now: Timestamp) -> Result<Option<Timestamp>> {
        let next = message_store::list(&self.inner.paths.messages_dir)?
            .into_iter()
            .filter_map(|message| message.wake_deadline(now))
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
        self.commit_queue(|queue| {
            let mut message = match queue.get(&message.message_id) {
                Some(existing)
                    if matches!(
                        existing.status,
                        MessageStatus::Queued | MessageStatus::Claimed
                    ) =>
                {
                    let mut existing = existing;
                    existing.pane_id = message.pane_id.clone();
                    existing
                }
                Some(existing) if existing.status == MessageStatus::Sent => return Ok(None),
                Some(_) => return Ok(None),
                None => message.clone(),
            };
            message.last_error = Some(error.to_owned());
            let errored = queue.terminalize(
                message,
                MessageStatus::Errored,
                session_name,
                Some(error),
                Timestamp::now(),
            );
            Ok(Some(errored))
        })
    }

    #[must_use = "durability barrier; check the result"]
    pub fn record_message_delivery_failures(
        &self,
        message_ids: &[MessageId],
        fallback_head: Option<&MessageRecord>,
        disposition: DeliveryFailureDisposition,
        error: &str,
        session_name: &str,
    ) -> Result<DeliveryFailureResult> {
        self.commit_queue(|queue| {
            let Some(head_id) = message_ids.first() else {
                return Ok(DeliveryFailureResult::default());
            };
            let message_ids = message_ids
                .iter()
                .map(MessageId::as_str)
                .collect::<BTreeSet<_>>();
            let now = Timestamp::now();
            let mut result = DeliveryFailureResult::default();
            queue.apply_all(session_name, now, |message| {
                if !message_ids.contains(message.message_id.as_str()) {
                    return MessageUpdate::Keep;
                }
                if message.message_id == *head_id {
                    result.head_found = true;
                    result.head_sent = message.status == MessageStatus::Sent;
                }
                if message.status == MessageStatus::Sent || message.status.is_terminal() {
                    return MessageUpdate::Keep;
                }
                if !matches!(
                    message.status,
                    MessageStatus::Queued | MessageStatus::Claimed
                ) {
                    return MessageUpdate::Keep;
                }
                message.requeue(now, error);
                if disposition == DeliveryFailureDisposition::Terminal {
                    MessageUpdate::Finalize {
                        status: MessageStatus::Errored,
                        reason: Some(error.to_owned()),
                    }
                } else if message.attempts >= MAX_DELIVERY_ATTEMPTS {
                    MessageUpdate::Finalize {
                        status: MessageStatus::Abandoned,
                        reason: Some(error.to_owned()),
                    }
                } else {
                    MessageUpdate::SilentRewrite
                }
            });
            if !result.head_found
                && let Some(fallback) = fallback_head
            {
                let mut fallback = fallback.clone();
                fallback.last_error = Some(error.to_owned());
                queue.terminalize(
                    fallback,
                    MessageStatus::Errored,
                    session_name,
                    Some(error),
                    now,
                );
            }
            Ok(result)
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
        self.commit(|txn| txn.append(&event))?;
        Ok(message_id)
    }

    #[must_use = "durability barrier; check the result"]
    pub fn cancel_message(
        &self,
        message_id: &MessageId,
        session_name: &str,
        reason: &str,
    ) -> Result<bool> {
        Ok(self
            .settle_message(
                message_id,
                MessageStatus::Canceled,
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
        let card = AgentCardRef::new(kind, agent_id, agent_name);
        self.commit_queue(|queue| {
            let cleared = queue.finalize_matching(
                MessageStatus::Canceled,
                session_name,
                "clear",
                |message| message.status.is_open() && message.same_card(card),
                |_| {},
            );
            Ok(cleared)
        })
    }

    #[must_use = "durability barrier; check the result"]
    pub fn clear_channel_messages(
        &self,
        channel: &str,
        session_name: &str,
    ) -> Result<Vec<MessageRecord>> {
        self.commit_queue(|queue| {
            let cleared = queue.finalize_matching(
                MessageStatus::Canceled,
                session_name,
                "clear",
                |message| message.status.is_open() && message.channel.as_deref() == Some(channel),
                |_| {},
            );
            Ok(cleared)
        })
    }

    #[must_use = "durability barrier; check the result"]
    pub fn archive_orphan_messages(&self, session_name: &str) -> Result<usize> {
        let snapshot = self.snapshot()?;
        let live_agents = snapshot.agents;
        let archived = self.commit_queue(|queue| {
            let archived = queue.apply_all(session_name, Timestamp::now(), |message| {
                if !message.status.is_open() {
                    return MessageUpdate::Keep;
                }
                let reason = if !live_agents
                    .iter()
                    .any(|agent| message.same_agent_card(agent))
                {
                    Some("receiver ended".to_owned())
                } else if message.status == MessageStatus::Queued {
                    message
                        .when
                        .iter()
                        .filter(|condition| condition.met_at.is_none())
                        .find(|condition| {
                            !live_agents
                                .iter()
                                .any(|agent| condition.card_ref().matches(agent.card_ref()))
                        })
                        .map(crate::message::WhenCondition::expiry_reason)
                } else {
                    None
                };
                let Some(reason) = reason else {
                    return MessageUpdate::Keep;
                };
                message.last_error = Some(reason.clone());
                MessageUpdate::Finalize {
                    status: MessageStatus::Archived,
                    reason: Some(reason),
                }
            });
            if !archived.is_empty() {
                queue.force_publish();
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
        let card = AgentCardRef::new(kind, agent_id, agent_name);
        let archived = self.commit_queue(|queue| {
            let archived = queue.finalize_matching(
                MessageStatus::Archived,
                session_name,
                reason,
                |message| message.status.is_open() && message.same_card(card),
                |message| message.last_error = Some(reason.to_owned()),
            );
            Ok(archived)
        })?;
        Ok(archived.len())
    }

    #[must_use = "durability barrier; check the result"]
    pub fn archive_messages_watching_card(
        &self,
        kind: &AgentKind,
        agent_id: &AgentSessionId,
        agent_name: Option<&str>,
        session_name: &str,
    ) -> Result<usize> {
        let card = AgentCardRef::new(kind, agent_id, agent_name);
        let archived = self.commit_queue(|queue| {
            let archived = queue.apply_all(session_name, Timestamp::now(), |message| {
                if message.status != MessageStatus::Queued {
                    return MessageUpdate::Keep;
                }
                let Some(condition) = message.when.iter().find(|condition| {
                    condition.met_at.is_none() && condition.card_ref().matches(card)
                }) else {
                    return MessageUpdate::Keep;
                };
                let reason = condition.expiry_reason();
                message.last_error = Some(reason.clone());
                MessageUpdate::Finalize {
                    status: MessageStatus::Archived,
                    reason: Some(reason),
                }
            });
            Ok(archived)
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
        let archived = self.commit_queue(|queue| {
            let archived = queue.finalize_matching(
                MessageStatus::Archived,
                session_name,
                reason,
                |message| message.status.is_open() && message.channel.as_deref() == Some(channel),
                |message| message.last_error = Some(reason.to_owned()),
            );
            Ok(archived)
        })?;
        Ok(archived.len())
    }
}

#[cfg(test)]
mod tests;
