//! Agent-agnostic, session-scoped context enrichment.
//!
//! [`AgentContext`] is the normalized shape for the rich, high-frequency
//! per-session data an agent publishes out of band — Claude's statusline,
//! Codex's rollout tail plus app-server metadata, and future provider surfaces.
//! It is sidecar enrichment, not durable store truth. Most fields are
//! render-only; turn-error, turn-settle, and native-attention markers also feed
//! the shared status projection so read paths agree about hookless state. Each agent
//! integration produces it from its own transport or local refresh via
//! [`super::AgentAdapter`]; lifecycle hooks also keep the current turn's
//! confirmed message openers here so an agent-authored send can retain exact
//! reply causality. Storage ([`crate::store::agent_context`]) and the snapshot
//! fold-in stay transport-agnostic; provider-specific wire fields normalize
//! into these shared slots before either layer sees them.

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::ids::MessageId;

/// Cache identity for account facts exposed by an agent adapter.
///
/// Most agents authenticate one provider per agent kind. Multi-provider agents
/// use `SubProvider` so a provider or region switch cannot reuse another
/// account's usage reading.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderAccountScope {
    #[default]
    KindWide,
    SubProvider {
        provider: String,
        variant: String,
    },
}

impl ProviderAccountScope {
    pub fn sub_provider(provider: impl Into<String>, variant: impl Into<String>) -> Self {
        Self::SubProvider {
            provider: provider.into(),
            variant: variant.into(),
        }
    }

    pub fn is_kind_wide(&self) -> bool {
        matches!(self, Self::KindWide)
    }
}

/// Rich per-session enrichment that has no first-class home on
/// [`crate::agents::AgentState`]. Attached whole as `AgentState.context` and
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
    /// Messages whose confirmed delivery opened the current turn. Lifecycle
    /// hooks replace this on every turn start; enqueue reads it to preserve
    /// exact inter-agent reply causality.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub turn_opened_by: Vec<MessageId>,
    /// A turn that died on a provider API error, detected from a provider hook
    /// or transcript/rollout tail. Status-projection marker: the projection
    /// reads it to refine a falsely-`running` row, or a same-turn `failed` row,
    /// into `paused`/`failed` with the provider's reason. The marker itself
    /// never reaches the event log. It self-clears once a newer hook event
    /// advances `last_activity` past [`AgentTurnError::at`], or once the
    /// rollup's `turn_started_at` proves the marker belongs to a prior turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_error: Option<AgentTurnError>,
    /// A turn that completed cleanly, detected from the rollout tail when the
    /// turn fired no `Stop` hook to record its end — Codex's `/review` runs in
    /// review mode and closes on a `task_complete` without a `Stop`.
    /// Status-projection marker like [`turn_error`](Self::turn_error) and
    /// self-clearing the same way: the projection settles a falsely-`running`
    /// row to `success` while the marker postdates `last_activity`, and a newer
    /// prompt advancing `last_activity` past it drops the row back to its
    /// lifecycle status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_complete: Option<Timestamp>,
    /// A completed Codex planning turn resting on its native plan selector,
    /// detected from the rollout tail when the `Stop` hook was missed.
    /// Status-projection marker like [`turn_complete`](Self::turn_complete):
    /// the projection settles a falsely-`running` row to `waiting` while the
    /// marker postdates `last_activity`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_proposed: Option<Timestamp>,
    /// A provider status channel currently reports a native tool-confirmation
    /// dialog. The marker time must postdate the latest lifecycle activity to
    /// project a waiting card; a subsequent tool/turn hook self-clears a stale
    /// marker even if the provider misses its `false` statusline refresh.
    /// Display-only: it creates no durable ask and the provider pane remains
    /// the answer surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_permission_wait: Option<Timestamp>,
    /// A turn that was interrupted with no `Stop` hook, detected from the
    /// rollout tail — Codex writes `turn_aborted` for `/clear` mid-turn and
    /// Esc. Status-projection marker like
    /// [`turn_complete`](Self::turn_complete) and self-clearing the same way:
    /// the projection settles a falsely-`running` row to `idle` while the
    /// marker postdates `last_activity`, and a newer prompt advancing
    /// `last_activity` past it drops the row back to its lifecycle status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_interrupted: Option<Timestamp>,
    /// When the producer observed this record. Snapshot liveness comes from
    /// the rollup row; a sidecar without a surviving row is not joined.
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
    /// When the producer observed this record. Snapshot liveness comes from
    /// the rollup row; a sidecar without a surviving row is not joined.
    pub observed_at: Timestamp,
}

