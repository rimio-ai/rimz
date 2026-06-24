//! Durable per-agent message queue domain model.

use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::lifecycle::LifecycleSignal;
use crate::agents::{AgentState, AgentStatus};
use crate::ids::{AgentKind, AgentSessionId, MessageId, PaneId, WorkspaceId};

pub const DEFAULT_SETTLE: Duration = Duration::from_millis(400);
pub const SETTLE_ENV: &str = "RIMZ_QUEUE_SETTLE_MS";
/// Default spacing between discrete steer/queue pane writes.
pub const DEFAULT_MESSAGE_INTERVAL: Duration = Duration::from_secs(1);
pub const MESSAGE_INTERVAL_ENV: &str = "RIMZ_MESSAGE_INTERVAL_MS";
pub const DEFAULT_DELIVERY_WINDOW: Duration = Duration::from_secs(30);
pub const DELIVERY_WINDOW_ENV: &str = "RIMZ_MESSAGE_DELIVERY_WINDOW_MS";
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
                name,
                profile,
                role,
                channel,
            } => {
                let handle = role
                    .as_deref()
                    .filter(|value| !value.is_empty())
                    .or_else(|| name.as_deref().filter(|value| !value.is_empty()))
                    .or_else(|| profile.as_deref().filter(|value| !value.is_empty()))
                    .unwrap_or_else(|| kind.as_str());
                let mut rendered = format!("@{handle}");
                if let Some(channel) = channel.as_deref().filter(|value| !value.is_empty()) {
                    rendered.push('#');
                    rendered.push_str(channel);
                }
                rendered
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryGate {
    Done,
    Any,
}

impl DeliveryGate {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Any => "any",
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
    Created,
    #[serde(alias = "pending")]
    Queued,
    Claimed,
    Sent,
    Delivered,
    TimedOut,
    Errored,
    Removed,
    Abandoned,
}

impl MessageStatus {
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Delivered | Self::TimedOut | Self::Errored | Self::Removed | Self::Abandoned
        )
    }

    pub const fn is_open(self) -> bool {
        matches!(self, Self::Queued | Self::Claimed)
    }

    pub const fn leaves_pending_queue(self) -> bool {
        !matches!(self, Self::Queued)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Queued => "queued",
            Self::Claimed => "claimed",
            Self::Sent => "sent",
            Self::Delivered => "delivered",
            Self::TimedOut => "timed_out",
            Self::Errored => "errored",
            Self::Removed => "removed",
            Self::Abandoned => "abandoned",
        }
    }
}

impl std::fmt::Display for MessageStatus {
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
    #[serde(default)]
    pub sender: MessageSender,
    pub text: String,
    pub enter: bool,
    pub gate: DeliveryGate,
    /// Deliver even when a pending ask reserves the agent's next input. Mirrors
    /// `steer --force`; without it a pending ask defers delivery to a later
    /// boundary.
    #[serde(default)]
    pub force: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<PaneId>,
    pub status: MessageStatus,
    pub enqueued_at: Timestamp,
    pub updated_at: Timestamp,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<Timestamp>,
    /// When set, deliver a `/compact` ahead of the text if the agent's context
    /// fill has reached this threshold at delivery time, so the message lands
    /// against a fresh window instead of racing the agent's own auto-compaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact: Option<AutoCompact>,
}

impl MessageRecord {
    pub fn new(
        workspace_id: WorkspaceId,
        agent: &AgentState,
        text: String,
        enter: bool,
        gate: DeliveryGate,
    ) -> Self {
        let now = Timestamp::now();
        Self {
            message_id: MessageId::new(),
            workspace_id,
            kind: agent.kind.clone(),
            agent_id: agent.agent_id.clone(),
            agent_name: agent.name.clone(),
            sender: MessageSender::Human,
            text,
            enter,
            gate,
            force: false,
            pane_id: None,
            status: MessageStatus::Queued,
            enqueued_at: now,
            updated_at: now,
            attempts: 0,
            last_attempt_at: None,
            last_error: None,
            delivered_at: None,
            auto_compact: None,
        }
    }

    /// Attach a context-fill threshold that delivers a `/compact` ahead of the
    /// text when the window is full enough at delivery time.
    #[must_use]
    pub fn with_auto_compact(mut self, auto_compact: Option<AutoCompact>) -> Self {
        self.auto_compact = auto_compact;
        self
    }

