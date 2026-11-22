//! Live-pane payload construction, exclusive paced writes, and the durable Sent-before-submit barrier.
//!
//! The bytes a delivery pastes into a real agent pane are proven only by the live
//! `journey::deep::tmux_steer_delivers_text_and_enter_to_real_agent_pane` and
//! `journey::deep::tmux_steer_without_enter_suppresses_submit_in_real_agent_pane` tests, which run
//! under `cargo xtask test -P live`, not `gate`: a change to those bytes runs them.

use std::thread::sleep;
use std::time::{Duration, Instant};

use jiff::Timestamp;

use crate::Store;
use crate::agents::{AgentState, AgentStatus, LifecycleSignal};
use crate::harness::auto_continue::CardStatus;
use crate::launcher::agent_pane::AgentPane;
use crate::message::{
    DeliveryKind, InterruptConfirmation, MessageDraft, Recipient, command_segments,
    command_submit_delay_from_env, message_interval_from_env,
};
use crate::mux::PaneWriter;
use crate::mux::typing::{InputSource, TypingRecord, TypingView};
use crate::pane::keys::NamedKey;
use crate::sidebar::produce::hosting::HostingErr;
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

/// Why a write stopped on the row read after its waits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Deferral {
    Status(AgentStatus),
    ProviderStarting,
    ModelHeld(crate::agents::ModelHold),
}

