//! The normalized lifecycle observation and the scaffolding both adapters share.
//!
//! [`AgentLifecycleObservation`] is the single event shape every downstream
//! reducer reads (see [agent.md](../../../../docs/internals/agent.md)); each
//! adapter's `observe_lifecycle` produces one. This module also owns the wiring
//! that is identical across adapters — worktree fields and the
//! payload-overrides-transcript pattern for the context gauge — so the
//! per-adapter code carries only its provider-specific mapping.

use serde_json::Value;

use crate::feed::{AgentStatus, PermissionPosture, RuntimeOwner};
use crate::ids::PaneId;

use super::optional_payload_string;

/// Status + mode transition observed from a lifecycle hook. Returned by
/// [`AgentIntegration::observe_lifecycle`](super::AgentIntegration::observe_lifecycle)
/// so the CLI layer can record an `agent.lifecycle` event without each adapter
/// touching the ledger.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentLifecycleObservation {
    /// Agent-supplied session/process identifier (e.g. Claude `session_id`,
    /// Codex root `session_id`, or Codex subagent `agent_id`). The CLI uses
    /// this as the `agent_id`.
    pub agent_id: Option<String>,
    pub status: AgentStatus,
    /// Process identity observed by the hook runner. The sidebar uses this
    /// best-effort liveness marker to suppress stale ledger overlays when the
    /// process disappears.
    pub agent_pid: Option<u32>,
    pub agent_process_start: Option<String>,
    pub runtime_owner: Option<RuntimeOwner>,
    /// Permission posture pill the event establishes. `None` means "this
    /// event does not report a posture" — the snapshot reducer carries the
    /// prior posture forward rather than resetting it, so a `UserPromptSubmit`
    /// can never demote a `yolo` agent to default (a security surface must
    /// stay visible).
    pub permission_posture: Option<PermissionPosture>,
    /// Optional absolute worktree path observed from the agent payload or
    /// filled by the CLI from the current Rimz workspace.
    pub worktree_path: Option<String>,
    /// Optional worktree branch label observed from the payload, surfaced in
    /// the sidebar's worktree grouping.
    pub worktree_branch: Option<String>,
    /// Display-only task descriptor. It never drives routing or decisions.
    pub task: Option<String>,
    /// The user's latest prompt for this session, carried only by the
    /// prompt-bearing event. The reducer persists it (unlike the activity-bound
    /// `task`), so the sidebar can label an unnamed session by its prompt once
    /// the turn ends, until a real session name exists.
    pub prompt: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Context-window utilization in percent reported by the agent (0..=100).
    /// Enrich-only / privacy-gated — the no-transcript-correctness rule.
    pub context_pct: Option<u8>,
    /// Cumulative token usage for this agent session.
    pub total_tokens: Option<u64>,
    /// Completed / total todos for the agent's current plan or task list.
    pub todo_done: Option<u32>,
    pub todo_total: Option<u32>,
    /// Normalized multiplexer pane id the agent process is running inside,
    /// read from the per-pane env var the mux exports (`TMUX_PANE` or
    /// `ZELLIJ_PANE_ID`). Lets the sidebar bind each agent row to its actual
    /// pane when two agents of the same kind share one worktree.
    pub pane_id: Option<PaneId>,
    /// The root session id this observation's agent is a *child* of, set only
    /// on `SubagentStart`/`SubagentStop` (the payload `session_id`, which both
    /// adapters report as the parent for a subagent event). `None` for root
    /// agents. Identity lifetime in the reducer, so a child row links to its
    /// parent row by `(kind, parent_agent_id)` for the whole child's life.
    pub parent_agent_id: Option<String>,
    /// Whether this event marks the agent compacting its context window (Claude
    /// `PreCompact`, Codex `SessionStart:compact`). The reducer stamps
    /// [`AgentState::compacting_since`](crate::feed::AgentState::compacting_since)
    /// from it without changing the agent's lifecycle status — compaction is a
    /// transient head the sidebar shows, not a state transition.
    pub compacting: bool,
}

impl AgentLifecycleObservation {
    pub(crate) fn new(agent_id: Option<String>, status: AgentStatus) -> Self {
        Self {
            agent_id,
            status,
            agent_pid: None,
            agent_process_start: None,
            runtime_owner: None,
            permission_posture: None,
            worktree_path: None,
            worktree_branch: None,
            task: None,
            prompt: None,
            model: None,
            effort: None,
            context_pct: None,
            total_tokens: None,
            todo_done: None,
            todo_total: None,
            pane_id: None,
            parent_agent_id: None,
            compacting: false,
        }
    }

    /// Fill the worktree fields every adapter reads identically from the payload
    /// (`worktree_path` falls back to `cwd`). Returns `self` for chaining off
    /// [`new`](Self::new).
    pub(crate) fn with_worktree_from_payload(mut self, payload: &Value) -> Self {
        self.worktree_path = optional_payload_string(payload, &["worktree_path", "cwd"]);
        self.worktree_branch = optional_payload_string(payload, &["worktree_branch"]);
        self
    }
}

/// Resolve the context-window percentage: an explicit payload field wins
/// (`context_pct` / `context_window_pct`, clamped to `0..=100`), else the
/// transcript-derived `fallback`. Both adapters share the override; they differ
/// only in how `fallback` is computed (Claude from raw tokens ÷ window, Codex
/// from the rollout's precomputed percentage).
pub(crate) fn payload_context_pct(payload: &Value, fallback: Option<u8>) -> Option<u8> {
    payload
        .get("context_pct")
        .or_else(|| payload.get("context_window_pct"))
        .and_then(Value::as_u64)
        .map(|pct| pct.min(100) as u8)
        .or(fallback)
}

/// Resolve cumulative token usage: an explicit payload field wins
/// (`total_tokens` / `token_count`), else the transcript-derived `fallback`.
pub(crate) fn payload_total_tokens(payload: &Value, fallback: Option<u64>) -> Option<u64> {
    payload
        .get("total_tokens")
        .or_else(|| payload.get("token_count"))
        .and_then(Value::as_u64)
        .or(fallback)
}
