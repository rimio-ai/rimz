//! Feed item vocabulary — `Surface`, `FeedStatus`, `FeedKind`, `Resolution`,
//! the resolver chain step shape, and `FeedItem` itself.
//!
//! The wire format here is the product's contract. The three surfaces are
//! `native_ui | bridge | script` per `DESIGN.md`; statuses follow the
//! lifecycle documented in `docs/internals/sidebar/ledger.md`. Do not rename
//! serialized values without updating the docs.

use std::collections::BTreeSet;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agents::lifecycle::{LifecycleState, TurnPhase};
use crate::agents::{AgentContext, AgentTurnError, RateLimitWindow, TurnErrorClass};
use crate::ids::{AgentKind, AgentSessionId, PaneId, RequestId, ResolverId, ViewKind, WorkspaceId};

/// One hour: the shared ceiling for attention heat and breath tempo, and the
/// default inactive window below which a card sinks beneath live work.
pub const ATTENTION_AGE_CEILING_SECS: i64 = 3_600;

/// Default `[sidebar.attention] inactive_after_secs`: a row with no activity for
/// this long sinks into the inactive partition, beneath every live row.
pub const DEFAULT_INACTIVE_AFTER_SECS: u32 = ATTENTION_AGE_CEILING_SECS as u32;

/// Runtime owner class for records that should appear in live views.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeOwnerKind {
    Agent,
    Script,
}

impl RuntimeOwnerKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Script => "script",
        }
    }
}

/// Process identity for read-time runtime projection.
///
/// Durable records remain on disk after the owner exits. Runtime views include
/// them only while this process identity is still alive. `process_start` is
/// the Linux `/proc/<pid>/stat` start-time token when available; it defeats PID
/// reuse without becoming a cross-platform requirement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeOwner {
    pub kind: RuntimeOwnerKind,
    pub subject_id: String,
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_start: Option<String>,
}

impl RuntimeOwner {
    pub fn new(
        kind: RuntimeOwnerKind,
        subject_id: impl Into<String>,
        pid: u32,
        process_start: Option<String>,
    ) -> Self {
        Self {
            kind,
            subject_id: subject_id.into(),
            pid,
            process_start,
        }
    }
}

/// A pane-local process hint for an agent CLI running under another real uid.
/// This is display metadata only: it never mutates [`PaneRef::command`], so
/// agent binding and idle synthesis continue to read only mux-reported process
/// identity.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ElevatedAgent {
    pub kind: AgentKind,
    pub uid: u32,
}

/// Which UI is responsible for collecting the answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Surface {
    /// Agent's own native UI in its pane. Rimz routes attention, not decisions.
    NativeUi,
    /// Hook is on the Rimz bridge waiting for a resolver answer.
    Bridge,
    /// Script called `rimz feed ask` and is blocked on the ledger.
    Script,
}

impl Surface {
    pub const fn supports_resolve(self) -> bool {
        matches!(self, Self::Bridge | Self::Script)
    }

    pub const fn supports_dismiss(self) -> bool {
        matches!(self, Self::NativeUi)
    }

    pub const fn hook_blocks(self) -> bool {
        matches!(self, Self::Bridge | Self::Script)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NativeUi => "native_ui",
            Self::Bridge => "bridge",
            Self::Script => "script",
        }
    }
}

impl std::fmt::Display for Surface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Lifecycle state for a feed item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedStatus {
    Pending,
    Resolved,
    TimedOut,
    Abandoned,
}

impl FeedStatus {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Resolved | Self::TimedOut | Self::Abandoned)
    }

    pub const fn allows_resolution(self) -> bool {
        matches!(self, Self::Pending)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Resolved => "resolved",
            Self::TimedOut => "timed_out",
            Self::Abandoned => "abandoned",
        }
    }
}

impl std::fmt::Display for FeedStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a feed item *is* — drives sidebar rendering and verb routing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedKind {
    Permission,
    PlanApproval,
    Question,
    NeedsInput,
    Completion,
    Failure,
    ToolEvent,
    SubAgentStarted,
    SubAgentStopped,
    Generic,
}

impl FeedKind {
    /// Parse a CLI-friendly `--kind` argument.
    ///
    /// Accepts both canonical names (`permission`) and friendlier aliases
    /// (`permission_request`).
    pub fn from_cli(value: &str) -> Self {
        match value {
            "permission" | "permission_request" => Self::Permission,
            "plan" | "plan_approval" => Self::PlanApproval,
            "question" | "user_question" => Self::Question,
            "needs_input" => Self::NeedsInput,
            "completion" => Self::Completion,
            "failure" => Self::Failure,
            "tool_event" => Self::ToolEvent,
            "sub_agent_started" => Self::SubAgentStarted,
            "sub_agent_stopped" => Self::SubAgentStopped,
            _ => Self::Generic,
        }
    }
}

/// How a resolution was delivered. Recorded for audit even when the value
/// landed through pane keystrokes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionMethod {
    HookBridge,
    PaneSend,
    Cli,
    Sidebar,
    Dismiss,
    AgentMovedOn,
    OwnerExited,
    WorkspaceReset,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Resolution {
    pub decision: Value,
    pub method: ResolutionMethod,
    pub resolver_id: Option<ResolverId>,
    pub override_chain: bool,
    pub effective: bool,
    pub late: bool,
    pub reason: Option<String>,
    pub resolved_at: Timestamp,
}

impl Resolution {
    pub fn new(decision: Value, method: ResolutionMethod) -> Self {
        Self {
            decision,
            method,
            resolver_id: None,
            override_chain: false,
            effective: true,
            late: false,
            reason: None,
            resolved_at: Timestamp::now(),
        }
    }
}

/// Canonical reasons the bridge gives up on a resolver chain. Rendered as
/// snake_case strings on disk (audit event `reason` field, resolver step
/// `reason` field); the enum is the vocabulary, the string is the wire form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AbandonReason {
    /// Per-step resolver budget elapsed without an answer; chain advances.
    BudgetElapsed,
    /// Active resolver's heartbeat went stale mid-flight; chain advances.
    HeartbeatStale,
    /// Hook cap fired before the chain could answer; item moves to TimedOut.
    BridgeCapElapsed,
    /// Chain ran out of links without anyone answering; item moves to TimedOut.
    ChainExhausted,
    /// A `rimz feed ask` script hit its own `--timeout`; item moves to TimedOut.
    ScriptWaitTimeout,
    /// A resolver answered after the hook had already returned neutral; the
    /// answer is recorded audit-only on the resolution.
    HookAlreadyReturnedNeutral,
    /// The agent session that raised the ask ended before it was answered; the
    /// pending item is expired so it can't outlive its session.
    AgentSessionEnded,
    /// The agent moved on from a native_ui ask without answering it through
    /// Rimz — a new prompt, the end of its turn, or a fresh ask superseding it.
    /// The agent answered (or dismissed) it in its own UI and will never report
    /// back, so the pending item is expired before it can pile up as attention.
    AgentMovedOn,
    /// The process that owned a pending runtime record exited before the item
    /// reached a terminal state.
    OwnerProcessExited,
    /// The workspace room was reset, killing panes and closing any surviving
    /// waiters that could have answered the pending record.
    WorkspaceReset,
}

