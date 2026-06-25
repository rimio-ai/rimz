use std::time::Duration;

use jiff::Timestamp;

use crate::ids::{AgentKind, AgentSessionId, MessageId};
use crate::message::{
    MAX_DELIVERY_ATTEMPTS, MessageBody, MessageRecord, MessageStatus, claim_expired, queue_head,
};
use crate::schema::event::{EventEnvelope, MessageEventMethod};

use super::super::{Ledger, Result, event_log, lock, message_store};

impl Ledger {
    pub fn list_messages(&self) -> Result<Vec<MessageRecord>> {
        Ok(message_store::list(&self.inner.paths.queue_dir)?)
    }

    pub fn list_pending_messages(&self) -> Result<Vec<MessageRecord>> {
        Ok(message_store::list_pending(&self.inner.paths.queue_dir)?)
    }

    #[must_use = "durability barrier; check the result"]
    pub fn queue_message(&self, message: &MessageRecord, session_name: &str) -> Result<()> {
        let event =
            EventEnvelope::message_event(message, session_name, MessageEventMethod::Queued, None);
        {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            message_store::write(&self.inner.paths.queue_dir, message)?;
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
                match message_store::load(&self.inner.paths.queue_dir, &message.message_id) {
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
            message_store::write(&self.inner.paths.queue_dir, &message)?;
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
        let queued = message_store::list_pending(&self.inner.paths.queue_dir)?;
        let Some(message) = queued
            .iter()
            .find(|message| message.message_id == *message_id)
        else {
            return Ok(None);
        };
        if !claim_expired(message.last_attempt_at, now) {
            return Ok(None);
        }
        let Some(head) = queue_head(
            queued.iter(),
            &message.kind,
            &message.agent_id,
            message.agent_name.as_deref(),
        ) else {
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
        claimed.updated_at = now;
        message_store::write(&self.inner.paths.queue_dir, &claimed)?;
        Ok(Some(claimed))
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
            let mut message = match message_store::load(&self.inner.paths.queue_dir, message_id) {
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
            message.status = status;
            message.updated_at = now;
            if status == MessageStatus::Delivered {
                message.delivered_at = Some(now);
            }
            message_store::write(&self.inner.paths.queue_dir, &message)?;
            let method = MessageEventMethod::for_terminal_status(status)
                .expect("settle_message only accepts terminal message statuses");
            let event = EventEnvelope::message_event(&message, session_name, method, reason);
            event_log::append(&self.inner.paths.events_log, &event)?;
            (message, event)
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
            let Some(mut message) = message_store::list(&self.inner.paths.queue_dir)?
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
            message.status = MessageStatus::Delivered;
            message.updated_at = now;
            message.delivered_at = Some(now);
            message_store::write(&self.inner.paths.queue_dir, &message)?;
            let event = EventEnvelope::message_event(
                &message,
                session_name,
                MessageEventMethod::Delivered,
                None,
            );
            event_log::append(&self.inner.paths.events_log, &event)?;
            Some((message, event))
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
            let mut message = match message_store::load(&self.inner.paths.queue_dir, message_id) {
                Ok(message) if message.status == MessageStatus::Sent => message,
                Ok(_) | Err(message_store::MessageStoreErr::NotFound(_)) => return Ok(None),
                Err(err) => return Err(err.into()),
            };
            message.status = MessageStatus::TimedOut;
            message.updated_at = Timestamp::now();
            let reason = reason.unwrap_or("delivery window elapsed");
            message.last_error = Some(reason.to_owned());
            message_store::write(&self.inner.paths.queue_dir, &message)?;
            let event = EventEnvelope::message_event(
                &message,
                session_name,
                MessageEventMethod::TimedOut,
                Some(reason),
            );
            event_log::append(&self.inner.paths.events_log, &event)?;
            Some((message, event))
        };
        let Some((message, event)) = outcome else {
            return Ok(None);
        };
        self.wake_sidebars_for_event_best_effort(&event);
        self.publish_snapshot_best_effort();
        Ok(Some(message))
    }

    #[must_use = "durability barrier; check the result"]
    pub fn timeout_sent_messages(
        &self,
        session_name: &str,
        now: Timestamp,
        window: Duration,
    ) -> Result<usize> {
        let mut timed_out = Vec::new();
        let mut events = Vec::new();
        {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            for mut message in message_store::list(&self.inner.paths.queue_dir)? {
                if message.status != MessageStatus::Sent {
                    continue;
                }
                let age = now.duration_since(message.updated_at);
                if !age.is_negative() && age.as_millis() < window.as_millis() as i128 {
                    continue;
                }
                message.status = MessageStatus::TimedOut;
                message.last_error = Some("delivery window elapsed".to_owned());
                message.updated_at = now;
                message_store::write(&self.inner.paths.queue_dir, &message)?;
                let event = EventEnvelope::message_event(
                    &message,
                    session_name,
                    MessageEventMethod::TimedOut,
                    Some("gc"),
                );
                event_log::append(&self.inner.paths.events_log, &event)?;
                events.push(event);
                timed_out.push(message);
            }
        }
        for event in &events {
            self.wake_sidebars_for_event_best_effort(event);
        }
        if !timed_out.is_empty() {
            self.publish_snapshot_forced();
        }
        Ok(timed_out.len())
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
                match message_store::load(&self.inner.paths.queue_dir, &message.message_id) {
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
            message.status = MessageStatus::Errored;
            message.last_error = Some(error.to_owned());
            message.updated_at = Timestamp::now();
            message_store::write(&self.inner.paths.queue_dir, &message)?;
            let event = EventEnvelope::message_event(
                &message,
                session_name,
                MessageEventMethod::Errored,
                Some(error),
            );
            event_log::append(&self.inner.paths.events_log, &event)?;
            (message, event)
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
            let mut message = match message_store::load(&self.inner.paths.queue_dir, message_id) {
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
                message.status = MessageStatus::Abandoned;
                message_store::write(&self.inner.paths.queue_dir, &message)?;
                let event = EventEnvelope::message_event(
                    &message,
                    session_name,
                    MessageEventMethod::Abandoned,
                    Some(error),
                );
                event_log::append(&self.inner.paths.events_log, &event)?;
                Some((message, Some(event)))
            } else {
                message.status = MessageStatus::Queued;
                message_store::write(&self.inner.paths.queue_dir, &message)?;
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
        let mut removed = Vec::new();
        let mut events = Vec::new();
        {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            for mut message in message_store::list(&self.inner.paths.queue_dir)? {
                if !message.status.is_open() || !message.same_card(kind, agent_id, agent_name) {
                    continue;
                }
                message.status = MessageStatus::Removed;
                message.updated_at = Timestamp::now();
                message_store::write(&self.inner.paths.queue_dir, &message)?;
                let event = EventEnvelope::message_event(
                    &message,
                    session_name,
                    MessageEventMethod::Removed,
                    Some("clear"),
                );
                event_log::append(&self.inner.paths.events_log, &event)?;
                events.push(event);
                removed.push(message);
            }
        }
        for event in &events {
            self.wake_sidebars_for_event_best_effort(event);
        }
        if !removed.is_empty() {
            self.publish_snapshot_best_effort();
        }
        Ok(removed.len())
    }

    #[must_use = "durability barrier; check the result"]
    pub fn abandon_orphan_messages(&self, session_name: &str) -> Result<usize> {
        let snapshot = self.snapshot()?;
        let live_agents = snapshot.agents;
        let mut abandoned = Vec::new();
        let mut events = Vec::new();
        {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            for mut message in message_store::list(&self.inner.paths.queue_dir)? {
                if !message.status.is_open()
                    || live_agents
                        .iter()
                        .any(|agent| message.same_agent_card(agent))
                {
                    continue;
                }
                message.status = MessageStatus::Abandoned;
                message.last_error = Some("agent no longer exists".to_owned());
                message.updated_at = Timestamp::now();
                message_store::write(&self.inner.paths.queue_dir, &message)?;
                let event = EventEnvelope::message_event(
                    &message,
                    session_name,
                    MessageEventMethod::Abandoned,
                    Some("gc"),
                );
                event_log::append(&self.inner.paths.events_log, &event)?;
                events.push(event);
                abandoned.push(message);
            }
        }
        for event in &events {
            self.wake_sidebars_for_event_best_effort(event);
        }
        if !abandoned.is_empty() {
            self.publish_snapshot_forced();
        }
        Ok(abandoned.len())
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::agents::AgentLifecycleObservation;
    use crate::agents::lifecycle::LifecycleSignal;
    use crate::agents::{AgentState, AgentStatus};
    use crate::ids::{MuxName, PaneId, WorkspaceId};
    use crate::message::{DeliveryGate, MessageSender};
    use crate::{RuntimePaths, StatePaths};

    #[test]
    fn claim_moves_message_out_of_pending_until_send_failure_requeues() {
        let (_dir, ledger, workspace_id) = ledger();
        let message = message(&workspace_id);
        ledger.queue_message(&message, "session").unwrap();

        let claimed = ledger
            .claim_message_for_delivery(&message.message_id, Timestamp::now())
            .unwrap()
            .expect("claimed");
        assert_eq!(claimed.status, MessageStatus::Claimed);
        assert_eq!(claimed.attempts, 1);
        assert!(ledger.list_pending_messages().unwrap().is_empty());

        ledger
            .record_message_delivery_failure(&message.message_id, "pane missing", "session")
            .unwrap();
        let pending = ledger.list_pending_messages().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].status, MessageStatus::Queued);
        assert_eq!(pending[0].last_error.as_deref(), Some("pane missing"));
    }

    #[test]
    fn fifth_send_failure_abandons_message() {
        let (_dir, ledger, workspace_id) = ledger();
        let message = message(&workspace_id);
        ledger.queue_message(&message, "session").unwrap();

        for attempt in 1..=MAX_DELIVERY_ATTEMPTS {
            let claimed = ledger
                .claim_message_for_delivery(
                    &message.message_id,
                    Timestamp::now() + jiff::SignedDuration::from_secs(i64::from(attempt) * 20),
                )
                .unwrap()
                .expect("claimed");
            assert_eq!(claimed.attempts, attempt);
            ledger
                .record_message_delivery_failure(&message.message_id, "pane missing", "session")
                .unwrap();
        }

        assert!(ledger.list_pending_messages().unwrap().is_empty());
        let messages = ledger.list_messages().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].status, MessageStatus::Abandoned);
    }

    #[test]
    fn orphan_gc_keeps_provisional_message_when_registered_card_name_is_live() {
        let (_dir, ledger, workspace_id) = ledger();
        let mut provisional = agent();
        provisional.agent_id = AgentSessionId::from("launch_a");
        provisional.name = Some("lucid-atlas".to_owned());
        let message = MessageRecord::new(
            workspace_id.clone(),
            &provisional,
            "next".to_owned(),
            true,
            DeliveryGate::Done,
        );
        ledger.queue_message(&message, "session").unwrap();

        let mut observation = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("real-session")),
            LifecycleSignal::Registered,
        );
        observation.agent_name = Some("lucid-atlas".to_owned());
        let event = EventEnvelope::agent_lifecycle(
            workspace_id,
            "session",
            "claude",
            "SessionStart",
            &observation,
        );
        ledger.append_event(&event).unwrap();

        let abandoned = ledger.abandon_orphan_messages("session").unwrap();

        assert_eq!(abandoned, 0);
        let messages = ledger.list_messages().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].status, MessageStatus::Queued);
    }

    #[test]
    fn only_fifo_head_can_be_claimed() {
        let (_dir, ledger, workspace_id) = ledger();
        let first = message(&workspace_id);
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = message(&workspace_id);
        ledger.queue_message(&first, "session").unwrap();
        ledger.queue_message(&second, "session").unwrap();

        assert!(
            ledger
                .claim_message_for_delivery(&second.message_id, Timestamp::now())
                .unwrap()
                .is_none()
        );
        assert!(
            ledger
                .claim_message_for_delivery(&first.message_id, Timestamp::now())
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn queued_message_persists_sender() {
        let (_dir, ledger, workspace_id) = ledger();
        let sender = MessageSender::Agent {
            kind: AgentKind::new_unchecked("codex"),
            name: Some("swift-otter".to_owned()),
            profile: None,
            role: None,
            channel: Some("docs".to_owned()),
        };
        let message = message(&workspace_id).with_sender(sender.clone());
        ledger.queue_message(&message, "session").unwrap();

        let messages = ledger.list_messages().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].sender, sender);
    }

    #[test]
    fn record_sent_then_turn_start_confirms_delivery() {
        let (_dir, ledger, workspace_id) = ledger();
        let message = message(&workspace_id).with_pane_id(PaneId::from_parts(MuxName::Tmux, "%1"));

        let sent = ledger
            .record_sent_message(&message, "session")
            .unwrap()
            .expect("sent");
        assert_eq!(sent.status, MessageStatus::Sent);
        assert_eq!(sent.pane_id.as_ref().map(PaneId::as_str), Some("tmux:%1"));
        assert!(ledger.list_pending_messages().unwrap().is_empty());

        let delivered = ledger
            .confirm_delivered_for_card(
                &message.kind,
                &message.agent_id,
                None,
                MessageBody::Prompt,
                "session",
            )
            .unwrap()
            .expect("delivered");
        assert_eq!(delivered.status, MessageStatus::Delivered);
        assert!(delivered.delivered_at.is_some());
    }

    #[test]
    fn confirmation_matches_message_body() {
        let (_dir, ledger, workspace_id) = ledger();
        let prompt = message(&workspace_id).with_pane_id(PaneId::from_parts(MuxName::Tmux, "%1"));
        let command = message(&workspace_id)
            .with_body(MessageBody::Command)
            .with_pane_id(PaneId::from_parts(MuxName::Tmux, "%1"));
        ledger.record_sent_message(&prompt, "session").unwrap();
        ledger.record_sent_message(&command, "session").unwrap();

        let delivered_command = ledger
            .confirm_delivered_for_card(
                &command.kind,
                &command.agent_id,
                None,
                MessageBody::Command,
                "session",
            )
            .unwrap()
            .expect("command delivered");
        assert_eq!(delivered_command.message_id, command.message_id);

        let messages = ledger.list_messages().unwrap();
        let prompt = messages
            .iter()
            .find(|message| message.message_id == prompt.message_id)
            .expect("prompt remains");
        assert_eq!(prompt.status, MessageStatus::Sent);
    }

    #[test]
    fn sent_message_times_out_after_window() {
        let (_dir, ledger, workspace_id) = ledger();
        let message = message(&workspace_id).with_pane_id(PaneId::from_parts(MuxName::Tmux, "%1"));
        ledger.record_sent_message(&message, "session").unwrap();

        let timed_out = ledger
            .timeout_sent_messages("session", Timestamp::now(), Duration::ZERO)
            .unwrap();

        assert_eq!(timed_out, 1);
        let messages = ledger.list_messages().unwrap();
        assert_eq!(messages[0].status, MessageStatus::TimedOut);
    }

    fn ledger() -> (tempfile::TempDir, Ledger, WorkspaceId) {
        let dir = tempdir().unwrap();
        let state_root = dir.path().join("state");
        let runtime_root = dir.path().join("runtime");
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let state = StatePaths::under(workspace_id.clone(), &state_root).unwrap();
        let runtime = RuntimePaths::under(workspace_id.clone(), &runtime_root).unwrap();
        (dir, Ledger::open(state, runtime).unwrap(), workspace_id)
    }

    fn message(workspace_id: &WorkspaceId) -> MessageRecord {
        MessageRecord::new(
            workspace_id.clone(),
            &agent(),
            "next".to_owned(),
            true,
            DeliveryGate::Done,
        )
    }

    fn agent() -> AgentState {
        let now = Timestamp::now();
        AgentState {
            agent_id: AgentSessionId::from("sess-1"),
            kind: AgentKind::new_unchecked("claude"),
            name: None,
            kind_ordinal: None,
            profile: None,
            role: None,
            team: None,
            status: AgentStatus::Idle,
            phase: crate::agents::TurnPhase::Idle,
            pane: None,
            agent_pid: None,
            agent_process_start: None,
            runtime_owner: None,
            parent_agent_id: None,
            worktree_path: None,
            worktree_branch: None,
            task: None,
            prompt: None,
            description: None,
            transcript_path: None,
            recent_prompts: Vec::new(),
            model: None,
            effort: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            context: None,
            subagent_description: None,
            subagent_started_at: None,
            turn_started_at: None,
            compacting_since: None,
            compaction_count: 0,
            last_seen: now,
            last_activity: now,
            registered_at: Some(now),
        }
    }
}
