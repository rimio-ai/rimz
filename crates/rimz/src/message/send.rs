//! Live-pane payload construction, exclusive paced writes, and the durable Sent-before-submit barrier.

use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::Store;
use crate::agents::{AgentState, AgentStatus};
use crate::message::{
    DeliveryKind, MessageDraft, Recipient, command_segments, command_submit_delay_from_env,
    message_interval_from_env,
};
use crate::mux::PaneWriter;
use crate::pane::keys::NamedKey;
use crate::store::message::{
    AutoCompact, MessageBody, MessageRecord, MessageSender, MessageStatus,
};
use crate::store::snapshot::{PaneAgent, SidebarSnapshot};
use crate::workspace::ResolvedWorkspace;

type Result<T> = std::result::Result<T, SendErr>;

#[derive(Debug, thiserror::Error)]
pub enum SendErr {
    #[error(transparent)]
    Store(#[from] crate::store::StoreErr),
    #[error("{0}")]
    Mux(#[from] crate::mux::MuxErr),
    #[error(
        "{label} runs {kind}, which has no interrupt key; use --steer to write into the live turn, or park it"
    )]
    NoInterruptKey {
        label: String,
        kind: crate::ids::AgentKind,
    },
    #[error("`{label}` cannot receive now and has no durable session to park")]
    NoDurableSession { label: String },
    #[error("agent started another turn before delivery")]
    TurnRestarted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Receipt {
    Sent { compacted: bool },
    SkippedWaiting,
    CompactionPending,
    ClaimSuperseded,
    InterruptUnproven { wait: Duration, key: NamedKey },
}

/// How a live-pane send is delivered: whether to send past Waiting, and pacing
/// state.
pub(super) struct LiveSend {
    force: bool,
    kind: DeliveryKind,
    pacer: Pacer,
    command_submit_delay: Duration,
}

impl LiveSend {
    /// Pacing comes from the environment; one value per send batch.
    pub(super) fn new(force: bool, kind: DeliveryKind) -> Self {
        Self {
            force,
            kind,
            pacer: Pacer::from_env(),
            command_submit_delay: command_submit_delay_from_env(),
        }
    }

    fn pause_raw_typing(&self, body: MessageBody) {
        self.pause_raw_typing_with(body, sleep);
    }