impl AbandonReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BudgetElapsed => "budget_elapsed",
            Self::HeartbeatStale => "heartbeat_stale",
            Self::BridgeCapElapsed => "bridge_cap_elapsed",
            Self::ChainExhausted => "chain_exhausted",
            Self::ScriptWaitTimeout => "wait_timeout_elapsed",
            Self::HookAlreadyReturnedNeutral => "hook_already_returned_neutral",
            Self::AgentSessionEnded => "agent_session_ended",
            Self::AgentMovedOn => "agent_moved_on",
            Self::OwnerProcessExited => "owner_process_exited",
            Self::WorkspaceReset => "workspace_reset",
        }
    }
}

impl std::fmt::Display for AbandonReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl AsRef<str> for AbandonReason {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// State of one slot in the resolver chain attached to a feed item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolverStepState {
    Queued,
    Active,
    Answered,
    Abstained,
    BudgetElapsed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResolverStep {
    pub resolver_id: ResolverId,
    pub display_name: Option<String>,
    pub order: i32,
    pub budget_ms: u64,
    pub state: ResolverStepState,
    pub reason: Option<String>,
}

/// Lean pane location attached to a feed item. Carried for routing humans to
/// the right pane — never used for correctness-critical state.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PaneRef {
    pub pane_id: PaneId,
    pub session_name: String,
    /// The view (tab/window) holding the pane, by the backend's *internal* id
    /// (Zellij `tab_15`, tmux `@3`). An opaque grouping key, never the view's
    /// on-screen label: a Zellij tab *named* "Tab #15" and the internal id
    /// `tab_15` are routinely different tabs — see
    /// docs/internals/sidebar/multiplexers.md → Pane and view IDs.
    #[serde(default)]
    pub view_id: Option<String>,
    #[serde(default)]
    pub view_kind: Option<ViewKind>,
    /// View name as reported by the multiplexer (tmux window name, Zellij tab
    /// name). Advisory UI metadata — used to recognise Rimz-launched background
    /// views such as the remote-control host. Never a correctness signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_name: Option<String>,
    /// Whether the pane is its mux view's active pane — the mux marks exactly
    /// one per tab/window, defined whether or not a client is viewing it. The
    /// sidebar derives its selection baseline from it. Advisory UI routing
    /// metadata; ledger correctness never depends on focus.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_focused: bool,
    /// Whether this pane is a floating overlay rather than part of the tiled
    /// room. Floating panes stay addressable but are not rendered as room rows.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_floating: bool,
    /// Foreground command as reported by the multiplexer, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Spawn command used to launch the pane, if the backend reports it.
    /// Advisory identity/classification metadata; display prefers
    /// [`Self::command`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn_command: Option<String>,
    /// Current working directory as reported by the multiplexer, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Best-effort live pane process id. This is advisory routing metadata,
    /// not correctness state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_pid: Option<u32>,
    /// Used to detect reused pane IDs across mux restarts.
    #[serde(default)]
    pub pane_process_start: Option<Timestamp>,
    /// Session id parsed from a resumed agent command such as
    /// `codex resume <session-id>`. Exact rebirth binding reads this before any
    /// cwd or process-start heuristic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed_session_id: Option<AgentSessionId>,
    /// Best-effort marker for an agent descendant running through an elevation
    /// wrapper as another real uid. It stays separate from `command` so the row
    /// can be relabelled without ever binding as a local agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elevated_agent: Option<ElevatedAgent>,
    /// Producer wall-clock millisecond when this pane id first appeared in a
    /// repaired frame. `None` means an older producer/cache wrote the frame, so
    /// newborn-specific guards stay disabled for wire compatibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_seen_at_ms: Option<u64>,
}

impl PaneRef {
    /// A minimal reference carrying just the normalized pane id — the ambient
    /// stamp hooks and script asks record. Live mux truth (command, cwd,
    /// focus, process start) joins at the pane fold.
    pub fn from_id(pane_id: PaneId) -> Self {
        Self {
            pane_id,
            session_name: String::new(),
            view_id: None,
            view_kind: None,
            view_name: None,
            is_focused: false,
            is_floating: false,
            command: None,
            spawn_command: None,
            cwd: None,
            pane_pid: None,
            pane_process_start: None,
            resumed_session_id: None,
            elevated_agent: None,
            first_seen_at_ms: None,
        }
    }

