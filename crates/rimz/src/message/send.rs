//! Live-pane message send engine.

use std::path::PathBuf;
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::agents::AgentState;
use crate::ids::{AgentSessionId, MessageId, WorkspaceId};
use crate::message::{
    AfterCondition, AutoCompact, DeliveryGate, MessageBody, MessageRecord, MessageSender,
    MessageStatus, WhenCondition,
};
use crate::mux::{NamedKey, paste_into_pane, press_pane_key, type_into_pane};
use crate::store::event::EventKind;
use crate::store::event_log;
use crate::workspace::ResolvedWorkspace;
use crate::{PaneAgent, SidebarSnapshot, Store};

pub type Result<T> = std::result::Result<T, SendErr>;

#[derive(Debug, thiserror::Error)]
pub enum SendErr {
    #[error(transparent)]
    Store(#[from] crate::store::StoreErr),
    #[error(transparent)]
    EventLog(#[from] crate::store::event_log::EventLogErr),
    #[error("{0}")]
    Mux(#[from] crate::mux::MuxErr),
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// What happened to one live-pane send in a fan-out. Every resolved pane target
/// carries a live pane, so the only soft skip is Waiting reserving the next
/// input.
pub enum Outcome {
    Sent {
        label: String,
        message_id: MessageId,
    },
    SkippedWaiting {
        label: String,
        message_id: MessageId,
    },
    CompactionPending {
        label: String,
        message_id: MessageId,
    },
}

/// How a live-pane send is delivered: whether to send past Waiting, and pacing
/// state.
pub struct LiveSend {
    pub force: bool,
    pub steer: bool,
    pub pacer: Pacer,
}

pub struct MessageDraft {
    pub text: String,
    pub body: MessageBody,
    pub address: Option<String>,
    pub enter: bool,
    pub gate: DeliveryGate,
    pub sender: MessageSender,
    pub automated: bool,
    pub force: bool,
    pub auto_compact: Option<AutoCompact>,
    pub after: Vec<AfterCondition>,
    pub when: Vec<WhenCondition>,
}

pub struct SentPrompt {
    pub outcome: Outcome,
    pub compacted: Option<MessageId>,
}

pub fn message_for_target(
    workspace_id: WorkspaceId,
    target: &PaneAgent,
    bound: Option<&AgentState>,
    scope_channel: Option<&str>,
    draft: MessageDraft,
) -> MessageRecord {
    let agent_id = bound
        .map(|agent| agent.agent_id.clone())
        .or_else(|| target.agent_id.clone())
        .unwrap_or_else(|| synthetic_session_for_pane(&target.pane_id));
    let agent_name = bound
        .and_then(|agent| agent.name.clone())
        .or_else(|| target.name.clone());
    MessageRecord::new_for_card(
        workspace_id,
        target.kind.clone(),
        agent_id,
        agent_name,
        draft.text,
        draft.enter,
        draft.gate,
    )
    .with_channel(crate::harness::target::recipient_channel(
        target,
        bound,
        scope_channel,
    ))
    .with_address(draft.address)
    .with_sender(draft.sender)
    .with_automated(draft.automated)
    .with_body(draft.body)
    .with_force(draft.force)
    .with_pane_id(target.pane_id.clone())
    .with_auto_compact(draft.auto_compact)
    .with_after(draft.after)
    .with_when(draft.when)
    .with_status(MessageStatus::Queued)
}

pub fn send_batch_to_live_pane(
    workspace: &ResolvedWorkspace,
    store: &Store,
    snapshot: &SidebarSnapshot,
    target: &PaneAgent,
    bound: Option<&AgentState>,
    batch: &[MessageRecord],
    send: &mut LiveSend,
) -> Result<SentPrompt> {
    // Dispatch and delivery claims always pass a non-empty batch.
    let head = batch
        .first()
        .expect("send_batch_to_live_pane requires at least one message");
    if head.body == MessageBody::Command {
        debug_assert_eq!(batch.len(), 1);
        let outcome = write_batch(workspace, store, snapshot, target, bound, batch, send)?;
        return Ok(SentPrompt {
            outcome,
            compacted: None,
        });
    }
    debug_assert!(
        batch
            .iter()
            .all(|message| message.body == MessageBody::Prompt)
    );
    let mut compacted = None;
    let command = batch
        .iter()
        .find_map(|message| compact_message_for_target(store, target, bound, message));
    if let Some(command) = command {
        store.queue_message(&command, &workspace.session_name)?;
        match write_batch(
            workspace,
            store,
            snapshot,
            target,
            bound,
            std::slice::from_ref(&command),
            send,
        ) {
            Ok(Outcome::Sent { message_id, .. }) => {
                compacted = Some(message_id);
                if !send.steer {
                    return Ok(SentPrompt {
                        outcome: Outcome::CompactionPending {
                            label: handle_for_pane_target(snapshot, target, bound),
                            message_id: head.message_id.clone(),
                        },
                        compacted,
                    });
                }
            }
            Ok(skipped @ Outcome::SkippedWaiting { .. }) => {
                return Ok(SentPrompt {
                    outcome: skipped,
                    compacted: None,
                });
            }
            Ok(Outcome::CompactionPending { .. }) => {
                unreachable!("write_batch only returns pane-write outcomes")
            }
            Err(err) => {
                store.record_send_error(&command, &err.to_string(), &workspace.session_name)?;
                return Err(err);
            }
        }
    }
    let outcome = write_batch(workspace, store, snapshot, target, bound, batch, send)?;
    Ok(SentPrompt { outcome, compacted })
}

pub fn synthetic_session_for_pane(pane_id: &crate::ids::PaneId) -> AgentSessionId {
    let mut rendered = String::from("pane_");
    rendered.extend(pane_id.as_str().chars().map(|ch| match ch {
        'a'..='z' | 'A'..='Z' | '0'..='9' => ch,
        _ => '_',
    }));
    AgentSessionId::from(rendered)
}

pub struct Pacer {
    interval: Duration,
    started: bool,
}

impl Pacer {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            started: false,
        }
    }

    /// Sleep before every delivered message after the first, so fan-outs land
    /// paced rather than coalesced.
    pub fn tick(&mut self) {
        self.tick_with(sleep);
    }

    fn tick_with(&mut self, sleeper: impl FnOnce(Duration)) -> bool {
        let should_sleep = self.started && !self.interval.is_zero();
        if should_sleep {
            sleeper(self.interval);
        }
        self.started = true;
        should_sleep
    }
}

fn write_batch(
    workspace: &ResolvedWorkspace,
    store: &Store,
    snapshot: &SidebarSnapshot,
    target: &PaneAgent,
    bound: Option<&AgentState>,
    batch: &[MessageRecord],
    send: &mut LiveSend,
) -> Result<Outcome> {
    // Dispatch and delivery claims always pass a non-empty batch.
    let head = batch
        .first()
        .expect("write_batch requires at least one message");
    debug_assert!(batch.iter().all(|message| message.enter == head.enter));
    let label = handle_for_pane_target(snapshot, target, bound);
    if !send.force && bound.is_some_and(AgentState::is_awaiting_input) {
        return Ok(Outcome::SkippedWaiting {
            label,
            message_id: head.message_id.clone(),
        });
    }
    let pane_id = &target.pane_id;
    send.pacer.tick();
    match head.body {
        MessageBody::Command => {
            debug_assert_eq!(batch.len(), 1);
            type_into_pane(pane_id, &head.text)?;
        }
        MessageBody::Prompt => {
            debug_assert!(
                batch
                    .iter()
                    .all(|message| message.body == MessageBody::Prompt)
            );
            let peers: Vec<&AgentState> = snapshot.root_agents().collect();
            let payload = batch
                .iter()
                .map(|message| {
                    match crate::harness::target::sender_prefix(
                        &message.sender,
                        &peers,
                        message.channel.as_deref(),
                    ) {
                        Some(prefix) => format!("{prefix}{}", message.text),
                        None => message.text.clone(),
                    }
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            paste_into_pane(pane_id, &payload)?;
        }
    }
    // Record the send once the text lands and before the submit keystroke, so a
    // submitted message is always preceded by its durable record and audit event.
    for message in batch {
        store.record_sent_message(message, &workspace.session_name)?;
    }
    if head.enter {
        press_pane_key(pane_id, NamedKey::Enter)?;
    }
    Ok(Outcome::Sent {
        label,
        message_id: head.message_id.clone(),
    })
}

pub fn handle_for_pane_target(
    snapshot: &SidebarSnapshot,
    target: &PaneAgent,
    bound: Option<&AgentState>,
) -> String {
    if let Some(agent) = bound {
        let peers: Vec<&AgentState> = snapshot.root_agents().collect();
        crate::harness::target::agent_handle(agent, &peers, true)
    } else {
        format!("@{}", target.label())
    }
}

pub fn compact_message_for_target(
    store: &Store,
    target: &PaneAgent,
    bound: Option<&AgentState>,
    prompt: &MessageRecord,
) -> Option<MessageRecord> {
    let threshold = prompt.auto_compact?;
    let agent = bound?;
    if !threshold.triggered(agent) {
        return None;
    }
    if agent.compacting_since.is_some() {
        return None;
    }
    let command = crate::agents::find_adapter(target.kind.as_str())?.compact_command()?;
    let occupied = agent.occupied_context_tokens();
    if let Some(used) = occupied
        && already_compacted_at(store, agent, command, used)
    {
        return None;
    }
    let mut record = message_for_target(
        prompt.workspace_id.clone(),
        target,
        bound,
        prompt.channel.as_deref(),
        MessageDraft {
            text: command.to_owned(),
            body: MessageBody::Command,
            address: prompt.address.clone(),
            enter: true,
            gate: prompt.gate,
            sender: MessageSender::System,
            automated: true,
            force: prompt.force,
            auto_compact: None,
            after: Vec::new(),
            when: Vec::new(),
        },
    );
    record.compacted_context_tokens = occupied;
    Some(record)
}

pub fn already_compacted_at(store: &Store, agent: &AgentState, command: &str, used: u64) -> bool {
    let live = store
        .list_messages()
        .map(|messages| {
            messages.iter().any(|message| {
                message.body == MessageBody::Command
                    && message.text == command
                    && message.compacted_context_tokens == Some(used)
                    && message.same_agent_card(agent)
            })
        })
        .unwrap_or(false);
    if live {
        return true;
    }
    agent.last_compact_command_tokens == Some(used)
}

/// The rollup session behind a bound pane target. A lazy pane carries no session,
/// so it never gates on Waiting or context compaction.
pub fn bound_agent<'a>(
    snapshot: &'a SidebarSnapshot,
    target: &PaneAgent,
) -> Option<&'a AgentState> {
    let agent_id = target.agent_id.as_ref()?;
    snapshot
        .agents
        .iter()
        .find(|agent| agent.kind == target.kind && &agent.agent_id == agent_id)
}

pub fn wait_for_message_until(
    store: &Store,
    message_id: &MessageId,
    session_name: &str,
    mut base: u64,
    deadline: Option<Instant>,
) -> Result<MessageStatus> {
    const POLL: Duration = Duration::from_millis(500);

    loop {
        if let Some(message) = store
            .list_messages()?
            .into_iter()
            .find(|message| message.message_id == *message_id)
        {
            if message.status == MessageStatus::Sent
                && deadline.is_some_and(|deadline| Instant::now() >= deadline)
            {
                let timed_out =
                    store.mark_message_timed_out(message_id, session_name, Some("wait"))?;
                return Ok(timed_out
                    .map(|message| message.status)
                    .unwrap_or(MessageStatus::TimedOut));
            }
        } else if let Some(status) = latest_terminal_message_status(store, message_id, &mut base)? {
            return Ok(status);
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Ok(MessageStatus::TimedOut);
        }
        let sleep = deadline.map_or(POLL, |deadline| {
            deadline.saturating_duration_since(Instant::now()).min(POLL)
        });
        std::thread::sleep(sleep);
    }
}

pub fn latest_terminal_message_status(
    store: &Store,
    message_id: &MessageId,
    base: &mut u64,
) -> Result<Option<MessageStatus>> {
    let mut latest = None;
    let path = &store.paths().events_log;
    let log_len = match std::fs::metadata(path) {
        Ok(meta) => meta.len(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => 0,
        Err(err) => {
            return Err(SendErr::Io {
                path: path.clone(),
                source: err,
            });
        }
    };
    if log_len < *base {
        *base = 0;
    }
    let (events, end) = event_log::read_from_offset(path, *base)?;
    *base = end;
    for event in events {
        let EventKind::Message { payload, .. } = event.kind() else {
            continue;
        };
        if payload.message_id == *message_id && payload.status.is_terminal() {
            latest = Some(payload.status);
        }
    }
    Ok(latest)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::agents::AgentStatus;
    use crate::ids::WorkspaceId;
    use crate::store::event::{EventEnvelope, MessageEventMethod};
    use crate::store::{RuntimePaths, StatePaths};
    fn agent() -> AgentState {
        let mut agent = AgentState::stub("claude", "sess-a", AgentStatus::Idle);
        agent.name = Some("lucid-atlas".to_owned());
        agent
    }

    fn delivered_message_event(message: &mut MessageRecord) -> EventEnvelope {
        message.status = MessageStatus::Delivered;
        EventEnvelope::message_event(message, "session", MessageEventMethod::Delivered, None)
    }

    #[test]
    fn terminal_message_poll_reads_only_appended_bytes_after_base() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        runtime.ensure_dirs().unwrap();
        let store = Store::open(paths.clone(), runtime).unwrap();
        let agent = agent();
        let mut old = MessageRecord::new(
            workspace_id.clone(),
            &agent,
            "old".to_owned(),
            true,
            DeliveryGate::Any,
        );
        event_log::append(&paths.events_log, &delivered_message_event(&mut old)).unwrap();
        let mut base = store.wait_fold_base().unwrap();

        let before = event_log::testkit::bytes_read();
        assert_eq!(
            latest_terminal_message_status(&store, &old.message_id, &mut base).unwrap(),
            None
        );
        assert_eq!(event_log::testkit::bytes_read() - before, 0);

        let mut message = MessageRecord::new(
            workspace_id,
            &agent,
            "new".to_owned(),
            true,
            DeliveryGate::Any,
        );
        event_log::append(&paths.events_log, &delivered_message_event(&mut message)).unwrap();
        let log_len = std::fs::metadata(&paths.events_log).unwrap().len();
        let appended = log_len - base;
        let before = event_log::testkit::bytes_read();

        assert_eq!(
            latest_terminal_message_status(&store, &message.message_id, &mut base).unwrap(),
            Some(MessageStatus::Delivered)
        );
        assert_eq!(event_log::testkit::bytes_read() - before, appended);
        assert_eq!(base, log_len);
    }

    #[test]
    fn pacer_skips_first_write_and_honors_configured_interval() {
        let mut pacer = Pacer::new(Duration::from_millis(40));
        let mut sleeps = Vec::new();

        assert!(!pacer.tick_with(|duration| sleeps.push(duration)));
        assert!(sleeps.is_empty());
        assert!(pacer.tick_with(|duration| sleeps.push(duration)));
        assert_eq!(sleeps, vec![Duration::from_millis(40)]);

        let mut zero = Pacer::new(Duration::ZERO);
        let mut zero_sleeps = Vec::new();
        for _ in 0..4 {
            assert!(!zero.tick_with(|duration| zero_sleeps.push(duration)));
        }
        assert!(zero_sleeps.is_empty());
    }
}
