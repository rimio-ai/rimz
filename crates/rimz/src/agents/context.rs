//! Agent-agnostic, session-scoped context enrichment.
//!
//! [`AgentContext`] is the normalized shape for the rich, high-frequency
//! per-session data an agent publishes out of band — Claude's statusline feed,
//! Codex's rollout tail plus app-server metadata, and future provider surfaces.
//! It is display-only and redactable: it never drives routing, ranking, or a
//! decision (the no-transcript-correctness rule). Each agent integration
//! produces it from its own transport or local refresh via [`super::AgentAdapter`];
//! storage ([`crate::ledger::agent_context`]) and the snapshot fold-in are
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
    /// Which agent kind produced this record. Stamped from the ingest `--source`
    /// tag or merge path, not parsed from provider payload content.
    pub source: String,
    /// Human-readable session or provider thread name. Claude fills this from
    /// the user-set session name (`--name` / `/rename`); Codex fills it from
    /// app-server thread `name`. Absent until named, so a renderer prefers it
    /// over the task descriptor only when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    /// Short provider-generated thread summary. Codex fills this from
    /// app-server `thread/read` / `thread/list` `preview`; renderers prefer it
    /// for the activity description when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_preview: Option<String>,
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
    /// A turn that died on a provider API error, detected from a provider hook
    /// or transcript/rollout tail. Display-only like every field here: the
    /// projection reads it to refine a falsely-`running` row, or a same-turn
    /// `failed` row, into `paused`/`failed` with the provider's reason. The
    /// marker itself never reaches the event log or a decision. It self-clears
    /// once a newer hook event advances `last_activity` past
    /// [`AgentTurnError::at`], or once the rollup's `turn_started_at` proves the
    /// marker belongs to a prior turn.
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
    /// The agent binary's version, when the out-of-band probe reads one. The
    /// panel header's fallback for a provider whose sessions carry no
    /// `agent_version` in their rich context, and the version input for
    /// display-only provider capability badges; a live session's reading still
    /// wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The raw subscription-provider id the account runs on, for a
    /// multi-provider client (Pi's `auth.json` keys: `anthropic`, `openai`).
    /// The dashboard maps it to the sibling agent kind metering that account
    /// ([`kind_for_sub_provider`](super::kind_for_sub_provider)) and borrows
    /// its budget windows. Single-provider probes leave it `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_provider: Option<String>,
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
    /// `used_percentage == 100` once the window is exhausted. Display code
    /// combines this with a per-agent pause certificate or a stalled running
    /// turn; the spent window alone does not change an agent's row.
    pub fn is_spent(&self) -> bool {
        self.used_percentage.is_some_and(|pct| pct >= 100)
    }
}

/// What kind of provider API error ended a turn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnErrorClass {
    /// The turn stopped on a spent rate-limit window. It projects to paused
    /// while any known spent window remains unreset, then to failed once all
    /// known spent windows have reset if no newer hook event self-clears it.
    PausedRateLimit,
    /// The provider was overloaded. There is no local reset window to wait for,
    /// so the row stays paused until a newer hook event self-clears it.
    PausedOverloaded,
    /// Any other provider API error: actionable failure with the upstream text
    /// on the card.
    #[default]
    Failed,
}

/// A turn that ended on a provider API error. Provider detectors read their
/// hook payload or local transcript/rollout tail and normalize the death
/// certificate into this marker. The projection compares [`at`](Self::at)
/// against the row's `last_activity` for live `running` rows and against
/// `turn_started_at` for terminal `failed` rows, so stale markers from prior
/// turns do not reclassify fresh work.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentTurnError {
    /// The display class for the dead turn. Older sidecars omitted this field;
    /// deserialize them as [`TurnErrorClass::Failed`] so stale markers remain
    /// conservative.
    #[serde(default)]
    pub class: TurnErrorClass,
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
    fn turn_error_class_round_trips_and_defaults_to_failed() {
        let error = AgentTurnError {
            class: TurnErrorClass::PausedRateLimit,
            at: Timestamp::from_second(1_700_000_000).unwrap(),
            label: Some("You've hit your usage limit".to_owned()),
        };
        let value = serde_json::to_value(&error).unwrap();
        assert_eq!(value["class"], "paused_rate_limit");
        let back: AgentTurnError = serde_json::from_value(value).unwrap();
        assert_eq!(back, error);

        let legacy: AgentTurnError = serde_json::from_value(serde_json::json!({
            "at": "2023-11-14T22:13:20Z",
            "label": "API Error: Server Error"
        }))
        .unwrap();
        assert_eq!(legacy.class, TurnErrorClass::Failed);
    }
}
