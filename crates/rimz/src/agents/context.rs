//! Agent-agnostic, session-scoped context enrichment.
//!
//! [`AgentContext`] is the normalized shape for the rich, high-frequency
//! per-session data an agent publishes out of band — Claude's statusline,
//! Codex's rollout tail plus app-server metadata, and future provider surfaces.
//! It is sidecar enrichment, not durable store truth. Most fields are
//! render-only; turn-error, turn-settle, and native-attention markers also feed
//! the shared status projection so read paths agree about hookless state. Each agent
//! integration produces it from its own transport or local refresh via
//! [`super::AgentDefinition`]; lifecycle hooks also keep the current turn's
//! confirmed message openers here so an agent-authored send can retain exact
//! reply causality. Storage ([`crate::store::agent_context`]) and the snapshot
//! fold-in stay transport-agnostic; provider-specific wire fields normalize
//! into these shared slots before either layer sees them.

use std::path::Path;

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::ids::{AgentSessionId, MessageId};

use super::LocalContextRefresh;
use super::definition::AgentSpec;

pub mod record;

/// One rich-context reading paired with the provider session that owns it.
#[derive(Clone, Debug, PartialEq)]
pub struct ContextObservation {
    pub agent_id: AgentSessionId,
    pub context: AgentContext,
}

impl ContextObservation {
    pub fn new(agent_id: impl Into<AgentSessionId>, context: AgentContext) -> Option<Self> {
        let agent_id = agent_id.into();
        (!agent_id.as_str().trim().is_empty()).then_some(Self { agent_id, context })
    }
}

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

    pub(super) fn sub_provider_parts(&self) -> Option<(&str, &str)> {
        match self {
            Self::SubProvider { provider, variant } => Some((provider, variant)),
            Self::KindWide => None,
        }
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
    /// its local session index and app-server thread `name`. Absent until named,
    /// so a renderer prefers it over the task definition only when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    /// Short provider-owned thread summary. Codex fills this from app-server
    /// `thread/read` / `thread/list` `preview`, and Kimi uses the `state.json`
    /// title. Renderers use it for the activity description when no session
    /// name is present.
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
    /// How the current turn came to rest when it fired no `Stop` hook to
    /// record its end. Status-projection marker like
    /// [`turn_error`](Self::turn_error) and self-clearing the same way: the
    /// projection settles a falsely-active row to the outcome's status while
    /// the marker postdates `last_activity`, and a newer prompt advancing
    /// `last_activity` past it drops the row back to its lifecycle status.
    /// Display-only — it never reaches the event log or a decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle: Option<TurnSettle>,
    /// When the producer observed this record. Snapshot liveness comes from
    /// the rollup row; a sidecar without a surviving row is not joined.
    pub observed_at: Timestamp,
}

/// Why a turn came to rest without a `Stop` hook to record its end. A provider
/// tail yields at most one resting outcome per turn, so the outcomes are
/// mutually exclusive and travel as one marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnSettleOutcome {
    /// The turn finished cleanly — Codex's `/review` runs in review mode and
    /// closes on a `task_complete` without a `Stop`. Settles a running row to
    /// `success` instead of letting the stall window misread a finished review
    /// as failed.
    Complete,
    /// A completed planning turn rests on the provider's native plan selector.
    /// Settles a running row to `waiting`.
    PlanProposed,
    /// A provider status channel or validated local transcript reports a native
    /// input dialog. Settles a running row to `waiting` as a display-only
    /// attention edge: it creates no durable ask and the provider pane remains
    /// the answer surface.
    NativeWait,
    /// The turn was interrupted with no result — Codex writes `turn_aborted`
    /// for `/clear` mid-turn and Esc. Settles a running or waiting row to
    /// `idle`, including a native ask that Esc cancelled without a hook.
    Interrupted,
}

/// One resting-turn marker: the outcome and the instant the provider proved it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnSettle {
    pub at: Timestamp,
    pub outcome: TurnSettleOutcome,
}

impl TurnSettle {
    pub fn new(at: Timestamp, outcome: TurnSettleOutcome) -> Self {
        Self { at, outcome }
    }
}