impl std::fmt::Display for Deferral {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Status(status) => write!(f, "the receiver is {}", status.as_str()),
            Self::ProviderStarting => f.write_str("the receiver's provider is still starting"),
            Self::ModelHeld(hold) => hold.fmt(f),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// The lookup read under the pane lock did not answer the claim's pane for the receiver;
    /// `reason` is the release's.
    NotHosted {
        compacted: bool,
        reason: &'static str,
    },
    TypingHeld {
        until: Timestamp,
        compacted: bool,
    },
    Deferred(Deferral),
    /// A cache keepalive whose cache went cold before its write: the ping would only pay to
    /// rebuild it.
    CacheCold,
    /// Every member's premise broke before the write and each was withdrawn, or one could not
    /// be checked and the batch's claims were released.
    Withdrawn {
        compacted: bool,
    },
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
    let steer_key = (matches!(send.kind, DeliveryKind::Steer) && head.enter)
        .then(|| {
            crate::agents::spec_by_kind(head.kind.as_str())?
                .launch
                .steer_key
        })
        .flatten();
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
    if let (Some(steer_key), Receipt::Sent { .. }) = (steer_key, &receipt)
        && row.as_ref().is_some_and(|agent| {
            matches!(
                agent.effective_status(Timestamp::now()),
                AgentStatus::Running | AgentStatus::Waiting
            )
        })
    {
        // A resting receiver opened a turn on the Enter; only a running one queued the text.
        sleep(STEER_KEY_DELAY);
        writer.press(steer_key)?;
    }
    let (Some(key), &Receipt::Sent { compacted, .. }) = (key, &receipt) else {
        return Ok(receipt);
    };
    // Decide on the row read after the write's waits: a resting agent gets a
    // plain send, since the key would cancel the very turn this paste starts.
    // Queued records can retain a provisional launch id; the row carries the live card's session.
    let decided_turn_started_at = row.as_ref().and_then(|agent| agent.turn_started_at);
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
    #[cfg(feature = "testkit")]
    crate::testkit::rendezvous("RIMZ_TEST_MESSAGE_BEFORE_INTERRUPT_KEY");
    // The delay leaves the receiver time to move: the key goes only to the session the paste
    // decided on, in the claim's pane.
    // The follower opens first, so no store I/O sits between the withheld check and the key.
    let follower = crate::agents::spec_by_kind(head.kind.as_str())
        .is_some_and(|spec| spec.launch.confirm_interrupt)
        .then(|| EventFollower::open(store.paths().clone(), false));
    let returns_queued_text = crate::agents::spec_by_kind(head.kind.as_str())
        .is_some_and(|spec| spec.launch.interrupt_returns_queued_text);
    let withheld = interrupt_withheld(workspace, store, target, bound, &agent_id).or_else(|| {
        // Such a kind queues text pasted behind a running turn and opens no turn for it
        // until the running one ends (agy: no invocation-0 PreInvocation until it dequeues,
        // probes P9 and P10). A turn start newer than the one the paste decided on is
        // therefore the paste's own turn, opened from a rest the row missed or after the
        // running turn ended during the delay, and the key would cancel it.
        let opened_own_turn = returns_queued_text
            && row_under_lock(store, head)
                .ok()
                .flatten()
                .is_some_and(|row| row.turn_started_at > decided_turn_started_at);
        opened_own_turn.then_some(
            "the message opened a new turn during the interrupt delay, and the key would cancel it",
        )
    });
    if let Some(cause) = withheld {
        return Ok(Receipt::Sent {
            compacted,
            interrupt: InterruptConfirmation::NotSent { cause },
        });
    }
    writer.press(key)?;
    if returns_queued_text {
        sleep(INTERRUPT_RESUBMIT_DELAY);
        writer.press(NamedKey::Enter)?;
    }
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

/// How long after its interrupt key a kind whose key returns queued text to the composer
/// waits before the Enter that submits it: agy submitted it once at 100 ms and at 500 ms,
/// and nothing when both keys went together (probe link 5, P9).
const INTERRUPT_RESUBMIT_DELAY: Duration = Duration::from_millis(500);

/// How long after a steer's Enter its kind's steer key waits: agy queues the submitted text
/// first, and took it into the running turn on Alt+M 0.5 s later on both backends
/// (`zellij-alt-m.md`, `send-immediately.md` probes of the agy-steer design).
const STEER_KEY_DELAY: Duration = Duration::from_millis(500);

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
    let decide = |send: &mut LiveSend, batch: &[MessageRecord], compacted: bool| {
        wait_and_decide(
            workspace,
            store,
            snapshot,
            target,
            bound,
            card_status,
            batch,
            compacted,
            send,
        )
    };
    let (fresh, batch) = match decide(send, batch, false)? {
        Decided::Write { row, batch } => (row, batch),
        Decided::Stop(receipt) => return Ok((receipt, None)),
    };
    let head = batch
        .first()
        .expect("a write decision keeps at least one message");
    let checked = |row: &Option<AgentState>| row.as_ref().or(bound).cloned();
    let sent = |compacted| Receipt::Sent {
        compacted,
        interrupt: InterruptConfirmation::NotChecked,
    };
    if head.body == MessageBody::Command {
        debug_assert_eq!(batch.len(), 1);
        let receiver = checked(&fresh);
        write_text(
            workspace,
            store,
            snapshot,
            writer,
            &batch,
            receiver.as_ref(),
            send,
        )?;
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
        let receiver = checked(&fresh);
        write_text(
            workspace,
            store,
            snapshot,
            writer,
            &batch,
            receiver.as_ref(),
            send,
        )?;
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
        checked(&fresh).as_ref(),
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
    let (fresh, batch) = match decide(send, &batch, true)? {
        Decided::Write { row, batch } => (row, batch),
        Decided::Stop(receipt) => return Ok((receipt, None)),
    };
    let receiver = checked(&fresh);
    write_text(
        workspace,
        store,
        snapshot,
        writer,
        &batch,
        receiver.as_ref(),
        send,
    )?;
    Ok((sent(true), fresh))
}

/// The receiver's live holder row as it stands now; a session a restart or molt replaced is
/// never the receiver.
fn row_under_lock(store: &Store, head: &MessageRecord) -> Result<Option<AgentState>> {
    let current = store.snapshot_with_context(|_, _| false)?;
    let holders = crate::agents::state::LaunchHolders::new(current.agents.iter());
    Ok(store
        .rows_with_live_context(current.agents.iter().find(|agent| {
            head.same_agent_card(agent) && agent.ended_at.is_none() && !holders.replaced(agent)
        }))
        .into_iter()
        .next()
        .map(std::borrow::Cow::into_owned))
}

/// The agent moved off the claim's pane while the write waited, or a launch record's agent has
/// no live holder yet: the record goes back to its queue for the holder's pane. A record an
/// older binary wrote, without a launch, is checked the same way against a holder of a launch,
/// the only rows a swap replaces; a hand-launched agent's records never carry one.
fn claim_moved(
    head: &MessageRecord,
    fresh: Option<&AgentState>,
    pane: &crate::ids::PaneId,
) -> bool {
    match fresh {
        Some(row) if head.launch_id.is_some() || row.launch_id.is_some() => {
            !holder_on_pane(fresh, pane)
        }
        Some(_) => false,
        None => head.launch_id.is_some(),
    }
}

fn holder_on_pane(holder: Option<&AgentState>, pane: &crate::ids::PaneId) -> bool {
    holder.is_some_and(|row| row.pane.as_ref().is_none_or(|bound| &bound.pane_id == pane))
}

/// Why the hosting lookup did not answer the pane a write into `target` is bound to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum LookupMiss {
    /// No listed pane hosts the receiver.
    Nowhere,
    /// The receiver is in this other pane.
    Elsewhere(crate::ids::PaneId),
    /// The multiplexer could not be read.
    Unread(String),
}

