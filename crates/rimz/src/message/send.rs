//! Live-pane message send engine.

use std::thread::sleep;
use std::time::Duration;

use crate::agents::AgentState;
use crate::ids::{AgentSessionId, MessageId, WorkspaceId};
use crate::message::{
    AfterCondition, AutoCompact, DeliveryGate, MessageBody, MessageRecord, MessageSender,
    MessageStatus, WhenCondition,
};
use crate::mux::{NamedKey, paste_into_pane, press_pane_key, type_into_pane};
use crate::workspace::ResolvedWorkspace;
use crate::{PaneAgent, SidebarSnapshot, Store};

pub type Result<T> = std::result::Result<T, SendErr>;

#[derive(Debug, thiserror::Error)]
pub enum SendErr {
    #[error(transparent)]
    Store(#[from] crate::store::StoreErr),
    #[error("{0}")]
    Mux(#[from] crate::mux::MuxErr),
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
    pub not_before: Option<jiff::Timestamp>,
    pub after: Vec<AfterCondition>,
    pub when: Vec<WhenCondition>,
}

pub enum Recipient<'a> {
    Agent {
        agent: &'a AgentState,
        pane: Option<&'a PaneAgent>,
    },
    Pane {
        pane: &'a PaneAgent,
        bound: Option<&'a AgentState>,
    },
}

impl MessageDraft {
    pub fn into_record(
        self,
        workspace_id: WorkspaceId,
        recipient: Recipient<'_>,
        scope_channel: Option<&str>,
    ) -> MessageRecord {
        match recipient {
            Recipient::Agent { agent, pane } => {
                let channel = crate::harness::target::agent_channel(agent).or_else(|| {
                    pane.and_then(|pane| {
                        crate::harness::target::recipient_channel(pane, Some(agent), scope_channel)
                    })
                });
                let record =
                    MessageRecord::new(workspace_id, agent, self.text, self.enter, self.gate)
                        .with_body(self.body)
                        .with_force(self.force)
                        .with_address(self.address)
                        .with_channel(channel)
                        .with_sender(self.sender)
                        .with_automated(self.automated)
                        .with_auto_compact(self.auto_compact)
                        .with_not_before(self.not_before)
                        .with_after(self.after)
                        .with_when(self.when);
                if let Some(pane) = pane {
                    record.with_pane_id(pane.pane_id.clone())
                } else {
                    record
                }
            }
            Recipient::Pane { pane, bound } => {
                let agent_id = bound
                    .map(|agent| agent.agent_id.clone())
                    .or_else(|| pane.agent_id.clone())
                    .unwrap_or_else(|| synthetic_session_for_pane(&pane.pane_id));
                let agent_name = bound
                    .and_then(|agent| agent.name.clone())
                    .or_else(|| pane.name.clone());
                MessageRecord::new_for_card(
                    workspace_id,
                    pane.kind.clone(),
                    agent_id,
                    agent_name,
                    self.text,
                    self.enter,
                    self.gate,
                )
                .with_channel(crate::harness::target::recipient_channel(
                    pane,
                    bound,
                    scope_channel,
                ))
                .with_address(self.address)
                .with_sender(self.sender)
                .with_automated(self.automated)
                .with_body(self.body)
                .with_force(self.force)
                .with_pane_id(pane.pane_id.clone())
                .with_auto_compact(self.auto_compact)
                .with_not_before(self.not_before)
                .with_after(self.after)
                .with_when(self.when)
                .with_status(MessageStatus::Queued)
            }
        }
    }
}

pub struct SentPrompt {
    pub outcome: Outcome,
    pub compacted: Option<MessageId>,
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
    let mut record = MessageDraft {
        text: command.to_owned(),
        body: MessageBody::Command,
        address: prompt.address.clone(),
        enter: true,
        gate: prompt.gate,
        sender: MessageSender::System,
        automated: true,
        force: prompt.force,
        auto_compact: None,
        not_before: None,
        after: Vec::new(),
        when: Vec::new(),
    }
    .into_record(
        prompt.workspace_id.clone(),
        Recipient::Pane {
            pane: target,
            bound,
        },
        prompt.channel.as_deref(),
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::agents::AgentStatus;
    use crate::ids::{MuxName, PaneId, WorkspaceId};
    fn agent() -> AgentState {
        let mut agent = AgentState::stub("claude", "sess-a", AgentStatus::Idle);
        agent.name = Some("lucid-atlas".to_owned());
        agent
    }

    fn draft(not_before: Option<jiff::Timestamp>) -> MessageDraft {
        MessageDraft {
            text: "hello".to_owned(),
            body: MessageBody::Prompt,
            address: Some("@claude".to_owned()),
            enter: true,
            gate: DeliveryGate::Done,
            sender: MessageSender::Human,
            automated: false,
            force: false,
            auto_compact: None,
            not_before,
            after: Vec::new(),
            when: Vec::new(),
        }
    }

    #[test]
    fn draft_record_uses_recipient_identity_and_live_pane_context() {
        let agent = agent();
        let pane = PaneAgent {
            kind: agent.kind.clone(),
            kind_ordinal: agent.kind_ordinal,
            name: agent.name.clone(),
            name_explicit: true,
            profile: None,
            role: None,
            channel: Some("auth".to_owned()),
            agent_id: Some(agent.agent_id.clone()),
            pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_1"),
            pane_pid: None,
            worktree_path: None,
            worktree_branch: None,
        };
        let workspace_id = WorkspaceId::from_project_root(std::path::Path::new("/repo"));
        let not_before = jiff::Timestamp::now();

        let live = draft(Some(not_before)).into_record(
            workspace_id.clone(),
            Recipient::Agent {
                agent: &agent,
                pane: Some(&pane),
            },
            None,
        );
        let parked = draft(Some(not_before)).into_record(
            workspace_id,
            Recipient::Agent {
                agent: &agent,
                pane: None,
            },
            None,
        );

        assert_eq!(live.agent_id, agent.agent_id);
        assert_eq!(live.pane_id.as_ref(), Some(&pane.pane_id));
        assert_eq!(live.channel.as_deref(), Some("auth"));
        assert_eq!(live.not_before, Some(not_before));
        assert_eq!(parked.pane_id, None);
        assert_eq!(parked.channel, None);
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
