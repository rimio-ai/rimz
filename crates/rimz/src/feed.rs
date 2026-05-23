//! Feed item vocabulary — `Surface`, `FeedStatus`, `FeedKind`, `Resolution`,
//! the resolver chain step shape, and `FeedItem` itself.
//!
//! The wire format here is the product's contract. The three surfaces are
//! `native_ui | bridge | script` per `DESIGN.md`; statuses follow the
//! lifecycle documented in `docs/internals/ledger.md`. Do not rename
//! serialized values without updating the docs.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::{PaneId, RequestId, ResolverId, ViewKind, WorkspaceId};

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
    ToolTelemetry,
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
            "tool_telemetry" => Self::ToolTelemetry,
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
    pub budget_ms: i64,
    pub state: ResolverStepState,
    pub reason: Option<String>,
}

/// Pane location attached to a feed item. Carried for routing humans to the
/// right pane — never used for correctness-critical state.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PaneRef {
    pub pane_id: PaneId,
    pub session_name: String,
    pub view_id: Option<String>,
    pub view_kind: Option<ViewKind>,
    /// Used to detect reused pane IDs across mux restarts.
    pub pane_process_start: Option<Timestamp>,
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
    pub worktree_path: Option<String>,
    pub payload: Value,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    /// Hook cap from the agent's protocol. 0 means "no hook is waiting"
    /// (the hook returned the neutral payload and exited).
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
            payload: Value::Object(serde_json::Map::new()),
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
        let budget_ms = u64::try_from(step.budget_ms).unwrap_or(0);
        Some(now + std::time::Duration::from_millis(budget_ms))
    }
}

/// Five-value agent status rollup; the agent owns this, Rimz observes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Running,
    Waiting,
    Idle,
    Success,
    Failed,
}

/// Agent mode pill; the agent owns this too.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMode {
    Interactive,
    Plan,
    Auto,
    Bypass,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentState {
    pub agent_id: String,
    pub kind: String,
    pub status: AgentStatus,
    pub mode: AgentMode,
    pub pane: Option<PaneRef>,
    pub worktree_path: Option<String>,
    pub worktree_branch: Option<String>,
    pub last_seen: Timestamp,
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
}