impl LookupMiss {
    /// The reason a claim released under the pane lock records.
    fn release_reason(&self) -> &'static str {
        match self {
            Self::Nowhere | Self::Elsewhere(_) => {
                "pane write stopped: the receiver is not in the claim's pane"
            }
            Self::Unread(_) => "pane write stopped: the receiver's pane could not be read",
        }
    }
}

/// Where the hosting lookup finds the receiver bound to `target`, read once on `target`'s
/// backend. `None` when the target pane carries no session or binds no row: the lookup has
/// no answer for such a pane, and its write goes ahead as bound.
fn locate_receiver(
    workspace: &ResolvedWorkspace,
    store: &Store,
    target: &PaneAgent,
    bound: Option<&AgentState>,
) -> Option<std::result::Result<Option<AgentPane>, HostingErr>> {
    target.agent_id.as_ref()?;
    let bound = bound?;
    let mut notices = Vec::new();
    let answer = crate::launcher::agent_pane::locate(
        workspace,
        store,
        Some(target.pane_id.mux()),
        &bound.kind,
        bound.launch_key(),
        &mut notices,
    );
    for notice in notices {
        tracing::debug!(%notice, pane = %target.pane_id, "delivery's hosting read");
    }
    Some(answer)
}

/// Why the lookup does not answer `target`'s pane for the receiver bound to it, or `None`
/// when it does or has no answer for the pane (see [`locate_receiver`]).
pub(super) fn lookup_miss(
    workspace: &ResolvedWorkspace,
    store: &Store,
    target: &PaneAgent,
    bound: Option<&AgentState>,
) -> Option<LookupMiss> {
    match locate_receiver(workspace, store, target, bound)? {
        Ok(Some(found)) if found.pane_id == target.pane_id => None,
        Ok(Some(found)) => Some(LookupMiss::Elsewhere(found.pane_id)),
        Ok(None) => Some(LookupMiss::Nowhere),
        Err(err) => Some(LookupMiss::Unread(err.to_string())),
    }
}

/// Why an interrupt key must not follow its paste: the lookup no longer answers the claim's
/// pane, or holds a session there other than `agent_id`, the one the paste decided on.
fn interrupt_withheld(
    workspace: &ResolvedWorkspace,
    store: &Store,
    target: &PaneAgent,
    bound: Option<&AgentState>,
    agent_id: &crate::ids::AgentSessionId,
) -> Option<&'static str> {
    match locate_receiver(workspace, store, target, bound)? {
        Ok(Some(found)) if found.pane_id == target.pane_id && found.holder.1 == *agent_id => None,
        Ok(Some(found)) if found.pane_id == target.pane_id => {
            Some("another session of the receiver holds its pane now")
        }
        Ok(_) => Some("the receiver left its pane during the interrupt delay"),
        Err(_) => Some("the receiver's pane could not be read after the interrupt delay"),
    }
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
    /// Write `batch`, the members whose premises still hold, deciding on `row`.
    Write {
        row: Option<AgentState>,
        batch: Vec<MessageRecord>,
    },
    Stop(Receipt),
}

