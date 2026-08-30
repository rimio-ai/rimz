//! Transcript recording for lifecycle hooks.

use super::*;

use std::borrow::Cow;

#[cfg(test)]
mod tests;

pub(super) fn record_assistant_response(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &AgentDefinition,
    decoded: &HookOutput,
    recorded: Option<&RecordedLifecycle>,
) -> Option<(rimz::ids::AgentSessionId, String)> {
    let message = decoded.assistant_message()?.trim().to_owned();
    if message.is_empty() {
        return None;
    }
    let agent_id = decoded.event_agent_id()?.clone();
    let state = store.snapshot_cached().ok().and_then(|snapshot| {
        snapshot
            .agents
            .into_iter()
            .find(|state| state.kind == agent.spec().kind && state.agent_id == agent_id)
    });
    let worktree_path = recorded
        .and_then(|recorded| recorded.observation.worktree_path.as_deref())
        .or(decoded.worktree_path())
        .or_else(|| {
            state
                .as_ref()
                .and_then(|state| state.worktree_path.as_deref())
        });
    let channel = rimz::transcript::entry_channel(
        recorded
            .and_then(|recorded| recorded.observation.launch.channel.as_deref())
            .or_else(|| state.as_ref().and_then(|state| state.channel.as_deref())),
        worktree_path,
    )
    .or_else(|| {
        workspace
            .worktree_root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    });
    let mut entry = rimz::transcript::TranscriptEntry::new(
        jiff::Timestamp::now(),
        agent.spec().kind_id(),
        agent_id.clone(),
        rimz::transcript::TranscriptKind::Assistant,
        message.clone(),
    );
    entry.channel = channel;
    entry.name = recorded
        .and_then(|recorded| recorded.observation.agent_name.clone())
        .or_else(|| state.as_ref().and_then(|state| state.name.clone()));
    entry.profile = recorded
        .and_then(|recorded| recorded.observation.launch.profile.clone())
        .or_else(|| state.as_ref().and_then(|state| state.profile.clone()));
    entry.role = recorded
        .and_then(|recorded| recorded.observation.launch.role.clone())
        .or_else(|| state.as_ref().and_then(|state| state.role.clone()));
    entry.reply_to = turn_opened_by(store, agent, &agent_id);
    if let Err(err) = rimz::transcript::append(store.paths(), &entry) {
        warn!(
            agent = agent.spec().kind,
            event = %decoded.event_name(),
            agent_id = %agent_id,
            error = %err,
            "lifecycle: failed to record assistant response",
        );
    }
    Some((agent_id, message))
}

