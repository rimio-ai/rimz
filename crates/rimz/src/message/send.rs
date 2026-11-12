//! Live-pane payload construction, exclusive paced writes, and the durable Sent-before-submit barrier.

use std::thread::sleep;
use std::time::{Duration, Instant};

use jiff::Timestamp;

use crate::Store;
use crate::agents::{AgentState, AgentStatus, LifecycleSignal};
use crate::harness::auto_continue::CardStatus;
use crate::message::{
    DeliveryKind, InterruptConfirmation, MessageDraft, Recipient, command_segments,
    command_submit_delay_from_env, message_interval_from_env,
};
use crate::mux::PaneWriter;
use crate::mux::typing::{InputSource, TypingRecord, TypingView};
use crate::pane::keys::NamedKey;
use crate::store::follow::{EventFollower, FollowEvent};
use crate::store::message::{
    AutoCompact, HarnessNotice, MessageBody, MessageRecord, MessageSender,
};
use crate::store::snapshot::{PaneAgent, SidebarSnapshot};
use crate::workspace::ResolvedWorkspace;

type Result<T> = std::result::Result<T, SendErr>;

#[derive(Debug, thiserror::Error)]
pub enum SendErr {
    #[error(transparent)]
    Hibernate(#[from] crate::harness::hibernate::HibernateErr),
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
    #[error("someone is using the receiver's pane; retry at {until}")]
    TypingHeld { until: Timestamp },
    #[error("not sent: {reason}")]
    Deferred { reason: String },
}

/// Why a boundary write stopped on the row read after its waits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Deferral {
    Status(AgentStatus),
    ProviderStarting,
}

