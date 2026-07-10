//! Agent rollup state, displayed-status projections, and context severity.
//!
//! This is the provider-agnostic model the store reducer writes and the
//! sidebar projects. The rollup itself lives with the agent integration layer.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::ids::{AgentKind, AgentSessionId, AskId};
use crate::pane::{PaneRef, RuntimeOwner, RuntimeOwnerKind};

use super::context::{
    AgentContext, AgentRateLimits, AgentTokenUsage, AgentTurnError, RateLimitWindow, TurnErrorClass,
};
use super::lifecycle::{AskKind, LifecycleState, TurnPhase};

/// Durable identity and summary for the blocking prompt currently owning an
/// agent's input. Structured question detail stays in the transcript and joins
/// by `id`; this projection is the authoritative openness record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenAsk {
    pub id: AskId,
    pub kind: AskKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub since: Timestamp,
}

/// One hour: the shared ceiling for attention heat and breath tempo, and the
/// default inactive window below which a card sinks beneath live work.
pub const ATTENTION_AGE_CEILING_SECS: i64 = 3_600;

/// Default `[agents.attention] inactive_after_secs`: a row with no activity for
/// this long sinks into the inactive partition, beneath every live row.
pub const DEFAULT_INACTIVE_AFTER_SECS: u32 = ATTENTION_AGE_CEILING_SECS as u32;

/// Default `[agents.attention] archive_after_secs`: a row with no activity for
/// this long stops competing with hot or warm work and parks in the archive
/// partition.
pub const DEFAULT_ARCHIVE_AFTER_SECS: u32 = 24 * 60 * 60;

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

    /// Attention-class: a human may want this row. `Waiting` and `Failed` are
    /// actionable; `Paused` is attention-class but parked. The
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
/// `[theme.display.context_meter]` bands and stamped on each agent's sidebar row where the
/// config is folded onto the snapshot, so the renderer's color ramp and future
/// notification hooks read one verdict instead of re-deriving it. Ordered, so a threshold reads
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
        bands: &crate::config::ContextMeterConfig,
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
/// future notification or automation hook can inspect (an auto-compact policy
/// matching `ContextSeverity { to: Amber, .. }` and sending `rimz pane send
/// /compact`). Defined now so the seam is typed against the verdicts the
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
/// stalled. The per-machine `[agents.attention] stalled_after_secs` setting
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

/// The class to use for display and auto-resume decisions. Current adapters
/// stamp the class directly; the label fallback keeps older sidecars that saw
/// a provider "session limit" or "spend limit" before Rimz knew that phrase
/// parked instead of actionable. Only the limit-park classes remap: a legacy
/// `Failed` with an overload-ish label stays `Failed`, so old markers never
/// start arming overload retries.
pub(crate) fn effective_turn_error_class(error: &AgentTurnError) -> TurnErrorClass {
    if error.class != TurnErrorClass::Failed {
        return error.class;
    }
    match TurnErrorClass::classify_label(error.label.as_deref()) {
        class @ (TurnErrorClass::PausedSpendLimit | TurnErrorClass::PausedRateLimit) => class,
        _ => TurnErrorClass::Failed,
    }
}

/// Account-scoped subscription budget for one agent kind. Rate-limit windows are
/// the included budget that refills on its reset clock. Park, resume, and
/// display decisions read this fused account truth, never one paused session's
/// frozen context reading.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct AccountBudget {
    pub windows: Vec<RateLimitWindow>,
}

impl AccountBudget {
    fn projected_windows(&self, now: Timestamp) -> impl Iterator<Item = RateLimitWindow> + '_ {
        self.windows
            .iter()
            .cloned()
            .map(move |window| window.projected_at(now))
    }

    /// Latest reset among subscription windows that are spent now and have a
    /// future reset. This is the only local clock a rate/spend-limit park can
    /// safely auto-continue against.
    pub(crate) fn latest_spent_window_reset(&self, now: Timestamp) -> Option<Timestamp> {
        self.projected_windows(now)
            .filter(|window| window_spent_unreset(window, now))
            .filter_map(|window| window.resets_at)
            .max()
    }

    /// Whether the fused subscription budget has a known, available reading.
    /// Auto-continue uses this to retire stale persisted deadlines after the
    /// mana bar has recovered; a cold or unknown cache keeps the record for the
    /// durability path.
    pub(crate) fn subscription_budget_available(&self, now: Timestamp) -> bool {
        let mut has_known_available = false;
        for window in self.projected_windows(now) {
            if window_spent_unreset(&window, now) {
                return false;
            }
            has_known_available |= window.used_percentage.is_some() && !window.is_spent();
        }
        has_known_available
    }
}

/// The producer's published per-provider rate-limit windows, account-scoped so
/// the budgets outlive a session ending or going idle.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct RateLimitsCache {
    /// When the producer last refreshed this map. Observability only: reset
    /// projection ages windows on each `resets_at`, not this stamp.
    pub refreshed_at_ms: u64,
    /// Last-known windows by agent kind.
    pub windows: BTreeMap<String, AgentRateLimits>,
    /// In-flight best-effort refill candidates by kind, one per window duration.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pending: BTreeMap<String, Vec<PendingRefill>>,
}

