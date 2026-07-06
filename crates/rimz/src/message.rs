//! Durable per-agent message queue domain model and delivery pipeline.

use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

pub mod deliver;
pub mod dispatch;
pub(crate) mod fire;
pub mod send;

use crate::agents::lifecycle::LifecycleSignal;
use crate::agents::{AgentState, AgentStatus};
use crate::ids::{AgentKind, AgentSessionId, MessageId, PaneId, WorkspaceId};

pub const DEFAULT_SETTLE: Duration = Duration::from_millis(400);
pub const SETTLE_ENV: &str = "RIMZ_MESSAGE_SETTLE_MS";
pub const MESSAGE_WAKE_FILE: &str = "message-wake.json";
/// Default spacing between discrete message pane writes.
pub const DEFAULT_MESSAGE_INTERVAL: Duration = Duration::from_secs(1);
pub const MESSAGE_INTERVAL_ENV: &str = "RIMZ_MESSAGE_INTERVAL_MS";
pub const DEFAULT_DELIVERY_WINDOW: Duration = Duration::from_secs(30);
pub const DELIVERY_WINDOW_ENV: &str = "RIMZ_MESSAGE_DELIVERY_WINDOW_MS";
/// Default cap for unconfirmed `Sent` reconciliation attempts.
pub const DEFAULT_MAX_DELIVERY_ATTEMPTS: u32 = 3;
pub const MAX_DELIVERY_ATTEMPTS_ENV: &str = "RIMZ_MESSAGE_MAX_DELIVERY_ATTEMPTS";
/// Cap for pre-send delivery failures after a queued claim.
pub const MAX_DELIVERY_ATTEMPTS: u32 = 5;
pub const CLAIM_TTL: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "origin")]
pub enum MessageSender {
    #[default]
    Human,
    Agent {
        kind: AgentKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<String>,
    },
}

impl MessageSender {
    pub fn attributed(&self) -> Option<Self> {
        match self {
            Self::Human => None,
            Self::Agent { .. } => Some(self.clone()),
        }
    }

    pub fn render(&self) -> String {
        match self {
            Self::Human => "you".to_owned(),
            Self::Agent {
                kind,
                profile,
                role,
                channel,
                ..
            } => {
                let mut rendered = identity_handle(kind, profile.as_deref(), role.as_deref());
                if let Some(channel) = channel.as_deref().filter(|value| !value.is_empty()) {
                    rendered.push('#');
                    rendered.push_str(channel);
                }
                rendered
            }
        }
    }
}

pub fn identity_handle(kind: &AgentKind, profile: Option<&str>, role: Option<&str>) -> String {
    let base = role
        .filter(|value| !value.is_empty())
        .or_else(|| profile.filter(|value| !value.is_empty()))
        .unwrap_or_else(|| kind.as_str());
    format!("@{base}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryGate {
    Done,
    Any,
    Resume,
}

impl DeliveryGate {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Any => "any",
            Self::Resume => "resume",
        }
    }
}

impl std::fmt::Display for DeliveryGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A context-fill threshold that triggers a manual `/compact` in the agent's own
/// composer before the steered or queued text is delivered. An agent compacts on
/// its own only at its ceiling (Codex around 90%), so a prompt sent past that
/// ceiling can be cut in half by a compaction that fires mid-turn. A lower
/// threshold compacts *first*, so the prompt always lands against a fresh window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoCompact {
    /// Compact once the window is at least this full, in percent (0..=100).
    Percent(u8),
    /// Compact once at least this many tokens occupy the window.
    Tokens(u64),
}