    /// Whether this pane is Rimz's own sidebar chrome. Worktree liveness checks
    /// ignore it because the sidebar inherits its view's cwd without being a
    /// user pane working in that tree.
    pub fn is_rimz_sidebar(&self) -> bool {
        self.command
            .as_deref()
            .is_some_and(crate::ledger::snapshot::command_is_sidebar_chrome)
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeedItem {
    pub workspace_id: WorkspaceId,
    pub request_id: RequestId,
    pub nonce: String,
    pub source: String,
    pub source_kind: String,
    pub kind: FeedKind,
    pub status: FeedStatus,
    pub surface: Surface,
    pub title: String,
    pub body: Option<String>,
    pub options: Vec<String>,
    pub pane: Option<PaneRef>,
    #[serde(default)]
    pub worktree_path: Option<String>,
    #[serde(default)]
    pub worktree_branch: Option<String>,
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_owner: Option<RuntimeOwner>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    /// Hook cap from the agent's protocol. 0 means "no hook is waiting"
    /// (the hook returned its neutral no-op and exited).
    pub hook_wait_timeout_seconds: u64,
    /// Wall-clock deadline for `script`-surface items. None means no upper
    /// bound; the caller has not asked for a timeout.
    pub feed_deadline_at: Option<Timestamp>,
    pub resolution: Option<Resolution>,
    pub chain: Vec<ResolverStep>,
    pub chain_active_resolver: Option<ResolverId>,
    pub chain_active_until: Option<Timestamp>,
}

impl FeedItem {
    pub fn new(
        workspace_id: WorkspaceId,
        surface: Surface,
        kind: FeedKind,
        title: impl Into<String>,
        source: impl Into<String>,
        source_kind: impl Into<String>,
    ) -> Self {
        let now = Timestamp::now();
        Self {
            workspace_id,
            request_id: RequestId::new(),
            nonce: uuid::Uuid::now_v7().simple().to_string(),
            source: source.into(),
            source_kind: source_kind.into(),
            kind,
            status: FeedStatus::Pending,
            surface,
            title: title.into(),
            body: None,
            options: Vec::new(),
            pane: None,
            worktree_path: None,
            worktree_branch: None,
            payload: Value::Object(serde_json::Map::new()),
            runtime_owner: None,
            created_at: now,
            updated_at: now,
            hook_wait_timeout_seconds: 0,
            feed_deadline_at: None,
            resolution: None,
            chain: Vec::new(),
            chain_active_resolver: None,
            chain_active_until: None,
        }
    }

    /// Attach a freshly selected resolver chain to this item. The first
    /// resolver starts active; later resolvers remain queued until an
    /// explicit handoff advances the chain.
    pub fn activate_resolver_chain(&mut self, mut chain: Vec<ResolverStep>) {
        let mut active = None;
        for step in &mut chain {
            if active.is_none() {
                step.state = ResolverStepState::Active;
                step.reason = None;
                active = Some(step.resolver_id.clone());
            } else {
                step.state = ResolverStepState::Queued;
                step.reason = None;
            }
        }

        self.chain = chain;
        self.chain_active_resolver = active;
        self.chain_active_until = self.active_chain_deadline(Timestamp::now());
    }

    /// Mark the resolver that delivered the effective answer. Human
    /// overrides usually have no resolver id; in that case the active chain
    /// slot is still closed so stale links cannot answer later.
    pub fn mark_resolver_answered(&mut self, resolver_id: Option<&ResolverId>) {
        let target = resolver_id.or(self.chain_active_resolver.as_ref());
        if let Some(target) = target
            && let Some(step) = self
                .chain
                .iter_mut()
                .find(|step| &step.resolver_id == target)
        {
            step.state = ResolverStepState::Answered;
            step.reason = None;
        }
        self.chain_active_resolver = None;
        self.chain_active_until = None;
    }

    /// Mark the current active resolver as elapsed and close the chain's
    /// active slot. This records why the bridge fell back without changing
    /// late-answer audit semantics.
    pub fn mark_active_resolver_budget_elapsed(&mut self, reason: AbandonReason) {
        if let Some(active) = self.chain_active_resolver.as_ref()
            && let Some(step) = self
                .chain
                .iter_mut()
                .find(|step| &step.resolver_id == active)
        {
            step.state = ResolverStepState::BudgetElapsed;
            step.reason = Some(reason.as_str().to_owned());
        }
        self.chain_active_resolver = None;
        self.chain_active_until = None;
    }

    /// Advance from the current resolver to the next queued resolver.
    /// Returns the next resolver id when one exists.
    pub fn advance_resolver_chain_after(&mut self, current: &ResolverId) -> Option<ResolverId> {
        let mut found_current = false;
        let mut next = None;
        for step in &mut self.chain {
            if found_current && step.state == ResolverStepState::Queued {
                step.state = ResolverStepState::Active;
                step.reason = None;
                next = Some(step.resolver_id.clone());
                break;
            }
            if &step.resolver_id == current {
                found_current = true;
            }
        }

        self.chain_active_resolver = next.clone();
        self.chain_active_until = self.active_chain_deadline(Timestamp::now());
        next
    }

    fn active_chain_deadline(&self, now: Timestamp) -> Option<Timestamp> {
        let active = self.chain_active_resolver.as_ref()?;
        let step = self.chain.iter().find(|step| &step.resolver_id == active)?;
        Some(now + std::time::Duration::from_millis(step.budget_ms))
    }

    /// The agent session this item belongs to, read from the hook payload
    /// (`agent_id`, falling back to `session_id`). Used to tie an ask to the
    /// agent that raised it so the snapshot can expire it when that session
    /// ends. `None` for non-agent items (scripts, CLI) and payloads without a
    /// session field.
    pub fn agent_session_id(&self) -> Option<&str> {
        ["agent_id", "session_id"].into_iter().find_map(|key| {
            self.payload
                .get(key)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
        })
    }
}

/// Agent status as the sidebar reads it. The first five are the lifecycle
/// rollup the agent owns and Rimz observes; [`Paused`](AgentStatus::Paused) is
/// the one Rimz-*derived* projection — never emitted by a hook, only projected
/// at snapshot time when a live running turn is known to have stopped on a
/// provider limit, the same way a stalled `Running` agent is projected to
/// `Failed`. It lives in the one status enum so it shares the cockpit tally,
/// ranking, and glyph machinery the lifecycle states flow through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Running,
    Waiting,
    Idle,
    Success,
    Failed,
    /// Parked because this agent stopped mid-turn on a provider limit.
    /// Attention-class but non-actionable: there is nothing to do until the
    /// provider recovers or its window resets. Projected from a `Running`
    /// status, never reported by the agent.
    Paused,
}

impl AgentStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Idle => "idle",
            Self::Success => "success",
            Self::Failed => "failed",
            Self::Paused => "paused",
        }
    }

    /// Attention-class: a human (or a resolver) may want this row. `Waiting`
    /// and `Failed` are actionable; `Paused` is attention-class but parked. The
    /// producer's ranking buckets use the full set; the renderer's
    /// triage key and heat-breath use the actionable subset
    /// ([`Self::is_actionable`]). Dispatch sites delegate to these predicates
    /// rather than re-matching the enum.
    pub fn is_attention(self) -> bool {
        matches!(self, Self::Waiting | Self::Failed | Self::Paused)
    }

    /// The actionable subset of [`Self::is_attention`] — a read `?`/`!` the
    /// `␣` triage key jumps to after unread rows, the heat-breath escalates,
    /// and the per-worktree cap never hides. Excludes the parked `Paused`,
    /// which wants the provider or rate-limit window to recover.
    pub fn is_actionable(self) -> bool {
        matches!(self, Self::Waiting | Self::Failed)
    }

    /// Rows that deserve one human look before returning to the read queue:
    /// attention-class states plus a finished result.
    pub fn needs_a_look(self) -> bool {
        self.is_attention() || matches!(self, Self::Success)
    }
}