impl Default for AgentContext {
    fn default() -> Self {
        Self {
            source: String::new(),
            session_name: None,
            session_preview: None,
            model_id: None,
            model_display_name: None,
            effort: None,
            thinking_enabled: None,
            output_style: None,
            vim_mode: None,
            agent_version: None,
            exceeds_200k_tokens: None,
            cost: None,
            tokens: None,
            rate_limits: None,
            pr: None,
            account: None,
            turn_opened_by: Vec::new(),
            turn_error: None,
            settle: None,
            observed_at: Timestamp::UNIX_EPOCH,
        }
    }
}

impl AgentContext {
    pub fn new(source: &str, observed_at: Timestamp) -> Self {
        Self {
            source: source.to_owned(),
            observed_at,
            ..Self::default()
        }
    }
}

/// What an out-of-band context refresh is handed to work from. The caller has
/// already resolved the runtime and read the prior sidecar, so the adapter only
/// reads its own provider source.
pub struct SessionContextInput<'a> {
    /// The provider's own session id, the key the sidecar is filed under.
    pub session_id: &'a str,
    /// The session's current model id, when the launcher knows one.
    pub model: Option<&'a str>,
    /// An embedded provider server's base URL, for adapters whose plugin
    /// reports one on its lifecycle envelope.
    pub server_url: Option<&'a str>,
    /// The sidecar as last written, so an adapter can throttle its expensive
    /// read and diff against what is already stored.
    pub prior: Option<&'a record::AgentContextRecord>,
    /// The shared price book, for adapters that fold cost while refreshing.
    pub pricing_cache_path: &'a Path,
    /// This session's warm broker socket, for adapters that host one.
    pub broker_socket: Option<&'a Path>,
}

/// The write intent one out-of-band refresh produced. Read-only: the adapter
/// performs no store I/O, and the caller owns every write and the sidebar
/// wakeup.
#[derive(Default)]
pub struct SessionContextRefresh {
    /// Local-source intent, applied through `merge_local_context`.
    pub local: Option<LocalContextRefresh>,
    /// A rich out-of-band reading, folded onto the record through
    /// [`ContextCapability::merge_session_context`](super::capabilities::ContextCapability::merge_session_context).
    pub observed: Option<AgentContext>,
    /// Provider account usage read during the same pass.
    pub realtime_usage: Option<crate::AccountUsageSnapshot>,
}

/// Explicit update for one optional local-context field.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum FieldPatch<T> {
    /// Preserve the value already stored by another producer.
    #[default]
    Keep,
    /// Replace the stored value.
    Set(T),
    /// Remove the stored value.
    Clear,
}

impl<T> FieldPatch<T> {
    pub fn apply(self, target: &mut Option<T>) {
        match self {
            Self::Keep => {}
            Self::Set(value) => *target = Some(value),
            Self::Clear => *target = None,
        }
    }

    pub fn is_keep(&self) -> bool {
        matches!(self, Self::Keep)
    }

    #[cfg(any(test, feature = "testkit"))]
    pub fn as_set(&self) -> Option<&T> {
        match self {
            Self::Set(value) => Some(value),
            Self::Keep | Self::Clear => None,
        }
    }

    #[cfg(any(test, feature = "testkit"))]
    pub fn into_set(self) -> Option<T> {
        match self {
            Self::Set(value) => Some(value),
            Self::Keep | Self::Clear => None,
        }
    }
}

/// Merge policy for token readings from a machine-local source.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum LocalTokenPatch {
    /// Preserve the stored token reading.
    #[default]
    Keep,
    /// Accept a new gauge unless it is the fresh-zero sentinel, preserving an
    /// established gauge and its exact provider-reported window in that case;
    /// merge cumulative session counters independently of current occupancy.
    PreserveEstablished(Option<AgentTokenUsage>),
    /// Replace current-call occupancy while retaining monotonic session totals.
    ReplaceCurrentPreservingSession(Option<AgentTokenUsage>),
}

