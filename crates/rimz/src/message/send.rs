//! Live-pane payload construction, paced writes, and the durable Sent-before-submit barrier.

use std::thread::sleep;
use std::time::Duration;

use crate::Store;
use crate::agents::AgentState;
use crate::message::{
    AutoCompact, MessageBody, MessageDraft, MessageRecord, MessageSender, Recipient,
};
use crate::mux::{NamedKey, paste_into_pane, press_pane_key, type_into_pane};
use crate::store::snapshot::{PaneAgent, SidebarSnapshot};
use crate::workspace::ResolvedWorkspace;

pub type Result<T> = std::result::Result<T, SendErr>;

#[derive(Debug, thiserror::Error)]
pub enum SendErr {
    #[error(transparent)]
    Store(#[from] crate::store::StoreErr),
    #[error("{0}")]
    Mux(#[from] crate::mux::MuxErr),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Receipt {
    Sent { compacted: bool },
    SkippedWaiting,
    CompactionPending,
}

/// How a live-pane send is delivered: whether to send past Waiting, and pacing
/// state.
pub(crate) struct LiveSend {
    pub force: bool,
    pub steer: bool,
    pub pacer: Pacer,
    pub command_submit_delay: Duration,
}

impl LiveSend {
    fn wait_before_submit(&self, body: MessageBody) {
        self.wait_before_submit_with(body, sleep);
    }

    fn wait_before_submit_with(&self, body: MessageBody, sleeper: impl FnOnce(Duration)) -> bool {
        let should_sleep = body == MessageBody::Command && !self.command_submit_delay.is_zero();
        if should_sleep {
            sleeper(self.command_submit_delay);
        }
        should_sleep
    }
}

pub(crate) fn send_batch_to_live_pane(
    workspace: &ResolvedWorkspace,
    store: &Store,
    snapshot: &SidebarSnapshot,
    target: &PaneAgent,
    bound: Option<&AgentState>,
    batch: &[MessageRecord],
    send: &mut LiveSend,
) -> Result<Receipt> {
    // Dispatch and delivery claims always pass a non-empty batch.
    let head = batch
        .first()
        .expect("send_batch_to_live_pane requires at least one message");
    if head.body == MessageBody::Command {
        debug_assert_eq!(batch.len(), 1);
        return Ok(
            match write_batch(workspace, store, snapshot, target, bound, batch, send)? {
                PaneWrite::Sent => Receipt::Sent { compacted: false },
                PaneWrite::SkippedWaiting => Receipt::SkippedWaiting,
            },
        );
    }
    debug_assert!(
        batch
            .iter()
            .all(|message| message.body == MessageBody::Prompt)
    );
    let mut compacted = false;
    let compact = batch
        .iter()
        .find_map(|message| compact_message_for_target(store, target, bound, message));
    if let Some((command, threshold, agent)) = compact {
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
            Ok(PaneWrite::Sent) => {
                compacted = true;
                let peers = crate::harness::target::addressable_agents(snapshot);
                crate::harness::assist_log::append(&crate::harness::assist_log::AssistRecord {
                    at: jiff::Timestamp::now(),
                    assist: crate::harness::assist_log::Assist::AutoCompact {
                        kind: target.kind.clone(),
                        agent_id: agent.agent_id.clone(),
                        label: Some(crate::harness::target::agent_handle(agent, &peers, false)),
                        threshold,
                        occupied_tokens: command.compacted_context_tokens,
                        message_id: command.message_id.to_string(),
                    },
                });
                if !send.steer {
                    return Ok(Receipt::CompactionPending);
                }
            }
            Ok(PaneWrite::SkippedWaiting) => return Ok(Receipt::SkippedWaiting),
            Err(err) => {
                store.record_send_error(&command, &err.to_string(), &workspace.session_name)?;
                return Err(err);
            }
        }
    }
    Ok(
        match write_batch(workspace, store, snapshot, target, bound, batch, send)? {
            PaneWrite::Sent => Receipt::Sent { compacted },
            PaneWrite::SkippedWaiting => Receipt::SkippedWaiting,
        },
    )
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
) -> Result<PaneWrite> {
    // Dispatch and delivery claims always pass a non-empty batch.
    let head = batch
        .first()
        .expect("write_batch requires at least one message");
    debug_assert!(batch.iter().all(|message| message.enter == head.enter));
    if !send.force && bound.is_some_and(AgentState::is_awaiting_input) {
        return Ok(PaneWrite::SkippedWaiting);
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
            let peers = crate::harness::target::addressable_agents(snapshot);
            let payload = batch
                .iter()
                .map(|message| {
                    match crate::harness::target::message_header(
                        &message.sender,
                        &peers,
                        message.channel.as_deref(),
                    ) {
                        Some(header) => format!("{header}{}", message.text),
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
    store.record_sent_batch(batch, &workspace.session_name)?;
    if head.enter {
        // Raw-typed commands carry no paste close marker. Codex groups chars
        // arriving within 8 ms into a paste burst and suppresses Enter for
        // another 120 ms; wait for that state to flush before submitting.
        send.wait_before_submit(head.body);
        press_pane_key(pane_id, NamedKey::Enter)?;
    }
    Ok(PaneWrite::Sent)
}

enum PaneWrite {
    Sent,
    SkippedWaiting,
}

fn compact_message_for_target<'a>(
    store: &Store,
    target: &PaneAgent,
    bound: Option<&'a AgentState>,
    prompt: &MessageRecord,
) -> Option<(MessageRecord, AutoCompact, &'a AgentState)> {
    let threshold = prompt.auto_compact?;
    let agent = bound?;
    if !threshold.triggered(agent) {
        return None;
    }
    if agent.compacting_since.is_some() {
        return None;
    }
    let command = crate::agents::spec_by_kind(target.kind.as_str())?
        .launch
        .compact_command()?;
    let occupied = agent.occupied_context_tokens();
    if let Some(used) = occupied
        && already_compacted_at(store, agent, command, used)
    {
        return None;
    }
    let mut record = MessageDraft {
        body: MessageBody::Command,
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
    .record(
        prompt.workspace_id.clone(),
        Recipient::Pane {
            pane: target,
            bound,
        },
        prompt.channel.as_deref(),
        command,
        prompt.address.as_deref(),
    );
    record.compacted_context_tokens = occupied;
    Some((record, threshold, agent))
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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn submit_delay_applies_only_to_commands() {
        let send = LiveSend {
            force: false,
            steer: false,
            pacer: Pacer::new(Duration::ZERO),
            command_submit_delay: Duration::from_millis(200),
        };
        let mut sleeps = Vec::new();

        assert!(
            !send.wait_before_submit_with(MessageBody::Prompt, |duration| {
                sleeps.push(duration);
            })
        );
        assert!(
            send.wait_before_submit_with(MessageBody::Command, |duration| {
                sleeps.push(duration);
            })
        );
        assert_eq!(sleeps, vec![Duration::from_millis(200)]);

        let no_delay = LiveSend {
            command_submit_delay: Duration::ZERO,
            ..send
        };
        assert!(
            !no_delay.wait_before_submit_with(MessageBody::Command, |_| {
                panic!("zero command delay must not sleep");
            })
        );
    }
}