/// The context meter's four-tier severity ramp — calm → yellow → amber → red.
/// Classified once ([`ContextSeverity::classify`]) from the configured
/// `[sidebar.context]` bands and stamped on each agent's sidebar row where the
/// config is folded onto the snapshot, so the renderer's color ramp and a
/// future hook flow (e.g. a resolver triggering `/compact` at amber) read one
/// verdict instead of re-deriving it. Ordered, so a threshold reads
/// `severity >= Amber`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextSeverity {
    Calm,
    Yellow,
    Amber,
    Red,
}

impl ContextSeverity {
    /// The worse of the fill-percentage ramp and the absolute-token overlay,
    /// each tier entered at its configured inclusive lower bound. The `green`
    /// stop is where the Yellow tier starts warming; `amber` and `red` enter at
    /// their named stops. A large-window model calm by percentage still climbs
    /// by sheer volume.
    /// Checked worst-first, so a misordered user config degrades to the
    /// highest matching tier.
    pub fn classify(
        percent: u8,
        used_tokens: Option<u64>,
        bands: &crate::config::ContextSeverityConfig,
    ) -> Self {
        let percent = percent.min(100);
        let tokens = used_tokens.unwrap_or(0);
        let reaches = |band: &crate::config::ContextBand| -> bool {
            percent >= band.percent || tokens >= band.tokens
        };
        if reaches(&bands.red) {
            Self::Red
        } else if reaches(&bands.amber) {
            Self::Amber
        } else if reaches(&bands.green) {
            Self::Yellow
        } else {
            Self::Calm
        }
    }
}

/// A threshold-crossing an agent's observed state can trip — the typed shape a
/// future hook flow emits and a resolver acts on, riding the same feed the
/// resolver chain already drains (an auto-compact policy matching
/// `ContextSeverity { to: Amber, .. }` and answering with `rimz pane send
/// /compact`, exactly as the pane-send reference resolver acts on a recognised
/// prompt today). Defined now so the seam is typed against the verdicts the
/// snapshot already stamps ([`ContextSeverity`] on each row,
/// [`AgentStatus::is_attention`] on the buckets); emission and handling are
/// deliberately unbuilt — see the hook-readiness note in
/// docs/internals/sidebar/sidebar.md.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentSignal {
    /// The context meter crossed into a different severity tier.
    ContextSeverity {
        from: ContextSeverity,
        to: ContextSeverity,
    },
    /// The agent entered an attention-class status.
    Attention { status: AgentStatus },
}

/// Default window before a `running` agent with no activity is treated as
/// stalled. The per-machine `[sidebar.attention] stalled_after_secs` setting
/// overrides this for the live sidebar projection.
pub const DEFAULT_STALL_AFTER_SECS: u32 = 30 * 60;

/// Whether a `running` agent has gone silent past `stalled_after_secs`. Only
/// `running` can stall: every other status is terminal, idle, or already an
/// attention state. The sidebar projects a stalled agent to the attention
/// bucket so a wedged agent becomes actionable instead of a frozen spinner.
/// "Activity" is the per-tool heartbeat the snapshot folds into
/// `last_activity` (see [`crate::agent_activity`]): it advances on every
/// *completed* tool call, so a busy multi-tool turn stays live. An agent that
/// completes no tool and crosses no turn boundary for the whole window — one
/// long-running tool, or a genuine wedge — is surfaced as `!` so it becomes
/// actionable. The escalation self-heals: the next heartbeat readvances
/// `last_activity`, [`is_stalled`] goes false, and the row leaves attention on
/// the following snapshot with no human action.
///
/// A `running` agent that has merely delegated to subagents is *not* stalled —
/// its work is the children's heartbeats, not its own — so the projection
/// caller suppresses this while the agent has a live child (see the sidebar's
/// "waiting for subagents" derivation).
pub fn is_stalled(
    status: AgentStatus,
    last_activity: Timestamp,
    now: Timestamp,
    stalled_after_secs: u32,
) -> bool {
    status == AgentStatus::Running
        && now.duration_since(last_activity).as_secs() >= i64::from(stalled_after_secs)
}

/// Whether a `running` agent's latest turn died on a provider API error with no
/// `Stop` hook to record it — the transcript-tail marker
/// ([`AgentTurnError`](crate::agents::AgentTurnError), folded in via the context
/// sidecar) postdates the agent's `last_activity`. The faster, more-specific
/// sibling of [`is_stalled`]: the death certificate is explicit, so the sidebar
/// escalates within a statusline push instead of waiting out the stall window.
/// Only `Running` can be turn-dead — a hook-reported turn end already resolved
/// every other status. Self-clearing: any newer hook event (a prompt, a resume,
/// a rewind) advances `last_activity` past the stale marker. The two clocks
/// (transcript wall-clock vs heartbeat) skew fail-safe — a suppressed real
/// death still hits the stall window, and a stale error can never escalate a
/// row whose activity moved past it. Like [`is_stalled`], a Rimz-derived
/// projection over enrichment, never a status the agent reports.
pub fn is_turn_dead(
    status: AgentStatus,
    context: Option<&AgentContext>,
    last_activity: Timestamp,
) -> bool {
    active_turn_error(status, context, last_activity).is_some()
}

/// The turn-error marker that explains a row's displayed state, or `None`. A
/// `running` row reads its *active* marker (one that postdates `last_activity`,
/// the [`is_turn_dead`] death certificate); a `failed` row reads its *terminal*
/// marker (one inside the current turn, so an old error never explains a fresh
/// failure). Every other status has resolved its turn already, so it carries no
/// marker. Shared by the displayed-status projection and the rate-limit-park
/// resume planner so the two never disagree about which error a row is parked
/// on.
pub(crate) fn display_turn_error(
    status: AgentStatus,
    context: Option<&AgentContext>,
    last_activity: Timestamp,
    turn_started_at: Option<Timestamp>,
) -> Option<&AgentTurnError> {
    active_turn_error(status, context, last_activity)
        .or_else(|| terminal_turn_error(status, context, turn_started_at))
}

/// A `running` row's active turn-error marker: it postdates `last_activity`, so
/// the explicit death certificate beats the stall window.
fn active_turn_error(
    status: AgentStatus,
    context: Option<&AgentContext>,
    last_activity: Timestamp,
) -> Option<&AgentTurnError> {
    if status != AgentStatus::Running {
        return None;
    }
    context
        .and_then(|context| context.turn_error.as_ref())
        .filter(|error| error.at > last_activity)
}