/// One child's enrichment paired with the `agent_id` it belongs to — the
/// adapter's output for a single `subagentStatusLine` task, before the store
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
    /// Provider and variant identity used by account-usage caches. Kind-wide is
    /// omitted so snapshots written before this field remain byte-compatible.
    #[serde(default, skip_serializing_if = "ProviderAccountScope::is_kind_wide")]
    pub scope: ProviderAccountScope,
    /// Raw plan/subscription tier the provider reports (`max`, `team`, `pro`);
    /// the renderer formats it into a brand label (`Claude Max`, `ChatGPT Pro`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// Provider-native account identifier when an integration exposes one.
    /// Display remains provider-specific; this stable value primarily keys
    /// account switches and machine-readable diagnostics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
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
    /// Single-provider probes leave it `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_provider: Option<String>,
    /// Credential-file mtime in Unix milliseconds, when the probe reads a
    /// file. The dashboard uses it as a login-recency signal; subprocess-only
    /// probes leave it unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials_updated_at_ms: Option<u64>,
}

/// Provenance for a session-cost total. The basis independently controls
/// approximate rendering and eligibility for live budget decisions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostBasis {
    /// A native provider total.
    #[default]
    ProviderReported,
    /// Provider-owned counters priced through Rimz's pinned price book.
    LocallyPriced,
    /// A lower-confidence projection retained for display only.
    DisplayEstimate,
}

impl CostBasis {
    pub const fn is_approximate(self) -> bool {
        !matches!(self, Self::ProviderReported)
    }

    pub const fn counts_toward_live_budget(self) -> bool {
        !matches!(self, Self::DisplayEstimate)
    }

    fn is_provider_reported(&self) -> bool {
        matches!(self, Self::ProviderReported)
    }
}

/// Cumulative spend for the session, carrying the basis needed by display and
/// live-budget policy.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentCost {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "CostBasis::is_provider_reported")]
    pub basis: CostBasis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_api_duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_lines_added: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_lines_removed: Option<u64>,
}

/// Token accounting from two deliberately separate scopes. `used_percentage`
/// and `current_usage` describe the current context window; `session_usage`
/// carries cumulative lifetime counters when a provider exposes those without
/// exposing context occupancy. Only the current-window fields drive gauges and
/// compaction decisions.
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
    /// Cumulative session-lifetime counters. These never establish context
    /// occupancy and stay out of [`Self::used_tokens`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_usage: Option<AgentSessionUsage>,
}

impl AgentTokenUsage {
    /// Tokens currently occupying the context window — the latest API
    /// response's `input + cache_creation + cache_read`, exactly the numerator
    /// [`AgentTokenUsage::used_percentage`] scales (output joins the window only
    /// next turn). `None` before the first call or right after `/compact`
    /// clears the breakdown.
    pub fn used_tokens(&self) -> Option<u64> {
        let usage = self.current_usage.as_ref()?;
        Some(
            usage.input_tokens.unwrap_or(0)
                + usage.cache_creation_input_tokens.unwrap_or(0)
                + usage.cache_read_input_tokens.unwrap_or(0),
        )
    }
}

/// Cumulative token counters for one provider session. Cache creation is
/// billable input and thinking is generated output; cache reads remain a
/// separate figure and stay outside the headline total, matching the shared
/// `◇ ↘ ↗ ◌` token grammar.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSessionUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_tokens: Option<u64>,
}

