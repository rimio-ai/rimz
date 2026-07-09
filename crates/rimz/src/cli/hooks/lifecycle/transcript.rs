//! Transcript recording for lifecycle hooks.

use super::*;

pub(super) fn record_native_answer(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &dyn AgentAdapter,
    event_name: &str,
    payload: &Value,
    recorded: Option<&RecordedLifecycle>,
) {
    let Some(answers) = agent.native_ask_answer(event_name, payload) else {
        return;
    };
    let answer = rimz::transcript::answers_text(&answers);
    if answers.is_empty() || answer.is_empty() {
        return;
    }
    let Some(agent_id) = payload_agent_id(payload) else {
        return;
    };

    let open_ask = latest_open_native_ask(store, agent.descriptor().kind, agent_id);
    // `rimz answer` can append its confirmation before Claude's PostToolUse
    // hook reaches this writer. Keep the newest ask identity even when that
    // confirmation already closed it, so the idempotent append suppresses the
    // native duplicate rather than emitting a legacy id-less answer.
    let ask_id = latest_native_ask_id(store, agent.descriptor().kind, agent_id);
    let awaiting = recorded.is_some_and(|recorded| recorded.waiting_cleared)
        || store
            .snapshot_cached()
            .ok()
            .and_then(|snapshot| {
                snapshot.agents.into_iter().find(|state| {
                    state.kind == agent.descriptor().kind && state.agent_id == agent_id
                })
            })
            .is_some_and(|state| state.is_awaiting_input())
        || open_ask.is_some();
    if !awaiting {
        return;
    }

    let worktree_path = payload
        .get("worktree_path")
        .or_else(|| payload.get("cwd"))
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(Cow::Borrowed)
        .unwrap_or_else(|| Cow::Owned(workspace.worktree_root.display().to_string()));
    let channel = rimz::transcript::entry_channel(
        recorded.and_then(|recorded| recorded.observation.launch.channel.as_deref()),
        Some(worktree_path.as_ref()),
    );
    let mut entry = rimz::transcript::TranscriptEntry::new(
        jiff::Timestamp::now(),
        rimz::ids::AgentKind::new_unchecked(agent.descriptor().kind),
        rimz::ids::AgentSessionId::from(agent_id),
        rimz::transcript::TranscriptKind::Answer,
        answer,
    );
    entry.channel = channel;
    entry.from = Some("you".to_owned());
    entry.answers = answers;
    entry.id = ask_id;
    if let Err(err) = rimz::transcript::append_answer_if_missing(store.paths(), &entry) {
        warn!(
            agent = agent.descriptor().kind,
            event = %event_name,
            agent_id,
            error = %err,
            "lifecycle: failed to record native ask answer",
        );
    }
}

#[cfg(test)]
pub(super) fn has_open_native_ask(store: &Store, kind: &str, agent_id: &str) -> bool {
    latest_open_native_ask(store, kind, agent_id).is_some()
}

fn latest_open_native_ask(
    store: &Store,
    kind: &str,
    agent_id: &str,
) -> Option<rimz::transcript::TranscriptEntry> {
    let kind = rimz::ids::AgentKind::new_unchecked(kind);
    let agent_id = rimz::ids::AgentSessionId::from(agent_id);
    rimz::transcript::latest_open_ask(store.paths(), &kind, &agent_id)
        .ok()
        .flatten()
}

pub(super) fn latest_native_ask_id(
    store: &Store,
    kind: &str,
    agent_id: &str,
) -> Option<rimz::ids::AskId> {
    rimz::transcript::read_all(store.paths())
        .ok()?
        .into_iter()
        .rev()
        .find(|entry| {
            entry.entry == rimz::transcript::TranscriptKind::Ask
                && entry.kind == kind
                && entry.agent_id == agent_id
        })
        .and_then(|entry| entry.id)
}