/// A `failed` row's terminal turn-error marker: it must fall inside the row's
/// current turn (`turn_started_at` or later), so a stale marker from a prior
/// turn never explains a fresh failure.
fn terminal_turn_error(
    status: AgentStatus,
    context: Option<&AgentContext>,
    turn_started_at: Option<Timestamp>,
) -> Option<&AgentTurnError> {
    if status != AgentStatus::Failed {
        return None;
    }
    let started = turn_started_at?;
    context
        .and_then(|context| context.turn_error.as_ref())
        .filter(|error| error.at >= started)
}

/// Each agent kind's rate-limit window standing, summarized across every session
/// of that kind (the windows are account-scoped, so any session's reading speaks
/// for the kind). A kind lands in `spent` while it has a window that is exhausted
/// and not yet reset, and in `reset` once a window it had spent has refilled — the
/// signal that lifts a `rate_limit` park.
#[derive(Default)]
pub(crate) struct RateLimitKindSummary {
    pub spent: BTreeSet<AgentKind>,
    pub reset: BTreeSet<AgentKind>,
}

/// Summarize every kind's spent/reset window standing from the agents' own
/// rate-limit readings. Drives the displayed-status projection: a `rate_limit`
/// park lifts to `failed` once its windows reset. The park resume planner derives
/// its own per-agent deadline ([`resume_park`]) so it can persist it before the
/// ephemeral reading turns over.
pub(crate) fn rate_limit_window_kinds(
    agents: &[AgentState],
    now: Timestamp,
) -> RateLimitKindSummary {
    let mut summary = RateLimitKindSummary::default();
    for agent in agents {
        if agent.parent_agent_id.is_some() {
            continue;
        }
        let Some(limits) = agent
            .context
            .as_ref()
            .and_then(|ctx| ctx.rate_limits.as_ref())
        else {
            continue;
        };
        let mut has_spent = false;
        let mut has_reset = false;
        for window in &limits.windows {
            if !window.is_spent() {
                continue;
            }
            if window_spent_unreset(window, now) {
                has_spent = true;
            } else {
                has_reset = true;
            }
        }
        if has_spent {
            summary.spent.insert(agent.kind.clone());
        }
        if has_reset {
            summary.reset.insert(agent.kind.clone());
        }
    }
    summary
}

/// Whether a window is spent and has not yet reset — the budget is gone *now*.
fn window_spent_unreset(window: &RateLimitWindow, now: Timestamp) -> bool {
    window.is_spent() && window.resets_at.is_none_or(|reset| reset > now)
}

/// How a parked root agent's turn may resume, or `None` when nothing is armed.
/// The producer persists the arm while the park is fresh so the resume outlives
/// the ephemeral context it was first seen through. A Rimz-derived projection over
/// enrichment, never a status the agent reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ResumeArm {
    /// A `rate_limit` park: the turn may resume at the latest reset among the
    /// windows still spent *now* — the instant the displayed-status projection
    /// would lift the park from `paused` to `failed`. Persisting this deadline
    /// keeps the resume alive once the reading turns over: neither a
    /// context-sidecar TTL expiry (a 5h/7d window outlasts the 3h context TTL) nor
    /// a fresh non-spent reading (Codex's app-server refresh rolls the window
    /// forward) can erase it.
    RateLimit { deadline: Timestamp },
    /// An `overloaded` park: the provider was overloaded, so there is no local
    /// reset window to wait on. The producer retries the resume on an expanding
    /// backoff while the park stays active, rather than against a window clock.
    Overloaded {
        /// The overload turn-error marker timestamp. The first retry is measured
        /// from this marker, so a late-observed park can fire immediately.
        overloaded_at: Timestamp,
    },
}

/// What kind of resume, if any, this root agent's parked turn is armed for. It
/// stopped its last turn on a provider park certificate ([`display_turn_error`]):
/// a `rate_limit` park arms for the latest reset of the windows still spent now
/// (and stops arming once every spent window has reset), while an `overloaded`
/// park arms while its marker stays active. Every other class — and a `rate_limit`
/// park whose budget has already refilled — arms nothing.
pub(crate) fn resume_park(agent: &AgentState, now: Timestamp) -> Option<ResumeArm> {
    if agent.parent_agent_id.is_some() || agent.agent_id.is_empty() {
        return None;
    }
    let error = display_turn_error(
        agent.status,
        agent.context.as_ref(),
        agent.last_activity,
        agent.turn_started_at,
    )?;
    match error.class {
        TurnErrorClass::PausedRateLimit => {
            let deadline = agent
                .context
                .as_ref()
                .and_then(|ctx| ctx.rate_limits.as_ref())
                .into_iter()
                .flat_map(|limits| limits.windows.iter())
                .filter(|window| window_spent_unreset(window, now))
                .filter_map(|window| window.resets_at)
                .max()?;
            Some(ResumeArm::RateLimit { deadline })
        }
        TurnErrorClass::PausedOverloaded => Some(ResumeArm::Overloaded {
            overloaded_at: error.at,
        }),
        TurnErrorClass::Failed => None,
    }
}

/// Whether a `running` agent's latest turn completed cleanly with no `Stop` hook
/// to record it — the rollout-tail marker (`AgentContext::turn_complete`, folded
/// in via the context sidecar) postdates the agent's `last_activity`. The
/// success sibling of [`is_turn_dead`]: a Codex `/review` runs in review mode and
/// ends on a `task_complete` that fires no `Stop`, so the lifecycle state machine
/// never leaves `running`; this settles the row to `success` instead of letting
/// the stall window misread a finished review as failed. Only `Running` can be
/// turn-complete — a hook-reported turn end already resolved every other status.
/// Self-clearing like [`is_turn_dead`]: any newer hook event advances
/// `last_activity` past the marker. A Rimz-derived projection over enrichment,
/// never a status the agent reports.
pub fn is_turn_complete(
    status: AgentStatus,
    context: Option<&crate::agents::context::AgentContext>,
    last_activity: Timestamp,
) -> bool {
    status == AgentStatus::Running
        && context
            .and_then(|context| context.turn_complete)
            .is_some_and(|at| at > last_activity)
}