/// A best-effort drop awaiting confirmation by the sidebar rate-limit fusion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingRefill {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_mins: Option<u32>,
    pub used_percentage: u8,
    pub first_seen_at: Timestamp,
}

/// Read the producer's published rate-limit window cache, or an empty cache on
/// a cold or corrupt file.
pub(crate) fn read_rate_limits_cache(path: &Path) -> RateLimitsCache {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub(crate) fn account_budgets_from_caches(
    runtime: &RuntimePaths,
    now: Timestamp,
) -> BTreeMap<AgentKind, AccountBudget> {
    let rate_limits = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    rate_limits
        .windows
        .into_iter()
        .map(|(kind, limits)| {
            (
                AgentKind::new_unchecked(kind),
                AccountBudget {
                    windows: limits
                        .windows
                        .into_iter()
                        .map(|window| window.projected_at(now))
                        .collect(),
                },
            )
        })
        .collect()
}

/// Whether a hidden resume-gated message may enter a paused agent now.
pub(crate) fn resume_gate_recovered(
    runtime: &RuntimePaths,
    agent: &AgentState,
    now: Timestamp,
) -> bool {
    if agent.effective_status() != AgentStatus::Paused {
        return false;
    }
    if let Some(park) = agent.budget_park.as_ref() {
        return park.resets_at.is_some_and(|resets_at| now >= resets_at);
    }
    let account_budgets = account_budgets_from_caches(runtime, now);
    let budget = account_budgets.get(&agent.kind);
    match resume_park(agent, budget, now) {
        Some(ResumeArm::Overloaded { .. }) => true,
        Some(ResumeArm::RateLimit { .. }) => false,
        None => display_turn_error(
            agent.status,
            agent.context.as_ref(),
            agent.last_activity,
            agent.turn_started_at,
        )
        .map(effective_turn_error_class)
        .is_some_and(|class| {
            matches!(
                class,
                TurnErrorClass::PausedRateLimit | TurnErrorClass::PausedSpendLimit
            ) && budget.is_some_and(|budget| budget.subscription_budget_available(now))
        }),
    }
}

/// Whether `kind`'s shortest account-scoped budget window is currently running
/// its clock. Window-priming callers use this to skip a ping when the window
/// has already started.
pub(crate) fn shortest_window_running(
    runtime: &RuntimePaths,
    kind: &str,
    now: Timestamp,
) -> Option<bool> {
    let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    shortest_window_running_in(&cache, kind, now)
}

fn shortest_window_running_in(cache: &RateLimitsCache, kind: &str, now: Timestamp) -> Option<bool> {
    let shortest = cache
        .windows
        .get(kind)?
        .windows
        .iter()
        .min_by_key(|window| window.duration_mins.unwrap_or(u32::MAX))?;
    window_running_verdict(shortest, now)
}

/// Whether `kind`'s longest account-scoped budget window is currently running
/// its clock. Reset-priming callers use this to skip a ping when the long
/// window has already started.
pub(crate) fn longest_window_running(
    runtime: &RuntimePaths,
    kind: &str,
    now: Timestamp,
) -> Option<bool> {
    let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    longest_window_running_in(&cache, kind, now)
}

fn longest_window_running_in(cache: &RateLimitsCache, kind: &str, now: Timestamp) -> Option<bool> {
    let longest = cache
        .windows
        .get(kind)?
        .windows
        .iter()
        .max_by_key(|window| window.duration_mins.unwrap_or(0))?;
    window_running_verdict(longest, now)
}

fn window_running_verdict(window: &RateLimitWindow, now: Timestamp) -> Option<bool> {
    let projected = window.clone().projected_at(now);
    projected.used_percentage?;
    if projected.not_started(now) {
        return Some(false);
    }
    match projected.resets_at {
        Some(reset) if reset > now => Some(true),
        _ => None,
    }
}

/// Raw reset stamp for `kind`'s longest dated account-scoped window. Callers use
/// this as an occurrence edge, so this intentionally does not project a passed
/// reset forward.
pub(crate) fn longest_window_reset_at(runtime: &RuntimePaths, kind: &str) -> Option<Timestamp> {
    let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    cache
        .windows
        .get(kind)?
        .windows
        .iter()
        .filter(|window| window.duration_mins.is_some())
        .max_by_key(|window| window.duration_mins)
        .and_then(|window| window.resets_at)
}

/// Each agent kind's rate-limit window standing, summarized from the fused
/// account budget. A kind lands in `spent` while it has a window that is
/// exhausted and not yet reset, and in `reset` only for a known spent reading
/// that cannot be projected to a refilled window.
#[derive(Default)]
pub(crate) struct RateLimitKindSummary {
    pub spent: BTreeSet<AgentKind>,
    pub reset: BTreeSet<AgentKind>,
}

/// Summarize every kind's spent/reset window standing from the fused account
/// budget. Drives the displayed-status projection; the per-agent context
/// windows are only an input to the fusion pipeline and do not make decisions.
pub(crate) fn rate_limit_window_kinds(
    account_budgets: &BTreeMap<AgentKind, AccountBudget>,
    now: Timestamp,
) -> RateLimitKindSummary {
    let mut summary = RateLimitKindSummary::default();
    for (kind, budget) in account_budgets {
        let mut has_spent = false;
        let mut has_reset = false;
        for window in budget.projected_windows(now) {
            if !window.is_spent() {
                continue;
            }
            if window_spent_unreset(&window, now) {
                has_spent = true;
            } else {
                has_reset = true;
            }
        }
        if has_spent {
            summary.spent.insert(kind.clone());
        }
        if has_reset {
            summary.reset.insert(kind.clone());
        }
    }
    summary
}

/// Whether a window is spent and has not yet reset — the budget is gone *now*.
fn window_spent_unreset(window: &RateLimitWindow, now: Timestamp) -> bool {
    window.is_spent() && window.resets_at.is_none_or(|reset| reset > now)
}

/// How a parked root agent's turn may resume, or `None` when nothing is armed.
/// The producer persists the arm so the resume outlives the ephemeral context it
/// was first seen through, and clears it only once the fused account budget
/// proves the subscription bar has recovered. A Rimz-derived projection over
/// enrichment, never a status the agent reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ResumeArm {
    /// A `rate_limit` or `spend_limit` park backed by a recovering subscription
    /// window. Extra credits may be disabled or exhausted; the refillable mana
    /// bar is the local clock, and the producer persists this deadline so the
    /// resume survives context TTL expiry or a later non-spent reading.
    RateLimit { deadline: Timestamp },
    /// A non-clocked park: the provider was overloaded or returned a transient
    /// server error, so there is no local reset window to wait on. The producer
    /// retries the resume on an expanding backoff while the park stays active,
    /// rather than against a window clock.
    Overloaded {
        /// The overload turn-error marker timestamp. The first retry is measured
        /// from this marker, so a late-observed park can fire immediately.
        overloaded_at: Timestamp,
    },
}

