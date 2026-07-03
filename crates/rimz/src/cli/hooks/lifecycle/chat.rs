//! Chat recording for lifecycle hooks.

use super::*;

pub(super) fn record_native_answer(
    workspace: &ResolvedWorkspace,
    ledger: &Ledger,
    agent: &dyn AgentAdapter,
    event_name: &str,
    payload: &Value,
) {
    let Some(answers) = agent.native_ask_answer(event_name, payload) else {
        return;
    };
    let answer = rimz::chat::answers_text(&answers);
    if answers.is_empty() || answer.is_empty() {
        return;
    }
    let Some(agent_id) = payload_agent_id(payload) else {
        return;
    };

    // Bridge asks already record resolver answers when resolved. A native answer
    // is recorded only when there is a pending native_ui ask to clear.
    match ledger.expire_agent_native_ui_asks(
        agent.descriptor().kind,
        agent_id,
        &workspace.session_name,
    ) {
        Ok(0) => return,
        Ok(_) => {}
        Err(err) => {
            warn!(
                agent = agent.descriptor().kind,
                event = %event_name,
                agent_id,
                error = %err,
                "lifecycle: failed to expire the answered native ask",
            );
            return;
        }
    }

    let worktree_path = payload
        .get("worktree_path")
        .or_else(|| payload.get("cwd"))
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(Cow::Borrowed)
        .unwrap_or_else(|| Cow::Owned(workspace.worktree_root.display().to_string()));
    let channel = rimz::harness::target::compose_channel(
        None,
        payload
            .get("worktree_branch")
            .and_then(Value::as_str)
            .filter(|branch| !branch.is_empty())
            .or(workspace.worktree_branch.as_deref()),
        rimz::harness::target::path_basename(&worktree_path),
        None,
    );
    let mut entry = rimz::chat::ChatEntry::new(
        jiff::Timestamp::now(),
        rimz::ids::AgentKind::new_unchecked(agent.descriptor().kind),
        rimz::ids::AgentSessionId::from(agent_id),
        rimz::chat::ChatKind::Answer,
        answer,
    );
    entry.channel = channel;
    entry.from = Some("you".to_owned());
    entry.answers = answers;
    if let Err(err) = rimz::chat::append(ledger.paths(), &entry) {
        warn!(
            agent = agent.descriptor().kind,
            event = %event_name,
            agent_id,
            error = %err,
            "lifecycle: failed to record native ask answer",
        );
    }
}

pub(super) fn record_chat_conversation(
    workspace: &ResolvedWorkspace,
    ledger: &Ledger,
    agent: &dyn AgentAdapter,
    event_name: &str,
    payload: &Value,
    recorded: &RecordedLifecycle,
) -> rimz::chat::Result<()> {
    let observation = &recorded.observation;
    if observation.parent_agent_id.is_some() {
        return Ok(());
    }
    let Some(agent_id) = observation.agent_id.clone() else {
        return Ok(());
    };
    let kind = rimz::ids::AgentKind::new_unchecked(agent.descriptor().kind);
    let channel = rimz::harness::target::compose_channel(
        observation.launch.channel.as_deref(),
        observation.worktree_branch.as_deref(),
        observation
            .worktree_path
            .as_deref()
            .and_then(rimz::harness::target::path_basename),
        observation.launch.team.as_deref(),
    )
    .or_else(|| workspace.worktree_branch.clone());
    let entry_base = |entry, text: String| {
        let mut entry = rimz::chat::ChatEntry::new(
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

    match observation.signal {
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
                        let mut entry = entry_base(rimz::chat::ChatKind::Message, body);
                        entry.from = Some(sender);
                        entry
                    } else {
                        entry_base(rimz::chat::ChatKind::Prompt, segment.to_owned())
                    };
                    rimz::chat::append(ledger.paths(), &entry)?;
                }
            }
        }
        LifecycleSignal::TurnEnded { .. } => {
            if let Some(message) = agent
                .last_assistant_message(event_name, payload, observation)
                .map(|message| message.trim().to_owned())
                .filter(|message| !message.is_empty())
            {
                rimz::chat::append(
                    ledger.paths(),
                    &entry_base(rimz::chat::ChatKind::Assistant, message),
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn turn_error_refresh_event(event_name: &str) -> bool {
    matches!(event_name, "Stop")
}

pub(super) fn merge_turn_error_marker_and_chat(
    workspace: &ResolvedWorkspace,
    ledger: &Ledger,
    agent: &dyn AgentAdapter,
    event_name: &str,
    context_agent_id: &str,
    marker: rimz::agents::AgentTurnError,
) -> bool {
    let updated =
        merge_turn_error_marker(ledger, agent, event_name, context_agent_id, marker.clone());
    if updated {
        record_turn_error_chat_entry(
            workspace,
            ledger,
            agent,
            event_name,
            context_agent_id,
            &marker,
        );
    }
    updated
}

pub(super) fn record_turn_error_chat_entry(
    workspace: &ResolvedWorkspace,
    ledger: &Ledger,
    agent: &dyn AgentAdapter,
    event_name: &str,
    context_agent_id: &str,
    marker: &rimz::agents::AgentTurnError,
) {
    let mut entry = rimz::chat::ChatEntry::new(
        marker.at,
        rimz::ids::AgentKind::new_unchecked(agent.descriptor().kind),
        rimz::ids::AgentSessionId::from(context_agent_id),
        rimz::chat::ChatKind::Error,
        marker
            .label
            .clone()
            .unwrap_or_else(|| "provider API error".to_owned()),
    );
    entry.channel = workspace.worktree_branch.clone();
    if let Err(err) = rimz::chat::append(ledger.paths(), &entry) {
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
    ledger: &Ledger,
    agent: &dyn AgentAdapter,
    event_name: &str,
    context_agent_id: &str,
    marker: rimz::agents::AgentTurnError,
) -> bool {
    let kind = agent.descriptor().kind;
    let class = marker.class;
    let label = marker.label.clone();
    match rimz::ledger::agent_context::merge_turn_error(
        ledger.runtime_paths(),
        kind,
        context_agent_id,
        marker,
    ) {
        Ok(updated) => {
            if updated {
                // The agent's turn ended on a provider condition (rate limit,
                // overload, or other API failure) — observed, not a Rimz fault.
                // Warn once per transition; the Sentry bridge lifts it to a
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