    /// Deliver past a pending ask at the boundary, mirroring `steer --force`.
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

    pub fn same_agent(&self, kind: &AgentKind, agent_id: &AgentSessionId) -> bool {
        self.kind == *kind && self.agent_id == *agent_id
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
        self.kind == *kind
            && (self.agent_id == *agent_id
                || (agent_name.is_some() && self.agent_name.as_deref() == agent_name))
    }

    pub fn same_agent_card(&self, agent: &AgentState) -> bool {
        self.same_card(&agent.kind, &agent.agent_id, agent.name.as_deref())
    }
}

pub fn gate_open(gate: DeliveryGate, status: AgentStatus) -> bool {
    match gate {
        DeliveryGate::Done => matches!(status, AgentStatus::Idle | AgentStatus::Success),
        DeliveryGate::Any => matches!(
            status,
            AgentStatus::Idle | AgentStatus::Success | AgentStatus::Failed
        ),
    }
}

/// The oldest queued message for one logical agent card, the next to deliver.
/// FIFO spans a card's provisional `launch_*` id and the session id it registers
/// as, so pass the stable `agent_name` when known: a message queued before
/// registration still sorts ahead of one queued after.
pub fn queue_head<'a>(
    pending: impl IntoIterator<Item = &'a MessageRecord>,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
    agent_name: Option<&str>,
) -> Option<&'a MessageRecord> {
    pending
        .into_iter()
        .filter(|message| {
            message.status == MessageStatus::Queued && message.same_card(kind, agent_id, agent_name)
        })
        .min_by(|a, b| a.message_id.as_str().cmp(b.message_id.as_str()))
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

/// Spacing between discrete steer/queue pane writes.
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