/// How long after its last compaction-start signal an agent still reads as
/// "compacting". The session's next lifecycle signal clears
/// [`AgentState::compacting_since`], but a crash mid-compact with no next
/// signal would otherwise leave the head pulsing forever, so the projection
/// also expires it past this window. Generous: a large context can take a while
/// to condense.
pub const COMPACTING_WINDOW_SECS: i64 = 90;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentState {
    pub agent_id: AgentSessionId,
    pub kind: AgentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind_ordinal: Option<u32>,
    /// The `[agents.aliases]` role this agent launched as (`planner`,
    /// `codex-yolo`), passed through `RIMZ_AGENT_ALIAS` and carried forward like
    /// `name`. The agent answers to `@<alias>` and renders by it; `None` for a
    /// bare-kind launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    pub status: AgentStatus,
    /// The running turn's shape (reasoning / acting / parked on background
    /// work), written verbatim from the lifecycle machine's output. Always
    /// [`TurnPhase::Idle`] outside `Running` — the machine normalizes it, so
    /// the illegal combinations are unrepresentable here too.
    #[serde(default)]
    pub phase: TurnPhase,
    pub pane: Option<PaneRef>,
    #[serde(default)]
    pub agent_pid: Option<u32>,
    #[serde(default)]
    pub agent_process_start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_owner: Option<RuntimeOwner>,
    /// The root session id this agent is a *child* of, set only when a
    /// `SubagentStart` established it (identity, carried forward). `None` for a
    /// root agent. The sidebar nests a child under its parent row by
    /// `(kind, parent_agent_id)` and never renders a child as a top-level row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_agent_id: Option<AgentSessionId>,
    pub worktree_path: Option<String>,
    pub worktree_branch: Option<String>,
    pub task: Option<String>,
    /// The user's latest prompt, carried forward across events (unlike the
    /// activity-bound `task`). Labels an unnamed session on the sidebar until a
    /// real session name exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Agent-reported transcript path for this session, carried forward from
    /// lifecycle events when available. Display/diagnostic metadata; sidecar
    /// readers keep their own freshness gates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    /// Recent user prompts for this session, newest last, capped by the rollup.
    /// The sidebar row keeps only `prompt`; snapshot JSON exposes the history on
    /// `agents[]` for diagnostics and future panes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_prompts: Vec<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Context-window utilization in percent (0..=100). Reported by the
    /// agent's hooks when available; `None` while the agent hasn't surfaced
    /// it. Display-only — never drives a decision (the no-transcript-correctness
    /// rule). Sidebar row projection renders that unknown state as the visible
    /// 0% baseline, but the reduced agent state keeps the distinction.
    #[serde(default)]
    pub context_pct: Option<u8>,
    /// The model's context window in tokens (`258_400`, `1_000_000`), resolved
    /// by the adapter at hook time. Same enrich-only, carry-forward discipline
    /// as `context_pct`; the card's identity line renders it (`258k`, `1M`).
    #[serde(default)]
    pub context_window: Option<u64>,
    /// Cumulative token usage for this agent session. Same enrich-only
    /// discipline as `context_pct`.
    #[serde(default)]
    pub total_tokens: Option<u64>,
    /// The latest API call's per-call token split (`◌` cache-read, `◍`
    /// cache-write, `↘` fresh input, `↗` output), carried forward like
    /// `total_tokens`. The card's composition line falls back to it when no
    /// richer realtime context (Claude's statusline) is present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// Completed and total todos for the agent's current plan, as reported
    /// by the agent's plan/todo tool. `todo_total = 0` (or `None`) renders as
    /// "no todo state".
    #[serde(default)]
    pub todo_done: Option<u32>,
    #[serde(default)]
    pub todo_total: Option<u32>,
    /// Rich session-scoped enrichment from a high-frequency out-of-band source
    /// (Claude's statusline). Folded in at snapshot time by
    /// `SidebarSnapshot::with_agent_context`, never reduced from the event log.
    /// Same enrich-only discipline as `context_pct`: display, never routing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<crate::agents::context::AgentContext>,
    /// What the parent asked this *subagent* to do, harvested from Claude's
    /// `subagentStatusLine`. Folded in at snapshot time by
    /// `SidebarSnapshot::with_subagent_context`, never reduced from the event
    /// log; always `None` for a root agent. The expanded card prefers it over the
    /// activity-bound `task` on a child's first row. Same enrich-only discipline
    /// as `context`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_description: Option<String>,
    /// When this *subagent* began (its `subagentStatusLine` `startTime`), folded
    /// in alongside `subagent_description`. The card derives elapsed work from it;
    /// `None` for a root agent or before the first render.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_started_at: Option<Timestamp>,
    /// When this agent's current turn began, stamped from the lifecycle state
    /// machine's `opened_turn` fact and from a context reset that rests the agent
    /// — a manual `/compact` (`CompactionEnded` landing on idle) or a `/clear`
    /// (`Registered`), each of which retires the prior turn's children (carried
    /// forward; `None` until the first such boundary). Automatic compaction
    /// *mid-turn* resumes the same turn and leaves this stamp untouched, so its
    /// in-flight children stay listed. Unlike `last_seen` it does *not* advance
    /// on `Stop`, so it marks the "next prompt" boundary the sidebar uses to
    /// clear a finished subagent: a completed child older than its parent's
    /// `turn_started_at` belongs to a past turn and drops from the parent's
    /// expanded list. A prompt waking a parked running row resumes the same
    /// logical turn and carries this stamp forward.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_started_at: Option<Timestamp>,
    /// When this agent last began compacting its context window — the timestamp
    /// of its most-recent compaction-start signal (`PreCompact` or Pi
    /// `session_before_compact`). Set by the rollup, cleared by the session's
    /// next lifecycle signal; the sidebar renders a transient "compacting" head
    /// while it is recent (see [`COMPACTING_WINDOW_SECS`]). Display-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compacting_since: Option<Timestamp>,
    /// How many times this session has condensed its context window — the count
    /// of completed compaction brackets. Derived by the rollup from the state
    /// machine's bracket-close fact, carried forward unchanged on every other
    /// event, and rendered by the card as `↻ N` from the first completed
    /// compaction. Display-only.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub compaction_count: u32,
    pub last_seen: Timestamp,
    pub last_activity: Timestamp,
    /// When this session first entered the rollup — the timestamp of its
    /// earliest reduced event, set once and carried forward unchanged
    /// (identity, never activity). The sidebar's calm tiebreak falls back to it
    /// as the row's spawn key when the backend reports no pane process start
    /// (Zellij), so a calm row holds a stable order without one. `None` only on
    /// a rollup persisted before the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registered_at: Option<Timestamp>,
}

fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

impl AgentState {
    /// The lifecycle-machine view of this rollup entry — exactly the `prev` the
    /// reducer (and the ingestion anomaly log) folds the next signal onto.
    /// Lossless: `status` and `phase` are stored verbatim from the machine's
    /// last output, and the compacting head persists as `compacting_since`.
    pub fn lifecycle(&self) -> LifecycleState {
        LifecycleState {
            status: self.status,
            phase: self.phase,
            compacting: self.compacting_since.is_some(),
        }
    }