impl AgentSessionUsage {
    pub fn displayed_input_tokens(&self) -> u64 {
        self.input_tokens
            .unwrap_or(0)
            .saturating_add(self.cache_creation_input_tokens.unwrap_or(0))
    }

    pub fn displayed_output_tokens(&self) -> u64 {
        self.output_tokens
            .unwrap_or(0)
            .saturating_add(self.thinking_tokens.unwrap_or(0))
    }

    pub fn displayed_total_tokens(&self) -> u64 {
        self.displayed_input_tokens()
            .saturating_add(self.displayed_output_tokens())
    }

    pub fn cache_read_tokens(&self) -> u64 {
        self.cache_read_input_tokens.unwrap_or(0)
    }

    pub fn is_zero(&self) -> bool {
        self.displayed_total_tokens() == 0 && self.cache_read_tokens() == 0
    }
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

impl AgentCurrentUsage {
    /// Whether the breakdown carries no token count at all: every field is
    /// either absent or explicitly zero.
    pub fn is_zero(&self) -> bool {
        [
            self.input_tokens,
            self.output_tokens,
            self.cache_creation_input_tokens,
            self.cache_read_input_tokens,
        ]
        .into_iter()
        .all(|count| count.unwrap_or(0) == 0)
    }
}

/// The rate-limit windows the agent surfaces. Temporal windows carry their own
/// length, so a renderer derives the label (`5h`, `7d`, …) and reset-to-max
/// roll-forward without provider-shaped buckets. Named quotas carry a stable
/// provider scope and compact label instead; their missing duration keeps them
/// out of temporal calculations.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentRateLimits {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<RateLimitWindow>,
}

impl AgentRateLimits {
    /// Stamp a capture timestamp onto every window that lacks one. Ingest
    /// builders set the `source`; the boundary that knows when the reading was
    /// taken — `into_context` for a live session, the merge for an out-of-band
    /// refresh — fills `observed_at` so the fusion can rank freshness.
    pub fn stamped_at(mut self, observed_at: Timestamp) -> Self {
        for window in &mut self.windows {
            window.observed_at.get_or_insert(observed_at);
        }
        self
    }

    /// Whether this reading's content predates its shortest temporal window's
    /// reset, so the whole payload is stale even where a longer window remains
    /// current. A payload with no dated temporal window falls back to its
    /// earliest dated named quota. An idle session re-emits a days-old payload
    /// with a fresh capture stamp, so `observed_at` cannot judge a best-effort
    /// reading's freshness. A reading with no dated window remains fresh as a
    /// last-resort backstop.
    pub fn content_stale_at(&self, now: Timestamp) -> bool {
        let duration_reset = self
            .windows
            .iter()
            .filter_map(|window| Some((window.duration_mins?, window.resets_at?)))
            .min_by_key(|(mins, _)| *mins)
            .map(|(_, resets_at)| resets_at);
        duration_reset
            .or_else(|| {
                self.windows
                    .iter()
                    .filter(|window| window.scope.is_some())
                    .filter_map(|window| window.resets_at)
                    .min()
            })
            .is_some_and(|resets_at| resets_at <= now)
    }
}

/// Provider-defined identity and compact presentation label for a named quota.
/// The stable `id` participates in fusion and cache identity; `label` is clipped
/// by the renderer to its fixed three-cell window-label slot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimitWindowScope {
    pub id: String,
    pub label: String,
}

