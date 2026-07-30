//! The normalized lifecycle observation and the scaffolding the adapters share.
//!
//! [`AgentLifecycleObservation`] is the single event shape every downstream
//! reducer reads (see [model.md](../../../../docs/internals/agents/model.md)); each
//! definition's hook capability may produce one. This module also owns the wiring
//! that is identical across adapters — worktree fields and the
//! payload-overrides-transcript pattern for the context gauge — so the
//! per-adapter code carries only its provider-specific mapping.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::harness::run::PermissionMode;
use crate::ids::{AgentSessionId, PaneId};
use crate::pane::{PaneRef, RuntimeOwner};

use super::lifecycle::LifecycleSignal;
use super::optional_payload_string;

/// A provider session's origin, read from the session store head record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionOrigin {
    /// A fresh `/clear` / `/new` conversation with no fork parent.
    Fresh,
    /// A `/side` / `/btw` / `/fork` thread carrying a parent id.
    Forked,
}

/// Launcher-selected parameters shared by launch and lifecycle event payloads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchParams {
    /// Direct parent for a pane-backed `rimz subagents` child. This is store
    /// identity, not process environment, and stays absent for top-level peer
    /// launches and provider-native subagents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_agent_id: Option<AgentSessionId>,
    /// Provider kind of `parent_agent_id`. Launched children can cross
    /// provider kinds; provider-native children default to their own kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_agent_kind: Option<crate::ids::AgentKind>,
    /// Launch generation from the human root. With a parent, `Some`
    /// distinguishes a pane-backed launched child from a provider-native,
    /// paneless subagent; without a parent, it tracks a top-level peer chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_depth: Option<u8>,
    /// The `[agents.profiles]` profile the launcher selected, passed through
    /// `RIMZ_AGENT_PROFILE`. Used as the card handle when no role is present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// The permission posture selected by the launcher. Stored durably so an
    /// explicit restart can reproduce it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<PermissionMode>,
    /// The `[agents.teams]` role the launcher selected, passed through
    /// `RIMZ_AGENT_ROLE`. The reducer projects it to the card handle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// The launcher-selected dollar cap, passed through `RIMZ_AGENT_BUDGET`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<String>,
    /// The `[agents.teams]` team name the launcher selected, passed through
    /// `RIMZ_TEAM`. It is role/cohort/resume identity; routing uses `channel`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// The inline multi-agent launch cohort the launcher minted, passed through
    /// `RIMZ_LAUNCH_GROUP`. Team launches use `RIMZ_TEAM` as the cohort key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_group: Option<String>,
    /// The agent's order inside its launch cohort, passed through
    /// `RIMZ_LAUNCH_ORDINAL`. Team launches use role-list order; inline
    /// layouts use agent-cell order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_ordinal: Option<u32>,
    /// The routing lane the launcher selected, passed through `RIMZ_CHANNEL`.
    /// In-place teams already arrive as a stamped `<dir>/<team>` lane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// Display ordinal within this kind for the current room incarnation.
    /// The reducer derives it when the event omits it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind_ordinal: Option<u32>,
}

/// One provider-neutral parent candidate for a hook whose own session id may
/// belong to a subagent. The hook path supplies only already-observed durable
/// identity and pane-local paths; the adapter decides whether its native
/// transcript proves the child relation.
#[derive(Clone, Copy, Debug)]
pub struct SubagentCorrelationInput<'a> {
    pub child_agent_id: &'a AgentSessionId,
    pub child_workspace: Option<&'a Path>,
    pub parent_agent_id: &'a AgentSessionId,
    pub parent_workspace: Option<&'a Path>,
    pub parent_transcript_path: Option<&'a Path>,
}

/// Display metadata recovered together with a provider-validated child
/// relation. Durable parent identity remains the hook path's responsibility.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SubagentCorrelation {
    pub agent_name: Option<String>,
    pub role: Option<String>,
    pub task: Option<String>,
    pub prompt: Option<String>,
    pub model: Option<String>,
}

/// One root turn whose durable provider transcript may name spawned children.
/// The hook path supplies the already-observed parent identity and paths; the
/// adapter validates its native transcript before returning any relation.
#[derive(Clone, Copy, Debug)]
pub struct SubagentSpawnInput<'a> {
    pub parent_agent_id: &'a AgentSessionId,
    pub parent_transcript_path: Option<&'a Path>,
    pub parent_workspace: Option<&'a Path>,
}

/// One settled child recovered from validated provider-owned evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnedSubagent {
    pub child_agent_id: AgentSessionId,
    pub agent_name: Option<String>,
    pub role: Option<String>,
    pub prompt: Option<String>,
    pub model: Option<String>,
    pub total_tokens: Option<u64>,
}