pub fn claim_expired(last_attempt_at: Option<Timestamp>, now: Timestamp) -> bool {
    let Some(last) = last_attempt_at else {
        return true;
    };
    let age = now.duration_since(last);
    age.is_negative() || (age.as_secs() as u64) >= CLAIM_TTL.as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gates_open_only_on_resting_statuses() {
        assert!(gate_open(DeliveryGate::Done, AgentStatus::Idle));
        assert!(gate_open(DeliveryGate::Done, AgentStatus::Success));
        assert!(!gate_open(DeliveryGate::Done, AgentStatus::Failed));
        assert!(gate_open(DeliveryGate::Any, AgentStatus::Failed));
        for status in [
            AgentStatus::Running,
            AgentStatus::Waiting,
            AgentStatus::Paused,
        ] {
            assert!(!gate_open(DeliveryGate::Done, status));
            assert!(!gate_open(DeliveryGate::Any, status));
        }
    }

    #[test]
    fn delivery_checkpoint_is_only_unparked_turn_end() {
        assert!(delivery_checkpoint(&LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        }));
        assert!(delivery_checkpoint(&LifecycleSignal::TurnEnded {
            errored: true,
            parked_on_background: false,
        }));
        assert!(!delivery_checkpoint(&LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: true,
        }));
        assert!(!delivery_checkpoint(&LifecycleSignal::Registered));
        assert!(!delivery_checkpoint(&LifecycleSignal::SubagentStopped {
            errored: false
        }));
    }

    #[test]
    fn message_status_lifecycle_helpers_match_queue_semantics() {
        assert!(MessageStatus::Queued.is_open());
        assert!(MessageStatus::Claimed.is_open());
        assert!(!MessageStatus::Sent.is_open());
        assert!(!MessageStatus::Sent.is_terminal());
        for status in [
            MessageStatus::Delivered,
            MessageStatus::TimedOut,
            MessageStatus::Errored,
            MessageStatus::Removed,
            MessageStatus::Abandoned,
        ] {
            assert!(status.is_terminal(), "{status}");
        }
        assert!(!MessageStatus::Queued.leaves_pending_queue());
        assert!(MessageStatus::Sent.leaves_pending_queue());

        let legacy: MessageStatus = serde_json::from_str("\"pending\"").unwrap();
        assert_eq!(legacy, MessageStatus::Queued);
    }

    #[test]
    fn claim_ttl_treats_future_stamp_as_expired() {
        let now = Timestamp::now();
        assert!(claim_expired(None, now));
        assert!(!claim_expired(
            Some(now - jiff::SignedDuration::from_secs(1)),
            now
        ));
        assert!(claim_expired(
            Some(now - jiff::SignedDuration::from_secs(15)),
            now
        ));
        assert!(claim_expired(
            Some(now + jiff::SignedDuration::from_secs(60)),
            now
        ));
    }

    #[test]
    fn message_matches_registered_card_by_remembered_name() {
        let mut provisional = agent("launch_1", Some("lucid-atlas"));
        let message = MessageRecord::new(
            WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message")),
            &provisional,
            "next".to_owned(),
            true,
            DeliveryGate::Done,
        );
        provisional.agent_id = AgentSessionId::from("real-session");

        assert!(message.same_agent_card(&provisional));
    }

    #[test]
    fn auto_compact_parses_percent_and_token_forms() {
        assert_eq!(AutoCompact::parse("70%").unwrap(), AutoCompact::Percent(70));
        assert_eq!(AutoCompact::parse(" 0% ").unwrap(), AutoCompact::Percent(0));
        assert_eq!(
            AutoCompact::parse("120000").unwrap(),
            AutoCompact::Tokens(120_000)
        );
        assert!(AutoCompact::parse("101%").is_err());
        assert!(AutoCompact::parse("abc").is_err());
        assert!(AutoCompact::parse("70.5%").is_err());
    }

    #[test]
    fn auto_compact_triggers_only_once_fill_is_reached() {
        let mut a = agent("s1", None);
        // An unknown fill is not a full window.
        assert!(!AutoCompact::Percent(70).triggered(&a));
        assert!(!AutoCompact::Tokens(1).triggered(&a));
        // The percent threshold reads the carried gauge.
        a.context_pct = Some(75);
        assert!(AutoCompact::Percent(70).triggered(&a));
        assert!(AutoCompact::Percent(75).triggered(&a));
        assert!(!AutoCompact::Percent(76).triggered(&a));
        // The token threshold reads the per-call split fallback.
        a.cache_read_input_tokens = Some(100_000);
        a.fresh_input_tokens = Some(20_000);
        assert!(AutoCompact::Tokens(120_000).triggered(&a));
        assert!(!AutoCompact::Tokens(120_001).triggered(&a));
    }

    #[test]
    fn auto_compact_round_trips_through_a_message_record() {
        let message = MessageRecord::new(
            WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message")),
            &agent("s1", None),
            "next".to_owned(),
            true,
            DeliveryGate::Done,
        )
        .with_auto_compact(Some(AutoCompact::Percent(70)));
        let json = serde_json::to_string(&message).unwrap();
        let back: MessageRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.auto_compact, Some(AutoCompact::Percent(70)));
    }

    #[test]
    fn force_defaults_off_and_round_trips_when_set() {
        let base = MessageRecord::new(
            WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message")),
            &agent("s1", None),
            "next".to_owned(),
            true,
            DeliveryGate::Done,
        );
        assert!(
            !base.force,
            "a fresh record never forces past a pending ask"
        );
        let forced = base.with_force(true);
        let json = serde_json::to_string(&forced).unwrap();
        let back: MessageRecord = serde_json::from_str(&json).unwrap();
        assert!(back.force);
        // A record written before the field existed reads as not-forced.
        let legacy = json.replace(",\"force\":true", "");
        let back: MessageRecord = serde_json::from_str(&legacy).unwrap();
        assert!(!back.force);
    }

    #[test]
    fn sender_defaults_to_human_and_agent_round_trips() {
        let base = MessageRecord::new(
            WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message")),
            &agent("s1", None),
            "next".to_owned(),
            true,
            DeliveryGate::Done,
        );
        assert_eq!(base.sender, MessageSender::Human);
        let agent_sender = MessageSender::Agent {
            kind: AgentKind::new_unchecked("claude"),
            name: Some("lucid-atlas".to_owned()),
            profile: Some("planner".to_owned()),
            role: None,
            channel: Some("main".to_owned()),
        };
        let attributed = base.with_sender(agent_sender.clone());
        let json = serde_json::to_string(&attributed).unwrap();
        let back: MessageRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.sender, agent_sender);
        // A record written before sender attribution reads as human-authored.
        let mut legacy = serde_json::to_value(&attributed).unwrap();
        legacy.as_object_mut().unwrap().remove("sender");
        let back: MessageRecord = serde_json::from_value(legacy).unwrap();
        assert_eq!(back.sender, MessageSender::Human);
    }

    #[test]
    fn sender_render_names_human_and_agent_address() {
        assert_eq!(MessageSender::Human.render(), "you");
        assert_eq!(
            MessageSender::Agent {
                kind: AgentKind::new_unchecked("claude"),
                name: Some("lucid-atlas".to_owned()),
                profile: Some("planner".to_owned()),
                role: None,
                channel: Some("docs".to_owned()),
            }
            .render(),
            "@lucid-atlas#docs"
        );
        assert_eq!(
            MessageSender::Agent {
                kind: AgentKind::new_unchecked("codex"),
                name: None,
                profile: None,
                role: None,
                channel: None,
            }
            .render(),
            "@codex"
        );
    }

    #[test]
    fn auto_compact_tokens_threshold_reads_the_carried_total() {
        // A transcript-derived session reports only a running total — no rich
        // context blob and no per-call split. The percent gauge already scales
        // off that total, so the token threshold must read it too rather than
        // silently never firing.
        let mut a = agent("s1", None);
        a.total_tokens = Some(120_000);
        a.context_window = Some(200_000);
        assert!(AutoCompact::Tokens(100_000).triggered(&a));
        assert!(AutoCompact::Tokens(120_000).triggered(&a));
        assert!(!AutoCompact::Tokens(120_001).triggered(&a));
    }

    #[test]
    fn queue_head_spans_provisional_and_registered_ids() {
        // A message queued against a provisional `launch_*` card and a later
        // message queued after the card registers share one logical agent, so
        // FIFO must return the older provisional-card message as the head.
        let provisional = agent("launch_1", Some("lucid-atlas"));
        let registered = agent("real-session", Some("lucid-atlas"));
        let ws = WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message"));
        let mut older = MessageRecord::new(
            ws.clone(),
            &provisional,
            "first".to_owned(),
            true,
            DeliveryGate::Done,
        );
        let mut newer = MessageRecord::new(
            ws,
            &registered,
            "second".to_owned(),
            true,
            DeliveryGate::Done,
        );
        older.message_id = MessageId::parse("msg_00000000000000000000000000000001").unwrap();
        newer.message_id = MessageId::parse("msg_00000000000000000000000000000002").unwrap();
        let pending = [newer.clone(), older.clone()];

        let head = queue_head(
            pending.iter(),
            &registered.kind,
            &registered.agent_id,
            registered.name.as_deref(),
        )
        .expect("the registered observation selects a head");
        assert_eq!(
            head.message_id, older.message_id,
            "the older provisional-card message is the head, not the newer registered one"
        );

        // Without the stable name the provisional record is invisible to the
        // registered id — the reordering this fix closes.
        let exact = queue_head(pending.iter(), &registered.kind, &registered.agent_id, None)
            .expect("the registered id still matches its own record");
        assert_eq!(exact.message_id, newer.message_id);
    }

    fn agent(id: &str, name: Option<&str>) -> AgentState {
        let now = Timestamp::now();
        AgentState {
            agent_id: AgentSessionId::from(id),
            kind: AgentKind::new_unchecked("claude"),
            name: name.map(ToOwned::to_owned),
            kind_ordinal: Some(1),
            profile: None,
            role: None,
            team: None,
            status: AgentStatus::Idle,
            phase: crate::agents::TurnPhase::Idle,
            pane: None,
            agent_pid: None,
            agent_process_start: None,
            runtime_owner: None,
            parent_agent_id: None,
            worktree_path: None,
            worktree_branch: None,
            task: None,
            prompt: None,
            description: None,
            transcript_path: None,
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
            todo_done: None,
            todo_total: None,
            context: None,
            subagent_description: None,
            subagent_started_at: None,
            turn_started_at: None,
            compacting_since: None,
            compaction_count: 0,
            last_seen: now,
            last_activity: now,
            registered_at: Some(now),
        }
    }
}