/// Stable provider-agnostic identity for one rate-limit lane. Existing temporal
/// windows retain duration identity; named provider quotas use their scope id.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RateLimitWindowKey {
    Duration(Option<u32>),
    Scope(String),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RateLimitWindow {
    /// Optional provider-defined identity for quotas that are not temporal
    /// windows. A scope id is stable across readings; its label is display-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<RateLimitWindowScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_percentage: Option<u8>,
    /// Reset instant, parsed to a typed timestamp on ingest so renderers format
    /// a countdown rather than re-parsing a raw value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<Timestamp>,
    /// The temporal window's length in minutes — its identity when `scope` is
    /// absent, the source of its bar label, and the roll-forward length once it
    /// refills while idle. Named quotas leave it absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_mins: Option<u32>,
    /// When this reading was captured. Provenance for fusion, not display. For a
    /// [`WindowSource::BestEffort`] statusline this is *capture* time, not
    /// content time — an idle session re-emits a days-old payload with a fresh
    /// stamp, so content freshness is judged by the shortest temporal reset or
    /// the named-quota fallback, and this only breaks ties. For
    /// [`WindowSource::Authoritative`] it is content time (the API was queried
    /// then), so it ranks recency directly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<Timestamp>,
    /// Where the reading came from, deciding how far the fusion trusts a drop.
    #[serde(default, skip_serializing_if = "WindowSource::is_best_effort")]
    pub source: WindowSource,
    /// An authoritative full reading omitted this previously reported duration,
    /// so the provider is not currently enforcing the limit. The next reading
    /// that reports the duration replaces this marker.
    #[serde(default, skip_serializing_if = "is_false")]
    pub lifted: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Where a [`RateLimitWindow`] reading came from, deciding how far the fusion
/// trusts it. Usage only climbs within a live window, so a reading that lowers
/// the bar is a refill that must be earned: an official-API query moves the bar
/// down at once, while a statusline-derived reading is held until confirmed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowSource {
    /// Queried from the provider's official usage API (Claude OAuth usage,
    /// Codex app-server `account/rateLimits/read`). Truth at its `observed_at`:
    /// it overrides older readings and may lower the bar immediately.
    Authoritative,
    /// Derived from the agent's statusline payload. Current while the agent
    /// works, but an idle session re-emits a stale payload, so a downward move
    /// is trusted only once confirmed across the sliding window.
    #[default]
    BestEffort,
}

impl WindowSource {
    /// Whether this is the default ([`WindowSource::BestEffort`]) — lets serde
    /// omit the common case and a cold cache deserialize to the safe reading.
    pub fn is_best_effort(&self) -> bool {
        matches!(self, WindowSource::BestEffort)
    }

    /// Whether this reading came from an official-API query and may lower the
    /// bar without waiting for confirmation.
    pub fn is_authoritative(&self) -> bool {
        matches!(self, WindowSource::Authoritative)
    }
}

/// Grace allowed when judging a not-started window: a reset still within this
/// much of a full window-length out counts as "clock not begun", absorbing the
/// small skew between the first token and the provider stamping the reset.
const NOT_STARTED_GRACE: SignedDuration = SignedDuration::from_secs(120);

impl RateLimitWindow {
    pub(crate) fn key(&self) -> RateLimitWindowKey {
        self.scope.as_ref().map_or_else(
            || RateLimitWindowKey::Duration(self.duration_mins),
            |scope| RateLimitWindowKey::Scope(scope.id.clone()),
        )
    }

    /// Project this cached reading to `now`: before its reset the reading stands
    /// unchanged; once the reset passes, a dated sliding window refills and its
    /// next reset rolls forward by the window length.
    pub fn projected_at(self, now: Timestamp) -> Self {
        match (self.resets_at, self.duration_mins) {
            (Some(resets_at), Some(mins)) if resets_at <= now => Self {
                scope: self.scope,
                used_percentage: Some(0),
                resets_at: now
                    .checked_add(SignedDuration::from_secs(i64::from(mins) * 60))
                    .ok(),
                duration_mins: Some(mins),
                observed_at: self.observed_at,
                source: self.source,
                lifted: self.lifted,
            },
            _ => self,
        }
    }

    /// Whether this window's budget is spent — the provider reports the cap as
    /// `used_percentage == 100` once the window is exhausted. Display code
    /// combines this with a per-agent pause certificate or a stalled running
    /// turn; the spent window alone does not change an agent's row.
    pub fn is_spent(&self) -> bool {
        self.used_percentage.is_some_and(|pct| pct >= 100)
    }

