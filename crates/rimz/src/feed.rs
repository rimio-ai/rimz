//! Feed item vocabulary — `Surface`, `FeedStatus`, `FeedKind`, `Resolution`,
//! the resolver chain step shape, and `FeedItem` itself.
//!
//! The wire format here is the product's contract. The three surfaces are
//! `native_ui | bridge | script` per `DESIGN.md`; statuses follow the
//! lifecycle documented in `docs/internals/sidebar/ledger.md`. Do not rename
//! serialized values without updating the docs.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agents::AgentState;
use crate::ids::{RequestId, ResolverId, WorkspaceId};
use crate::pane::{PaneRef, RuntimeOwner};

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
    /// Accepts both canonical names (`permission`) and friendlier profilees
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

    pub const fn is_ask(self) -> bool {
        matches!(
            self,
            Self::Permission | Self::PlanApproval | Self::Question | Self::NeedsInput
        )
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

pub fn ask_summary(title: &str, body: Option<&str>, options: &[String]) -> String {
    let mut text = title.to_owned();
    if let Some(body) = body.map(str::trim).filter(|body| !body.is_empty()) {
        text.push_str(": ");
        text.push_str(body);
    }
    if !options.is_empty() {
        text.push_str(" [");
        text.push_str(&options.join(", "));
        text.push(']');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{AgentStatus, TurnPhase};
    use crate::ids::{AgentKind, AgentSessionId};

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

    #[test]
    fn pending_ask_matches_agent_hook_session_until_agent_moves_on() {
        let now = Timestamp::now();
        let workspace = WorkspaceId::from_project_root(std::path::Path::new("/tmp/x"));
        let mut agent = AgentState {
            agent_id: AgentSessionId::from("sess-1"),
            kind: AgentKind::new_unchecked("claude"),
            name: None,
            kind_ordinal: None,
            profile: None,
            role: None,
            team: None,
            channel: None,
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
            description: None,
            transcript_path: None,
            origin: None,
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
            context: None,
            subagent_description: None,
            subagent_started_at: None,
            turn_started_at: None,
            compacting_since: None,
            compaction_count: 0,
            last_compact_command_tokens: None,
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

    #[test]
    fn ask_summary_renders_body_and_options() {
        assert_eq!(
            ask_summary(
                "approve patch",
                Some(" choose one "),
                &["allow".to_owned(), "deny".to_owned()],
            ),
            "approve patch: choose one [allow, deny]"
        );
        assert_eq!(
            ask_summary("approve patch", Some(" "), &[]),
            "approve patch"
        );
    }
}