/// Fields one local transcript, rollout, or telemetry refresh may update.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LocalContextPatch {
    pub session_name: FieldPatch<String>,
    pub session_preview: FieldPatch<String>,
    pub model_id: FieldPatch<String>,
    pub model_display_name: FieldPatch<String>,
    pub effort: FieldPatch<String>,
    pub tokens: LocalTokenPatch,
    pub cost: FieldPatch<AgentCost>,
    pub turn_error: FieldPatch<AgentTurnError>,
    pub settle: FieldPatch<TurnSettle>,
}

impl LocalContextPatch {
    pub fn authoritative_current() -> Self {
        Self {
            tokens: LocalTokenPatch::PreserveEstablished(None),
            settle: FieldPatch::Clear,
            ..Self::default()
        }
    }

    /// Apply adapter-owned policy without consulting provider identity.
    pub fn apply(self, context: &mut AgentContext, definition: &AgentSpec) {
        let prior_model_id = context.model_id.clone();
        let prior_model_display_name = context.model_display_name.clone();
        let prior_tokens = context.tokens.clone();

        self.session_name.apply(&mut context.session_name);
        self.session_preview.apply(&mut context.session_preview);
        self.model_id.apply(&mut context.model_id);
        self.model_display_name
            .apply(&mut context.model_display_name);
        self.effort.apply(&mut context.effort);

        let model_changed = prior_model_id != context.model_id
            || prior_model_display_name != context.model_display_name;
        self.tokens.apply(
            &mut context.tokens,
            prior_tokens.as_ref(),
            model_changed,
            definition.default_context_window,
            prior_model_id.as_deref(),
            context.model_id.as_deref(),
        );
        self.cost.apply(&mut context.cost);
        self.turn_error.apply(&mut context.turn_error);
        self.settle.apply(&mut context.settle);
    }
}

impl LocalTokenPatch {
    #[cfg(test)]
    pub(super) fn as_value(&self) -> Option<&AgentTokenUsage> {
        match self {
            Self::PreserveEstablished(value) | Self::ReplaceCurrentPreservingSession(value) => {
                value.as_ref()
            }
            Self::Keep => None,
        }
    }

    #[cfg(test)]
    pub(super) fn into_value(self) -> Option<AgentTokenUsage> {
        match self {
            Self::PreserveEstablished(value) | Self::ReplaceCurrentPreservingSession(value) => {
                value
            }
            Self::Keep => None,
        }
    }

    fn apply(
        self,
        target: &mut Option<AgentTokenUsage>,
        prior: Option<&AgentTokenUsage>,
        model_changed: bool,
        default_context_window: Option<u64>,
        prior_model_id: Option<&str>,
        final_model_id: Option<&str>,
    ) {
        match self {
            Self::Keep => (),
            Self::PreserveEstablished(mut incoming) => {
                preserve_established_tokens(prior, &mut incoming);
                preserve_cached_context_window(
                    prior,
                    default_context_window,
                    prior_model_id,
                    final_model_id,
                    incoming.as_mut(),
                );
                *target = incoming;
            }
            Self::ReplaceCurrentPreservingSession(mut incoming) => {
                replace_current_preserving_session(prior, &mut incoming, model_changed);
                *target = incoming;
            }
        }
    }
}

fn replace_current_preserving_session(
    prior: Option<&AgentTokenUsage>,
    incoming: &mut Option<AgentTokenUsage>,
    model_changed: bool,
) {
    let Some(prior) = prior else {
        return;
    };
    let Some(incoming) = incoming.as_mut() else {
        let mut preserved = prior.clone();
        preserved.used_percentage = None;
        preserved.remaining_percentage = None;
        preserved.current_context_tokens = None;
        preserved.current_usage = None;
        if model_changed {
            preserved.context_window_size = None;
        }
        *incoming = Some(preserved);
        return;
    };
    if !model_changed && incoming.context_window_size.is_none() {
        incoming.context_window_size = prior.context_window_size;
    }
    merge_session_usage(&mut incoming.session_usage, prior.session_usage.clone());
}