impl AutoCompact {
    /// Parse a threshold: `70%` is a percentage of the window, a bare integer
    /// (`120000`) is an absolute occupied-token count.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        if let Some(pct) = raw.strip_suffix('%') {
            let pct: u8 = pct
                .trim()
                .parse()
                .map_err(|_| format!("invalid auto-compact percentage `{raw}`"))?;
            if pct > 100 {
                return Err(format!("auto-compact percentage `{pct}` exceeds 100"));
            }
            Ok(Self::Percent(pct))
        } else {
            let tokens: u64 = raw.parse().map_err(|_| {
                format!("invalid auto-compact threshold `{raw}`; use `70%` or a token count")
            })?;
            Ok(Self::Tokens(tokens))
        }
    }

    /// Whether `agent`'s current context fill has reached this threshold. An
    /// unknown fill never triggers — a missing reading is not a full window.
    pub fn triggered(self, agent: &AgentState) -> bool {
        match self {
            Self::Percent(pct) => agent
                .context_fill_pct()
                .is_some_and(|fill| fill >= f64::from(pct)),
            Self::Tokens(tokens) => agent
                .occupied_context_tokens()
                .is_some_and(|used| used >= tokens),
        }
    }
}

impl std::fmt::Display for AutoCompact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Percent(pct) => write!(f, "{pct}%"),
            Self::Tokens(tokens) => write!(f, "{tokens} tokens"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus {
    #[serde(alias = "pending")]
    Queued,
    Claimed,
    Sent,
    Delivered,
    TimedOut,
    Errored,
    Removed,
    Abandoned,
    Archived,
}

impl MessageStatus {
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Delivered
                | Self::TimedOut
                | Self::Errored
                | Self::Removed
                | Self::Abandoned
                | Self::Archived
        )
    }

    pub const fn is_open(self) -> bool {
        matches!(self, Self::Queued | Self::Claimed)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Claimed => "claimed",
            Self::Sent => "sent",
            Self::Delivered => "delivered",
            Self::TimedOut => "timed_out",
            Self::Errored => "errored",
            Self::Removed => "removed",
            Self::Abandoned => "abandoned",
            Self::Archived => "archived",
        }
    }
}

impl std::fmt::Display for MessageStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageBody {
    #[default]
    Prompt,
    Command,
}

impl MessageBody {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Command => "command",
        }
    }
}

impl std::fmt::Display for MessageBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageRecord {
    pub message_id: MessageId,
    pub workspace_id: WorkspaceId,
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    /// Receiver handle as resolved at enqueue, e.g. `@coder#auth`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(default)]
    pub sender: MessageSender,
    #[serde(default)]
    pub body: MessageBody,
    pub text: String,
    pub enter: bool,
    pub gate: DeliveryGate,
    /// Deliver even when a pending ask reserves the agent's next input. Mirrors
    /// `message --steer --force`; without it a pending ask defers delivery to a later
    /// boundary.
    #[serde(default)]
    pub force: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<PaneId>,
    pub status: MessageStatus,
    pub enqueued_at: Timestamp,
    pub updated_at: Timestamp,
    /// Delivery claims made against this record. Claim/pre-send failures use
    /// this as the `MAX_DELIVERY_ATTEMPTS` guard.
    #[serde(default)]
    pub attempts: u32,
    /// Sends that reached a pane but were not confirmed by a lifecycle hook.
    /// Stale-`Sent` reconciliation uses this as the
    /// `DEFAULT_MAX_DELIVERY_ATTEMPTS` guard.
    #[serde(default)]
    pub unconfirmed_sends: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<Timestamp>,
    /// Earliest delivery time for scheduled messages. The turn-boundary gate
    /// still decides which agent states may receive the message once this floor
    /// has passed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_before: Option<Timestamp>,
    /// Wake-only retry floor set by the elder sweep when a ready queued head
    /// cannot deliver. This never gates FIFO readiness or turn-boundary
    /// delivery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<Timestamp>,
    /// When set, deliver a `/compact` ahead of the text if the agent's context
    /// fill has reached this threshold at delivery time, so the message lands
    /// against a fresh window instead of racing the agent's own auto-compaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact: Option<AutoCompact>,
    /// For a smart-compact command, the occupied-context-token reading the
    /// trigger fired on. While a carried-forward stale gauge still equals this
    /// baseline, the send path suppresses duplicate `/compact` commands; a new
    /// reading re-enables compaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compacted_context_tokens: Option<u64>,
    /// Shared stamp for records sent in one batched paste; the head's message id.
    /// Set at send time, cleared when the reconciler requeues an unconfirmed send.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<MessageId>,
}