    /// Tokens currently occupying the window: the folded statusline breakdown,
    /// else the per-call split (`cache_read + cache_write + fresh_input`) the lifecycle rail
    /// reduces. `None` when nothing has reported occupancy yet.
    pub fn context_used_tokens(&self) -> Option<u64> {
        self.context
            .as_ref()
            .and_then(|context| context.tokens.as_ref())
            .and_then(crate::agents::AgentTokenUsage::used_tokens)
            .or_else(|| {
                let fresh = self.fresh_input_tokens?;
                Some(
                    self.cache_read_input_tokens.unwrap_or(0)
                        + self.cache_write_input_tokens.unwrap_or(0)
                        + fresh,
                )
            })
    }

    /// Tokens occupying the window for a `--auto-compact <tokens>` threshold: the
    /// precise composition when known, else the carried turn total. The gauge's
    /// `context_used_tokens` withholds a bare total so it never legends a partial
    /// composition; a threshold instead scales against the same numerator the
    /// percent gauge derives from, so `--auto-compact 100000` fires for a
    /// transcript-derived session that reports only a running total — matching
    /// `--auto-compact 70%`, which already reads that total through the gauge.
    pub fn occupied_context_tokens(&self) -> Option<u64> {
        self.context_used_tokens().or(self.total_tokens)
    }

    /// The window denominator: the folded statusline's `context_window_size`,
    /// else the adapter-resolved `context_window`, else the model descriptor's
    /// default.
    pub fn resolved_context_window(&self) -> Option<u64> {
        self.context
            .as_ref()
            .and_then(|context| context.tokens.as_ref())
            .and_then(|tokens| tokens.context_window_size)
            .or(self.context_window)
            .or_else(|| {
                crate::agents::descriptor_by_kind(self.kind.as_str())
                    .and_then(|descriptor| descriptor.default_context_window)
            })
    }

    /// The real context-window fill (0..=100): the live token composition over
    /// the model's window. Prefers the precise used/window fraction, then the
    /// folded statusline's `used_percentage`, then the carried `context_pct`, so
    /// a session with no rich context still reads its last gauge. `None` when no
    /// source has reported a fill.
    pub fn context_fill_pct(&self) -> Option<f64> {
        match (self.context_used_tokens(), self.resolved_context_window()) {
            (Some(used), Some(window)) if window > 0 => {
                Some((used as f64 / window as f64 * 100.0).clamp(0.0, 100.0))
            }
            _ => self
                .context
                .as_ref()
                .and_then(|context| context.tokens.as_ref())
                .and_then(|tokens| tokens.used_percentage)
                .or(self.context_pct)
                .map(f64::from),
        }
    }
}