pub(crate) fn merge_session_usage(
    target: &mut Option<AgentSessionUsage>,
    incoming: Option<AgentSessionUsage>,
) {
    let Some(incoming) = incoming else {
        return;
    };
    let target = target.get_or_insert_with(AgentSessionUsage::default);
    target.input_tokens = monotonic_count(target.input_tokens, incoming.input_tokens);
    target.output_tokens = monotonic_count(target.output_tokens, incoming.output_tokens);
    target.cache_creation_input_tokens = monotonic_count(
        target.cache_creation_input_tokens,
        incoming.cache_creation_input_tokens,
    );
    target.cache_read_input_tokens = monotonic_count(
        target.cache_read_input_tokens,
        incoming.cache_read_input_tokens,
    );
    target.thinking_tokens = monotonic_count(target.thinking_tokens, incoming.thinking_tokens);
}

fn monotonic_count(prior: Option<u64>, incoming: Option<u64>) -> Option<u64> {
    match (prior, incoming) {
        (Some(prior), Some(incoming)) => Some(prior.max(incoming)),
        (prior, incoming) => prior.or(incoming),
    }
}

fn preserve_established_tokens(
    prior: Option<&AgentTokenUsage>,
    refresh: &mut Option<AgentTokenUsage>,
) {
    let Some(prior) =
        prior.filter(|tokens| established_token_usage(tokens) || tokens.session_usage.is_some())
    else {
        return;
    };
    let incoming_session = refresh
        .as_ref()
        .and_then(|tokens| tokens.session_usage.clone());
    match refresh {
        None => *refresh = Some(prior.clone()),
        Some(tokens)
            if inferred_fresh_tokens(tokens)
                || (tokens.session_usage.is_some()
                    && tokens.context_window_size.is_none()
                    && tokens.used_percentage.is_none()
                    && tokens.remaining_percentage.is_none()
                    && tokens.current_context_tokens.is_none()
                    && tokens.current_usage.is_none()) =>
        {
            *tokens = prior.clone();
        }
        Some(_) => {}
    }
    if let Some(tokens) = refresh {
        merge_session_usage(&mut tokens.session_usage, incoming_session);
        merge_session_usage(&mut tokens.session_usage, prior.session_usage.clone());
    }
}

fn established_token_usage(tokens: &AgentTokenUsage) -> bool {
    tokens.used_percentage.is_some_and(|pct| pct > 0)
        || tokens
            .current_context_tokens
            .is_some_and(|tokens| tokens > 0)
        || tokens
            .current_usage
            .as_ref()
            .is_some_and(|usage| !usage.is_zero())
}

fn inferred_fresh_tokens(tokens: &AgentTokenUsage) -> bool {
    tokens.used_percentage.is_none()
        && tokens.current_context_tokens.is_none()
        && tokens
            .current_usage
            .as_ref()
            .is_some_and(AgentCurrentUsage::is_zero)
}

fn preserve_cached_context_window(
    prior: Option<&AgentTokenUsage>,
    default_context_window: Option<u64>,
    prior_model_id: Option<&str>,
    final_model_id: Option<&str>,
    incoming: Option<&mut AgentTokenUsage>,
) {
    let (Some(prior), Some(default_context_window), Some(incoming)) =
        (prior, default_context_window, incoming)
    else {
        return;
    };
    let Some(prior_context_window) = prior.context_window_size else {
        return;
    };
    if incoming.context_window_size != Some(default_context_window)
        || prior_context_window == default_context_window
        || prior_model_id != final_model_id
    {
        return;
    }
    incoming.context_window_size = Some(prior_context_window);
}

/// Round and clamp a reported percentage to the `0..=100` gauge range.
pub(super) fn clamp_pct(value: Option<f64>) -> Option<u8> {
    value
        .filter(|value| value.is_finite())
        .map(|value| value.round().clamp(0.0, 100.0) as u8)
}