/// What kind of resume, if any, this root agent's parked turn is armed for. It
/// stopped its last turn on a provider park certificate ([`display_turn_error`]):
/// a `rate_limit` or `spend_limit` park arms from the fused account budget when
/// a subscription window is still spent and has a future reset, regardless of
/// whether extra paid credits are exhausted. A limit park with no recovering
/// subscription window arms nothing; a non-clocked overload/server-error park
/// arms on retry backoff while its marker stays active.
pub(crate) fn resume_park(
    agent: &AgentState,
    budget: Option<&AccountBudget>,
    now: Timestamp,
) -> Option<ResumeArm> {
    if agent.parent_agent_id.is_some() || agent.agent_id.is_empty() {
        return None;
    }
    let error = display_turn_error(
        agent.status,
        agent.context.as_ref(),
        agent.last_activity,
        agent.turn_started_at,
    )?;
    match effective_turn_error_class(error) {
        TurnErrorClass::PausedRateLimit | TurnErrorClass::PausedSpendLimit => {
            let deadline = budget?.latest_spent_window_reset(now)?;
            Some(ResumeArm::RateLimit { deadline })
        }
        TurnErrorClass::PausedOverloaded => Some(ResumeArm::Overloaded {
            overloaded_at: error.at,
        }),
        TurnErrorClass::Unknown | TurnErrorClass::Failed => None,
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
    context: Option<&AgentContext>,
    last_activity: Timestamp,
) -> bool {
    status == AgentStatus::Running
        && context
            .and_then(|context| context.turn_complete)
            .is_some_and(|at| at > last_activity)
}

/// Whether a `running` or `waiting` agent's latest turn was interrupted with no
/// `Stop` hook to record it — the provider marker
/// (`AgentContext::turn_interrupted`, folded in via the context sidecar)
/// postdates the agent's `last_activity`. This settles a falsely active row to
/// `idle`, including a native ask that Esc cancelled without a lifecycle hook.
/// Self-clearing like [`is_turn_complete`]: any newer hook event advances
/// `last_activity` past the marker. A Rimz-derived projection over enrichment,
/// never a status the agent reports.
pub fn is_turn_interrupted(
    status: AgentStatus,
    context: Option<&AgentContext>,
    last_activity: Timestamp,
) -> bool {
    matches!(status, AgentStatus::Running | AgentStatus::Waiting)
        && context
            .and_then(|context| context.turn_interrupted)
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
#[serde(from = "AgentStateWire")]
pub struct AgentState {
    pub agent_id: AgentSessionId,
    pub kind: AgentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Whether `name` is the launch-stamped user-chosen name. Explicit names
    /// render as the handle after a team role; minted and soft names remain
    /// last-resort instance selectors.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub name_explicit: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind_ordinal: Option<u32>,
    /// The `[agents.profiles]` profile this agent launched as (`planner`,
    /// `codex-yolo`), stamped by the launch event and carried forward like
    /// `name`. The agent answers to `@<profile>` and renders by it; `None` for
    /// a bare-kind launch. `RIMZ_AGENT_PROFILE` remains the pane's
    /// sender-attribution identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// The permission posture selected for this launch, carried forward so an
    /// explicit restart can reproduce it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<crate::harness::run::PermissionMode>,
    /// The `[agents.teams]` role this agent launched as (`planner`, `coder`),
    /// stamped by the launch event and carried forward like `profile`. The
    /// agent answers to `@<role>` when that role uniquely names it in scope.
    /// `RIMZ_AGENT_ROLE` remains the pane's sender-attribution identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// The `[agents.teams]` team this agent launched under, stamped by the
    /// launch event and carried forward like `role`. It identifies the launch
    /// cohort for resume; routing uses the launch-stamped `channel`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// Inline multi-agent launch cohort, stamped by `RIMZ_LAUNCH_GROUP` and
    /// carried forward like `team`. Team launches use `team` as their cohort
    /// key; inline layouts use this generated id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_group: Option<String>,
    /// Stable order inside the launch cohort: team role-list index or inline
    /// agent-cell index. Roleless team cells leave it unset and tail the block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_ordinal: Option<u32>,
    /// The routing lane stamped by `RIMZ_CHANNEL` at launch and carried forward
    /// like `team`. When absent, read paths fall back to the worktree directory
    /// basename.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    pub status: AgentStatus,
    /// The running turn's shape (reasoning / acting / parked on background
    /// work), written verbatim from the lifecycle machine's output. Always
    /// [`TurnPhase::Idle`] outside `Running` — the machine normalizes it, so
    /// the illegal combinations are unrepresentable here too.
    #[serde(default)]
    pub phase: TurnPhase,
    pub pane: Option<PaneRef>,
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
    /// Launch-seeded card label (`rimz agents --description`), carried forward.
    /// Ranks below the agent's own session naming and above the prompt on the
    /// card's description line; replaced once the agent emits its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Agent-reported transcript path for this session, carried forward from
    /// lifecycle events when available. Display/diagnostic metadata; sidecar
    /// readers keep their own freshness gates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    /// Provider-reported session lineage from the session store head (Codex
    /// today), carried forward so the rollup projection can collapse the
    /// superseded same-pane `/clear` conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<crate::agents::SessionOrigin>,
    /// Recent user prompts for this session, newest last, capped by the rollup.
    /// The sidebar row keeps only `prompt`; snapshot JSON exposes the history on
    /// `agents[]` for diagnostics and future panes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_prompts: Vec<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Canonical launch-carried budget (`$5.00` or `$20.00/day`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<String>,
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
    /// Rich session-scoped enrichment from a high-frequency out-of-band source
    /// (Claude's statusline). Folded in at snapshot time by
    /// `SidebarSnapshot::with_agent_context`, never reduced from the event log.
    /// Same enrich-only discipline as `context_pct`: display, never routing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<AgentContext>,
    /// Runtime-ledger projection. The producer and consumers rebuild it from
    /// the budget cache; the event reducer never treats it as durable truth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_park: Option<crate::harness::budget::BudgetPark>,
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
    /// Timestamp of the native prompt that put this session in `Waiting`.
    /// Activity after this instant proves the prompt was answered in the
    /// agent's own UI, so read paths project the row back to work even before
    /// the next lifecycle boundary arrives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_since: Option<Timestamp>,
    /// The prompt associated with `waiting_since`. Old lifecycle records have
    /// no identity and therefore replay with this field absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_ask: Option<OpenAsk>,
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
    /// Occupied-context-token reading for the latest smart-compact command this
    /// agent received. The send path suppresses duplicate `/compact` sends while
    /// the carried-forward gauge still equals this baseline, without rescanning
    /// old message events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_compact_command_tokens: Option<u64>,
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

#[derive(Deserialize)]
struct AgentStateWire {
    agent_id: AgentSessionId,
    kind: AgentKind,
    name: Option<String>,
    #[serde(default)]
    name_explicit: bool,
    kind_ordinal: Option<u32>,
    profile: Option<String>,
    #[serde(default)]
    mode: Option<crate::harness::run::PermissionMode>,
    role: Option<String>,
    team: Option<String>,
    launch_group: Option<String>,
    launch_ordinal: Option<u32>,
    channel: Option<String>,
    status: AgentStatus,
    #[serde(default)]
    phase: TurnPhase,
    pane: Option<PaneRef>,
    #[serde(default)]
    agent_pid: Option<u32>,
    #[serde(default)]
    agent_process_start: Option<String>,
    runtime_owner: Option<RuntimeOwner>,
    parent_agent_id: Option<AgentSessionId>,
    worktree_path: Option<String>,
    worktree_branch: Option<String>,
    task: Option<String>,
    prompt: Option<String>,
    description: Option<String>,
    transcript_path: Option<String>,
    origin: Option<crate::agents::SessionOrigin>,
    #[serde(default)]
    recent_prompts: Vec<String>,
    model: Option<String>,
    effort: Option<String>,
    #[serde(default)]
    budget: Option<String>,
    context_pct: Option<u8>,
    context_window: Option<u64>,
    total_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_write_input_tokens: Option<u64>,
    fresh_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    context: Option<AgentContext>,
    #[serde(default)]
    budget_park: Option<crate::harness::budget::BudgetPark>,
    subagent_description: Option<String>,
    subagent_started_at: Option<Timestamp>,
    turn_started_at: Option<Timestamp>,
    #[serde(default)]
    waiting_since: Option<Timestamp>,
    #[serde(default)]
    open_ask: Option<OpenAsk>,
    compacting_since: Option<Timestamp>,
    #[serde(default)]
    compaction_count: u32,
    last_compact_command_tokens: Option<u64>,
    last_seen: Timestamp,
    last_activity: Timestamp,
    registered_at: Option<Timestamp>,
}

impl From<AgentStateWire> for AgentState {
    fn from(wire: AgentStateWire) -> Self {
        let runtime_owner = wire.runtime_owner.or_else(|| {
            wire.agent_pid.map(|pid| {
                RuntimeOwner::new(
                    RuntimeOwnerKind::Agent,
                    wire.agent_id.to_string(),
                    pid,
                    wire.agent_process_start,
                )
            })
        });
        Self {
            agent_id: wire.agent_id,
            kind: wire.kind,
            name: wire.name,
            name_explicit: wire.name_explicit,
            kind_ordinal: wire.kind_ordinal,
            profile: wire.profile,
            mode: wire.mode,
            role: wire.role,
            team: wire.team,
            launch_group: wire.launch_group,
            launch_ordinal: wire.launch_ordinal,
            channel: wire.channel,
            status: wire.status,
            phase: wire.phase,
            pane: wire.pane,
            runtime_owner,
            parent_agent_id: wire.parent_agent_id,
            worktree_path: wire.worktree_path,
            worktree_branch: wire.worktree_branch,
            task: wire.task,
            prompt: wire.prompt,
            description: wire.description,
            transcript_path: wire.transcript_path,
            origin: wire.origin,
            recent_prompts: wire.recent_prompts,
            model: wire.model,
            effort: wire.effort,
            budget: wire.budget,
            context_pct: wire.context_pct,
            context_window: wire.context_window,
            total_tokens: wire.total_tokens,
            cache_read_input_tokens: wire.cache_read_input_tokens,
            cache_write_input_tokens: wire.cache_write_input_tokens,
            fresh_input_tokens: wire.fresh_input_tokens,
            output_tokens: wire.output_tokens,
            context: wire.context,
            budget_park: wire.budget_park,
            subagent_description: wire.subagent_description,
            subagent_started_at: wire.subagent_started_at,
            turn_started_at: wire.turn_started_at,
            waiting_since: wire.waiting_since,
            open_ask: wire.open_ask,
            compacting_since: wire.compacting_since,
            compaction_count: wire.compaction_count,
            last_compact_command_tokens: wire.last_compact_command_tokens,
            last_seen: wire.last_seen,
            last_activity: wire.last_activity,
            registered_at: wire.registered_at,
        }
    }
}

fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

impl AgentState {
    /// Minimal test fixture with stable identity fields and empty enrichment.
    #[cfg(any(test, feature = "testkit"))]
    pub fn stub(kind: &str, id: &str, status: AgentStatus) -> Self {
        let now = Timestamp::now();
        Self {
            agent_id: AgentSessionId::from(id),
            kind: AgentKind::new_unchecked(kind),
            name: Some(format!("{id}-name")),
            name_explicit: false,
            kind_ordinal: Some(1),
            profile: None,
            mode: None,
            role: None,
            team: None,
            launch_group: None,
            launch_ordinal: None,
            channel: None,
            status,
            phase: match status {
                AgentStatus::Running => TurnPhase::Reasoning,
                _ => TurnPhase::Idle,
            },
            pane: None,
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
            budget: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            context: None,
            budget_park: None,
            subagent_description: None,
            subagent_started_at: None,
            turn_started_at: None,
            waiting_since: None,
            open_ask: None,
            compacting_since: None,
            compaction_count: 0,
            last_compact_command_tokens: None,
            last_seen: now,
            last_activity: now,
            registered_at: Some(now),
        }
    }

    /// One-line activity label for CLI and sidebar rows: rich session preview,
    /// rich session name, launch description, live task, then latest prompt.
    pub fn activity_description(&self) -> Option<&str> {
        self.context
            .as_ref()
            .and_then(|context| context.session_preview.as_deref())
            .filter(|preview| usable_description(preview))
            .or_else(|| {
                self.context
                    .as_ref()
                    .and_then(|context| context.session_name.as_deref())
                    .filter(|name| usable_description(name))
            })
            .or_else(|| {
                self.description
                    .as_deref()
                    .filter(|description| usable_description(description))
            })
            .or_else(|| self.task.as_deref().filter(|task| usable_description(task)))
            .or_else(|| {
                self.prompt
                    .as_deref()
                    .filter(|prompt| usable_description(prompt))
            })
    }

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

    /// Status after cheap, context-only projections that every read path can
    /// share. A live turn with an active provider park certificate reads as
    /// `paused` even when the lifecycle rollup is still `running`; provider
    /// completion markers settle falsely-running rows to `success`, and
    /// interruption markers settle falsely-running or waiting rows to `idle`,
    /// which opens message delivery gates. Budget-aware callers may still
    /// upgrade a paused projection to `failed`.
    pub fn effective_status(&self) -> AgentStatus {
        if self.budget_park.is_some() && self.status != AgentStatus::Waiting {
            return AgentStatus::Paused;
        }
        if self.status == AgentStatus::Waiting
            && is_turn_interrupted(self.status, self.context.as_ref(), self.last_activity)
        {
            return AgentStatus::Idle;
        }
        if self.status != AgentStatus::Running {
            return self.status;
        }
        if let Some((class, _)) = self.displayed_turn_error() {
            return match class {
                TurnErrorClass::PausedRateLimit
                | TurnErrorClass::PausedSpendLimit
                | TurnErrorClass::PausedOverloaded => AgentStatus::Paused,
                TurnErrorClass::Unknown | TurnErrorClass::Failed => self.status,
            };
        }
        if is_turn_complete(self.status, self.context.as_ref(), self.last_activity) {
            return AgentStatus::Success;
        }
        if is_turn_interrupted(self.status, self.context.as_ref(), self.last_activity) {
            return AgentStatus::Idle;
        }
        self.status
    }

    /// True when the current `Waiting` state still predates all observed agent
    /// activity. A later activity heartbeat means the user answered in the
    /// native UI and the row should stop reserving input.
    pub fn is_awaiting_input(&self) -> bool {
        self.status == AgentStatus::Waiting
            && self
                .waiting_since
                .is_some_and(|waiting_since| self.last_activity <= waiting_since)
    }

    /// Provider API error currently explaining this row's displayed state. The
    /// returned class includes legacy label remapping, and the label is the
    /// upstream text to surface on user-facing cards.
    pub fn displayed_turn_error(&self) -> Option<(TurnErrorClass, Option<&str>)> {
        let error = display_turn_error(
            self.status,
            self.context.as_ref(),
            self.last_activity,
            self.turn_started_at,
        )?;
        Some((effective_turn_error_class(error), error.label.as_deref()))
    }

    /// Tokens currently occupying the window: the folded statusline breakdown,
    /// else the per-call split (`cache_read + cache_write + fresh_input`) the lifecycle rail
    /// reduces. `None` when nothing has reported occupancy yet.
    pub fn context_used_tokens(&self) -> Option<u64> {
        self.context
            .as_ref()
            .and_then(|context| context.tokens.as_ref())
            .and_then(AgentTokenUsage::used_tokens)
            .or_else(|| {
                let fresh = self.fresh_input_tokens?;
                Some(
                    self.cache_read_input_tokens.unwrap_or(0)
                        + self.cache_write_input_tokens.unwrap_or(0)
                        + fresh,
                )
            })
    }

    /// Tokens occupying the window for a `--smart-compact <tokens>` threshold: the
    /// precise composition when known, else the carried turn total. The gauge's
    /// `context_used_tokens` withholds a bare total so it never legends a partial
    /// composition; a threshold instead scales against the same numerator the
    /// percent gauge derives from, so `--smart-compact 100000` fires for a
    /// transcript-derived session that reports only a running total — matching
    /// `--smart-compact 70%`, which already reads that total through the gauge.
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

    pub(crate) fn backfill_rotation_enrichment_from(&mut self, base: &Self) {
        // Fill-only rotation merge. `store::snapshot::project::carried_state`
        // owns the authoritative reducer lifetime list.
        if self.transcript_path.is_none() {
            self.transcript_path = base.transcript_path.clone();
        }
        if self.worktree_path.is_none() {
            self.worktree_path = base.worktree_path.clone();
        }
        if self.worktree_branch.is_none() {
            self.worktree_branch = base.worktree_branch.clone();
        }
        if self.role.is_none() {
            self.role = base.role.clone();
        }
        if self.team.is_none() {
            self.team = base.team.clone();
        }
        if self.channel.is_none() {
            self.channel = base.channel.clone();
        }
        if self.profile.is_none() {
            self.profile = base.profile.clone();
        }
        if self.model.is_none() {
            self.model = base.model.clone();
        }
        if self.effort.is_none() {
            self.effort = base.effort.clone();
        }
        if self.budget.is_none() {
            self.budget = base.budget.clone();
        }
        if self.context_window.is_none() {
            self.context_window = base.context_window;
        }
        if self.last_compact_command_tokens.is_none() {
            self.last_compact_command_tokens = base.last_compact_command_tokens;
        }
    }
}

/// Collapse whitespace and drop control characters, returning `None` for blank
/// values. Callers keep the original borrowed field for precedence decisions and
/// run this only at render boundaries.
pub fn single_line_description(value: &str) -> Option<String> {
    let mut out = String::new();
    let mut pending_space = false;
    for ch in value.chars() {
        if ch.is_whitespace() {
            if !out.is_empty() {
                pending_space = true;
            }
            continue;
        }
        if ch.is_control() {
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(ch);
    }
    (!out.is_empty()).then_some(out)
}

pub fn usable_description(value: &str) -> bool {
    single_line_description(value).is_some() && !looks_like_control_text(value)
}

/// Whether a description candidate is a harness-injected control turn rather
/// than human-authored text.
pub fn looks_like_control_text(value: &str) -> bool {
    let trimmed = value.trim_start();
    crate::agents::CONTROL_TAG_PREFIXES
        .iter()
        .any(|tag| trimmed.starts_with(tag))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_agent_pid_deserializes_to_runtime_owner() {
        let agent: AgentState = serde_json::from_value(serde_json::json!({
            "agent_id": "sess-1",
            "kind": "codex",
            "status": "running",
            "agent_pid": 4242,
            "agent_process_start": "12345",
            "last_seen": "2026-07-01T00:00:00Z",
            "last_activity": "2026-07-01T00:00:00Z"
        }))
        .expect("legacy agent state");

        let owner = agent.runtime_owner.as_ref().expect("owner synthesized");
        assert_eq!(owner.kind, RuntimeOwnerKind::Agent);
        assert_eq!(owner.subject_id, "sess-1");
        assert_eq!(owner.pid, 4242);
        assert_eq!(owner.process_start.as_deref(), Some("12345"));

        let encoded = serde_json::to_value(&agent).expect("encode");
        assert!(encoded.get("agent_pid").is_none());
        assert!(encoded.get("agent_process_start").is_none());
    }

    #[test]
    fn activity_description_prefers_rich_context_then_fallbacks() {
        let mut agent = test_agent(AgentStatus::Running, 1_000);
        agent.prompt = Some("latest prompt".to_owned());
        agent.task = Some("live task".to_owned());
        agent.description = Some("launch label".to_owned());
        agent.context = Some(AgentContext {
            source: "codex".to_owned(),
            session_name: Some("thread name".to_owned()),
            session_preview: Some("thread preview".to_owned()),
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
            turn_complete: None,
            turn_interrupted: None,
            observed_at: Timestamp::from_second(1_000).unwrap(),
        });

        assert_eq!(agent.activity_description(), Some("thread preview"));
        agent.context.as_mut().unwrap().session_preview = None;
        assert_eq!(agent.activity_description(), Some("thread name"));
        agent.context = None;
        assert_eq!(agent.activity_description(), Some("launch label"));
        agent.description = None;
        assert_eq!(agent.activity_description(), Some("live task"));
        agent.task = None;
        assert_eq!(agent.activity_description(), Some("latest prompt"));
    }

    #[test]
    fn activity_description_rejects_blank_and_control_text() {
        let mut agent = test_agent(AgentStatus::Running, 1_000);
        agent.task = Some(" \n\t".to_owned());
        agent.prompt =
            Some("<task-notification>synthetic</task-notification> real prompt".to_owned());

        assert_eq!(agent.activity_description(), None);
        assert_eq!(
            single_line_description("ship\nwide\tlabel\rnow\u{0007}").as_deref(),
            Some("ship wide label now")
        );
    }

    /// The context tier climbs calm → yellow → amber → red, taking the worse
    /// of two axes — fill percentage and absolute tokens. Defaults: the Yellow
    /// tier starts warming at 40% / 100k, amber starts at 75% / 258k, and red
    /// starts at 90% / 420k.
    #[test]
    fn context_severity_takes_the_worse_of_percent_and_tokens() {
        let bands = crate::config::ContextMeterConfig::default();
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

    #[test]
    fn effective_turn_error_class_parks_legacy_limit_labels() {
        let turn_error = |label: &str| AgentTurnError {
            class: TurnErrorClass::Failed,
            at: Timestamp::from_second(1_700_000_000).unwrap(),
            label: Some(label.to_owned()),
        };

        assert_eq!(
            effective_turn_error_class(&turn_error("You've hit your monthly spend limit.")),
            TurnErrorClass::PausedSpendLimit
        );
        assert_eq!(
            effective_turn_error_class(&turn_error(
                "You've hit your session limit · resets 10:50am (UTC)"
            )),
            TurnErrorClass::PausedRateLimit
        );
        assert_eq!(
            effective_turn_error_class(&turn_error("API Error: Bad Request")),
            TurnErrorClass::Failed
        );
    }

    fn test_agent(status: AgentStatus, activity: i64) -> AgentState {
        let at = Timestamp::from_second(activity).unwrap();
        AgentState {
            status,
            ..crate::testkit::agent_state("claude", "sess", at)
        }
    }

    fn context_error(class: TurnErrorClass, at: i64) -> AgentContext {
        AgentContext {
            source: "claude".to_owned(),
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
            turn_error: Some(AgentTurnError {
                class,
                at: Timestamp::from_second(at).unwrap(),
                label: Some("provider parked".to_owned()),
            }),
            turn_complete: None,
            turn_interrupted: None,
            observed_at: Timestamp::from_second(at).unwrap(),
        }
    }

    fn context_settle(complete: Option<i64>, interrupted: Option<i64>) -> AgentContext {
        AgentContext {
            source: "codex".to_owned(),
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
            turn_complete: complete.map(|at| Timestamp::from_second(at).unwrap()),
            turn_interrupted: interrupted.map(|at| Timestamp::from_second(at).unwrap()),
            observed_at: Timestamp::from_second(1_000).unwrap(),
        }
    }

    #[test]
    fn effective_status_projects_active_provider_parks_to_paused() {
        for class in [
            TurnErrorClass::PausedSpendLimit,
            TurnErrorClass::PausedRateLimit,
            TurnErrorClass::PausedOverloaded,
        ] {
            let mut agent = test_agent(AgentStatus::Running, 1_000);
            agent.context = Some(context_error(class, 1_010));
            assert_eq!(agent.effective_status(), AgentStatus::Paused, "{class:?}");
        }
    }

    #[test]
    fn effective_status_keeps_raw_status_without_active_park() {
        let mut failed = test_agent(AgentStatus::Failed, 1_000);
        failed.turn_started_at = Some(Timestamp::from_second(900).unwrap());
        failed.context = Some(context_error(TurnErrorClass::PausedSpendLimit, 1_010));
        assert_eq!(failed.effective_status(), AgentStatus::Failed);

        let mut running = test_agent(AgentStatus::Running, 1_000);
        running.context = Some(context_error(TurnErrorClass::Failed, 1_010));
        assert_eq!(running.effective_status(), AgentStatus::Running);

        let mut unknown = test_agent(AgentStatus::Running, 1_000);
        unknown.context = Some(context_error(TurnErrorClass::Unknown, 1_010));
        assert_eq!(unknown.effective_status(), AgentStatus::Running);
    }

    #[test]
    fn waiting_and_interruption_outrank_a_budget_park() {
        let mut waiting = test_agent(AgentStatus::Waiting, 1_000);
        waiting.budget_park = Some(crate::harness::budget::BudgetPark {
            cap_usd: 5.0,
            spend_usd: 5.25,
            window: crate::harness::budget::BudgetWindow::Session,
            at: Timestamp::from_second(1_000).unwrap(),
            scope: crate::harness::budget::BudgetScope::Agent,
            account_kind: None,
            resets_at: None,
        });
        assert_eq!(waiting.effective_status(), AgentStatus::Waiting);

        waiting.context = Some(context_settle(None, Some(1_010)));
        assert_eq!(waiting.effective_status(), AgentStatus::Idle);
    }

    #[test]
    fn effective_status_projects_hookless_turn_settle_markers() {
        let mut complete = test_agent(AgentStatus::Running, 1_000);
        complete.context = Some(context_settle(Some(1_010), None));
        assert_eq!(complete.effective_status(), AgentStatus::Success);

        let mut interrupted = test_agent(AgentStatus::Running, 1_000);
        interrupted.context = Some(context_settle(None, Some(1_010)));
        assert_eq!(interrupted.effective_status(), AgentStatus::Idle);

        let mut interrupted_waiting = test_agent(AgentStatus::Waiting, 1_000);
        interrupted_waiting.context = Some(context_settle(None, Some(1_010)));
        assert_eq!(interrupted_waiting.effective_status(), AgentStatus::Idle);

        let mut stale_waiting = test_agent(AgentStatus::Waiting, 1_000);
        stale_waiting.context = Some(context_settle(None, Some(990)));
        assert_eq!(stale_waiting.effective_status(), AgentStatus::Waiting);

        let mut stale = test_agent(AgentStatus::Running, 1_000);
        stale.context = Some(context_settle(Some(990), Some(990)));
        assert_eq!(stale.effective_status(), AgentStatus::Running);

        let mut non_running = test_agent(AgentStatus::Idle, 1_000);
        non_running.context = Some(context_settle(Some(1_010), Some(1_010)));
        assert_eq!(non_running.effective_status(), AgentStatus::Idle);

        let mut parked = test_agent(AgentStatus::Running, 1_000);
        let mut context = context_error(TurnErrorClass::PausedRateLimit, 1_010);
        context.turn_complete = Some(Timestamp::from_second(1_010).unwrap());
        context.turn_interrupted = Some(Timestamp::from_second(1_010).unwrap());
        parked.context = Some(context);
        assert_eq!(parked.effective_status(), AgentStatus::Paused);
    }

    #[test]
    fn displayed_turn_error_projects_active_running_marker() {
        let mut agent = test_agent(AgentStatus::Running, 1_000);
        agent.context = Some(context_error(TurnErrorClass::PausedOverloaded, 1_010));

        assert_eq!(
            agent.displayed_turn_error(),
            Some((TurnErrorClass::PausedOverloaded, Some("provider parked")))
        );
    }

    #[test]
    fn displayed_turn_error_projects_terminal_marker_in_current_turn() {
        let mut agent = test_agent(AgentStatus::Failed, 1_100);
        agent.turn_started_at = Some(Timestamp::from_second(1_000).unwrap());
        agent.context = Some(context_error(TurnErrorClass::Failed, 1_010));

        assert_eq!(
            agent.displayed_turn_error(),
            Some((TurnErrorClass::Failed, Some("provider parked")))
        );
    }

    #[test]
    fn displayed_turn_error_self_clears_when_marker_is_stale() {
        let mut running = test_agent(AgentStatus::Running, 1_100);
        running.context = Some(context_error(TurnErrorClass::PausedOverloaded, 1_000));
        assert_eq!(running.displayed_turn_error(), None);

        let mut failed = test_agent(AgentStatus::Failed, 1_100);
        failed.turn_started_at = Some(Timestamp::from_second(1_050).unwrap());
        failed.context = Some(context_error(TurnErrorClass::Failed, 1_000));
        assert_eq!(failed.displayed_turn_error(), None);
    }

    /// The bands come from `[theme.display.context_meter]`, so a custom set moves every
    /// edge; a misordered set degrades to the highest matching tier (the red
    /// band is checked first), never to a calmer one.
    #[test]
    fn context_severity_honours_custom_and_misordered_bands() {
        use crate::config::{ContextBand, ContextMeterConfig};
        let tight = ContextMeterConfig {
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
        let misordered = ContextMeterConfig {
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
