use super::feed_item::{build_item, payload_agent_id};
use super::*;

pub(super) fn handle_blocking_feed(
    workspace: &ResolvedWorkspace,
    ledger: &Ledger,
    agent: &dyn AgentAdapter,
    event_name: &str,
    feed_kind: FeedKind,
    payload: Value,
) -> Result<()> {
    // A fresh ask supersedes any earlier native_ui ask this session left
    // pending — the agent only ever shows one at a time in its own UI. The
    // push below expires the priors inside its own critical section, so the
    // sidebar never stacks two rows for one session and the hook pays one
    // lock cycle.
    let superseded_session: Option<String> = payload_agent_id(&payload).map(ToOwned::to_owned);
    let supersede = superseded_session
        .as_deref()
        .map(|agent_id| (agent.descriptor().kind, agent_id));

    if agent.descriptor().capabilities.native_ask_ui {
        let item = build_item(workspace, Surface::NativeUi, feed_kind, agent, payload);
        push_feed_item_recording_ask(
            ledger,
            agent,
            event_name,
            &item,
            supersede,
            &workspace.session_name,
        )?;
    }
    emit_neutral(agent, event_name)
}

fn push_feed_item_recording_ask(
    ledger: &Ledger,
    agent: &dyn AgentAdapter,
    event_name: &str,
    item: &FeedItem,
    supersede: Option<(&str, &str)>,
    session_name: &str,
) -> Result<()> {
    let chat = chat_ask_entry(agent, event_name, item);
    ledger.push_feed_item_superseding(item, supersede, session_name)?;
    if let Some(entry) = chat.as_ref()
        && let Err(err) = rimz::chat::append(ledger.paths(), entry)
    {
        warn!(
            agent = agent.descriptor().kind,
            request_id = %item.request_id,
            error = %err,
            "ask: failed to record transcript ask",
        );
    }
    Ok(())
}

fn chat_ask_entry(
    agent: &dyn AgentAdapter,
    event_name: &str,
    item: &FeedItem,
) -> Option<rimz::chat::ChatEntry> {
    if item.source_kind != "agent-hook" || !item.kind.is_ask() {
        return None;
    }
    let agent_id = item
        .agent_session_id()
        .map(rimz::ids::AgentSessionId::from)?;
    let questions = agent.ask_question_detail(event_name, &item.payload)?;
    if questions.is_empty() {
        return None;
    }
    let mut observation =
        AgentLifecycleObservation::new(Some(agent_id.clone()), LifecycleSignal::TurnStarted);
    observation.worktree_path = item.worktree_path.clone();
    observation.worktree_branch = item.worktree_branch.clone();
    observation.transcript_path = item
        .payload
        .get("transcript_path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(ToOwned::to_owned);

    let last = agent
        .last_assistant_message(event_name, &item.payload, &observation)
        .map(|message| message.trim().to_owned())
        .filter(|message| !message.is_empty());
    let text = last.unwrap_or_default();
    // lane: basename fallback; item carries no stamped channel.
    let channel = rimz::chat::entry_channel(None, item.worktree_path.as_deref());
    let mut entry = rimz::chat::ChatEntry::new(
        item.created_at,
        rimz::ids::AgentKind::new_unchecked(item.source.clone()),
        agent_id,
        rimz::chat::ChatKind::Ask,
        text,
    );
    entry.channel = channel;
    entry.request_id = Some(item.request_id.clone());
    entry.questions = questions;
    Some(entry)
}

fn emit_neutral(agent: &dyn AgentAdapter, event_name: &str) -> Result<()> {
    if let Some(payload) = agent.render_neutral(event_name)? {
        let rendered = serde_json::to_string(&payload)?;
        #[expect(clippy::print_stdout, reason = "hook stdout is the decision channel")]
        {
            println!("{rendered}");
        }
    }
    Ok(())
}