impl MessageRecord {
    pub fn new(
        workspace_id: WorkspaceId,
        agent: &AgentState,
        text: String,
        enter: bool,
        gate: DeliveryGate,
    ) -> Self {
        Self::new_for_card(
            workspace_id,
            agent.kind.clone(),
            agent.agent_id.clone(),
            agent.name.clone(),
            text,
            enter,
            gate,
        )
    }

    pub fn new_for_card(
        workspace_id: WorkspaceId,
        kind: AgentKind,
        agent_id: AgentSessionId,
        agent_name: Option<String>,
        text: String,
        enter: bool,
        gate: DeliveryGate,
    ) -> Self {
        let now = Timestamp::now();
        Self {
            message_id: MessageId::new(),
            workspace_id,
            kind,
            agent_id,
            agent_name,
            address: None,
            channel: None,
            sender: MessageSender::Human,
            body: MessageBody::Prompt,
            text,
            enter,
            gate,
            force: false,
            pane_id: None,
            status: MessageStatus::Queued,
            enqueued_at: now,
            updated_at: now,
            attempts: 0,
            unconfirmed_sends: 0,
            last_attempt_at: None,
            last_error: None,
            delivered_at: None,
            not_before: None,
            retry_after: None,
            auto_compact: None,
            compacted_context_tokens: None,
            batch_id: None,
        }
    }

    #[must_use]
    pub fn with_address(mut self, address: Option<String>) -> Self {
        self.address = address;
        self
    }

    /// Attach a context-fill threshold that delivers a `/compact` ahead of the
    /// text when the window is full enough at delivery time.
    #[must_use]
    pub fn with_auto_compact(mut self, auto_compact: Option<AutoCompact>) -> Self {
        self.auto_compact = auto_compact;
        self
    }

    #[must_use]
    pub fn with_body(mut self, body: MessageBody) -> Self {
        self.body = body;
        self
    }

    /// Deliver past a pending ask at the boundary, mirroring `message --steer --force`.
    #[must_use]
    pub fn with_force(mut self, force: bool) -> Self {
        self.force = force;
        self
    }

    #[must_use]
    pub fn with_pane_id(mut self, pane_id: PaneId) -> Self {
        self.pane_id = Some(pane_id);
        self
    }

    #[must_use]
    pub fn with_not_before(mut self, not_before: Option<Timestamp>) -> Self {
        self.not_before = not_before;
        self
    }

    #[must_use]
    pub fn with_channel(mut self, channel: Option<String>) -> Self {
        self.channel = channel;
        self
    }

    #[must_use]
    pub fn with_status(mut self, status: MessageStatus) -> Self {
        self.status = status;
        self
    }

    /// Record who queued the message without duplicating the message body.
    #[must_use]
    pub fn with_sender(mut self, sender: MessageSender) -> Self {
        self.sender = sender;
        self
    }

    /// Whether this record belongs to the logical agent card `(kind, agent_id)`:
    /// the exact session id, or the stable `agent_name` when one is carried. A
    /// message queued against a provisional `launch_*` card keeps the launch id,
    /// so name matching is what folds it into the session that card becomes on
    /// registration — one card, one FIFO queue.
    pub fn same_card(
        &self,
        kind: &AgentKind,
        agent_id: &AgentSessionId,
        agent_name: Option<&str>,
    ) -> bool {
        card_matches(
            &self.kind,
            &self.agent_id,
            self.agent_name.as_deref(),
            kind,
            agent_id,
            agent_name,
        )
    }

    pub fn same_agent_card(&self, agent: &AgentState) -> bool {
        self.same_card(&agent.kind, &agent.agent_id, agent.name.as_deref())
    }

    pub fn is_ready(&self, now: Timestamp) -> bool {
        self.not_before.is_none_or(|not_before| not_before <= now)
    }

    pub fn sent_reconcile_deadline(&self, window: Duration) -> Option<Timestamp> {
        (self.status == MessageStatus::Sent).then_some(self.updated_at + window)
    }

