//! The normalized lifecycle observation and the scaffolding the adapters share.
//!
//! [`AgentLifecycleObservation`] is the single event shape every downstream
//! reducer reads (see [agent.md](../../../../docs/internals/agents/agent.md)); each
//! adapter's `observe_lifecycle` produces one. This module also owns the wiring
//! that is identical across adapters — worktree fields and the
//! payload-overrides-transcript pattern for the context gauge — so the
//! per-adapter code carries only its provider-specific mapping.

use serde::Serialize;
use serde_json::Value;

use crate::feed::RuntimeOwner;
use crate::ids::{AgentSessionId, PaneId};

use super::optional_payload_string;
use super::{AgentTurnError, lifecycle::LifecycleSignal};

/// One lifecycle observation: the agent-agnostic [`LifecycleSignal`] a native
/// event carries plus the enrichment it reports. Returned by
/// [`AgentAdapter::observe_lifecycle`](super::AgentAdapter::observe_lifecycle)
/// so the CLI layer can record an `agent.lifecycle` event without each adapter
/// touching the ledger. The status is *derived* from the signal through
/// [`step`](super::lifecycle::step), never decided by the adapter.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AgentLifecycleObservation {
    /// Agent-supplied session/process identifier (e.g. Claude `session_id`,
    /// Codex root `session_id`, or Codex subagent `agent_id`). The CLI uses
    /// this as the `agent_id`.
    pub agent_id: Option<AgentSessionId>,
    /// Rimz-minted durable card name. Launchers pass this through
    /// `RIMZ_AGENT_NAME`; hand-launched agents get a deterministic fallback
    /// during reduction.
    pub agent_name: Option<String>,
    /// Display ordinal within this kind for the current room incarnation.
    /// The reducer derives it when the event omits it.
    pub kind_ordinal: Option<u32>,
    /// The agent-agnostic lifecycle intent this event carries. The reducer and
    /// the ingestion path fold it onto the rollup through the one
    /// [`step`](super::lifecycle::step) table; the adapter no longer decides a
    /// final [`AgentStatus`](crate::feed::AgentStatus).
    pub signal: LifecycleSignal,
    /// Process identity observed by the hook runner. The sidebar uses this
    /// best-effort liveness marker to suppress stale ledger overlays when the
    /// process disappears.
    pub agent_pid: Option<u32>,
    pub agent_process_start: Option<String>,
    pub runtime_owner: Option<RuntimeOwner>,
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
    /// The transcript path the agent names for this session, when the adapter
    /// has one. Carry-forward enrichment; readers use it for traceability and
    /// sidecar refresh hints, never as routing truth.
    pub transcript_path: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Context-window utilization in percent reported by the agent (0..=100).
    /// Enrich-only / privacy-gated — the no-transcript-correctness rule.
    pub context_pct: Option<u8>,
    /// The model's context window in tokens, as the adapter resolves it at hook
    /// time (Claude from the `[1m]`-marked payload model, Codex from the
    /// rollout's `model_context_window`). Carry-forward enrichment like
    /// `context_pct`; the sidebar's identity line renders it (`258k`, `1M`).
    pub context_window: Option<u64>,
    /// Cumulative token usage for this agent session.
    pub total_tokens: Option<u64>,
    /// Provider-native turn-death marker discovered while building this
    /// observation. The CLI merges it into the context sidecar; it is skipped in
    /// the durable lifecycle event so the ledger still carries only the
    /// normalized signal.
    #[serde(skip)]
    pub turn_error: Option<AgentTurnError>,
    /// The latest API call's per-call token split — what the agent card's
    /// composition line legends (`◌` cache-read, `◍` cache-write, `↘` fresh
    /// input, `↗` output). Carry-forward enrichment for an agent with no richer
    /// realtime source; Claude's statusline context supersedes it at render.
    pub cache_read_input_tokens: Option<u64>,
    pub cache_write_input_tokens: Option<u64>,
    pub fresh_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
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
    pub parent_agent_id: Option<AgentSessionId>,
}

impl AgentLifecycleObservation {
    pub fn new(agent_id: Option<AgentSessionId>, signal: LifecycleSignal) -> Self {
        Self {
            agent_id,
            agent_name: None,
            kind_ordinal: None,
            signal,
            agent_pid: None,
            agent_process_start: None,
            runtime_owner: None,
            worktree_path: None,
            worktree_branch: None,
            task: None,
            prompt: None,
            transcript_path: None,
            model: None,
            effort: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            turn_error: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            todo_done: None,
            todo_total: None,
            pane_id: None,
            parent_agent_id: None,
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

/// An authoritative context-window percentage stamped on the payload
/// (`context_pct` / `context_window_pct`, clamped to `0..=100`), else the
/// `fallback`. Pi's extension stamps its in-process gauge on every envelope and
/// is the one adapter that reports a percentage directly; Claude and Codex omit
/// it and let the snapshot fold derive the gauge from tokens over the resolved
/// window, so the bar can never disagree with the window it is drawn against.
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
