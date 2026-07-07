//! Message delivery confirmation and queued-delivery wakeups.

use super::*;

pub(super) fn confirm_sent_message_for_lifecycle(
    store: &Store,
    agent: &dyn AgentAdapter,
    recorded: &RecordedLifecycle,
    session_name: &str,
) {
    let body = match recorded.observation.signal {
        LifecycleSignal::TurnStarted => rimz::message::MessageBody::Prompt,
        LifecycleSignal::Compacting => rimz::message::MessageBody::Command,
        _ => return,
    };
    let Some(agent_id) = recorded.observation.agent_id.as_ref() else {
        return;
    };
    let kind = rimz::ids::AgentKind::new_unchecked(agent.descriptor().kind);
    if let Err(err) = store.confirm_delivered_for_card(
        &kind,
        agent_id,
        recorded.observation.agent_name.as_deref(),
        body,
        session_name,
    ) {
        warn!(
            agent = agent.descriptor().kind,
            agent_id = %agent_id,
            error = %err,
            "lifecycle: failed to confirm sent message delivery",
        );
    }
}

pub(super) fn spawn_queue_delivery_if_checkpoint(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &dyn AgentAdapter,
    recorded: &RecordedLifecycle,
) {
    if !rimz::message::delivery_checkpoint(&recorded.observation.signal) {
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
    // FIFO spans this card's provisional and registered ids, so the stable
    // agent name folds a message queued before registration into the same queue.
    let Some(head) = rimz::message::queue_head(
        pending.iter(),
        &kind,
        agent_id,
        recorded.observation.agent_name.as_deref(),
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