impl std::fmt::Display for Deferral {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Status(status) => write!(f, "the receiver is {}", status.as_str()),
            Self::ProviderStarting => f.write_str("the receiver's provider is still starting"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Receipt {
    Sent {
        compacted: bool,
        interrupt: InterruptConfirmation,
    },
    SkippedWaiting(super::InputWait),
    CompactionPending,
    ClaimLost {
        compacted: bool,
    },
    TypingHeld {
        until: Timestamp,
        compacted: bool,
    },
    Deferred(Deferral),
    /// A cache keepalive whose cache went cold before its write: the ping would only pay to
    /// rebuild it.
    CacheCold,
}

/// How a live-pane send is delivered: whether to send past Waiting, and pacing
/// state.
pub(super) struct LiveSend {
    pub(super) force: bool,
    pub(super) kind: DeliveryKind,
    /// A forced cache keepalive: the typing hold is the one check it skips.
    pub(super) past_typing_hold: bool,
    pacer: Pacer,
    command_submit_delay: Duration,
}

impl LiveSend {
    /// Pacing comes from the environment; one value per send batch.
    pub(super) fn new(force: bool, kind: DeliveryKind) -> Self {
        Self {
            force,
            kind,
            past_typing_hold: false,
            pacer: Pacer::from_env(),
            command_submit_delay: command_submit_delay_from_env(),
        }
    }

    pub(super) fn typing_hold_until(
        &self,
        workspace: &ResolvedWorkspace,
        runtime: &crate::RuntimePaths,
        pane: &crate::ids::PaneId,
    ) -> Option<Timestamp> {
        if self.past_typing_hold || (self.kind == DeliveryKind::Interrupt && self.force) {
            return None;
        }
        let hold = crate::config::MachineConfig::load_lenient()
            .harness
            .typing_hold?;
        let read_view = || match crate::mux::backend_for(pane.mux()).client_view(
            crate::mux::ClientFocusOptions {
                session_name: Some(workspace.session_name.clone()),
                command_timeout: Some(Duration::from_millis(500)),
            },
        ) {
            Ok(view) => Some(view),
            Err(error) => {
                tracing::debug!(%error, %pane, "cannot read typing hold; sending without it");
                None
            }
        };
        let until = if InputSource::uses_tty() {
            let sample = match TypingRecord::load(runtime).sample(pane) {
                Ok(sample) => sample,
                Err(error) => {
                    tracing::debug!(
                        %error, %pane, "cannot stat typing tty; sending without hold"
                    );
                    return None;
                }
            };
            let started_at = Instant::now();
            let view = read_view()?;
            let clients = view.typing_clients();
            let view = TypingView::for_mux(pane.mux(), &clients, &view.presence.client_input_ms);
            match TypingRecord::update(runtime, |record| {
                record.observe(
                    pane,
                    &sample,
                    &view,
                    Some(started_at),
                    Timestamp::now().as_millisecond().try_into().ok()?,
                );
                InputSource::Tty(record).deadline(pane, Some(hold))
            }) {
                Ok(until) => until,
                Err(error) => {
                    tracing::debug!(
                        %error, %pane, "cannot update typing record; sending without hold"
                    );
                    None
                }
            }
        } else {
            let view = read_view()?;
            InputSource::ClientClocks {
                clients: &view.clients,
                input_ms: &view.presence.client_input_ms,
            }
            .deadline(pane, Some(hold))
        };
        let until = i64::try_from(until?).ok()?;
        Timestamp::from_millisecond(until)
            .ok()
            .filter(|until| *until > Timestamp::now())
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

#[expect(
    clippy::too_many_arguments,
    reason = "one live write under its own pane lock"
)]
pub(super) fn send_batch_to_live_pane(
    workspace: &ResolvedWorkspace,
    store: &Store,
    snapshot: &SidebarSnapshot,
    target: &PaneAgent,
    bound: Option<&AgentState>,
    card_status: &CardStatus,
    batch: &[MessageRecord],
    send: &mut LiveSend,
) -> Result<Receipt> {
    // Dispatch and delivery claims always pass a non-empty batch.
    let head = batch
        .first()
        .expect("send_batch_to_live_pane requires at least one message");
    let key = match send.kind {
        DeliveryKind::Interrupt => {
            Some(interrupt_key(&head.kind, &format!("@{}", target.label()))?)
        }
        DeliveryKind::Boundary | DeliveryKind::Steer => None,
    };
    crate::harness::hibernate::wake_pane(store, &target.pane_id)?;
    let writer = PaneWriter::open(
        Some(store.runtime_paths()),
        &target.pane_id,
        &workspace.session_name,
    )?;
    let (receipt, row) = write_live(
        workspace,
        store,
        snapshot,
        target,
        bound,
        card_status,
        batch,
        send,
        &writer,
    )?;
    let (Some(key), Receipt::Sent { compacted, .. }) = (key, receipt) else {
        return Ok(receipt);
    };
    // Decide on the row read after the write's waits: a resting agent gets a
    // plain send, since the key would cancel the very turn this paste starts.
    // Queued records can retain a provisional launch id; the row carries the live card's session.
    let Some(agent_id) = row
        .filter(|agent| {
            matches!(
                agent.effective_status(Timestamp::now()),
                AgentStatus::Running | AgentStatus::Waiting
            )
        })
        .map(|agent| agent.agent_id)
    else {
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

/// Write a batch, returning the receipt and the receiver row its last write
/// decided on.
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
    card_status: &CardStatus,
    batch: &[MessageRecord],
    send: &mut LiveSend,
    writer: &PaneWriter,
) -> Result<(Receipt, Option<AgentState>)> {
    let head = batch
        .first()
        .expect("send_batch_to_live_pane requires at least one message");
    let decide = |send: &mut LiveSend, compacted: bool| {
        wait_and_decide(
            workspace,
            store,
            snapshot,
            bound,
            card_status,
            batch,
            compacted,
            send,
        )
    };
    let fresh = match decide(send, false)? {
        Decided::Write(row) => row,
        Decided::Stop(receipt) => return Ok((receipt, None)),
    };
    let sent = |compacted| Receipt::Sent {
        compacted,
        interrupt: InterruptConfirmation::NotChecked,
    };
    if head.body == MessageBody::Command {
        debug_assert_eq!(batch.len(), 1);
        write_text(workspace, store, snapshot, writer, batch, send)?;
        return Ok((sent(false), fresh));
    }
    debug_assert!(
        batch
            .iter()
            .all(|message| message.body == MessageBody::Prompt)
    );
    let compact = batch
        .iter()
        .find_map(|message| compact_message_for_target(store, target, fresh.as_ref(), message));
    let Some((command, threshold, agent)) = compact else {
        write_text(workspace, store, snapshot, writer, batch, send)?;
        return Ok((sent(false), fresh));
    };
    // Claimed only once the decisions above passed, so the command record
    // exists only for a write that follows at once.
    let command =
        store.queue_claimed_message(&command, &workspace.session_name, Timestamp::now())?;
    if let Err(err) = write_text(
        workspace,
        store,
        snapshot,
        writer,
        std::slice::from_ref(&command),
        send,
    ) {
        store.record_send_error(&command, &err.to_string(), &workspace.session_name)?;
        return Err(err);
    }
    let peers = crate::address::addressable_agents(snapshot);
    crate::harness::assist_log::append(&crate::harness::assist_log::AssistRecord {
        at: Timestamp::now(),
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
        return Ok((Receipt::CompactionPending, fresh));
    }
    // The prompts wait their own turn and decide on the row as it stands after the command.
    let fresh = match decide(send, true)? {
        Decided::Write(row) => row,
        Decided::Stop(receipt) => return Ok((receipt, None)),
    };
    write_text(workspace, store, snapshot, writer, batch, send)?;
    Ok((sent(true), fresh))
}

fn row_under_lock(store: &Store, head: &MessageRecord) -> Result<Option<AgentState>> {
    let current = store.snapshot_with_context(|_, _| false)?;
    Ok(store
        .rows_with_live_context(
            current
                .agents
                .iter()
                .find(|agent| head.same_agent_card(agent) && agent.ended_at.is_none()),
        )
        .into_iter()
        .next()
        .map(std::borrow::Cow::into_owned))
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

#[expect(
    clippy::large_enum_variant,
    reason = "built once per pane write and matched at once"
)]
enum Decided {
    Write(Option<AgentState>),
    Stop(Receipt),
}

/// Run a write's waits, then read the receiver's row once and make every
/// write decision on it: the receiver can open a turn, raise a prompt, or
/// start compacting while the write waits.
#[expect(
    clippy::too_many_arguments,
    reason = "one live write under the caller's pane lock"
)]
fn wait_and_decide(
    workspace: &ResolvedWorkspace,
    store: &Store,
    snapshot: &SidebarSnapshot,
    bound: Option<&AgentState>,
    card_status: &CardStatus,
    batch: &[MessageRecord],
    compacted: bool,
    send: &mut LiveSend,
) -> Result<Decided> {
    // Dispatch and delivery claims always pass a non-empty batch.
    let head = batch
        .first()
        .expect("wait_and_decide requires at least one message");
    debug_assert!(batch.iter().all(|message| message.enter == head.enter));
    send.pacer.tick();
    #[cfg(feature = "testkit")]
    crate::testkit::rendezvous("RIMZ_TEST_MESSAGE_BEFORE_WRITE");
    // Every delivery binds its claim to the pane before entering the write path.
    let pane = head.pane_id.as_ref().expect("live send has a pane pin");
    if let Some(until) = send.typing_hold_until(workspace, store.runtime_paths(), pane) {
        return Ok(Decided::Stop(Receipt::TypingHeld { until, compacted }));
    }
    if !store.message_claims_held(batch)? {
        return Ok(Decided::Stop(Receipt::ClaimLost { compacted }));
    }
    let fresh = row_under_lock(store, head)?;
    let Some(agent) = fresh.as_ref().or(bound) else {
        return Ok(Decided::Write(fresh));
    };
    if matches!(
        head.sender,
        MessageSender::Harness {
            notice: HarnessNotice::CacheKeepalive
        }
    ) && crate::config::MachineConfig::load_lenient()
        .harness
        .cache_cold_at(agent)
        .is_none_or(|cold_at| Timestamp::now() >= cold_at)
    {
        return Ok(Decided::Stop(Receipt::CacheCold));
    }
    let now = Timestamp::now();
    if !send.force && agent.is_awaiting_input() {
        return Ok(Decided::Stop(Receipt::SkippedWaiting(
            super::InputWait::for_agent(agent, now),
        )));
    }
    if send.kind == DeliveryKind::Boundary {
        if !super::gate_open_for_agent(head.gate, agent, send.force, now, snapshot, card_status) {
            let status = super::gate_status(head.gate, agent, now, snapshot, card_status);
            return Ok(Decided::Stop(Receipt::Deferred(Deferral::Status(status))));
        }
        if super::provider_start_pending(agent, now) {
            return Ok(Decided::Stop(Receipt::Deferred(Deferral::ProviderStarting)));
        }
    }
    Ok(Decided::Write(fresh))
}

/// Paste or type the batch, record it `Sent`, then submit it.
fn write_text(
    workspace: &ResolvedWorkspace,
    store: &Store,
    snapshot: &SidebarSnapshot,
    writer: &PaneWriter,
    batch: &[MessageRecord],
    send: &LiveSend,
) -> Result<()> {
    let head = batch
        .first()
        .expect("write_text requires at least one message");
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
                        message.channel.as_ref(),
                        Some(&workspace.project_root),
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
    Ok(())
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

fn compact_message_for_target<'a>(
    store: &Store,
    target: &PaneAgent,
    bound: Option<&'a AgentState>,
    prompt: &MessageRecord,
) -> Option<(MessageRecord, AutoCompact, &'a AgentState)> {
    let threshold = prompt.auto_compact?;
    let agent = bound?;
    if agent.holds_open_turn() || !threshold.triggered(agent) {
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
        about_run: None,
    }
    .record(
        prompt.workspace_id.clone(),
        Recipient::Pane {
            pane: target,
            bound,
        },
        prompt.channel.as_ref(),
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
        assert_eq!(target.closed_turn_id, None);
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
            past_typing_hold: false,
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
            past_typing_hold: false,
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