    /// Whether this window's sliding clock has not begun. These budgets start on
    /// the first billable token, so until then the provider keeps `resets_at`
    /// slid ~a full window-length ahead. Detection keys on that reset distance,
    /// not a 0% reading — a fresh 5h window still reports ~1% used, never 0 — so
    /// any usage above the ~1% floor short-circuits to "started" regardless of
    /// the reset (this also covers a spent window at 100%). An absent reset or
    /// duration can't be judged this way, so it reads as started: a known
    /// reading whose countdown is a real one. Drives the dashboard's
    /// no-countdown "ready to start" treatment and the window-priming ping guard.
    pub fn not_started(&self, now: Timestamp) -> bool {
        if self.used_percentage > Some(1) {
            return false;
        }
        let (Some(reset), Some(mins)) = (self.resets_at, self.duration_mins) else {
            return false;
        };
        let full = SignedDuration::from_secs(i64::from(mins) * 60);
        reset.duration_since(now) >= full - NOT_STARTED_GRACE
    }
}

/// What kind of provider API error ended a turn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnErrorClass {
    /// The turn stopped on a spent rate-limit window. It projects and
    /// auto-continues from the fused account budget: a recovering subscription
    /// window parks the row until its reset deadline, independent of one
    /// paused session's frozen context reading.
    PausedRateLimit,
    /// The turn stopped on a paid extra-credit/spend cap. It remains resumable
    /// when the fused account budget still has a recovering subscription mana
    /// bar; disabled or exhausted extra credits do not make that park terminal.
    PausedSpendLimit,
    /// The provider was overloaded or returned a transient server error. There
    /// is no local reset window to wait for, so the row stays paused until a
    /// newer hook event self-clears it.
    PausedOverloaded,
    /// The turn ended without machine-readable cause. It renders like a failed
    /// turn and never arms automatic resume until another evidence channel
    /// proves a resumable class.
    Unknown,
    /// Any other provider API error: actionable failure with the upstream text
    /// on the card.
    #[default]
    Failed,
}

impl TurnErrorClass {
    /// Whether provider capacity paused the turn instead of failing it.
    pub(crate) fn pauses_turn(self) -> bool {
        matches!(
            self,
            Self::PausedRateLimit | Self::PausedSpendLimit | Self::PausedOverloaded
        )
    }

    /// Whether the pause follows a resumable rate or spend window.
    pub(crate) fn is_limit(self) -> bool {
        matches!(self, Self::PausedRateLimit | Self::PausedSpendLimit)
    }

    /// Classify a capped upstream provider-error label into the display and
    /// auto-resume bucket shared by every adapter.
    pub(crate) fn classify_label(label: Option<&str>) -> Self {
        let Some(label) = label else {
            return Self::Failed;
        };
        let lower = label.to_ascii_lowercase();
        if lower.contains("spend limit") {
            Self::PausedSpendLimit
        } else if lower.contains("usage limit")
            || lower.contains("session limit")
            || lower.contains("rate limit")
            || lower.contains("quota")
            || lower.contains("too many requests")
        {
            Self::PausedRateLimit
        } else if is_transient_server_error(&lower) {
            Self::PausedOverloaded
        } else {
            Self::Failed
        }
    }
}