    fn pause_raw_typing_with(&self, body: MessageBody, sleeper: impl FnOnce(Duration)) -> bool {
        let should_sleep = body == MessageBody::Command && !self.command_submit_delay.is_zero();
        if should_sleep {
            sleeper(self.command_submit_delay);
        }
        should_sleep
    }
}

pub(super) fn send_batch_to_live_pane(
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
    if send.kind == DeliveryKind::Interrupt
        && let Some(receipt) = interrupt_before_send(store, target, head, send.force)?
    {
        return Ok(receipt);
    }
    let writer = PaneWriter::open(store.runtime_paths(), &target.pane_id)?;
    if send.kind == DeliveryKind::Interrupt
        && !store.list_messages()?.iter().any(|message| {
            message.message_id == head.message_id
                && message.status == MessageStatus::Claimed
                && message.last_attempt_at == head.last_attempt_at
        })
    {
        return Ok(Receipt::ClaimSuperseded);
    }
    let current = (send.kind == DeliveryKind::Interrupt)
        .then(|| store.snapshot_cached())
        .transpose()?;
    let (snapshot, bound) = if let Some(current) = current.as_ref() {
        let agent = interrupt_agent(current, head, &target.label())?;
        // Another writer may have opened a turn while we waited for this lock.
        if matches!(
            agent.effective_status(),
            AgentStatus::Running | AgentStatus::Waiting
        ) {
            return Err(SendErr::TurnRestarted);
        }
        (current, Some(agent))
    } else {
        (snapshot, bound)
    };
    if head.body == MessageBody::Command {
        debug_assert_eq!(batch.len(), 1);
        return Ok(
            match write_batch(workspace, store, snapshot, &writer, bound, batch, send)? {
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
            &writer,
            bound,
            std::slice::from_ref(&command),
            send,
        ) {
            Ok(PaneWrite::Sent) => {
                compacted = true;
                let peers = crate::address::addressable_agents(snapshot);
                crate::harness::assist_log::append(&crate::harness::assist_log::AssistRecord {
                    at: jiff::Timestamp::now(),
                    assist: crate::harness::assist_log::Assist::AutoCompact {
                        kind: target.kind.clone(),
                        agent_id: agent.agent_id.clone(),
                        label: Some(crate::address::agent_handle(agent, &peers, false)),
                        threshold,
                        occupied_tokens: command.compacted_context_tokens,
                        message_id: command.message_id.to_string(),
                    },
                });
                if send.kind == DeliveryKind::Boundary {
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
        match write_batch(workspace, store, snapshot, &writer, bound, batch, send)? {
            PaneWrite::Sent => Receipt::Sent { compacted },
            PaneWrite::SkippedWaiting => Receipt::SkippedWaiting,
        },
    )
}

pub(super) fn interrupt_key(kind: &crate::ids::AgentKind, label: &str) -> Result<NamedKey> {
    crate::agents::spec_by_kind(kind.as_str())
        .and_then(|spec| spec.launch.interrupt_key)
        .ok_or_else(|| SendErr::NoInterruptKey {
            label: label.to_owned(),
            kind: kind.clone(),
        })
}

fn interrupt_agent<'a>(
    snapshot: &'a SidebarSnapshot,
    message: &MessageRecord,
    label: &str,
) -> Result<&'a AgentState> {
    snapshot
        .agents
        .iter()
        .find(|agent| message.same_agent_card(agent) && agent.ended_at.is_none())
        .ok_or_else(|| SendErr::NoDurableSession {
            label: format!("@{label}"),
        })
}

fn interrupt_before_send(
    store: &Store,
    target: &PaneAgent,
    head: &MessageRecord,
    force: bool,
) -> Result<Option<Receipt>> {
    let key = interrupt_key(&head.kind, &format!("@{}", target.label()))?;
    {
        let writer = PaneWriter::open(store.runtime_paths(), &target.pane_id)?;
        let snapshot = store.snapshot_cached()?;
        let agent = interrupt_agent(&snapshot, head, &target.label())?;
        if !force && agent.effective_status() == AgentStatus::Waiting {
            return Ok(Some(Receipt::SkippedWaiting));
        }
        if !matches!(
            agent.effective_status(),
            AgentStatus::Running | AgentStatus::Waiting
        ) {
            return Ok(None);
        }
        writer.press(key)?;
    }
    let wait = super::interrupt_wait_from_env();
    let started = Instant::now();
    loop {
        let snapshot = store.snapshot_cached()?;
        let agent = interrupt_agent(&snapshot, head, &target.label())?;
        if !matches!(
            agent.effective_status(),
            AgentStatus::Running | AgentStatus::Waiting
        ) {
            return Ok(None);
        }
        let remaining = wait.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Ok(Some(Receipt::InterruptUnproven { wait, key }));
        }
        sleep(remaining.min(Duration::from_millis(250)));
    }
}

pub struct Pacer {
    interval: Duration,
    started: bool,
}

impl Pacer {
    /// Pace pane writes using `RIMZ_MESSAGE_INTERVAL_MS`.
    pub fn from_env() -> Self {
        Self::new(message_interval_from_env())
    }

    fn new(interval: Duration) -> Self {
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
    writer: &PaneWriter,
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
    send.pacer.tick();
    match head.body {
        MessageBody::Command => {
            debug_assert_eq!(batch.len(), 1);
            let declared = crate::agents::spec_by_kind(head.kind.as_str())
                .and_then(|spec| spec.launch.compact_command)
                .map(|compact| compact.command);
            type_command_with(
                &head.text,
                declared,
                send,
                |segment| writer.type_text(segment),
                sleep,
            )?;
        }
        MessageBody::Prompt => {
            debug_assert!(
                batch
                    .iter()
                    .all(|message| message.body == MessageBody::Prompt)
            );
            let peers = crate::address::addressable_agents(snapshot);
            let payload = batch
                .iter()
                .map(|message| {
                    match crate::address::message_header(
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
            writer.paste(&payload)?;
        }
    }
    // Record the send once the text lands and before the submit keystroke, so a
    // submitted message is always preceded by its durable record and audit event.
    store.record_sent_batch(batch, &workspace.session_name)?;
    if head.enter {
        // Raw-typed commands carry no paste close marker, so wait for composer
        // paste-burst state to flush before submitting.
        send.pause_raw_typing(head.body);
        writer.press(NamedKey::Enter)?;
    }
    Ok(PaneWrite::Sent)
}

fn type_command_with<E>(
    text: &str,
    declared: Option<&str>,
    send: &LiveSend,
    mut write: impl FnMut(&str) -> std::result::Result<(), E>,
    sleeper: impl FnOnce(Duration),
) -> std::result::Result<(), E> {
    let (token, arguments) =
        declared.map_or((text, None), |command| command_segments(text, command));
    write(token)?;
    #[cfg(feature = "testkit")]
    crate::testkit::rendezvous("RIMZ_TEST_COMMAND_TOKEN_WRITTEN");
    if let Some(arguments) = arguments {
        send.pause_raw_typing_with(MessageBody::Command, sleeper);
        write(arguments)?;
    }
    Ok(())
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
    if agent.compaction_unprompted(jiff::Timestamp::now()) {
        return None;
    }
    let config = crate::config::MachineConfig::load_lenient();
    let command = crate::agents::compact_command(agent, &config.harness)?;
    let occupied = agent.occupied_context_tokens();
    if let Some(used) = occupied
        && already_compacted_at(store, agent, used)
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
        &command,
        prompt.address.as_deref(),
    );
    record.compacted_context_tokens = occupied;
    Some((record, threshold, agent))
}

pub fn already_compacted_at(store: &Store, agent: &AgentState, used: u64) -> bool {
    let live = store
        .list_messages()
        .map(|messages| {
            messages.iter().any(|message| {
                message.body == MessageBody::Command
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
            kind: DeliveryKind::Boundary,
            pacer: Pacer::new(Duration::ZERO),
            command_submit_delay: Duration::from_millis(200),
        };
        let mut sleeps = Vec::new();

        assert!(
            !send.pause_raw_typing_with(MessageBody::Prompt, |duration| {
                sleeps.push(duration);
            })
        );
        assert!(
            send.pause_raw_typing_with(MessageBody::Command, |duration| {
                sleeps.push(duration);
            })
        );
        assert!(
            send.pause_raw_typing_with(MessageBody::Command, |duration| {
                sleeps.push(duration);
            })
        );
        assert_eq!(sleeps, vec![Duration::from_millis(200); 2]);

        let no_delay = LiveSend {
            command_submit_delay: Duration::ZERO,
            ..send
        };
        assert!(!no_delay.pause_raw_typing_with(MessageBody::Command, |_| {
            panic!("zero command delay must not sleep");
        }));
    }

    #[test]
    fn argument_write_is_paced_from_the_declared_command() {
        let send = LiveSend {
            force: false,
            kind: DeliveryKind::Boundary,
            pacer: Pacer::new(Duration::ZERO),
            command_submit_delay: Duration::from_millis(200),
        };
        let events = std::cell::RefCell::new(Vec::new());

        type_command_with(
            "/compact preserve context",
            Some("/compact"),
            &send,
            |segment| {
                events.borrow_mut().push(format!("write:{segment}"));
                Ok::<_, ()>(())
            },
            |duration| events.borrow_mut().push(format!("sleep:{duration:?}")),
        )
        .expect("command writes");

        assert_eq!(
            events.into_inner(),
            ["write:/compact ", "sleep:200ms", "write:preserve context"]
        );
    }
}