pub(super) fn record_conversation(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &dyn AgentAdapter,
    event_name: &str,
    payload: &Value,
    recorded: &RecordedLifecycle,
) -> rimz::transcript::Result<()> {
    let observation = &recorded.observation;
    if observation.parent_agent_id.is_some() {
        return Ok(());
    }
    let Some(agent_id) = observation.agent_id.clone() else {
        return Ok(());
    };
    let kind = rimz::ids::AgentKind::new_unchecked(agent.descriptor().kind);
    let channel = rimz::transcript::entry_channel(
        observation.launch.channel.as_deref(),
        observation.worktree_path.as_deref(),
    )
    .or_else(|| {
        workspace
            .worktree_root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    });
    let entry_base = |entry, text: String| {
        let mut entry = rimz::transcript::TranscriptEntry::new(
            jiff::Timestamp::now(),
            kind.clone(),
            agent_id.clone(),
            entry,
            text,
        );
        entry.channel = channel.clone();
        entry.name = observation.agent_name.clone();
        entry.profile = observation.launch.profile.clone();
        entry.role = observation.launch.role.clone();
        entry
    };

    match &observation.signal {
        LifecycleSignal::TurnStarted => {
            if let Some(prompt) = observation
                .prompt
                .as_deref()
                .map(str::trim)
                .filter(|prompt| !prompt.is_empty())
            {
                for segment in rimz::harness::target::split_batched_prompt(prompt) {
                    let segment = segment.trim();
                    if segment.is_empty() {
                        continue;
                    }
                    let entry = if let Some((sender, body)) =
                        rimz::harness::target::parse_sender_prefix(segment)
                    {
                        let mut entry = entry_base(rimz::transcript::TranscriptKind::Message, body);
                        entry.from = Some(sender);
                        entry
                    } else {
                        entry_base(rimz::transcript::TranscriptKind::Prompt, segment.to_owned())
                    };
                    rimz::transcript::append(store.paths(), &entry)?;
                }
            }
        }
        LifecycleSignal::TurnEnded { .. } => {
            if let Some(message) = agent
                .last_assistant_message(event_name, payload, observation)
                .map(|message| message.trim().to_owned())
                .filter(|message| !message.is_empty())
            {
                rimz::transcript::append(
                    store.paths(),
                    &entry_base(rimz::transcript::TranscriptKind::Assistant, message),
                )?;
            }
        }
        LifecycleSignal::AwaitingInput { ask_id, .. } => {
            let questions = agent
                .ask_question_detail(event_name, payload)
                .unwrap_or_default();
            if questions.is_empty() {
                return Ok(());
            }
            let last = agent
                .last_assistant_message(event_name, payload, observation)
                .map(|message| message.trim().to_owned())
                .filter(|message| !message.is_empty())
                .unwrap_or_default();
            let mut entry = entry_base(rimz::transcript::TranscriptKind::Ask, last);
            entry.id = ask_id.clone();
            entry.questions = questions;
            rimz::transcript::append(store.paths(), &entry)?;
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn turn_error_refresh_event(event_name: &str) -> bool {
    matches!(event_name, "Stop")
}

pub(super) fn merge_turn_error_marker_and_transcript(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &dyn AgentAdapter,
    event_name: &str,
    context_agent_id: &str,
    marker: rimz::agents::AgentTurnError,
) -> bool {
    let updated =
        merge_turn_error_marker(store, agent, event_name, context_agent_id, marker.clone());
    if updated {
        record_turn_error_entry(
            workspace,
            store,
            agent,
            event_name,
            context_agent_id,
            &marker,
        );
    }
    updated
}

pub(super) fn record_turn_error_entry(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &dyn AgentAdapter,
    event_name: &str,
    context_agent_id: &str,
    marker: &rimz::agents::AgentTurnError,
) {
    let mut entry = rimz::transcript::TranscriptEntry::new(
        marker.at,
        rimz::ids::AgentKind::new_unchecked(agent.descriptor().kind),
        rimz::ids::AgentSessionId::from(context_agent_id),
        rimz::transcript::TranscriptKind::Error,
        marker
            .label
            .clone()
            .unwrap_or_else(|| "provider API error".to_owned()),
    );
    entry.channel = workspace
        .worktree_root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    if let Err(err) = rimz::transcript::append(store.paths(), &entry) {
        warn!(
            agent = agent.descriptor().kind,
            event = %event_name,
            session = %context_agent_id,
            error = %err,
            "lifecycle: failed to record turn-error transcript entry",
        );
    }
}

pub(super) fn merge_turn_error_marker(
    store: &Store,
    agent: &dyn AgentAdapter,
    event_name: &str,
    context_agent_id: &str,
    marker: rimz::agents::AgentTurnError,
) -> bool {
    let kind = agent.descriptor().kind;
    let class = marker.class;
    let label = marker.label.clone();
    match rimz::store::agent_context::merge_turn_error(
        store.runtime_paths(),
        kind,
        context_agent_id,
        marker,
    ) {
        Ok(updated) => {
            if updated {
                // The agent's turn ended on a provider condition (rate limit,
                // overload, or other API failure) — observed, not a Rimz fault.
                // Warn once per transition; the Sentry reporting layer lifts it to a
                // warning event keyed by `class`.
                warn!(
                    target: "rimz::agent::turn_error",
                    agent = kind,
                    session = %context_agent_id,
                    tags.operation = "agent.turn_error",
                    class = ?class,
                    label = label.as_deref().unwrap_or_default(),
                    "agent turn ended on a provider error",
                );
            }
            updated
        }
        Err(err) => {
            warn!(
                agent = kind,
                session = %context_agent_id,
                event = %event_name,
                tags.operation = "agent.turn_error_merge",
                error = &err as &dyn std::error::Error,
                "lifecycle: failed to merge turn-error marker",
            );
            false
        }
    }
}
