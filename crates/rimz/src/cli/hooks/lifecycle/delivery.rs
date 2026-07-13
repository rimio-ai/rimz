//! Message delivery confirmation and queued-delivery wakeups.

use super::*;

pub(super) fn confirm_sent_message_for_lifecycle(
    store: &Store,
    agent: &dyn AgentAdapter,
    recorded: &RecordedLifecycle,
    session_name: &str,
) -> Vec<rimz::message::MessageRecord> {
    let body = match recorded.observation.signal {
        LifecycleSignal::TurnStarted => rimz::message::MessageBody::Prompt,
        LifecycleSignal::Compacting => rimz::message::MessageBody::Command,
        _ => return Vec::new(),
    };
    let Some(agent_id) = recorded.observation.agent_id.as_ref() else {
        return Vec::new();
    };
    let kind = rimz::ids::AgentKind::new_unchecked(agent.descriptor().kind);
    match store.confirm_delivered_for_card(
        &kind,
        agent_id,
        recorded.observation.agent_name.as_deref(),
        body,
        session_name,
    ) {
        Ok(delivered) => delivered,
        Err(err) => {
            warn!(
                agent = agent.descriptor().kind,
                agent_id = %agent_id,
                error = %err,
                "lifecycle: failed to confirm sent message delivery",
            );
            Vec::new()
        }
    }
}

pub(super) fn spawn_queue_delivery_if_checkpoint(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &dyn AgentAdapter,
    recorded: &RecordedLifecycle,
) {
    let delivery_checkpoint = rimz::message::delivery_checkpoint(&recorded.observation.signal);
    let condition_checkpoint = matches!(
        recorded.observation.signal,
        LifecycleSignal::Registered
            | LifecycleSignal::TurnStarted
            | LifecycleSignal::TurnEnded { .. }
            | LifecycleSignal::AwaitingInput { .. }
            | LifecycleSignal::SubagentStarted
            | LifecycleSignal::SubagentStopped { .. }
            | LifecycleSignal::CompactionEnded { .. }
    );
    if !delivery_checkpoint && !condition_checkpoint {
        return;
    }
    let Some(agent_id) = recorded.observation.agent_id.as_ref() else {
        return;
    };
    let pending = match store.list_pending_messages() {
        Ok(messages) => messages,
        Err(err) => {
            debug!(
                agent = agent.descriptor().kind,
                agent_id = %agent_id,
                error = %err,
                "message delivery skipped; queued messages unreadable",
            );
            return;
        }
    };
    let kind = rimz::ids::AgentKind::new_unchecked(agent.descriptor().kind);
    let agent_name = recorded.observation.agent_name.as_deref();
    if pending.iter().any(|message| {
        message.status == rimz::message::MessageStatus::Queued
            && message.after.iter().any(|condition| {
                condition.met_at.is_none()
                    && rimz::message::card_matches(
                        &condition.kind,
                        &condition.agent_id,
                        condition.agent_name.as_deref(),
                        &kind,
                        agent_id,
                        agent_name,
                    )
            })
            || message.status == rimz::message::MessageStatus::Queued
                && message.when.iter().any(|condition| {
                    condition.met_at.is_none()
                        && rimz::message::card_matches(
                            &condition.kind,
                            &condition.agent_id,
                            condition.agent_name.as_deref(),
                            &kind,
                            agent_id,
                            agent_name,
                        )
                })
    }) {
        spawn_refresh_detached(&rimz::agents::RefreshSpawn {
            args: vec![
                "--root".to_owned(),
                workspace.project_root.display().to_string(),
                "message".to_owned(),
                "sweep".to_owned(),
            ],
        });
    }
    if !delivery_checkpoint {
        return;
    }
    // FIFO spans this card's provisional and registered ids, so the stable
    // agent name folds a message queued before registration into the same queue.
    let Some(head) = rimz::message::queue_head(
        pending.iter(),
        &kind,
        agent_id,
        agent_name,
        jiff::Timestamp::now(),
    ) else {
        return;
    };
    spawn_refresh_detached(&rimz::agents::RefreshSpawn {
        args: vec![
            "--root".to_owned(),
            workspace.project_root.display().to_string(),
            "message".to_owned(),
            "deliver".to_owned(),
            "--message-id".to_owned(),
            head.message_id.to_string(),
        ],
    });
}
