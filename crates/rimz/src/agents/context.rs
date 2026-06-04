//! Agent-agnostic, session-scoped context enrichment.
//!
//! [`AgentContext`] is the normalized shape for the rich, high-frequency
//! per-session data an agent publishes out of band — Claude's statusline feed
//! today, Codex's JSON-RPC poll later. It is display-only and redactable: it
//! never drives routing, ranking, or a decision (the no-transcript-correctness
//! rule). Each agent integration produces it from its own transport via
//! [`super::AgentAdapter::observe_context`]; storage
//! ([`crate::ledger::agent_context`]) and the snapshot fold-in are
//! transport-agnostic, so a new agent slots in with only a new producer — no
//! change to this type, the sidecar, or the fold-in.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// Rich per-session enrichment that has no first-class home on
/// [`crate::feed::AgentState`]. Attached whole as `AgentState.context` and
/// dropped whole when the session ends. The record is identity-free — the
/// session it belongs to is the key it is filed under, never a field here, so
/// the two cannot drift. Overlapping scalars (`model`, `effort`) are carried
/// too: the statusline reports them more precisely than the transcript tail,
/// and a future renderer can prefer them for display.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentContext {
    /// Which transport produced this record (`"claude"` today). Stamped from
    /// the ingest `--source` tag, not parsed from the payload.
    pub source: String,
    /// Human-readable session name the user set (`--name` / `/rename`). Absent
    /// until named, so a renderer prefers it over the task descriptor only when
    /// present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_style: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vim_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exceeds_200k_tokens: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<AgentCost>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<AgentTokenUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limits: Option<AgentRateLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<AgentPullRequest>,
    /// The provider account/plan this session authenticates against. Account-
    /// scoped, not session-scoped, so the sidebar's provider dashboard reads it
    /// from the freshest session of each kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<AgentAccount>,
    /// A turn that died on a provider API error with no `Stop` hook to record
    /// it — detected from the transcript tail (Claude's "API Error" abort fires
    /// no hook). Display-only like every field here: the projection reads it to
    /// escalate a falsely-`running` row to the attention `!`, and it never
    /// reaches the event log, a decision, or the rollup. Self-clears once a
    /// newer hook event advances `last_activity` past
    /// [`AgentTurnError::at`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_error: Option<AgentTurnError>,
    /// When the producer observed this record. The snapshot reaper drops a
    /// sidecar past the ghost-session TTL even if a `SessionEnd` was missed.
    pub observed_at: Timestamp,
}

/// Per-subagent enrichment a paneless child cannot publish for itself. Claude's
/// `subagentStatusLine` is `exec`d to render the agent panel's child rows and is
/// handed each task's `type`, `description`, `tokenCount`, and `startTime`; Rimz
/// harvests those into one of these per child so the expanded card paints what the
/// child is doing, what it has spent, and how long it has run. Identity-free like
/// [`AgentContext`] — the child it belongs to is the `(kind, agent_id)` key it is
/// filed under, never a field here. `subagentStatusLine` is Claude-only, so a
/// Codex child simply has no record and the card degrades to its bare type line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SubagentContext {
    /// The agent's type label (`Explore`, `review`, …) from the task's `type`
    /// field. Folds onto `AgentState.task` when the lifecycle events never
    /// provided one — the common case for fork agents that carry no `agent_type`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    /// What the parent asked this child to do (the Task tool's `description`).
    /// Painted after the child's type on the first row; absent before the first
    /// render.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Cumulative tokens the child has spent. Folds onto the child's
    /// `AgentState.total_tokens`, which is otherwise always `None` for a paneless
    /// subagent that never reads a transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_count: Option<u64>,
    /// When the child began, from `startTime`. The card derives elapsed work as
    /// `(running ? now : last_activity) − started_at`. Absent when the upstream
    /// value is missing or unparseable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<Timestamp>,
    /// When the producer observed this record. The snapshot reaper drops a
    /// sidecar past the ghost-session TTL even if a stop was missed.
    pub observed_at: Timestamp,
}

/// One child's enrichment paired with the `agent_id` it belongs to — the
/// adapter's output for a single `subagentStatusLine` task, before the ledger
/// stamps the `kind` it is filed under. A payload renders many rows, so one
/// observation maps to one sidecar write keyed by `(kind, agent_id)`.
#[derive(Clone, Debug, PartialEq)]
pub struct SubagentObservation {
    pub agent_id: String,
    pub context: SubagentContext,
}

/// The provider account/plan a session authenticates against. Account-scoped —
/// every session of one provider shares it — so the sidebar's provider
/// dashboard reads it from the freshest session, never paints it per row.
/// Source-agnostic: Codex fills it from the app-server `account/rateLimits/read`
/// plan type, Claude from `claude auth status`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentAccount {
    /// Raw plan/subscription tier the provider reports (`max`, `team`, `pro`);
    /// the renderer formats it into a brand label (`Claude Max`, `ChatGPT Pro`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// Whether the account is metered by rate-limit windows. `Some(false)` marks
    /// an unmetered (API-key) account, which the dashboard paints as an
    /// "infinite power" bar instead of a draining budget; `None` is unknown, and
    /// the dashboard infers metering from whether rate-limit windows are present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metered: Option<bool>,
}

