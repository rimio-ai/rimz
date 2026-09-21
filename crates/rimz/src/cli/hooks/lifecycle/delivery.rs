//! Message delivery confirmation and queued-delivery wakeups.

use super::*;

#[cfg(test)]
mod tests;

pub(super) fn in_flight_messages_for_lifecycle(
    store: &Store,
    agent: &AgentDefinition,
    recorded: &RecordedLifecycle,
) -> Vec<rimz::store::message::MessageRecord> {
    if !matches!(
        recorded.observation.signal,
        LifecycleSignal::TurnStarted { .. }
    ) {
        return Vec::new();
    }
    let Some(agent_id) = recorded.observation.agent_id.as_ref() else {
        return Vec::new();
    };
    let kind = agent.spec().kind_id();
    let card = rimz::agents::AgentCardRef::new(
        &kind,
        agent_id,
        recorded.observation.agent_name.as_deref(),
    );
    match store.list_messages() {
        Ok(messages) => messages
            .into_iter()
            .filter(|record| record.in_flight() && record.same_card(card))
            .collect(),
        Err(err) => {
            warn!(error = %err, "lifecycle: failed to read in-flight messages");
            Vec::new()
        }
    }
}

pub(super) fn confirm_sent_message_for_lifecycle(
    store: &Store,
    agent: &AgentDefinition,
    recorded: &RecordedLifecycle,
    session_name: &str,
) -> Vec<rimz::store::message::MessageRecord> {
    let ack = match recorded.observation.signal {
        LifecycleSignal::TurnStarted { .. } => rimz::store::writer::DeliveryAck::TurnStarted {
            prompt: recorded
                .observation
                .prompt
                .as_deref()
                .map(str::trim)
                .filter(|prompt| !prompt.is_empty()),
        },
        LifecycleSignal::Compacting => rimz::store::writer::DeliveryAck::Compaction,
        _ => return Vec::new(),
    };
    let Some(agent_id) = recorded.observation.agent_id.as_ref() else {
        return Vec::new();
    };
    let kind = agent.spec().kind_id();
    match store.confirm_delivered_for_card(
        &kind,
        agent_id,
        recorded.observation.agent_name.as_deref(),
        ack,
        session_name,
    ) {
        Ok(delivered) => delivered,
        Err(err) => {
            warn!(
                agent = agent.spec().kind,
                agent_id = %agent_id,
                error = %err,
                "lifecycle: failed to confirm sent message delivery",
            );
            Vec::new()
        }
    }
}

pub(super) fn record_user_input_for_lifecycle(
    workspace: &ResolvedWorkspace,
    agent: &AgentDefinition,
    recorded: &RecordedLifecycle,
    sections: &[rimz::store::message::PromptSection<'_>],
    supervised: bool,
    state_root: Option<&std::path::Path>,
) {
    if !matches!(
        recorded.observation.signal,
        LifecycleSignal::TurnStarted { .. }
    ) || supervised
    {
        return;
    }
    // Narrow only where the classifier had text to judge. A turn start with no
    // prompt (an antigravity transcript read that found nothing, a plugin that
    // omits the optional `prompt`) says nothing about who opened the turn, and
    // the ledger's asymmetry decides the default: a missing record leaves the
    // five-hour window shut, while a duplicate resolves to the same boundary
    // in `spending::aggregate::burst_cutoff`.
    if !sections.is_empty()
        && !sections.iter().any(|section| {
            section.origin == rimz::store::message::SectionOrigin::Human
                && section.record.is_none_or(|record| record.is_user_input())
        })
    {
        return;
    }
    let record = rimz::agents::spending::user_input::UserInputRecord {
        at: jiff::Timestamp::now(),
        kind: agent.spec().kind_id(),
        origin: Some(
            recorded
                .observation
                .worktree_path
                .as_deref()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| workspace.project_root.clone()),
        ),
    };
    if let Some(state_root) = state_root {
        rimz::agents::spending::user_input::append_in(state_root, &record);
    } else {
        rimz::agents::spending::user_input::append(&record);
    }
}

pub(super) fn spawn_queue_delivery_if_checkpoint(
    workspace: &ResolvedWorkspace,
    store: &Store,
    event: &rimz::agents::LifecycleEvent,
) {
    let delivery_checkpoint = rimz::agents::DELIVERY_CHECKPOINT.contains(&event.signal);
    let condition_checkpoint = rimz::agents::CONDITION_CHECKPOINT.contains(&event.signal);
    if !delivery_checkpoint && !condition_checkpoint {
        return;
    }
    let agent_id = &event.agent_id;
    let pending = match store.list_pending_messages() {
        Ok(messages) => messages,
        Err(err) => {
            debug!(
                agent = %event.kind,
                agent_id = %agent_id,
                error = %err,
                "message delivery skipped; queued messages unreadable",
            );
            return;
        }
    };
    let kind = &event.kind;
    let agent_name = event.agent_name.as_deref();
    let card = rimz::agents::AgentCardRef::new(kind, agent_id, agent_name);
    if pending.iter().any(|message| {
        message.status == rimz::store::message::MessageStatus::Queued
            && delivery_checkpoint
            && message
                .after
                .iter()
                .any(|condition| condition.met_at.is_none() && condition.card_ref().matches(card))
            || message.status == rimz::store::message::MessageStatus::Queued
                && condition_checkpoint
                && message.when.iter().any(|condition| {
                    condition.met_at.is_none() && condition.card_ref().matches(card)
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
    let Some(head) = rimz::store::message::queue_head(
        pending.iter(),
        kind,
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
