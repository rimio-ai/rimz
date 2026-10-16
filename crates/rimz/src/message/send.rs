//! Live-pane payload construction, exclusive paced writes, and the durable Sent-before-submit barrier.

use std::path::PathBuf;
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::Store;
use crate::agents::{AgentState, AgentStatus, LifecycleSignal};
use crate::message::{
    DeliveryKind, InterruptConfirmation, MessageDraft, Recipient, command_segments,
    command_submit_delay_from_env, message_interval_from_env,
};
use crate::mux::PaneWriter;
use crate::pane::keys::NamedKey;
use crate::store::follow::{EventFollower, FollowEvent};
use crate::store::message::{AutoCompact, MessageBody, MessageRecord, MessageSender};
use crate::store::snapshot::{PaneAgent, SidebarSnapshot};
use crate::workspace::ResolvedWorkspace;

type Result<T> = std::result::Result<T, SendErr>;

#[derive(Debug, thiserror::Error)]
pub enum SendErr {
    #[error(transparent)]
    Hibernate(#[from] crate::harness::hibernate::HibernateErr),
    #[error(transparent)]
    Store(#[from] crate::store::StoreErr),
    #[error(transparent)]
    Atomic(#[from] crate::disk::atomic::AtomicErr),
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Mux(#[from] crate::mux::MuxErr),
    #[error(
        "{label} runs {kind}, which has no interrupt key; use --steer to write into the live turn, or park it"
    )]
    NoInterruptKey {
        label: String,
        kind: crate::ids::AgentKind,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Receipt {
    Sent {
        compacted: bool,
        interrupt: InterruptConfirmation,
    },
    SkippedWaiting,
    CompactionPending,
    ClaimLost,
}

/// How a live-pane send is delivered: whether to send past Waiting, and pacing
/// state.
pub(super) struct LiveSend {
    force: bool,
    pub(super) kind: DeliveryKind,
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
    crate::harness::hibernate::wake_pane(store, &target.pane_id)?;
    let writer = PaneWriter::open(
        store.runtime_paths(),
        &target.pane_id,
        &workspace.session_name,
    )?;
    // Decide under the pane lock: a resting agent gets a plain send, since the
    // key would cancel the very turn this paste starts.
    // Queued records can retain a provisional launch id; confirm the live card's session.
    let interrupt = if send.kind == DeliveryKind::Interrupt {
        let key = interrupt_key(&head.kind, &format!("@{}", target.label()))?;
        let current = store.snapshot_cached()?;
        current
            .agents
            .iter()
            .find(|agent| head.same_agent_card(agent) && agent.ended_at.is_none())
            .filter(|agent| {
                matches!(
                    agent.effective_status(),
                    AgentStatus::Running | AgentStatus::Waiting
                )
            })
            .map(|agent| (key, agent.agent_id.clone()))
    } else {
        None
    };
    let receipt = write_live(
        workspace, store, snapshot, target, bound, batch, send, &writer,
    )?;
    let (Some((key, agent_id)), Receipt::Sent { compacted, .. }) = (interrupt, receipt) else {
        return Ok(receipt);
    };
    sleep(super::interrupt_delay_from_env());
    let follower = crate::agents::spec_by_kind(head.kind.as_str())
        .is_some_and(|spec| spec.launch.confirm_interrupt)
        .then(|| EventFollower::open(store.paths().clone(), false));
    writer.press(key)?;
    drop(writer);
    let window = super::interrupt_confirmation_window_from_env();
    let interrupt = match follower {
        None => InterruptConfirmation::NotChecked,
        Some(Ok(follower)) => confirm_interrupt(follower, &head.kind, &agent_id, window),
        Some(Err(error)) => {
            tracing::debug!(%error, "cannot open interrupt confirmation cursor");
            InterruptConfirmation::NotConfirmed { window }
        }
    };
    Ok(Receipt::Sent {
        compacted,
        interrupt,
    })
}

fn confirm_interrupt(
    mut follower: EventFollower,
    kind: &crate::ids::AgentKind,
    agent_id: &crate::ids::AgentSessionId,
    window: Duration,
) -> InterruptConfirmation {
    let started = Instant::now();
    loop {
        let batch = match follower.poll() {
            Ok(batch) => batch,
            Err(error) => {
                tracing::debug!(%error, "cannot read interrupt confirmation");
                break;
            }
        };
        if batch.events.iter().any(|event| {
            matches!(event,
                FollowEvent::Lifecycle(event) if event.kind == *kind
                    && event.agent_id == *agent_id
                    && matches!(event.signal, LifecycleSignal::TurnInterrupted { .. })
            )
        }) {
            return InterruptConfirmation::Confirmed;
        }
        let remaining = window.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break;
        }
        sleep(remaining.min(Duration::from_millis(25)));
    }
    InterruptConfirmation::NotConfirmed { window }
}

#[expect(
    clippy::too_many_arguments,
    reason = "one live write under the caller's pane lock"
)]
fn write_live(
    workspace: &ResolvedWorkspace,
    store: &Store,
    snapshot: &SidebarSnapshot,
    target: &PaneAgent,
    bound: Option<&AgentState>,
    batch: &[MessageRecord],
    send: &mut LiveSend,
    writer: &PaneWriter,
) -> Result<Receipt> {
    let head = batch
        .first()
        .expect("send_batch_to_live_pane requires at least one message");
    if head.body == MessageBody::Command {
        debug_assert_eq!(batch.len(), 1);
        return Ok(
            match write_batch(workspace, store, snapshot, writer, bound, batch, send)? {
                PaneWrite::Sent => Receipt::Sent {
                    compacted: false,
                    interrupt: InterruptConfirmation::NotChecked,
                },
                PaneWrite::SkippedWaiting => Receipt::SkippedWaiting,
                PaneWrite::ClaimLost => Receipt::ClaimLost,
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
        let command = store.queue_claimed_message(
            &command,
            &workspace.session_name,
            jiff::Timestamp::now(),
        )?;
        match write_batch(
            workspace,
            store,
            snapshot,
            writer,
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
            Ok(PaneWrite::SkippedWaiting) => {
                store.release_message_claims(
                    std::slice::from_ref(&command),
                    "agent is waiting on input in its pane",
                    &workspace.session_name,
                )?;
                return Ok(Receipt::SkippedWaiting);
            }
            Ok(PaneWrite::ClaimLost) => {
                store.release_message_claims(
                    batch,
                    "pane write stopped: message claim no longer held",
                    &workspace.session_name,
                )?;
                return Ok(Receipt::ClaimLost);
            }
            Err(err) => {
                store.record_send_error(&command, &err.to_string(), &workspace.session_name)?;
                return Err(err);
            }
        }
    }
    Ok(
        match write_batch(workspace, store, snapshot, writer, bound, batch, send)? {
            PaneWrite::Sent => Receipt::Sent {
                compacted,
                interrupt: InterruptConfirmation::NotChecked,
            },
            PaneWrite::SkippedWaiting => Receipt::SkippedWaiting,
            PaneWrite::ClaimLost => Receipt::ClaimLost,
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
    if !store.message_claims_held(batch)? {
        store.release_message_claims(
            batch,
            "pane write stopped: message claim no longer held",
            &workspace.session_name,
        )?;
        return Ok(PaneWrite::ClaimLost);
    }
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
            let root_channel = crate::address::root_lane_channel(&workspace.project_root);
            let payload = batch
                .iter()
                .map(|message| {
                    match crate::address::message_header(
                        &message.sender,
                        &peers,
                        message.channel.as_deref(),
                        root_channel.as_deref(),
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
    let recorded = store.record_sent_batch(batch, &workspace.session_name)?;
    let unrecorded = batch
        .iter()
        .filter(|message| {
            !recorded
                .iter()
                .any(|sent| sent.message_id == message.message_id)
        })
        .map(|message| message.message_id.as_str())
        .collect::<Vec<_>>();
    if !unrecorded.is_empty() {
        tracing::warn!(
            ?unrecorded,
            "text written for messages finalized during the write"
        );
    }
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
    ClaimLost,
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
    fn interrupt_confirmation_keeps_superseded_events_and_matches_identity() {
        use crate::agents::{AgentLifecycleObservation, LifecycleSignal};
        use crate::disk::paths::{RuntimePaths, StatePaths};
        use crate::ids::{AgentKind, AgentSessionId, WorkspaceId};
        use crate::store::event::EventEnvelope;
        use crate::store::follow::EventFollower;

        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
        let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
        let store = Store::open(paths.clone(), runtime).unwrap();
        let append = |kind: &str, id: &str, signal| {
            let observation =
                AgentLifecycleObservation::new(Some(AgentSessionId::from(id)), signal);
            store
                .append_event(&EventEnvelope::agent_lifecycle(
                    workspace.clone(),
                    "room",
                    kind,
                    "test",
                    &observation,
                ))
                .unwrap();
        };
        let interrupted = || LifecycleSignal::TurnInterrupted {
            turn_id: Some("old".into()),
        };
        append("codex", "target", LifecycleSignal::Registered);
        append(
            "codex",
            "target",
            LifecycleSignal::TurnStarted {
                turn_id: Some("old".into()),
            },
        );
        append("codex", "target", interrupted());
        let follower = EventFollower::open(paths.clone(), false).unwrap();
        append("claude", "target", interrupted());
        append("codex", "other", interrupted());
        let kind = AgentKind::new_unchecked("codex");
        let id = AgentSessionId::from("target");
        assert_eq!(
            confirm_interrupt(follower, &kind, &id, Duration::ZERO),
            super::super::InterruptConfirmation::NotConfirmed {
                window: Duration::ZERO
            }
        );
        append(
            "codex",
            "target",
            LifecycleSignal::TurnStarted {
                turn_id: Some("old".into()),
            },
        );
        let follower = EventFollower::open(paths, false).unwrap();
        append("codex", "target", interrupted());
        append(
            "codex",
            "target",
            LifecycleSignal::TurnStarted {
                turn_id: Some("new".into()),
            },
        );
        let snapshot = store.snapshot_cached().unwrap();
        let target = snapshot
            .agents
            .iter()
            .find(|agent| agent.kind == kind && agent.agent_id == id)
            .unwrap();
        assert_eq!(target.started_turn_id.as_deref(), Some("new"));
        assert_eq!(target.interrupted_turn_id, None);
        assert_eq!(
            confirm_interrupt(follower, &kind, &id, Duration::ZERO),
            super::super::InterruptConfirmation::Confirmed
        );
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
