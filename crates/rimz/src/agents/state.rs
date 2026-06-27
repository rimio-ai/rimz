//! Agent rollup state, displayed-status projections, and context severity.
//!
//! This is the provider-agnostic model the ledger reducer writes and the
//! sidebar projects. Feed items reference agents by session id, but the
//! rollup itself lives with the agent integration layer.

use std::collections::BTreeSet;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::ids::{AgentKind, AgentSessionId};
use crate::pane::{PaneRef, RuntimeOwner};

use super::context::{
    AgentContext, AgentTokenUsage, AgentTurnError, RateLimitWindow, TurnErrorClass,
};
use super::lifecycle::{LifecycleState, TurnPhase};

/// One hour: the shared ceiling for attention heat and breath tempo, and the
/// default inactive window below which a card sinks beneath live work.
pub const ATTENTION_AGE_CEILING_SECS: i64 = 3_600;

/// Default `[agents.attention] inactive_after_secs`: a row with no activity for
/// this long sinks into the inactive partition, beneath every live row.
pub const DEFAULT_INACTIVE_AFTER_SECS: u32 = ATTENTION_AGE_CEILING_SECS as u32;

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

    /// Attention-class: a human (or a resolver) may want this row. `Waiting`
    /// and `Failed` are actionable; `Paused` is attention-class but parked. The
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
/// config is folded onto the snapshot, so the renderer's color ramp and a
/// future hook flow (e.g. a resolver triggering `/compact` at amber) read one
/// verdict instead of re-deriving it. Ordered, so a threshold reads
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
/// future hook flow emits and a resolver acts on, riding the same feed the
/// resolver chain already drains (an auto-compact policy matching
/// `ContextSeverity { to: Amber, .. }` and answering with `rimz pane send
/// /compact`, exactly as the pane-send reference resolver acts on a recognised
/// prompt today). Defined now so the seam is typed against the verdicts the
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

/// Each agent kind's rate-limit window standing, summarized across every session
/// of that kind (the windows are account-scoped, so any session's reading speaks
/// for the kind). A kind lands in `spent` while it has a window that is exhausted
/// and not yet reset, and in `reset` once a window it had spent has refilled — the
/// signal that lifts a `rate_limit` park.
#[derive(Default)]
pub(crate) struct RateLimitKindSummary {
    pub spent: BTreeSet<AgentKind>,
    pub reset: BTreeSet<AgentKind>,
}

/// Summarize every kind's spent/reset window standing from the agents' own
/// rate-limit readings. Drives the displayed-status projection: a `rate_limit`
/// park lifts to `failed` once its windows reset. The park resume planner derives
/// its own per-agent deadline ([`resume_park`]) so it can persist it before the
/// ephemeral reading turns over.
pub(crate) fn rate_limit_window_kinds(
    agents: &[AgentState],
    now: Timestamp,
) -> RateLimitKindSummary {
    let mut summary = RateLimitKindSummary::default();
    for agent in agents {
        if agent.parent_agent_id.is_some() {
            continue;
        }
        let Some(limits) = agent
            .context
            .as_ref()
            .and_then(|ctx| ctx.rate_limits.as_ref())
        else {
            continue;
        };
        let mut has_spent = false;
        let mut has_reset = false;
        for window in &limits.windows {
            if !window.is_spent() {
                continue;
            }
            if window_spent_unreset(window, now) {
                has_spent = true;
            } else {
                has_reset = true;
            }
        }
        if has_spent {
            summary.spent.insert(agent.kind.clone());
        }
        if has_reset {
            summary.reset.insert(agent.kind.clone());
        }
    }
    summary
}

/// Whether a window is spent and has not yet reset — the budget is gone *now*.
fn window_spent_unreset(window: &RateLimitWindow, now: Timestamp) -> bool {
    window.is_spent() && window.resets_at.is_none_or(|reset| reset > now)
}