/// Durable provider-neutral token and context-window enrichment.
///
/// Providers may report any subset. Reduction carries each reported value
/// forward and derives the gauge from the resolved input-side occupancy and
/// window when no provider supplied an authoritative percentage.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentUsageSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_pct: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
}

impl AgentUsageSummary {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    /// Current input-side context composition, when a provider reports the
    /// fresh-input anchor that makes the split meaningful.
    pub fn input_context_tokens(&self) -> Option<u64> {
        let fresh = self.fresh_input_tokens?;
        Some(
            self.cache_read_input_tokens.unwrap_or(0)
                + self.cache_write_input_tokens.unwrap_or(0)
                + fresh,
        )
    }

    /// Gauge numerator: the input-side call split when known, else the latest
    /// cumulative token reading.
    pub fn context_used_tokens(&self) -> Option<u64> {
        self.input_context_tokens().or(self.total_tokens)
    }

    pub fn resolved_context_window(&self, default_window: Option<u64>) -> Option<u64> {
        self.context_window.or(default_window)
    }

    pub fn resolved_context_pct(&self, default_window: Option<u64>) -> Option<u8> {
        self.context_pct.or_else(|| {
            let used = self.context_used_tokens()?;
            let window = self.resolved_context_window(default_window)?;
            (window > 0).then(|| (used.saturating_mul(100) / window).min(100) as u8)
        })
    }

    /// Carry sparse enrichment forward. An explicit incoming percentage wins;
    /// otherwise derive from the merged numerator/window before retaining the
    /// prior percentage.
    pub fn merge(&self, prior: Option<&Self>, default_window: Option<u64>) -> Self {
        let prior = prior.cloned().unwrap_or_default();
        let mut merged = Self {
            context_pct: None,
            context_window: self.context_window.or(prior.context_window),
            total_tokens: self.total_tokens.or(prior.total_tokens),
            cache_read_input_tokens: self
                .cache_read_input_tokens
                .or(prior.cache_read_input_tokens),
            cache_write_input_tokens: self
                .cache_write_input_tokens
                .or(prior.cache_write_input_tokens),
            fresh_input_tokens: self.fresh_input_tokens.or(prior.fresh_input_tokens),
            output_tokens: self.output_tokens.or(prior.output_tokens),
        };
        merged.context_pct = self
            .context_pct
            .or_else(|| merged.resolved_context_pct(default_window))
            .or(prior.context_pct);
        merged
    }
}

/// One lifecycle observation: the agent-agnostic [`LifecycleSignal`] a native
/// event carries plus the enrichment it reports. A definition's hook capability attaches it
/// to the decoded hook so the Store can record an `agent.lifecycle` event
/// without each adapter touching durable state. The status is *derived* from
/// the signal through [`step`](super::lifecycle::step), never decided by the
/// adapter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentLifecycleObservation {
    /// Agent-supplied session/process identifier (e.g. Claude `session_id`,
    /// Codex root `session_id`, or Codex subagent `agent_id`). The CLI uses
    /// this as the `agent_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<AgentSessionId>,
    /// RimZ-minted durable card name. Launchers pass this through
    /// `RIMZ_AGENT_NAME`; hand-launched agents get a deterministic fallback
    /// during reduction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    #[serde(flatten)]
    pub launch: LaunchParams,
    /// The agent-agnostic lifecycle intent this event carries. The reducer and
    /// the ingestion path fold it onto the rollup through the one
    /// [`step`](super::lifecycle::step) table; the adapter no longer decides a
    /// final [`AgentStatus`](crate::agents::AgentStatus).
    pub signal: LifecycleSignal,
    /// Process identity observed by the hook runner. The sidebar uses this
    /// best-effort liveness marker to suppress stale store overlays when the
    /// process disappears.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_process_start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_owner: Option<RuntimeOwner>,
    /// Optional absolute worktree path observed from the agent payload or
    /// filled by the CLI from the current RimZ workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    /// Optional worktree branch label observed from the payload, surfaced in
    /// the sidebar's worktree grouping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_branch: Option<String>,
    /// Display-only task definition. It never drives routing or decisions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// The user's latest prompt for this session, carried only by the
    /// prompt-bearing event. The reducer persists it (unlike the activity-bound
    /// `task`), so the sidebar can label an unnamed session by its prompt once
    /// the turn ends, until a real session name exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Adapter-reported durable card label, such as a native session title or
    /// a subagent task description. The reducer carries the latest value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The transcript path the agent names for this session, when the adapter
    /// has one. Carry-forward enrichment; readers use it for traceability and
    /// sidecar refresh hints, never as routing truth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    /// Provider-reported session lineage from the Codex rollout head or Claude
    /// `SessionStart` source, carried forward so the rollup projection can
    /// collapse the superseded same-pane `/clear` conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<SessionOrigin>,
    /// The predecessor root condensed into this session, set only from
    /// provider evidence that compaction created a successor session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compacted_from: Option<AgentSessionId>,
    /// Token and context enrichment, flattened to preserve lifecycle event
    /// compatibility.
    #[serde(default, flatten)]
    pub usage: AgentUsageSummary,
    /// Normalized multiplexer pane id the agent process is running inside,
    /// read from the per-pane env var the mux exports (`TMUX_PANE` or
    /// `ZELLIJ_PANE_ID`). Lets the sidebar bind each agent row to its actual
    /// pane when two agents of the same kind share one worktree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<PaneId>,
    /// Full pane identity observed from the live pane frame when the hook can
    /// resolve it. This durable stamp lets runtime liveness key daemon-routed
    /// sessions to the in-pane process rather than the shared daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_stamp: Option<PaneRef>,
    /// The root session id this observation's agent is a *child* of, set on
    /// every hook that carries a distinct child and parent identity. `None` for
    /// root agents. Identity lifetime in the reducer, so a child row links to
    /// its parent row by `(kind, parent_agent_id)` for the whole child's life.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_agent_id: Option<AgentSessionId>,
}