fn is_transient_server_error(lower: &str) -> bool {
    lower.contains("overloaded")
        || lower.contains("at capacity")
        || lower.contains("server is busy")
        || lower.contains("internal server error")
        || lower.contains("server error")
        || lower.contains("service unavailable")
        || lower.contains("bad gateway")
        || lower.contains("gateway timeout")
        || lower.contains("stalled")
        || lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("connection error")
        || lower.contains("network error")
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

    #[test]
    fn cost_basis_separates_rendering_from_live_budget_policy() {
        for (basis, approximate, counts, wire) in [
            (
                CostBasis::ProviderReported,
                false,
                true,
                "provider_reported",
            ),
            (CostBasis::LocallyPriced, true, true, "locally_priced"),
            (CostBasis::DisplayEstimate, true, false, "display_estimate"),
        ] {
            assert_eq!(basis.is_approximate(), approximate);
            assert_eq!(basis.counts_toward_live_budget(), counts);
            assert_eq!(serde_json::to_value(basis).unwrap(), wire);
        }
        let legacy_without_basis: AgentCost =
            serde_json::from_value(serde_json::json!({"total_cost_usd": 1.0})).unwrap();
        assert_eq!(legacy_without_basis.basis, CostBasis::ProviderReported);
    }

    fn window(used: Option<u8>) -> RateLimitWindow {
        RateLimitWindow {
            used_percentage: used,
            resets_at: None,
            duration_mins: Some(300),
            ..Default::default()
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
    fn window_not_started_keys_on_reset_distance_above_the_floor() {
        let now = Timestamp::from_second(2_000_000_000).unwrap();
        let full = SignedDuration::from_secs(300 * 60);
        let started = |used, reset| RateLimitWindow {
            used_percentage: Some(used),
            resets_at: Some(reset),
            duration_mins: Some(300),
            ..Default::default()
        };

        // Reset slid a full window out at the ~1% floor — the clock has not begun.
        assert!(started(1, now.checked_add(full).unwrap()).not_started(now));
        // Any usage above the floor is a clearly-started window, reset notwithstanding.
        assert!(!started(2, now.checked_add(full).unwrap()).not_started(now));
        // A reset that has ticked well below full is a real countdown — started.
        assert!(
            !started(1, now.checked_add(SignedDuration::from_secs(3600)).unwrap()).not_started(now)
        );
        // An absent reset or duration can't be judged, so it reads as started.
        assert!(!window(Some(0)).not_started(now));
    }

    #[test]
    fn scoped_window_identity_projection_and_wire_round_trip() {
        let now = Timestamp::from_second(2_000_000_000).unwrap();
        let scope = RateLimitWindowScope {
            id: "premium_interactions".to_owned(),
            label: "prm".to_owned(),
        };
        let window = RateLimitWindow {
            scope: Some(scope.clone()),
            used_percentage: Some(40),
            resets_at: Some(now - SignedDuration::from_secs(1)),
            duration_mins: None,
            ..Default::default()
        };
        assert_eq!(
            window.key(),
            RateLimitWindowKey::Scope("premium_interactions".to_owned())
        );
        assert_eq!(window.clone().projected_at(now), window);

        let encoded = serde_json::to_value(&window).unwrap();
        assert_eq!(encoded["scope"]["id"], "premium_interactions");
        assert_eq!(encoded["scope"]["label"], "prm");
        assert_eq!(
            serde_json::from_value::<RateLimitWindow>(encoded).unwrap(),
            window
        );
        assert_eq!(
            serde_json::from_value::<RateLimitWindow>(serde_json::json!({
                "used_percentage": 20,
                "duration_mins": 300
            }))
            .unwrap()
            .scope,
            None,
            "legacy duration-only windows remain wire-compatible"
        );
    }

    #[test]
    fn scoped_reset_is_content_staleness_fallback_only_without_a_duration_clock() {
        let now = Timestamp::from_second(2_000_000_000).unwrap();
        let scoped = |reset| RateLimitWindow {
            scope: Some(RateLimitWindowScope {
                id: "chat".to_owned(),
                label: "cht".to_owned(),
            }),
            used_percentage: Some(20),
            resets_at: Some(reset),
            ..Default::default()
        };
        assert!(
            AgentRateLimits {
                windows: vec![scoped(now - SignedDuration::from_secs(1))]
            }
            .content_stale_at(now)
        );

        let limits = AgentRateLimits {
            windows: vec![
                scoped(now - SignedDuration::from_secs(1)),
                RateLimitWindow {
                    duration_mins: Some(300),
                    resets_at: Some(now + SignedDuration::from_secs(60)),
                    ..Default::default()
                },
            ],
        };
        assert!(
            !limits.content_stale_at(now),
            "a real duration clock remains the primary freshness signal"
        );
    }

    #[test]
    fn current_usage_token_accounting() {
        // used_tokens sums the current message's window composition, excluding
        // output (it joins the window only next turn), and is None before the
        // first API call.
        assert_eq!(AgentTokenUsage::default().used_tokens(), None);
        let tokens = AgentTokenUsage {
            context_window_size: Some(1_000_000),
            used_percentage: Some(30),
            current_usage: Some(AgentCurrentUsage {
                input_tokens: Some(5_000),
                output_tokens: Some(9_999),
                cache_creation_input_tokens: Some(100_000),
                cache_read_input_tokens: Some(200_000),
            }),
            ..AgentTokenUsage::default()
        };
        assert_eq!(tokens.used_tokens(), Some(305_000));

        // is_zero holds when every count is absent or explicitly zero, and fails
        // the moment one is non-zero.
        assert!(AgentCurrentUsage::default().is_zero());
        assert!(
            AgentCurrentUsage {
                input_tokens: Some(0),
                ..AgentCurrentUsage::default()
            }
            .is_zero()
        );
        assert!(
            !AgentCurrentUsage {
                cache_read_input_tokens: Some(1),
                ..AgentCurrentUsage::default()
            }
            .is_zero()
        );
    }

    #[test]
    fn turn_error_class_round_trips_and_defaults_to_failed() {
        for (class, wire, label) in [
            (
                TurnErrorClass::PausedRateLimit,
                "paused_rate_limit",
                "You've hit your usage limit",
            ),
            (
                TurnErrorClass::PausedSpendLimit,
                "paused_spend_limit",
                "You've hit your monthly spend limit",
            ),
            (
                TurnErrorClass::PausedOverloaded,
                "paused_overloaded",
                "API Error: Overloaded",
            ),
            (
                TurnErrorClass::Unknown,
                "unknown",
                "turn ended with no final message",
            ),
            (TurnErrorClass::Failed, "failed", "API Error: Bad Request"),
        ] {
            let error = AgentTurnError {
                class,
                at: Timestamp::from_second(1_700_000_000).unwrap(),
                label: Some(label.to_owned()),
            };
            let value = serde_json::to_value(&error).unwrap();
            assert_eq!(value["class"], wire);
            let back: AgentTurnError = serde_json::from_value(value).unwrap();
            assert_eq!(back, error);
        }

        let legacy: AgentTurnError = serde_json::from_value(serde_json::json!({
            "at": "2023-11-14T22:13:20Z",
            "label": "API Error: Server Error"
        }))
        .unwrap();
        assert_eq!(legacy.class, TurnErrorClass::Failed);
    }

    #[test]
    fn turn_error_label_classifier_maps_provider_labels() {
        for (label, class) in [
            (
                "You've hit your monthly spend limit.",
                TurnErrorClass::PausedSpendLimit,
            ),
            (
                "You've hit your session limit · resets 10:50am (UTC)",
                TurnErrorClass::PausedRateLimit,
            ),
            (
                "API Error: rate limit exceeded",
                TurnErrorClass::PausedRateLimit,
            ),
            ("API Error: Server Error", TurnErrorClass::PausedOverloaded),
            (
                "API Error: Response stalled mid-stream. The response above may be incomplete.",
                TurnErrorClass::PausedOverloaded,
            ),
            (
                "API Error: request timed out",
                TurnErrorClass::PausedOverloaded,
            ),
            (
                "API Error: connection error",
                TurnErrorClass::PausedOverloaded,
            ),
            (
                "Selected model is at capacity. Please try a different model.",
                TurnErrorClass::PausedOverloaded,
            ),
            ("API Error: Bad Request", TurnErrorClass::Failed),
        ] {
            assert_eq!(
                TurnErrorClass::classify_label(Some(label)),
                class,
                "{label}"
            );
        }
        assert_eq!(TurnErrorClass::classify_label(None), TurnErrorClass::Failed);
    }
}