/// The pending ask currently blocking `agent`, if any.
///
/// This is the shared authority behind the sidebar's displayed `waiting`
/// projection and the message-sending gates. A newer agent activity timestamp
/// means the agent moved past the ask in its own UI, so the stale item no
/// longer blocks pane input.
pub fn pending_ask_for<'a>(
    agent: &AgentState,
    items: impl IntoIterator<Item = &'a FeedItem>,
) -> Option<&'a FeedItem> {
    items.into_iter().find(|item| {
        item.source_kind == "agent-hook"
            && item.status == FeedStatus::Pending
            && item.source == agent.kind
            && item.agent_session_id() == Some(agent.agent_id.as_str())
            && agent.last_activity <= item.updated_at
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_serializes_to_documented_wire_format() {
        assert_eq!(
            serde_json::to_string(&Surface::NativeUi).unwrap(),
            "\"native_ui\""
        );
        assert_eq!(
            serde_json::to_string(&Surface::Bridge).unwrap(),
            "\"bridge\""
        );
        assert_eq!(
            serde_json::to_string(&Surface::Script).unwrap(),
            "\"script\""
        );
    }

    #[test]
    fn feed_status_round_trips() {
        for status in [
            FeedStatus::Pending,
            FeedStatus::Resolved,
            FeedStatus::TimedOut,
            FeedStatus::Abandoned,
        ] {
            let wire = serde_json::to_string(&status).unwrap();
            let back: FeedStatus = serde_json::from_str(&wire).unwrap();
            assert_eq!(status, back);
        }
    }

    #[test]
    fn pane_ref_classifies_rimz_sidebar_from_command() {
        fn pane(command: Option<&str>) -> PaneRef {
            PaneRef {
                command: command.map(ToOwned::to_owned),
                cwd: Some("/repo-worktrees/demo".to_owned()),
                ..PaneRef::from_id(PaneId::from_parts(
                    crate::ids::MuxName::Zellij,
                    "terminal_1",
                ))
            }
        }

        assert!(pane(Some("rimz-sidebar")).is_rimz_sidebar());
        assert!(!pane(Some("codex")).is_rimz_sidebar());
        assert!(!pane(Some("zsh")).is_rimz_sidebar());
        assert!(!pane(None).is_rimz_sidebar());
    }

    #[test]
    fn surface_capabilities_match_design() {
        assert!(Surface::NativeUi.supports_dismiss());
        assert!(!Surface::NativeUi.supports_resolve());
        assert!(Surface::Bridge.supports_resolve());
        assert!(Surface::Script.supports_resolve());
        assert!(Surface::Bridge.hook_blocks());
        assert!(!Surface::NativeUi.hook_blocks());
    }

    #[test]
    fn pending_allows_resolution_only() {
        assert!(FeedStatus::Pending.allows_resolution());
        assert!(!FeedStatus::Resolved.allows_resolution());
        assert!(FeedStatus::Resolved.is_terminal());
        assert!(!FeedStatus::Pending.is_terminal());
    }

    #[test]
    fn pending_ask_matches_agent_hook_session_until_agent_moves_on() {
        let now = Timestamp::now();
        let workspace = WorkspaceId::from_project_root(std::path::Path::new("/tmp/x"));
        let mut agent = AgentState {
            agent_id: AgentSessionId::from("sess-1"),
            kind: AgentKind::new_unchecked("claude"),
            name: None,
            kind_ordinal: None,
            alias: None,
            status: AgentStatus::Running,
            phase: TurnPhase::Reasoning,
            pane: None,
            agent_pid: None,
            agent_process_start: None,
            runtime_owner: None,
            parent_agent_id: None,
            worktree_path: None,
            worktree_branch: None,
            task: None,
            prompt: None,
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
        };
        let mut item = FeedItem::new(
            workspace,
            Surface::NativeUi,
            FeedKind::Permission,
            "approve?",
            "claude",
            "agent-hook",
        );
        item.payload = serde_json::json!({ "session_id": "sess-1" });
        item.updated_at = now + std::time::Duration::from_secs(1);

        assert_eq!(
            pending_ask_for(&agent, [&item]).map(|ask| &ask.request_id),
            Some(&item.request_id),
        );
        agent.last_activity = item.updated_at + std::time::Duration::from_secs(1);
        assert!(pending_ask_for(&agent, [&item]).is_none());
    }

    /// The context tier climbs calm → yellow → amber → red, taking the worse
    /// of two axes — fill percentage and absolute tokens. Defaults: the Yellow
    /// tier starts warming at 40% / 100k, amber starts at 75% / 258k, and red
    /// starts at 90% / 420k.
    #[test]
    fn context_severity_takes_the_worse_of_percent_and_tokens() {
        let bands = crate::config::ContextSeverityConfig::default();
        let tier = |percent, tokens| ContextSeverity::classify(percent, tokens, &bands);
        // Low fill, low tokens: calm.
        assert_eq!(tier(20, Some(50_000)), ContextSeverity::Calm);
        // Just under both green-start bounds stays calm; the bound itself enters.
        assert_eq!(tier(39, Some(99_999)), ContextSeverity::Calm);
        assert_eq!(tier(40, Some(10_000)), ContextSeverity::Yellow);
        assert_eq!(tier(10, Some(100_000)), ContextSeverity::Yellow);
        // The percentage ramp alone climbs through all four tiers.
        assert_eq!(tier(75, Some(10_000)), ContextSeverity::Amber);
        assert_eq!(tier(90, Some(10_000)), ContextSeverity::Red);
        // Calm by percentage, but the token volume escalates it.
        assert_eq!(tier(20, Some(258_000)), ContextSeverity::Amber);
        assert_eq!(tier(20, Some(420_000)), ContextSeverity::Red);
        // The worse severity wins regardless of which axis it comes from.
        assert_eq!(tier(89, Some(419_999)), ContextSeverity::Amber);
        // No token reading falls back to the percentage ramp alone.
        assert_eq!(tier(75, None), ContextSeverity::Amber);
        assert_eq!(tier(10, None), ContextSeverity::Calm);
        // An out-of-range percent clamps to full and reads red.
        assert_eq!(tier(200, None), ContextSeverity::Red);
        // The tiers order, so a future hook threshold reads naturally.
        assert!(ContextSeverity::Amber > ContextSeverity::Yellow);
    }

    /// The bands come from `[sidebar.context]`, so a custom set moves every
    /// edge; a misordered set degrades to the highest matching tier (the red
    /// band is checked first), never to a calmer one.
    #[test]
    fn context_severity_honours_custom_and_misordered_bands() {
        use crate::config::{ContextBand, ContextSeverityConfig};
        let tight = ContextSeverityConfig {
            green: ContextBand {
                percent: 10,
                tokens: 1_000,
            },
            yellow: ContextBand {
                percent: 20,
                tokens: 2_000,
            },
            amber: ContextBand {
                percent: 30,
                tokens: 3_000,
            },
            red: ContextBand {
                percent: 40,
                tokens: 4_000,
            },
        };
        assert_eq!(
            ContextSeverity::classify(5, Some(500), &tight),
            ContextSeverity::Calm
        );
        assert_eq!(
            ContextSeverity::classify(25, Some(0), &tight),
            ContextSeverity::Yellow
        );
        assert_eq!(
            ContextSeverity::classify(35, Some(0), &tight),
            ContextSeverity::Amber
        );
        assert_eq!(
            ContextSeverity::classify(5, Some(4_000), &tight),
            ContextSeverity::Red
        );

        // Red configured *below* yellow: a mid fill reaches the red band even
        // though the calmer tiers do not — worst-first keeps the warning loud.
        let misordered = ContextSeverityConfig {
            green: ContextBand {
                percent: 95,
                tokens: 950_000,
            },
            yellow: ContextBand {
                percent: 90,
                tokens: 900_000,
            },
            amber: ContextBand {
                percent: 80,
                tokens: 800_000,
            },
            red: ContextBand {
                percent: 50,
                tokens: 500_000,
            },
        };
        assert_eq!(
            ContextSeverity::classify(60, None, &misordered),
            ContextSeverity::Red
        );
    }

    /// Pins the signal's wire shape now, so the first emitter and handler
    /// build against a stable contract rather than re-negotiating it.
    #[test]
    fn agent_signal_serializes_to_a_tagged_wire_shape() {
        assert_eq!(
            serde_json::to_value(AgentSignal::ContextSeverity {
                from: ContextSeverity::Yellow,
                to: ContextSeverity::Amber,
            })
            .unwrap(),
            serde_json::json!({
                "kind": "context_severity",
                "from": "yellow",
                "to": "amber",
            })
        );
        assert_eq!(
            serde_json::to_value(AgentSignal::Attention {
                status: AgentStatus::Waiting,
            })
            .unwrap(),
            serde_json::json!({ "kind": "attention", "status": "waiting" })
        );
    }

    #[test]
    fn attention_predicates_split_actionable_from_parked() {
        // The two intentional flavors: ranking spans the parked Paused,
        // the triage/heat subset does not. Calm states are in neither.
        for status in [AgentStatus::Waiting, AgentStatus::Failed] {
            assert!(status.is_attention());
            assert!(status.is_actionable());
            assert!(status.needs_a_look());
        }
        assert!(AgentStatus::Paused.is_attention());
        assert!(!AgentStatus::Paused.is_actionable());
        assert!(AgentStatus::Paused.needs_a_look());
        assert!(!AgentStatus::Success.is_attention());
        assert!(!AgentStatus::Success.is_actionable());
        assert!(AgentStatus::Success.needs_a_look());
        for status in [AgentStatus::Running, AgentStatus::Idle] {
            assert!(!status.is_attention());
            assert!(!status.is_actionable());
            assert!(!status.needs_a_look());
        }
    }

    #[test]
    fn agent_status_round_trips_including_paused() {
        for status in [
            AgentStatus::Running,
            AgentStatus::Waiting,
            AgentStatus::Idle,
            AgentStatus::Success,
            AgentStatus::Failed,
            AgentStatus::Paused,
        ] {
            let wire = serde_json::to_string(&status).unwrap();
            let back: AgentStatus = serde_json::from_str(&wire).unwrap();
            assert_eq!(status, back);
        }
        // The derived state has a stable snake_case wire form like the rest.
        assert_eq!(
            serde_json::to_string(&AgentStatus::Paused).unwrap(),
            "\"paused\""
        );
    }
}