    /// Next time the elder should sweep this record, or `None` if it arms nothing.
    pub fn wake_deadline(&self, now: Timestamp, window: Duration) -> Option<Timestamp> {
        match self.status {
            MessageStatus::Queued => Some(
                self.not_before
                    .filter(|not_before| *not_before > now)
                    .or(self.retry_after)
                    .unwrap_or(self.updated_at),
            ),
            MessageStatus::Sent => self.sent_reconcile_deadline(window),
            _ => None,
        }
    }

    pub fn batch_key(&self) -> Option<&str> {
        match &self.sender {
            MessageSender::Agent { channel, .. } => channel.as_deref(),
            MessageSender::Human => self.channel.as_deref(),
        }
    }

    pub fn batchable(&self) -> bool {
        self.body == MessageBody::Prompt && self.enter && !self.text.trim_start().starts_with('/')
    }
}

pub fn card_matches(
    record_kind: &AgentKind,
    record_agent_id: &AgentSessionId,
    record_agent_name: Option<&str>,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
    agent_name: Option<&str>,
) -> bool {
    record_kind == kind
        && (record_agent_id == agent_id
            || (agent_name.is_some() && record_agent_name == agent_name))
}

pub fn gate_open(gate: DeliveryGate, status: AgentStatus) -> bool {
    match gate {
        DeliveryGate::Done => matches!(status, AgentStatus::Idle | AgentStatus::Success),
        DeliveryGate::Any => matches!(
            status,
            AgentStatus::Idle | AgentStatus::Success | AgentStatus::Failed
        ),
        DeliveryGate::Resume => status == AgentStatus::Paused,
    }
}

/// The oldest ordinary queued message for one logical agent card, the next to
/// deliver at a turn boundary. FIFO spans a card's provisional `launch_*` id
/// and the session id it registers as, so pass the stable `agent_name` when
/// known: a message queued before registration still sorts ahead of one queued
/// after. Hidden resume nudges live in a separate control lane.
pub fn queue_head<'a>(
    pending: impl IntoIterator<Item = &'a MessageRecord>,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
    agent_name: Option<&str>,
    now: Timestamp,
) -> Option<&'a MessageRecord> {
    pending
        .into_iter()
        .filter(|message| {
            message.status == MessageStatus::Queued
                && message.gate != DeliveryGate::Resume
                && message.same_card(kind, agent_id, agent_name)
                && message.is_ready(now)
        })
        .min_by(|a, b| a.message_id.as_str().cmp(b.message_id.as_str()))
}

/// The oldest queued message in the candidate's delivery lane. Resume nudges
/// are control traffic for a parked turn, so they do not wait behind ordinary
/// user messages that can only deliver after the turn resumes.
pub fn queue_head_for_message<'a>(
    pending: impl IntoIterator<Item = &'a MessageRecord>,
    candidate: &MessageRecord,
    now: Timestamp,
) -> Option<&'a MessageRecord> {
    pending
        .into_iter()
        .filter(|message| {
            message.status == MessageStatus::Queued
                && message.same_card(
                    &candidate.kind,
                    &candidate.agent_id,
                    candidate.agent_name.as_deref(),
                )
                && message.is_ready(now)
                && same_delivery_lane(candidate.gate, message.gate)
        })
        .min_by(|a, b| a.message_id.as_str().cmp(b.message_id.as_str()))
}

/// Contiguous batchable followers behind `head`, oldest first: the ready FIFO
/// prefix of the head's card and lane whose members share the head's batch key
/// and force flag and whose own gate is open. Stops at the first non-matching
/// record so delivery never reorders the queue.
pub fn queue_batch_tail<'a>(
    pending: impl IntoIterator<Item = &'a MessageRecord>,
    head: &MessageRecord,
    status: AgentStatus,
    now: Timestamp,
) -> Vec<&'a MessageRecord> {
    if head.gate == DeliveryGate::Resume || !head.batchable() {
        return Vec::new();
    }
    let head_key = head.batch_key();
    let mut candidates: Vec<&MessageRecord> = pending
        .into_iter()
        .filter(|message| {
            message.status == MessageStatus::Queued
                && message.gate != DeliveryGate::Resume
                && message.same_card(&head.kind, &head.agent_id, head.agent_name.as_deref())
                && message.is_ready(now)
                && message.message_id.as_str() > head.message_id.as_str()
        })
        .collect();
    candidates.sort_by(|a, b| a.message_id.as_str().cmp(b.message_id.as_str()));
    candidates
        .into_iter()
        .take_while(|message| {
            message.batchable()
                && message.batch_key() == head_key
                && message.force == head.force
                && gate_open(message.gate, status)
        })
        .collect()
}

