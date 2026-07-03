//! Live-pane message send engine.

use std::path::PathBuf;
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::agents::AgentState;
use crate::feed::pending_ask_in_snapshot;
use crate::ids::{AgentSessionId, MessageId, WorkspaceId};
use crate::ledger::event::EventKind;
use crate::ledger::event_log;
use crate::message::{
    AutoCompact, DeliveryGate, MessageBody, MessageRecord, MessageSender, MessageStatus,
};
use crate::mux::{NamedKey, paste_into_pane, press_pane_key, type_into_pane};
use crate::workspace::ResolvedWorkspace;
use crate::{Ledger, PaneAgent, SidebarSnapshot};

pub type Result<T> = std::result::Result<T, SendErr>;

#[derive(Debug, thiserror::Error)]
pub enum SendErr {
    #[error(transparent)]
    Ledger(#[from] crate::ledger::LedgerErr),
    #[error(transparent)]
    EventLog(#[from] crate::ledger::event_log::EventLogErr),
    #[error("{0}")]
    Mux(#[from] crate::mux::MuxErr),
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// What happened to one live-pane send in a fan-out. Every resolved pane target
/// carries a live pane, so the only soft skip is a pending ask reserving the
/// next input.
pub enum Outcome {
    Sent {
        label: String,
        message_id: MessageId,
    },
    SkippedPending {
        label: String,
        message_id: MessageId,
        request_id: String,
    },
}

/// How a live-pane send is delivered: whether to send past a pending ask,
/// and pacing state.
pub struct LiveSend {
    pub force: bool,
    pub pacer: Pacer,
}

pub struct MessageDraft {
    pub text: String,
    pub body: MessageBody,
    pub enter: bool,
    pub gate: DeliveryGate,
    pub sender: MessageSender,
    pub force: bool,
    pub auto_compact: Option<AutoCompact>,
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
    .with_sender(draft.sender)
    .with_body(draft.body)
    .with_force(draft.force)
    .with_pane_id(target.pane_id.clone())
    .with_auto_compact(draft.auto_compact)
    .with_status(MessageStatus::Created)
}

pub fn send_batch_to_live_pane(
    workspace: &ResolvedWorkspace,
    ledger: &Ledger,
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
        let outcome = write_batch(workspace, ledger, snapshot, target, bound, batch, send)?;
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
        .find_map(|message| compact_message_for_target(ledger, target, bound, message));
    if let Some(command) = command {
        match write_batch(
            workspace,
            ledger,
            snapshot,
            target,
            bound,
            std::slice::from_ref(&command),
            send,
        ) {
            Ok(Outcome::Sent { message_id, .. }) => {
                compacted = Some(message_id);
            }
            Ok(skipped @ Outcome::SkippedPending { .. }) => {
                return Ok(SentPrompt {
                    outcome: skipped,
                    compacted: None,
                });
            }
            Err(err) => {
                ledger.record_send_error(&command, &err.to_string(), &workspace.session_name)?;
                return Err(err);
            }
        }
    }
    let outcome = write_batch(workspace, ledger, snapshot, target, bound, batch, send)?;
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
    ledger: &Ledger,
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
    if !send.force
        && let Some(agent) = bound
        && let Some(ask) = pending_ask_in_snapshot(agent, snapshot)
    {
        return Ok(Outcome::SkippedPending {
            label,
            message_id: head.message_id.clone(),
            request_id: ask.request_id.to_string(),
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
        ledger.record_sent_message(message, &workspace.session_name)?;
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
    ledger: &Ledger,
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
        && already_compacted_at(ledger, agent, command, used)
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
            enter: true,
            gate: prompt.gate,
            sender: prompt.sender.clone(),
            force: prompt.force,
            auto_compact: None,
        },
    );
    record.compacted_context_tokens = occupied;
    Some(record)
}

pub fn already_compacted_at(ledger: &Ledger, agent: &AgentState, command: &str, used: u64) -> bool {
    let live = ledger
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
/// so it never gates on pending asks or context compaction.
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
    ledger: &Ledger,
    message_id: &MessageId,
    session_name: &str,
    mut base: u64,
    deadline: Instant,
) -> Result<MessageStatus> {
    const POLL: Duration = Duration::from_millis(500);

    loop {
        if let Some(message) = ledger
            .list_messages()?
            .into_iter()
            .find(|message| message.message_id == *message_id)
        {
            if message.status == MessageStatus::Sent && Instant::now() >= deadline {
                let timed_out =
                    ledger.mark_message_timed_out(message_id, session_name, Some("wait"))?;
                return Ok(timed_out
                    .map(|message| message.status)
                    .unwrap_or(MessageStatus::TimedOut));
            }
        } else if let Some(status) = latest_terminal_message_status(ledger, message_id, &mut base)?
        {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            return Ok(MessageStatus::TimedOut);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        std::thread::sleep(remaining.min(POLL));
    }
}

pub fn latest_terminal_message_status(
    ledger: &Ledger,
    message_id: &MessageId,
    base: &mut u64,
) -> Result<Option<MessageStatus>> {
    let mut latest = None;
    let path = &ledger.paths().events_log;
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
    use std::time::Instant;

    use crate::agents::{AgentStatus, TurnPhase};
    use crate::ids::{AgentKind, WorkspaceId};
    use crate::ledger::event::{EventEnvelope, MessageEventMethod};
    use crate::ledger::{RuntimePaths, StatePaths};
    use jiff::Timestamp;

    fn agent() -> AgentState {
        let now = Timestamp::now();
        AgentState {
            agent_id: AgentSessionId::from("sess-a"),
            kind: AgentKind::new_unchecked("claude"),
            name: Some("lucid-atlas".to_owned()),
            kind_ordinal: Some(1),
            profile: None,
            role: None,
            team: None,
            launch_group: None,
            launch_ordinal: None,
            channel: None,
            status: AgentStatus::Idle,
            phase: TurnPhase::Idle,
            pane: None,
            runtime_owner: None,
            parent_agent_id: None,
            worktree_path: None,
            worktree_branch: None,
            task: None,
            prompt: None,
            description: None,
            transcript_path: None,
            origin: None,
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
            last_compact_command_tokens: None,
            last_seen: now,
            last_activity: now,
            registered_at: Some(now),
        }
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
        let ledger = Ledger::open(paths.clone(), runtime).unwrap();
        let agent = agent();
        let mut old = MessageRecord::new(
            workspace_id.clone(),
            &agent,
            "old".to_owned(),
            true,
            DeliveryGate::Any,
        );
        event_log::append(&paths.events_log, &delivered_message_event(&mut old)).unwrap();
        let mut base = ledger.wait_fold_base().unwrap();

        let before = event_log::testkit::bytes_read();
        assert_eq!(
            latest_terminal_message_status(&ledger, &old.message_id, &mut base).unwrap(),
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
            latest_terminal_message_status(&ledger, &message.message_id, &mut base).unwrap(),
            Some(MessageStatus::Delivered)
        );
        assert_eq!(event_log::testkit::bytes_read() - before, appended);
        assert_eq!(base, log_len);
    }

    #[test]
    fn pacer_sleeps_after_first_tick() {
        let mut pacer = Pacer::new(Duration::from_millis(40));

        assert!(!pacer.tick_with(|_| panic!("first tick must not sleep")));

        let second = Instant::now();
        pacer.tick();
        assert!(
            second.elapsed() >= Duration::from_millis(40),
            "second tick should sleep at least the configured interval"
        );
    }

    #[test]
    fn zero_interval_pacer_never_sleeps() {
        let mut pacer = Pacer::new(Duration::ZERO);

        for _ in 0..4 {
            assert!(!pacer.tick_with(|_| panic!("zero interval must not sleep")));
        }
    }
}