pub(super) fn record_native_answer(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &AgentDefinition,
    decoded: &HookOutput,
    recorded: Option<&RecordedLifecycle>,
) {
    let Some(answers) = decoded.native_answers() else {
        return;
    };
    let answer = rimz::transcript::answers_text(answers);
    if answers.is_empty() || answer.is_empty() {
        return;
    }
    let Some(agent_id) = decoded.event_agent_id() else {
        return;
    };

    let open_ask = latest_open_native_ask(store, agent.spec().kind, agent_id.as_str());
    // `rimz answer` can append its confirmation before Claude's PostToolUse
    // hook reaches this writer. Keep the newest ask identity even when that
    // confirmation already closed it, so the idempotent append suppresses the
    // native duplicate rather than emitting a legacy id-less answer.
    let ask_id = latest_native_ask_id(store, agent.spec().kind, agent_id.as_str());
    let awaiting = recorded.is_some_and(|recorded| recorded.waiting_cleared)
        || store
            .snapshot_cached()
            .ok()
            .and_then(|snapshot| {
                snapshot
                    .agents
                    .into_iter()
                    .find(|state| state.kind == agent.spec().kind && state.agent_id == *agent_id)
            })
            .is_some_and(|state| state.is_awaiting_input())
        || open_ask.is_some();
    if !awaiting {
        return;
    }

    let worktree_path = decoded
        .worktree_path()
        .filter(|path| !path.is_empty())
        .map(Cow::Borrowed)
        .unwrap_or_else(|| Cow::Owned(workspace.worktree_root.display().to_string()));
    let channel = rimz::transcript::entry_channel(
        recorded.and_then(|recorded| recorded.observation.launch.channel.as_deref()),
        Some(worktree_path.as_ref()),
    );
    let mut entry = rimz::transcript::TranscriptEntry::new(
        jiff::Timestamp::now(),
        agent.spec().kind_id(),
        agent_id.clone(),
        rimz::transcript::TranscriptKind::Answer,
        answer,
    );
    entry.channel = channel;
    entry.from = Some("you".to_owned());
    entry.answers = answers.to_vec();
    entry.id = ask_id;
    if let Err(err) = rimz::transcript::append_answer_if_missing(store.paths(), &entry) {
        warn!(
            agent = agent.spec().kind,
            event = %decoded.event_name(),
            agent_id = %agent_id,
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
    agent: &AgentDefinition,
    recorded: &RecordedLifecycle,
    assistant_message: Option<&str>,
    questions: &[rimz::transcript::AskQuestion],
    delivered: &[rimz::message::MessageRecord],
) -> rimz::transcript::Result<()> {
    let observation = &recorded.observation;
    if observation.parent_agent_id.is_some() {
        return Ok(());
    }
    let Some(agent_id) = observation.agent_id.clone() else {
        return Ok(());
    };
    let kind = agent.spec().kind_id();
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
            let mut entries = Vec::new();
            let mut matched_ids = Vec::new();
            let mut delivered_cursor = 0;
            if let Some(prompt) = observation
                .prompt
                .as_deref()
                .map(str::trim)
                .filter(|prompt| !prompt.is_empty())
            {
                let mut open_ask_id = recorded
                    .waiting_cleared
                    .then(|| latest_open_native_ask(store, agent.spec().kind, agent_id.as_str()))
                    .flatten()
                    .and_then(|ask| ask.id);
                let delivered_refs = delivered.iter().collect::<Vec<_>>();
                let aligned =
                    rimz::harness::target::align_submitted_prompt(prompt, &delivered_refs);
                let batch_aligned = aligned.is_some();
                let segments =
                    aligned.unwrap_or_else(|| rimz::harness::target::split_batched_prompt(prompt));
                for segment in segments {
                    let segment = segment.trim();
                    if segment.is_empty() {
                        continue;
                    }
                    let (mut entry, delivered_text) =
                        match rimz::harness::target::parse_message_header(segment) {
                            Some((rimz::harness::target::HeaderKind::Agent, sender, body)) => {
                                let delivered_text = body.clone();
                                let mut entry =
                                    entry_base(rimz::transcript::TranscriptKind::Message, body);
                                entry.from = Some(sender);
                                (entry, delivered_text)
                            }
                            Some((rimz::harness::target::HeaderKind::User, _, body)) => {
                                let delivered_text = body.clone();
                                (
                                    entry_base(rimz::transcript::TranscriptKind::Prompt, body),
                                    delivered_text,
                                )
                            }
                            None => (
                                entry_base(
                                    rimz::transcript::TranscriptKind::Prompt,
                                    segment.to_owned(),
                                ),
                                segment.to_owned(),
                            ),
                        };
                    let matched = if batch_aligned {
                        delivered.get(delivered_cursor).map(|message| (0, message))
                    } else {
                        delivered[delivered_cursor..]
                            .iter()
                            .enumerate()
                            .find(|(_, message)| message.text == delivered_text)
                    };
                    if let Some((offset, message)) = matched {
                        entry.message_id = Some(message.message_id.clone());
                        entry.reply_to = message.in_reply_to.clone();
                        if matches!(&message.sender, rimz::message::MessageSender::System) {
                            entry.from = Some("rimz".to_owned());
                        }
                        matched_ids.push(message.message_id.clone());
                        delivered_cursor += offset + 1;
                    }
                    // ponytail: agent messages do not answer an open ask; add
                    // explicit sender semantics before classifying them.
                    let fallback_prompt = if entry.entry == rimz::transcript::TranscriptKind::Prompt
                        && let Some(ask_id) = open_ask_id.take()
                    {
                        let fallback = entry.clone();
                        entry.entry = rimz::transcript::TranscriptKind::Answer;
                        entry.id = Some(ask_id);
                        entry.from = Some("you".to_owned());
                        entry.answers = vec![rimz::transcript::AskAnswer {
                            question: None,
                            chosen: vec![entry.text.clone()],
                            note: None,
                        }];
                        Some(fallback)
                    } else {
                        None
                    };
                    entries.push((entry, fallback_prompt));
                }
            }
            replace_turn_opened_by(store, agent, &agent_id, matched_ids);
            for (entry, fallback_prompt) in entries {
                append_turn_entry(store.paths(), &entry, fallback_prompt.as_ref())?;
            }
        }
        LifecycleSignal::TurnEnded { .. } => {
            if let Some(message) = assistant_message
                .map(str::trim)
                .filter(|message| !message.is_empty())
            {
                let mut entry = entry_base(
                    rimz::transcript::TranscriptKind::Assistant,
                    message.to_owned(),
                );
                entry.reply_to = turn_opened_by(store, agent, &agent_id);
                rimz::transcript::append(store.paths(), &entry)?;
            }
        }
        LifecycleSignal::TurnInterrupted { .. } => {}
        LifecycleSignal::AwaitingInput { ask_id, .. } => {
            if questions.is_empty() {
                return Ok(());
            }
            let last = assistant_message
                .map(str::trim)
                .filter(|message| !message.is_empty())
                .unwrap_or_default()
                .to_owned();
            let mut entry = entry_base(rimz::transcript::TranscriptKind::Ask, last);
            entry.id = ask_id.clone();
            entry.questions = questions.to_vec();
            entry.reply_to = turn_opened_by(store, agent, &agent_id);
            rimz::transcript::append(store.paths(), &entry)?;
        }
        _ => {}
    }
    Ok(())
}