/// Resume state for exact, incremental pricing of one child's transcript.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SubagentUsageCursor {
    /// Provider transcript this cursor resumes. A path change resets the fold.
    pub transcript_path: String,
    /// Byte offset just past the last complete JSONL record consumed.
    pub offset: u64,
    /// Newest non-synthetic `message.model` consumed from the child transcript,
    /// retained across incremental folds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Cumulative per-request-priced cost for every consumed child request.
    pub cost_usd: f64,
    /// At least one priceable request had no price, so the cumulative figure is
    /// incomplete and must not be displayed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub unpriced: bool,
    /// Fingerprint of every price source used for this cursor. A changed
    /// fingerprint forces one full replay so both healed and changed rates
    /// apply to the cumulative figure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub book_fingerprint: Option<String>,
    /// Last keyed request, retained across statusline ticks so a contiguous
    /// duplicate can replace its earlier, less complete record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_request: Option<PricedRequest>,
}

impl SubagentUsageCursor {
    /// Exact cumulative cost, hidden whenever any consumed request was unpriced.
    pub fn display_cost(&self) -> Option<f64> {
        (!self.unpriced).then_some(self.cost_usd)
    }
}

/// One request retained for the child-transcript duplicate replacement guard.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PricedRequest {
    /// `message.id`, a NUL separator, and `requestId`.
    pub key: String,
    pub cost_usd: f64,
    pub token_total: u64,
    /// Whether the request carried Claude's fast/priority pricing marker.
    #[serde(default, skip_serializing_if = "is_false")]
    pub has_speed: bool,
}

/// Per-subagent enrichment a paneless child cannot publish for itself. Claude's
/// `subagentStatusLine` is `exec`d to render the agent panel's child rows and is
/// handed each task's `type`, `model`, `effort`, `description`, `tokenCount`,
/// and `startTime`; RimZ harvests those into one of these per child so the
/// expanded card paints what the child is doing, what it has spent, and how long it has run. Identity-free like
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
    /// Child-transcript model enrichment. Folds onto `AgentState.model` only
    /// when the lifecycle bracket never established one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Reasoning effort from the task statusline. Folds only while lifecycle
    /// metadata has not established the child's effort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
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
    /// Exact cumulative per-request-priced child cost, when the provider exposes
    /// a dedicated transcript and every priceable request resolved. Display-only:
    /// parent/session spend already includes child requests, so this is never
    /// added to a group or provider total.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
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

/// Temporal coverage for a cost total. Cumulative session totals are additive;
/// point-in-time current-usage prices stay display-only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostCoverage {
    /// A cumulative session total.
    #[default]
    Session,
    /// A price for replace-style current usage that cannot be added over time.
    CurrentUsage,
}

impl CostCoverage {
    pub const fn contributes_to_live_spend(self) -> bool {
        matches!(self, Self::Session)
    }

    fn is_session(&self) -> bool {
        matches!(self, Self::Session)
    }
}

/// A USD cost total plus the temporal coverage needed by additive spend policy.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentCost {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "CostCoverage::is_session")]
    pub coverage: CostCoverage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_api_duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_lines_added: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_lines_removed: Option<u64>,
}