fn same_delivery_lane(candidate: DeliveryGate, queued: DeliveryGate) -> bool {
    match candidate {
        DeliveryGate::Resume => queued == DeliveryGate::Resume,
        DeliveryGate::Done | DeliveryGate::Any => queued != DeliveryGate::Resume,
    }
}

pub fn parse_schedule_at(raw: &str, now: &jiff::Zoned) -> Result<Timestamp, String> {
    const UNITS: &[(&str, u64)] = &[("s", 1), ("m", 60), ("h", 3600), ("d", 86_400)];
    if let Ok(duration) = crate::harness::schedule::parse_duration_units(raw, UNITS) {
        if duration.is_zero() {
            return Err("schedule duration must be greater than zero".to_owned());
        }
        return now
            .checked_add(duration)
            .map(|target| target.timestamp())
            .map_err(|err| format!("schedule `{raw}` cannot be resolved: {err}"));
    }
    let (hour, minute) = parse_hhmm(raw).ok_or_else(|| {
        format!(
            "invalid schedule `{raw}`; use a duration like `60m` or a 24-hour time like `14:30`"
        )
    })?;
    let candidate = now
        .date()
        .at(hour as i8, minute as i8, 0, 0)
        .to_zoned(now.time_zone().clone())
        .map_err(|err| format!("schedule `{raw}` cannot be resolved today: {err}"))?;
    if candidate.timestamp() > now.timestamp() {
        return Ok(candidate.timestamp());
    }
    now.date()
        .tomorrow()
        .map_err(|err| format!("schedule `{raw}` cannot be resolved tomorrow: {err}"))?
        .at(hour as i8, minute as i8, 0, 0)
        .to_zoned(now.time_zone().clone())
        .map(|target| target.timestamp())
        .map_err(|err| format!("schedule `{raw}` cannot be resolved tomorrow: {err}"))
}

fn parse_hhmm(raw: &str) -> Option<(u8, u8)> {
    let (hh, mm) = raw.trim().split_once(':')?;
    let hour: u8 = hh.parse().ok()?;
    let minute: u8 = mm.parse().ok()?;
    (hour <= 23 && minute <= 59).then_some((hour, minute))
}

pub fn delivery_checkpoint(signal: &LifecycleSignal) -> bool {
    matches!(
        signal,
        LifecycleSignal::TurnEnded {
            parked_on_background: false,
            ..
        }
    )
}

pub fn settle_duration_from_env() -> Duration {
    std::env::var(SETTLE_ENV)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_SETTLE)
}

/// Spacing between discrete message pane writes.
pub fn message_interval_from_env() -> Duration {
    std::env::var(MESSAGE_INTERVAL_ENV)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_MESSAGE_INTERVAL)
}

pub fn delivery_window_from_env() -> Duration {
    std::env::var(DELIVERY_WINDOW_ENV)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_DELIVERY_WINDOW)
}

pub fn max_delivery_attempts_from_env() -> u32 {
    std::env::var(MAX_DELIVERY_ATTEMPTS_ENV)
        .ok()
        .and_then(|raw| raw.parse::<u32>().ok())
        .filter(|attempts| *attempts > 0)
        .unwrap_or(DEFAULT_MAX_DELIVERY_ATTEMPTS)
}

pub fn claim_expired(last_attempt_at: Option<Timestamp>, now: Timestamp) -> bool {
    let Some(last) = last_attempt_at else {
        return true;
    };
    let age = now.duration_since(last);
    age.is_negative() || (age.as_secs() as u64) >= CLAIM_TTL.as_secs()
}

#[cfg(test)]
mod tests;