fn append_turn_entry(
    paths: &rimz::StatePaths,
    entry: &rimz::transcript::TranscriptEntry,
    fallback_prompt: Option<&rimz::transcript::TranscriptEntry>,
) -> rimz::transcript::Result<()> {
    let Some(fallback_prompt) = fallback_prompt else {
        return rimz::transcript::append(paths, entry);
    };
    if !rimz::transcript::append_answer_if_missing(paths, entry)? {
        rimz::transcript::append(paths, fallback_prompt)?;
    }
    Ok(())
}

fn replace_turn_opened_by(
    store: &Store,
    agent: &AgentDefinition,
    agent_id: &rimz::ids::AgentSessionId,
    message_ids: Vec<rimz::ids::MessageId>,
) {
    if let Err(err) = rimz::store::agent_context::merge_turn_opened_by(
        store.runtime_paths(),
        agent.spec().kind,
        agent_id.as_str(),
        message_ids,
    ) {
        warn!(
            agent = agent.spec().kind,
            agent_id = %agent_id,
            error = %err,
            "lifecycle: failed to record turn message causality",
        );
    }
}

fn turn_opened_by(
    store: &Store,
    agent: &AgentDefinition,
    agent_id: &rimz::ids::AgentSessionId,
) -> Vec<rimz::ids::MessageId> {
    rimz::store::agent_context::read_one(
        store.runtime_paths(),
        agent.spec().kind,
        agent_id.as_str(),
    )
    .map(|record| record.context.turn_opened_by)
    .unwrap_or_default()
}

pub(super) fn merge_turn_error_marker_and_transcript(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &AgentDefinition,
    event_name: &str,
    context_agent_id: &str,
    marker: &rimz::agents::AgentTurnError,
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
            marker,
        );
    }
    updated
}

pub(super) fn record_turn_error_entry(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &AgentDefinition,
    event_name: &str,
    context_agent_id: &str,
    marker: &rimz::agents::AgentTurnError,
) {
    let mut entry = rimz::transcript::TranscriptEntry::new(
        marker.at,
        agent.spec().kind_id(),
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
            agent = agent.spec().kind,
            event = %event_name,
            session = %context_agent_id,
            error = %err,
            "lifecycle: failed to record turn-error transcript entry",
        );
    }
}

pub(super) fn merge_turn_error_marker(
    store: &Store,
    agent: &AgentDefinition,
    event_name: &str,
    context_agent_id: &str,
    marker: rimz::agents::AgentTurnError,
) -> bool {
    let kind = agent.spec().kind;
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
                // overload, or other API failure) — observed, not a RimZ fault.
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