/// Token accounting from two deliberately separate scopes. `used_percentage`,
/// `current_context_tokens`, and `current_usage` describe the current context
/// window; `session_usage` carries cumulative lifetime counters when a provider
/// exposes those without exposing context occupancy. Only the current-window
/// fields drive gauges and compaction decisions.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentTokenUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_percentage: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_percentage: Option<u8>,
    /// Provider-reported tokens currently occupying the context window when
    /// their categories are unavailable. This authoritative numerator takes
    /// precedence over a categorized split when both are present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_context_tokens: Option<u64>,
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
    /// Tokens currently occupying the context window — the provider-reported
    /// scalar when available, else the latest API response's
    /// `input + cache_creation + cache_read`. This is the numerator
    /// [`AgentTokenUsage::used_percentage`] scales (output joins the window only
    /// next turn). `None` before the first measurement clears the breakdown.
    pub fn used_tokens(&self) -> Option<u64> {
        self.current_context_tokens.or_else(|| {
            let usage = self.current_usage.as_ref()?;
            Some(
                usage.input_tokens.unwrap_or(0)
                    + usage.cache_creation_input_tokens.unwrap_or(0)
                    + usage.cache_read_input_tokens.unwrap_or(0),
            )
        })
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

const CACHE_HIT_GOOD_MIN: u8 = 90;
const CACHE_HIT_CAUTION_MIN: u8 = 70;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheHealth {
    Good,
    Caution,
    Alarm,
}

impl CacheHealth {
    pub const fn classify(percent: u8) -> Self {
        if percent >= CACHE_HIT_GOOD_MIN {
            Self::Good
        } else if percent >= CACHE_HIT_CAUTION_MIN {
            Self::Caution
        } else {
            Self::Alarm
        }
    }
}

pub(crate) fn cache_hit_percent(cache_read: u64, fresh_or_written_input: u64) -> Option<u8> {
    let denominator = u128::from(cache_read) + u128::from(fresh_or_written_input);
    if denominator == 0 {
        return None;
    }
    let rounded = (u128::from(cache_read) * 100 + denominator / 2) / denominator;
    Some(rounded.min(100) as u8)
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

    pub fn cache_hit_percent(&self) -> Option<u8> {
        cache_hit_percent(self.cache_read_tokens(), self.displayed_input_tokens())
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

/// The rate-limit windows the agent surfaces. Temporal windows carry their own length for labels and refill projection. Scoped windows retain provider identity: durationless named quotas stand alone, while model sub-caps carry their parent's duration without becoming account-wide temporal limits.
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

/// Provider-defined identity and compact presentation label for a named quota or model sub-cap (`model:<name>`).
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
    /// Optional provider-defined identity for named quotas and model sub-caps. A scope id is stable across readings; its label is display-only.
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
    /// refills while idle. Named quotas leave it absent; model sub-caps carry their parent's duration and fold onto its bar.
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
/// trusts it. A current official-API reading anchors the window in either
/// direction; only a later best-effort reading may overlay it, with a refill
/// requiring confirmation unless the reset timer advances.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowSource {
    /// Queried from the provider's official usage API (Claude OAuth usage,
    /// Codex app-server `account/rateLimits/read`). Truth at its `observed_at`:
    /// it overrides readings observed no later than itself, climb or drop.
    Authoritative,
    /// Derived from the agent's statusline payload. Current while the agent
    /// works, but an idle session re-emits a stale payload, so a downward move
    /// requires a reset-timer advance or a confirmed near-full refill.
    #[default]
    BestEffort,
}

impl WindowSource {
    /// Whether this is the default ([`WindowSource::BestEffort`]) — lets serde
    /// omit the common case and a cold cache deserialize to the safe reading.
    pub(super) fn is_best_effort(&self) -> bool {
        matches!(self, WindowSource::BestEffort)
    }

    /// Whether this reading came from an official-API query.
    pub fn is_authoritative(&self) -> bool {
        matches!(self, WindowSource::Authoritative)
    }
}

/// Grace allowed when judging a not-started window: a reset still within this
/// much of a full window-length out counts as "clock not begun", absorbing the
/// small skew between the first token and the provider stamping the reset.
const NOT_STARTED_GRACE: SignedDuration = SignedDuration::from_secs(120);

/// Usage at or below this percentage still represents a fresh provider window.
pub(crate) const FRESH_WINDOW_USAGE_FLOOR: u8 = 1;

impl RateLimitWindow {
    pub(crate) fn key(&self) -> RateLimitWindowKey {
        self.scope.as_ref().map_or_else(
            || RateLimitWindowKey::Duration(self.duration_mins),
            |scope| RateLimitWindowKey::Scope(scope.id.clone()),
        )
    }

    /// Whether this scoped window partitions the unscoped parent's allowance.
    pub(crate) fn sub_cap_of(&self, parent: &Self) -> bool {
        self.scope.is_some()
            && parent.scope.is_none()
            && self.duration_mins.is_some()
            && self.duration_mins == parent.duration_mins
    }

    /// Project dated unscoped windows to `now`, refilling and rolling their reset forward. Scoped readings retain provider truth so status consumers can distinguish spent and elapsed quotas; display consumers clear expired scoped usage separately.
    pub fn projected_at(self, now: Timestamp) -> Self {
        match (self.resets_at, self.duration_mins) {
            (Some(resets_at), Some(mins)) if self.scope.is_none() && resets_at <= now => Self {
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

    /// Whether this spent duration window still has a future natural reset.
    /// Missing and elapsed reset clocks cannot drive redemption or attention
    /// policy.
    pub fn spent_with_future_reset(&self, now: Timestamp) -> bool {
        self.scope.is_none()
            && self.duration_mins.is_some_and(|mins| mins > 0)
            && self.is_spent()
            && self.resets_at.is_some_and(|reset| reset > now)
    }

    /// Whether this window's sliding clock has not begun. These budgets start on
    /// the first billable token, so until then the provider keeps `resets_at`
    /// slid ~a full window-length ahead. Detection keys on that reset distance,
    /// not a 0% reading — a fresh 5h window still reports ~1% used, never 0 — so
    /// any usage above the ~1% floor short-circuits to "started" regardless of
    /// the reset (this also covers a spent window at 100%). An absent reset or
    /// duration can't be judged this way, so it reads as started: a known
    /// reading whose countdown is a real one. Drives the dashboard's
    /// no-countdown "ready to start" treatment.
    pub fn not_started(&self, now: Timestamp) -> bool {
        if self.used_percentage > Some(FRESH_WINDOW_USAGE_FLOOR) {
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
        let status = http_error_status(&lower);
        if lower.contains("spend limit") {
            Self::PausedSpendLimit
        } else if lower.contains("usage limit")
            || lower.contains("session limit")
            || lower.contains("rate limit")
            || lower.contains("quota")
            || lower.contains("too many requests")
            || status == Some(429)
        {
            Self::PausedRateLimit
        } else if is_transient_server_error(&lower)
            || status.is_some_and(|code| (500..600).contains(&code))
        {
            Self::PausedOverloaded
        } else {
            Self::Failed
        }
    }
}

/// The HTTP status a provider error text reports. The status must immediately
/// follow a marker so an unrelated URL, source line, or token count cannot cast
/// a verdict.
fn http_error_status(lower: &str) -> Option<u16> {
    const MARKERS: [&str; 3] = ["status", "gateway", "error code"];
    for marker in MARKERS {
        for (offset, _) in lower.match_indices(marker) {
            if lower[..offset]
                .chars()
                .next_back()
                .is_some_and(|ch| ch.is_ascii_alphanumeric())
            {
                continue;
            }
            let suffix = &lower[offset + marker.len()..];
            let digits = suffix.trim_start_matches(|ch: char| {
                ch.is_ascii_whitespace() || matches!(ch, ':' | '/' | '=')
            });
            if digits.len() == suffix.len() {
                continue;
            }
            let bytes = digits.as_bytes();
            if bytes.len() < 3
                || !bytes[..3].iter().all(u8::is_ascii_digit)
                || bytes.get(3).is_some_and(u8::is_ascii_digit)
            {
                continue;
            }
            let code = u16::from(bytes[0] - b'0') * 100
                + u16::from(bytes[1] - b'0') * 10
                + u16::from(bytes[2] - b'0');
            if (400..600).contains(&code) {
                return Some(code);
            }
        }
    }
    None
}

fn is_transient_server_error(lower: &str) -> bool {
    lower.contains("overloaded")
        || lower.contains("at capacity")
        || lower.contains("high demand")
        || lower.contains("server is busy")
        || lower.contains("internal server error")
        || lower.contains("server error")
        || lower.contains("service unavailable")
        || lower.contains("bad gateway")
        || lower.contains("gateway timeout")
        || lower.contains("no response from api")
        || lower.contains("stalled")
        || lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("connection error")
        || lower.contains("connection closed")
        || lower.contains("connection reset")
        || lower.contains("connection lost")
        || lower.contains("socket hang up")
        || lower.contains("broken pipe")
        || lower.contains("econnreset")
        || lower.contains("mid-response")
        || lower.contains("mid-stream")
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
mod tests;