/// Run a write's waits, then read the receiver's row and make every write
/// decision on it: the receiver can open a turn, raise a prompt, or start
/// compacting while the write waits. Every member whose premise that row (or
/// its harness subject) shows broken is withdrawn here and never typed; after
/// a withdrawal the row is read again and the survivors judged again.
#[expect(
    clippy::too_many_arguments,
    reason = "one live write under the caller's pane lock"
)]
fn wait_and_decide(
    workspace: &ResolvedWorkspace,
    store: &Store,
    snapshot: &SidebarSnapshot,
    target: &PaneAgent,
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
    let pane = head
        .pane_id
        .as_ref()
        .expect("a claimed send carries its pane");
    if let Some(until) = send.typing_hold_until(workspace, store.runtime_paths(), pane) {
        return Ok(Decided::Stop(Receipt::TypingHeld { until, compacted }));
    }
    // The lookup lists the room's panes, so it comes before the claim check and the row read:
    // only store reads sit between those and the decisions made on them.
    let not_hosted = || {
        lookup_miss(workspace, store, target, bound).map(|miss| Receipt::NotHosted {
            compacted,
            reason: miss.release_reason(),
        })
    };
    if let Some(receipt) = not_hosted() {
        return Ok(Decided::Stop(receipt));
    }
    if !store.message_claims_held(batch)? {
        return Ok(Decided::Stop(Receipt::ClaimLost { compacted }));
    }
    let mut fresh = row_under_lock(store, head)?;
    if claim_moved(head, fresh.as_ref(), pane) {
        return Ok(Decided::Stop(Receipt::ClaimLost { compacted }));
    }
    let mut batch = batch.to_vec();
    // A withdrawal commit waits on the workspace lock, behind any hook frame applied meanwhile,
    // and can cancel a "still queued" warning in the batch: judge the survivors again on a
    // fresh row until a pass withdraws nothing.
    while let Some(survivors) = withdraw_broken(
        workspace,
        store,
        &batch,
        fresh.as_ref().or(bound),
        compacted,
    )? {
        if survivors.is_empty() {
            return Ok(Decided::Stop(Receipt::Withdrawn { compacted }));
        }
        if let Some(receipt) = not_hosted() {
            return Ok(Decided::Stop(receipt));
        }
        if !store.message_claims_held(&survivors)? {
            return Ok(Decided::Stop(Receipt::ClaimLost { compacted }));
        }
        fresh = row_under_lock(store, &survivors[0])?;
        if claim_moved(head, fresh.as_ref(), pane) {
            return Ok(Decided::Stop(Receipt::ClaimLost { compacted }));
        }
        batch = survivors;
    }
    let head = &batch[0];
    let Some(agent) = fresh.as_ref().or(bound) else {
        return Ok(Decided::Write { row: fresh, batch });
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
    // No kind or gate, forced or not, goes into an agent its provider refuses. The waits above
    // can outlast a refusal record landing, so the record is read again here.
    if let Some(hold) = card_status.held_now(snapshot, agent, now) {
        return Ok(Decided::Stop(Receipt::Deferred(Deferral::ModelHeld(hold))));
    }
    if !send.force && agent.is_awaiting_input() {
        return Ok(Decided::Stop(Receipt::SkippedWaiting(
            super::InputWait::for_agent(agent, now),
        )));
    }
    if send.kind == DeliveryKind::Boundary
        && !super::gate_open_for_agent(head.gate, agent, send.force, now, snapshot, card_status)
    {
        let status = super::gate_status(head.gate, agent, now, snapshot, card_status);
        return Ok(Decided::Stop(Receipt::Deferred(Deferral::Status(status))));
    }
    // A resume follow-up waits for the provider's own session start under every unforced
    // kind: typing into a provider that has not started one is what kills the resume. Only a
    // kind whose resume reports that start is held here; another keeps the boundary's timed hold.
    let follow_up_held =
        head.register_by.is_some() && !send.force && agent.resumed_provider_unreported();
    if (send.kind == DeliveryKind::Boundary || follow_up_held)
        && super::provider_start_pending(agent, head.register_by.is_some(), now)
    {
        return Ok(Decided::Stop(Receipt::Deferred(Deferral::ProviderStarting)));
    }
    Ok(Decided::Write { row: fresh, batch })
}

/// Withdraw each member of `batch` whose premise no longer holds and return the rest, in order,
/// or `None` when every member holds; the survivors alone set their `batch_id`. A premise that
/// cannot be checked (a fleet digest's run scan failing) releases the whole batch's claims and
/// returns no survivors.
fn withdraw_broken(
    workspace: &ResolvedWorkspace,
    store: &Store,
    batch: &[MessageRecord],
    row: Option<&AgentState>,
    compacted: bool,
) -> Result<Option<Vec<MessageRecord>>> {
    let now = Timestamp::now();
    let mut broken = Vec::new();
    let mut held = Vec::with_capacity(batch.len());
    for message in batch {
        match super::premise::broken(store, message, row, now) {
            Ok(Some(reason)) => broken.push((message.message_id.clone(), reason)),
            Ok(None) => held.push(message.clone()),
            Err(error) => {
                tracing::warn!(
                    message_id = %message.message_id,
                    %error,
                    "deferring message: cannot check whether its premises hold"
                );
                store.release_message_claims(
                    batch,
                    "deferred: cannot check or settle joined runs",
                    compacted,
                    &workspace.session_name,
                )?;
                return Ok(Some(Vec::new()));
            }
        }
    }
    if broken.is_empty() {
        return Ok(None);
    }
    #[cfg(feature = "testkit")]
    crate::testkit::rendezvous("RIMZ_TEST_MESSAGE_BEFORE_WITHDRAW");
    store.withdraw_messages(&workspace.session_name, |message| {
        broken
            .iter()
            .find(|(id, _)| *id == message.message_id)
            .map(|(_, reason)| reason.clone())
    })?;
    let batch_id = (held.len() > 1).then(|| held[0].message_id.clone());
    for message in &mut held {
        message.batch_id = batch_id.clone();
    }
    Ok(Some(held))
}

/// Paste or type the batch, record it `Sent` at the epoch of `receiver` (the row the write was
/// checked against), then submit it. A command records the text it typed.
fn write_text(
    workspace: &ResolvedWorkspace,
    store: &Store,
    snapshot: &SidebarSnapshot,
    writer: &PaneWriter,
    batch: &[MessageRecord],
    receiver: Option<&AgentState>,
    send: &LiveSend,
) -> Result<()> {
    let head = batch
        .first()
        .expect("write_text requires at least one message");
    let mut typed = None;
    match head.body {
        MessageBody::Command => {
            debug_assert_eq!(batch.len(), 1);
            let kind = receiver.map_or(&head.kind, |row| &row.kind);
            let declared = crate::agents::spec_by_kind(kind.as_str())
                .and_then(|spec| spec.launch.compact_command)
                .map(|compact| compact.command);
            let text = compaction_for_receiver(head, receiver).unwrap_or_else(|| head.text.clone());
            type_command_with(
                &text,
                declared,
                send,
                |segment| writer.type_text(segment),
                sleep,
            )?;
            typed = Some(text);
        }
        MessageBody::Prompt => {
            debug_assert!(
                batch
                    .iter()
                    .all(|message| message.body == MessageBody::Prompt)
            );
            let peers = crate::address::addressable_agents(snapshot);
            writer.paste(&prompt_payload(batch, &peers))?;
        }
    }
    // Record the send once the text lands and before the submit keystroke, so a
    // submitted message is always preceded by its durable record and audit event.
    let stamped = batch
        .iter()
        .cloned()
        .map(|mut message| {
            message.sent_epoch = receiver.map(|row| row.epoch);
            if let Some(text) = &typed {
                message.text.clone_from(text);
            }
            message
        })
        .collect::<Vec<_>>();
    let recorded = store.record_sent_batch(&stamped, &workspace.session_name)?;
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

/// One paste of a prompt batch: each member's header and text in paste order, joined by a
/// blank line.
fn prompt_payload(batch: &[MessageRecord], peers: &[&AgentState]) -> String {
    crate::store::message::paste_order(batch)
        .into_iter()
        .map(
            |message| match crate::address::record_header(message, peers) {
                Some(header) => format!("{header}{}", message.text),
                None => message.text.clone(),
            },
        )
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The receiver's own compaction command for a queued compaction its agent's molt to another kind
/// left in the old kind's words, carrying the queued instruction; `None` for any other command
/// or a receiver of the queued kind. A receiver without a compaction command types it as queued.
fn compaction_for_receiver(head: &MessageRecord, receiver: Option<&AgentState>) -> Option<String> {
    let receiver = receiver.filter(|row| row.kind != head.kind)?;
    let queued = crate::agents::spec_by_kind(head.kind.as_str())?
        .launch
        .compact_command?;
    let rest = head.text.strip_prefix(queued.command)?;
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    crate::agents::spec_by_kind(receiver.kind.as_str())?
        .launch
        .compact_command(rest.trim())
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
    let now = jiff::Timestamp::now();
    // RimZ compacts nothing while the login reads a spent usage window that no usable paid
    // extra usage carries, so the prompt goes uncompacted.
    if agent.compaction_unprompted(now)
        || crate::agents::ProviderCapacity::login_compaction_blocked(
            store.runtime_paths(),
            agent,
            now,
        )
    {
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
        about_run: None,
        premises: crate::store::message::Premise::NONE,
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
        reply_wanted: false,
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

    fn queued_command(kind: &str, text: &str) -> MessageRecord {
        let receiver = AgentState::stub(kind, "sess-old", crate::agents::AgentStatus::Idle);
        MessageRecord::new(
            crate::ids::WorkspaceId::parse("ws_000000000000000000000000").unwrap(),
            &receiver,
            text.to_owned(),
            crate::store::message::DeliveryGate::Done,
        )
        .with_body(MessageBody::Command)
    }

    #[test]
    fn a_batch_pastes_the_users_bare_text_ahead_of_headered_members() {
        use crate::store::message::MessageSender;
        let receiver = AgentState::stub("claude", "sess", crate::agents::AgentStatus::Idle);
        let prompt = |text: &str, sender| {
            MessageRecord::new(
                crate::ids::WorkspaceId::parse("ws_000000000000000000000000").unwrap(),
                &receiver,
                text.to_owned(),
                crate::store::message::DeliveryGate::Done,
            )
            .with_sender(sender)
        };
        let stage = MessageSender::Harness {
            notice: crate::store::message::HarnessNotice::Stage,
        };
        let batch = [
            prompt("go on", stage),
            prompt("say hi", MessageSender::Human),
            prompt("continue", MessageSender::System),
            prompt("and this", MessageSender::Human),
        ];
        assert_eq!(
            prompt_payload(&batch, &[]),
            "say hi\n\nand this\n\nType: STAGE\nFrom: @rimz\nContent:\ngo on\n\ncontinue"
        );
        assert_eq!(prompt_payload(&batch[1..2], &[]), "say hi");
    }

    #[test]
    fn a_compaction_queued_before_a_molt_types_the_holders_command() {
        let hermes = AgentState::stub("hermes", "sess-new", crate::agents::AgentStatus::Idle);
        let claude = AgentState::stub("claude", "sess-new", crate::agents::AgentStatus::Idle);
        let compact = queued_command("claude", "/compact keep the open questions");
        assert_eq!(
            compaction_for_receiver(&compact, Some(&hermes)).as_deref(),
            Some("/compress keep the open questions")
        );
        assert_eq!(
            compaction_for_receiver(&queued_command("claude", "/compact"), Some(&hermes))
                .as_deref(),
            Some("/compress")
        );
        assert_eq!(compaction_for_receiver(&compact, Some(&claude)), None);
        assert_eq!(compaction_for_receiver(&compact, None), None);
        for other in ["/model opus", "/compactor"] {
            assert_eq!(
                compaction_for_receiver(&queued_command("claude", other), Some(&hermes)),
                None,
                "{other}"
            );
        }
    }

    #[test]
    fn a_legacy_claim_on_a_replaced_pane_goes_back_to_its_queue() {
        let old_pane = crate::ids::PaneId::from_parts(crate::ids::MuxName::Tmux, "%1");
        let mut holder = AgentState::stub("claude", "sess-new", crate::agents::AgentStatus::Idle);
        holder.launch_id = Some("launch_coder".into());
        holder.epoch = 1;
        holder.pane = Some(crate::pane::PaneRef::from_id(
            crate::ids::PaneId::from_parts(crate::ids::MuxName::Tmux, "%2"),
        ));
        let legacy = queued_command("claude", "hello");
        assert!(legacy.launch_id.is_none());
        assert!(claim_moved(&legacy, Some(&holder), &old_pane));
        assert!(
            !claim_moved(&legacy, None, &old_pane),
            "no holder found: write as before"
        );
        holder.launch_id = None;
        assert!(
            !claim_moved(&legacy, Some(&holder), &old_pane),
            "a hand-launched agent has no swap to follow"
        );
        holder.launch_id = Some("launch_coder".into());
        holder.pane = Some(crate::pane::PaneRef::from_id(old_pane.clone()));
        assert!(!claim_moved(&legacy, Some(&holder), &old_pane));
    }

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