impl AgentLifecycleObservation {
    pub fn new(agent_id: Option<AgentSessionId>, signal: LifecycleSignal) -> Self {
        Self {
            agent_id,
            agent_name: None,
            launch: LaunchParams::default(),
            signal,
            agent_pid: None,
            agent_process_start: None,
            runtime_owner: None,
            worktree_path: None,
            worktree_branch: None,
            task: None,
            prompt: None,
            description: None,
            transcript_path: None,
            origin: None,
            compacted_from: None,
            usage: AgentUsageSummary::default(),
            pane_id: None,
            pane_stamp: None,
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

/// Whether a hook payload carries any canonical context observation field.
/// Presence is evidence even when the provider explicitly reports JSON null.
pub(crate) fn payload_has_context_observation(payload: &Value) -> bool {
    [
        "model",
        "effort",
        "rate_limits",
        "total_cost_usd",
        "context_window",
        "total_tokens",
        "context_pct",
    ]
    .into_iter()
    .any(|key| payload.get(key).is_some())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{AgentUsageSummary, payload_has_context_observation};

    #[test]
    fn context_observation_evidence_is_presence_based() {
        assert!(!payload_has_context_observation(&json!({})));
        assert!(!payload_has_context_observation(
            &json!({"unrelated": null})
        ));
        for key in [
            "model",
            "effort",
            "rate_limits",
            "total_cost_usd",
            "context_window",
            "total_tokens",
            "context_pct",
        ] {
            assert!(
                payload_has_context_observation(&json!({key: null})),
                "{key} presence is context evidence"
            );
        }
    }

    #[test]
    fn usage_merge_carries_sparse_values_and_resolves_percentage_once() {
        let prior = AgentUsageSummary {
            context_pct: Some(91),
            context_window: Some(200_000),
            total_tokens: Some(80_000),
            cache_read_input_tokens: Some(60_000),
            cache_write_input_tokens: Some(5_000),
            fresh_input_tokens: Some(15_000),
            output_tokens: Some(2_000),
        };
        let incoming = AgentUsageSummary {
            total_tokens: Some(100_000),
            fresh_input_tokens: Some(35_000),
            ..AgentUsageSummary::default()
        };

        let merged = incoming.merge(Some(&prior), Some(1_000_000));

        assert_eq!(merged.context_window, Some(200_000));
        assert_eq!(merged.context_used_tokens(), Some(100_000));
        assert_eq!(merged.context_pct, Some(50));
        assert_eq!(merged.total_tokens, Some(100_000));
        assert_eq!(merged.output_tokens, Some(2_000));
    }

    #[test]
    fn usage_merge_honors_explicit_prior_and_default_window_precedence() {
        let explicit = AgentUsageSummary {
            context_pct: Some(7),
            total_tokens: Some(100_000),
            ..AgentUsageSummary::default()
        }
        .merge(None, Some(200_000));
        assert_eq!(explicit.context_pct, Some(7));

        let derived = AgentUsageSummary {
            total_tokens: Some(100_000),
            ..AgentUsageSummary::default()
        }
        .merge(None, Some(200_000));
        assert_eq!(derived.context_pct, Some(50));
        assert_eq!(derived.context_window, None);

        let prior = AgentUsageSummary {
            context_pct: Some(63),
            ..AgentUsageSummary::default()
        };
        assert_eq!(
            AgentUsageSummary::default()
                .merge(Some(&prior), None)
                .context_pct,
            Some(63)
        );
        assert!(AgentUsageSummary::default().is_empty());
        assert!(!prior.is_empty());
    }
}