/// Cumulative spend for the session, as the agent reports it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentCost {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_api_duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_lines_added: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_lines_removed: Option<u64>,
}

/// Context-window token accounting. `used_percentage` is the authoritative
/// gauge value the statusline reports directly (0..=100). The statusline's
/// `total_input_tokens` / `total_output_tokens` are not captured: since Claude
/// Code v2.1.132 they read "tokens in the current context window", which
/// `current_usage` already carries component by component.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentTokenUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_percentage: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_percentage: Option<u8>,
    /// The most-recent API response's token composition. Its input side
    /// (`input + cache_creation + cache_read`) is exactly what `used_percentage`
    /// measures, so a renderer can color the context bar by where the window
    /// went. Absent before the first API call and right after `/compact`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_usage: Option<AgentCurrentUsage>,
}

/// The token breakdown of the most-recent API response. Cache reads dominate a
/// long session; cache writes spike on fresh file reads; `input_tokens` is the
/// live, uncached turn.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentCurrentUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
}

/// The rate-limit windows the agent surfaces, ordered short→long by duration.
/// Each window carries its own length, so a renderer derives its label (`5h`,
/// `7d`, …) and its reset-to-max roll-forward from the window itself — no
/// provider-shaped buckets. Both Claude and Codex report a 5-hour and a 7-day
/// window; carrying the duration means a provider reporting a different count or
/// length (a window kind changing, or a transient server bug) just works.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentRateLimits {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<RateLimitWindow>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RateLimitWindow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_percentage: Option<u8>,
    /// Reset instant, parsed to a typed timestamp on ingest so renderers format
    /// a countdown rather than re-parsing a raw value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<Timestamp>,
    /// The window's length in minutes — its identity across sessions (the
    /// stable-window pick groups readings by it), the source of its bar label,
    /// and the roll-forward length once it refills while idle. Providers stamp
    /// it: Claude from the window kind it names, Codex from `windowDurationMins`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_mins: Option<u32>,
}

impl RateLimitWindow {
    /// Whether this window's budget is spent — the provider reports the cap as
    /// `used_percentage == 100` once it starts refusing requests, so any agent
    /// on the account can only wait for [`resets_at`](Self::resets_at). The
    /// sidebar projects every resting agent of the kind to
    /// [`AgentStatus::RateLimited`](crate::feed::AgentStatus::RateLimited) when
    /// this is true.
    pub fn is_spent(&self) -> bool {
        self.used_percentage.is_some_and(|pct| pct >= 100)
    }
}

impl AgentRateLimits {
    /// Whether any reported window is [`spent`](RateLimitWindow::is_spent) — the
    /// account-level "nothing to do but wait" verdict the sidebar projects onto
    /// every resting agent of the kind.
    pub fn any_spent(&self) -> bool {
        self.windows.iter().any(RateLimitWindow::is_spent)
    }
}

/// A turn that ended on a provider API error without a `Stop` hook — the
/// transcript is the only record of the death (an `assistant` entry flagged
/// `isApiErrorMessage` and nothing newer), so the detector that reads the tail
/// emits one of these. The projection compares [`at`](Self::at) against the
/// row's `last_activity`: newer means the row's `running` is a corpse and the
/// sidebar escalates it; any later hook event self-clears it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentTurnError {
    /// The transcript wall-clock timestamp of the dead turn's error entry — the
    /// guard the projection compares against `last_activity`. A clock skew
    /// fails safe: a suppressed real death still hits the stall window, and a
    /// stale error can never escalate a row whose activity has moved past it.
    pub at: Timestamp,
    /// The upstream error text ("API Error: Overloaded"), length-capped by the
    /// detector. Provider-generated, not user content, but content-ish all the
    /// same — gate it under a payload-mode content loader when one lands, never
    /// the timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// The pull request the agent associates with the session, when it reports one.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentPullRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_state: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(used: Option<u8>) -> RateLimitWindow {
        RateLimitWindow {
            used_percentage: used,
            resets_at: None,
            duration_mins: Some(300),
        }
    }

    #[test]
    fn window_is_spent_only_at_the_cap() {
        assert!(window(Some(100)).is_spent());
        assert!(!window(Some(99)).is_spent());
        assert!(!window(Some(0)).is_spent());
        // An unreported window is not a spent one.
        assert!(!window(None).is_spent());
    }

    #[test]
    fn any_spent_reads_any_window() {
        let spent = window(Some(100));
        let fresh = window(Some(10));
        assert!(
            AgentRateLimits {
                windows: vec![spent.clone(), fresh.clone()]
            }
            .any_spent()
        );
        assert!(
            AgentRateLimits {
                windows: vec![fresh.clone(), spent]
            }
            .any_spent()
        );
        assert!(
            !AgentRateLimits {
                windows: vec![fresh.clone(), fresh]
            }
            .any_spent()
        );
        assert!(!AgentRateLimits::default().any_spent());
    }
}