/// How a parked root agent's turn may resume, or `None` when nothing is armed.
/// The producer persists the arm while the park is fresh so the resume outlives
/// the ephemeral context it was first seen through. A Rimz-derived projection over
/// enrichment, never a status the agent reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ResumeArm {
    /// A `rate_limit` park: the turn may resume at the latest reset among the
    /// windows still spent *now* — the instant the displayed-status projection
    /// would lift the park from `paused` to `failed`. Persisting this deadline
    /// keeps the resume alive once the reading turns over: neither a
    /// context-sidecar TTL expiry (a 5h/7d window outlasts the 3h context TTL) nor
    /// a fresh non-spent reading (Codex's app-server refresh rolls the window
    /// forward) can erase it.
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
/// a `rate_limit` park arms for the latest reset of the windows still spent now
/// (and stops arming once every spent window has reset), while a non-clocked
/// backoff park arms while its marker stays active. Every other class — and a
/// `rate_limit` park whose budget has already refilled — arms nothing.
pub(crate) fn resume_park(agent: &AgentState, now: Timestamp) -> Option<ResumeArm> {
    if agent.parent_agent_id.is_some() || agent.agent_id.is_empty() {
        return None;
    }
    let error = display_turn_error(
        agent.status,
        agent.context.as_ref(),
        agent.last_activity,
        agent.turn_started_at,
    )?;
    match error.class {
        TurnErrorClass::PausedRateLimit => {
            let deadline = agent
                .context
                .as_ref()
                .and_then(|ctx| ctx.rate_limits.as_ref())
                .into_iter()
                .flat_map(|limits| limits.windows.iter())
                .filter(|window| window_spent_unreset(window, now))
                .filter_map(|window| window.resets_at)
                .max()?;
            Some(ResumeArm::RateLimit { deadline })
        }
        TurnErrorClass::PausedOverloaded => Some(ResumeArm::Overloaded {
            overloaded_at: error.at,
        }),
        TurnErrorClass::Failed => None,
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

/// How long after its last compaction-start signal an agent still reads as
/// "compacting". The session's next lifecycle signal clears
/// [`AgentState::compacting_since`], but a crash mid-compact with no next
/// signal would otherwise leave the head pulsing forever, so the projection
/// also expires it past this window. Generous: a large context can take a while
/// to condense.
pub const COMPACTING_WINDOW_SECS: i64 = 90;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentState {
    pub agent_id: AgentSessionId,
    pub kind: AgentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind_ordinal: Option<u32>,
    /// The `[agents.profiles]` profile this agent launched as (`planner`,
    /// `codex-yolo`), stamped by the launch event and carried forward like
    /// `name`. The agent answers to `@<profile>` and renders by it; `None` for
    /// a bare-kind launch. `RIMZ_AGENT_PROFILE` remains the pane's
    /// sender-attribution identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// The `[agents.teams]` role this agent launched as (`planner`, `coder`),
    /// stamped by the launch event and carried forward like `profile`. The
    /// agent answers to `@<role>` when that role uniquely names it in scope.
    /// `RIMZ_AGENT_ROLE` remains the pane's sender-attribution identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// The `[agents.teams]` team this agent launched under, stamped by the
    /// launch event and carried forward like `role`. In-place team launches use
    /// it as the channel suffix when no worktree branch exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// A named cooperation lane, stamped by `RIMZ_CHANNEL` and carried forward
    /// like `team`. When present it is the routing channel ahead of worktree
    /// branch, team, and directory fallback.
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
    #[serde(default)]
    pub agent_pid: Option<u32>,
    #[serde(default)]
    pub agent_process_start: Option<String>,
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
    /// Recent user prompts for this session, newest last, capped by the rollup.
    /// The sidebar row keeps only `prompt`; snapshot JSON exposes the history on
    /// `agents[]` for diagnostics and future panes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_prompts: Vec<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
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

fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

impl AgentState {
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
