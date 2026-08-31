//! Agent rollup state, displayed-status projections, and context severity.
//!
//! This is the provider-agnostic model the store reducer writes and the
//! sidebar projects. The rollup itself lives with the agent integration layer.

use std::collections::BTreeMap;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agent_activity::ToolRepeat;
use crate::ids::{AgentKind, AgentSessionId, AskId};
use crate::pane::{PaneRef, RuntimeOwner, RuntimeOwnerKind};

use super::context::{
    AgentContext, AgentTokenUsage, AgentTurnError, TurnErrorClass, TurnSettleOutcome,
};
use super::lifecycle::{AskKind, LifecycleState, TurnPhase};
use super::observation::AgentUsageSummary;

/// Durable identity and summary for the blocking prompt currently owning an
/// agent's input. Structured question detail stays in the transcript and joins
/// by `id`; this projection is the authoritative openness record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenAsk {
    pub id: AskId,
    pub kind: AskKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_key: Option<String>,
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
/// rollup the agent owns and RimZ observes; [`Paused`](AgentStatus::Paused) is
/// the one RimZ-*derived* projection — never emitted by a hook, only projected
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

/// Default window before a `running` agent with no activity is treated as
/// stalled. The per-machine `[agents.attention] stalled_after_secs` setting
/// overrides this for the live sidebar projection.
pub const DEFAULT_STALL_AFTER_SECS: u32 = 30 * 60;

/// Consecutive identical tool calls before the sidebar annotates a card.
pub const DEFAULT_TOOL_REPEAT_WARN_AFTER: u32 = 3;

/// Consecutive identical tool calls before the sidebar routes attention.
pub const DEFAULT_TOOL_REPEAT_ATTENTION_AFTER: u32 = 20;

/// Default silence window credited to a working span before estimated active
/// time pauses. The next progress signal resumes accrual without counting the
/// intervening idle gap.
pub const DEFAULT_ACTIVE_GRACE_SECS: u32 = 3 * 60;

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
/// "waiting for subagents" derivation). A clean turn parked on background work
/// settles to success in the sidebar projection before this predicate becomes
/// relevant.
pub fn is_stalled(
    status: AgentStatus,
    last_activity: Timestamp,
    now: Timestamp,
    stalled_after_secs: u32,
) -> bool {
    status == AgentStatus::Running
        && now.duration_since(last_activity).as_secs() >= i64::from(stalled_after_secs)
}

/// Whether a `running` agent has repeated one identical named tool call enough
/// times to warrant attention. A loop completes tools and refreshes the
/// activity heartbeat, so it cannot trip [`is_stalled`]; the repeat run is the
/// explicit progress-failure certificate. The next differing tool or other
/// progress event clears the run, so the escalation self-heals without human
/// action.
pub fn is_tool_looping(
    status: AgentStatus,
    repeat: Option<&ToolRepeat>,
    attention_after: u32,
) -> bool {
    status == AgentStatus::Running && repeat.is_some_and(|repeat| repeat.count >= attention_after)
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
/// row whose activity moved past it. Like [`is_stalled`], a RimZ-derived
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
/// a provider "session limit" or "spend limit" before RimZ knew that phrase
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

/// How a turn came to rest when the lifecycle state machine never saw its end,
/// or `None` when no marker applies. The provider marker
/// (`AgentContext::settle`, folded in via the context sidecar) must postdate the
/// agent's `last_activity`, and the row's status must admit the outcome: only a
/// `running` row can complete, propose a plan, or open a native wait, because a
/// hook-reported turn end already resolved every other status, while an
/// interruption also settles a `waiting` row whose native ask Esc cancelled.
///
/// Self-clearing: any newer hook event advances `last_activity` past the marker
/// and drops the row back to its lifecycle status. A RimZ-derived projection
/// over enrichment, never a status the agent reports.
pub fn settled_outcome(
    status: AgentStatus,
    context: Option<&AgentContext>,
    last_activity: Timestamp,
) -> Option<TurnSettleOutcome> {
    let settle = context.and_then(|context| context.settle)?;
    if settle.at <= last_activity {
        return None;
    }
    let admitted = match settle.outcome {
        TurnSettleOutcome::Interrupted => {
            matches!(status, AgentStatus::Running | AgentStatus::Waiting)
        }
        TurnSettleOutcome::Complete
        | TurnSettleOutcome::PlanProposed
        | TurnSettleOutcome::NativeWait => status == AgentStatus::Running,
    };
    admitted.then_some(settle.outcome)
}

/// How long after its last compaction-start signal an agent still reads as
/// "compacting". The session's next lifecycle signal clears
/// [`AgentState::compacting_since`], but a crash mid-compact with no next
/// signal would otherwise leave the head pulsing forever, so the projection
/// also expires it past this window. Generous: a large context can take a while
/// to condense.
pub const COMPACTING_WINDOW_SECS: i64 = 90;

/// How many user prompts a session rollup retains, newest last.
const RECENT_PROMPTS_LIMIT: usize = 16;

/// Borrowed identity for one logical agent card.
///
/// A provisional launch and its registered session share a card when they
/// carry the same stable name, while exact session ids keep unnamed sessions
/// distinct.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentCardRef<'a> {
    pub kind: &'a AgentKind,
    pub agent_id: &'a AgentSessionId,
    pub name: Option<&'a str>,
}

impl<'a> AgentCardRef<'a> {
    pub const fn new(
        kind: &'a AgentKind,
        agent_id: &'a AgentSessionId,
        name: Option<&'a str>,
    ) -> Self {
        Self {
            kind,
            agent_id,
            name,
        }
    }

    pub fn matches(self, other: Self) -> bool {
        self.kind == other.kind
            && (self.agent_id == other.agent_id || (self.name.is_some() && self.name == other.name))
    }
}

/// Append one concrete prompt without duplicating a repeated observation.
pub(crate) fn append_recent_prompt(recent_prompts: &mut Vec<String>, prompt: &str) {
    if prompt.is_empty() || recent_prompts.last().is_some_and(|prior| prior == prompt) {
        return;
    }
    recent_prompts.push(prompt.to_owned());
    let excess = recent_prompts.len().saturating_sub(RECENT_PROMPTS_LIMIT);
    if excess > 0 {
        recent_prompts.drain(0..excess);
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "AgentStateWire")]
pub struct AgentState {
    pub agent_id: AgentSessionId,
    /// Stable RimZ launch identity. Unlike `agent_id`, this survives adoption
    /// of the provider's native session id and remains equal to the id exported
    /// to the launched process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_id: Option<AgentSessionId>,
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
    /// The launch profile selected from `[agents.profiles]` or
    /// `[subagents.profiles]`, stamped by the launch event and carried forward
    /// like `name`. The agent answers to `@<profile>` and renders by it; `None`
    /// for a bare-kind launch. `RIMZ_AGENT_PROFILE` remains the pane's
    /// sender-attribution identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// The permission posture selected for this launch, carried forward so an
    /// explicit restart can reproduce it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<crate::agents::PermissionMode>,
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
    /// When this session explicitly ended or the store proved its root process
    /// dead. Runtime views hide stamped rows; audit views retain them so an
    /// explicit resume can recover the provider session within retention.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<Timestamp>,
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
    /// The parent session id set by a provider subagent observation or a
    /// `rimz subagents` launch stamp. The sidebar nests a child under its
    /// parent row and never renders a child as a top-level row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_agent_id: Option<AgentSessionId>,
    /// Provider kind of `parent_agent_id`; absent for legacy and same-kind
    /// provider-native subagent records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_agent_kind: Option<AgentKind>,
    /// Launch generation from the human root. Set for peer-chain agents and
    /// `rimz subagents` children; provider-native subagents stay `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_depth: Option<u8>,
    pub worktree_path: Option<String>,
    pub worktree_branch: Option<String>,
    pub task: Option<String>,
    /// The session's first usable user prompt. Set once and carried for the
    /// whole session so an unnamed card has a stable label across later turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_prompt: Option<String>,
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
    /// The predecessor root condensed into this session, carried forward from
    /// provider compact evidence so the predecessor can be superseded exactly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compacted_from: Option<AgentSessionId>,
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
    /// Durable token and context enrichment. Flattening preserves the persisted
    /// state wire and sidebar JSON keys.
    #[serde(default, flatten)]
    pub usage: AgentUsageSummary,
    /// Rich session-scoped enrichment from a high-frequency out-of-band source
    /// (Claude's statusline). Folded in at snapshot time by
    /// `SidebarSnapshot::with_agent_context`, never reduced from the event log.
    /// Same enrich-only discipline as `context_pct`: display, never routing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<AgentContext>,
    /// Runtime-sidecar projection of this root session's estimated working
    /// time. Rebuilt on every enrichment fold and kept out of the durable
    /// rollup wire.
    #[serde(skip)]
    pub estimated_active_secs: Option<u64>,
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
    /// Exact display-only cost for this subagent, folded from its runtime
    /// context sidecar. Never reduced from the event log or added to parent
    /// spend; `None` for root agents and children without an exact source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_cost_usd: Option<f64>,
    /// When this *subagent* began (its `subagentStatusLine` `startTime`), folded
    /// in alongside `subagent_description`. The card derives elapsed work from it;
    /// `None` for a root agent or before the first render.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_started_at: Option<Timestamp>,
    /// When this agent's current turn began, stamped from the lifecycle state
    /// machine's `opened_turn` fact and from a context reset that rests an existing
    /// session — a manual `/compact` (`CompactionEnded` landing on idle) or a
    /// `/clear` (`Registered`). Each boundary retires the prior turn's children;
    /// otherwise the existing stamp is carried forward. A first-event registration
    /// leaves it `None` until the first turn opens. Automatic compaction *mid-turn*
    /// resumes the same turn and leaves this stamp untouched, so its in-flight
    /// children stay listed. Unlike `last_seen` it does *not* advance on `Stop`, so
    /// it marks the "next prompt" boundary the sidebar uses to clear a finished
    /// subagent: a completed child older than its parent's `turn_started_at`
    /// belongs to a past turn and drops from the parent's expanded list. A prompt
    /// waking a parked running row resumes the same logical turn and carries this
    /// stamp forward.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_started_at: Option<Timestamp>,
    /// Timestamp of the native prompt that put this session in `Waiting`.
    /// Activity after this instant proves a keyless prompt was answered in the
    /// agent's own UI, so read paths project the row back to work even before
    /// the next lifecycle boundary arrives. Keyed prompts use their durable
    /// completion edge instead because parallel sibling tools also touch the
    /// activity heartbeat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_since: Option<Timestamp>,
    /// The prompt associated with `waiting_since`. Old lifecycle records have
    /// no identity and therefore replay with this field absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_ask: Option<OpenAsk>,
    /// Provider turn id most recently canceled for this session. A matching
    /// trailing tool completion is ignored instead of reopening the turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupted_turn_id: Option<String>,
    /// When this agent last began compacting its context window — the timestamp
    /// of its most-recent compaction-start signal (`PreCompact` or Pi
    /// `session_before_compact`). Set by the rollup, cleared by the session's
    /// next lifecycle signal; the sidebar renders a transient "compacting" head
    /// while it is recent (see [`COMPACTING_WINDOW_SECS`]); delivery also
    /// treats a recent marker as busy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compacting_since: Option<Timestamp>,
    /// How many times this session has condensed its context window — the count
    /// of completed compaction brackets. Derived by the rollup from the state
    /// machine's bracket-close fact, carried forward unchanged on every other
    /// event, and rendered by the card as `↻ N` from the first completed
    /// compaction. Display-only.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub compaction_count: u32,
    /// Named tool calls observed for this session.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_calls: BTreeMap<String, u32>,
    /// Open run of consecutive identical tool calls from the activity sidecar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_repeat: Option<ToolRepeat>,
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
    #[serde(default)]
    launch_id: Option<AgentSessionId>,
    kind: AgentKind,
    name: Option<String>,
    #[serde(default)]
    name_explicit: bool,
    kind_ordinal: Option<u32>,
    profile: Option<String>,
    #[serde(default)]
    mode: Option<crate::agents::PermissionMode>,
    role: Option<String>,
    team: Option<String>,
    launch_group: Option<String>,
    launch_ordinal: Option<u32>,
    channel: Option<String>,
    #[serde(default)]
    ended_at: Option<Timestamp>,
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
    #[serde(default)]
    parent_agent_kind: Option<AgentKind>,
    #[serde(default)]
    launch_depth: Option<u8>,
    worktree_path: Option<String>,
    worktree_branch: Option<String>,
    task: Option<String>,
    #[serde(default)]
    first_prompt: Option<String>,
    prompt: Option<String>,
    description: Option<String>,
    transcript_path: Option<String>,
    origin: Option<crate::agents::SessionOrigin>,
    #[serde(default)]
    compacted_from: Option<AgentSessionId>,
    #[serde(default)]
    recent_prompts: Vec<String>,
    model: Option<String>,
    effort: Option<String>,
    #[serde(default)]
    budget: Option<String>,
    #[serde(default, flatten)]
    usage: AgentUsageSummary,
    context: Option<AgentContext>,
    #[serde(default)]
    budget_park: Option<crate::harness::budget::BudgetPark>,
    subagent_description: Option<String>,
    #[serde(default)]
    subagent_cost_usd: Option<f64>,
    subagent_started_at: Option<Timestamp>,
    turn_started_at: Option<Timestamp>,
    #[serde(default)]
    waiting_since: Option<Timestamp>,
    #[serde(default)]
    open_ask: Option<OpenAsk>,
    #[serde(default)]
    interrupted_turn_id: Option<String>,
    compacting_since: Option<Timestamp>,
    #[serde(default)]
    compaction_count: u32,
    #[serde(default)]
    tool_calls: BTreeMap<String, u32>,
    #[serde(default)]
    tool_repeat: Option<ToolRepeat>,
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
            launch_id: wire.launch_id,
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
            ended_at: wire.ended_at,
            status: wire.status,
            phase: wire.phase,
            pane: wire.pane,
            runtime_owner,
            parent_agent_id: wire.parent_agent_id,
            parent_agent_kind: wire.parent_agent_kind,
            launch_depth: wire.launch_depth,
            worktree_path: wire.worktree_path,
            worktree_branch: wire.worktree_branch,
            task: wire.task,
            first_prompt: wire.first_prompt,
            prompt: wire.prompt,
            description: wire.description,
            transcript_path: wire.transcript_path,
            origin: wire.origin,
            compacted_from: wire.compacted_from,
            recent_prompts: wire.recent_prompts,
            model: wire.model,
            effort: wire.effort,
            budget: wire.budget,
            usage: wire.usage,
            context: wire.context,
            estimated_active_secs: None,
            budget_park: wire.budget_park,
            subagent_description: wire.subagent_description,
            subagent_cost_usd: wire.subagent_cost_usd,
            subagent_started_at: wire.subagent_started_at,
            turn_started_at: wire.turn_started_at,
            waiting_since: wire.waiting_since,
            open_ask: wire.open_ask,
            interrupted_turn_id: wire.interrupted_turn_id,
            compacting_since: wire.compacting_since,
            compaction_count: wire.compaction_count,
            tool_calls: wire.tool_calls,
            tool_repeat: wire.tool_repeat,
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
    pub(crate) fn seed(
        kind: AgentKind,
        agent_id: AgentSessionId,
        status: AgentStatus,
        at: Timestamp,
    ) -> Self {
        Self {
            agent_id,
            launch_id: None,
            kind,
            name: None,
            name_explicit: false,
            kind_ordinal: None,
            profile: None,
            mode: None,
            role: None,
            team: None,
            launch_group: None,
            launch_ordinal: None,
            channel: None,
            ended_at: None,
            status,
            phase: match status {
                AgentStatus::Running => TurnPhase::Reasoning,
                _ => TurnPhase::Idle,
            },
            pane: None,
            runtime_owner: None,
            parent_agent_id: None,
            parent_agent_kind: None,
            launch_depth: None,
            worktree_path: None,
            worktree_branch: None,
            task: None,
            first_prompt: None,
            prompt: None,
            description: None,
            transcript_path: None,
            origin: None,
            compacted_from: None,
            recent_prompts: Vec::new(),
            model: None,
            effort: None,
            budget: None,
            usage: AgentUsageSummary::default(),
            context: None,
            estimated_active_secs: None,
            budget_park: None,
            subagent_description: None,
            subagent_cost_usd: None,
            subagent_started_at: None,
            turn_started_at: None,
            waiting_since: None,
            open_ask: None,
            interrupted_turn_id: None,
            compacting_since: None,
            compaction_count: 0,
            tool_calls: BTreeMap::new(),
            tool_repeat: None,
            last_compact_command_tokens: None,
            last_seen: at,
            last_activity: at,
            registered_at: Some(at),
        }
    }

    pub fn card_ref(&self) -> AgentCardRef<'_> {
        AgentCardRef::new(&self.kind, &self.agent_id, self.name.as_deref())
    }

    /// A pane-backed child created through `rimz subagents`.
    pub fn is_launched_child(&self) -> bool {
        self.parent_agent_id.is_some() && self.launch_depth.is_some()
    }

    /// A provider-native, paneless child rather than a full agent session.
    pub fn is_provider_subagent(&self) -> bool {
        self.parent_agent_id.is_some() && self.launch_depth.is_none()
    }

    /// Whether this agent is inside the bounded compaction window.
    pub fn is_compacting(&self, now: Timestamp) -> bool {
        self.compacting_since
            .is_some_and(|since| now.duration_since(since).as_secs() < COMPACTING_WINDOW_SECS)
    }

    /// Minimal test fixture with stable identity fields and empty enrichment.
    #[cfg(any(test, feature = "testkit"))]
    pub fn stub(kind: &str, id: &str, status: AgentStatus) -> Self {
        let now = Timestamp::now();
        let mut state = Self::seed(
            AgentKind::new_unchecked(kind),
            AgentSessionId::from(id),
            status,
            now,
        );
        state.name = Some(format!("{id}-name"));
        state.kind_ordinal = Some(1);
        state
    }

    /// One-line activity label for CLI and sidebar rows: a rich session name
    /// that does not merely prefix the prompt, rich session preview, launch
    /// description, live task, first prompt, then latest prompt.
    pub fn activity_description(&self) -> Option<&str> {
        select_activity_description(
            self.context.as_ref(),
            self.description.as_deref(),
            self.task.as_deref(),
            self.first_prompt.as_deref(),
            self.prompt.as_deref(),
        )
    }

    /// [`Self::activity_description`] collapsed to a single presentable line —
    /// the form every row-oriented surface (CLI tables, key/value reports) renders.
    pub fn activity_line(&self) -> Option<String> {
        self.activity_description()
            .and_then(single_line_description)
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
    /// `paused` even when the lifecycle rollup is still `running`; native-input
    /// markers raise `waiting`, provider completion markers settle falsely-running
    /// rows to `success`, interruption markers settle falsely-running or waiting
    /// rows to `idle`, and a clean turn parked on background work reads as
    /// `success`, which opens message delivery gates. Budget-aware callers may
    /// still upgrade a paused projection to `failed`.
    pub fn effective_status(&self) -> AgentStatus {
        let settled = settled_outcome(self.status, self.context.as_ref(), self.last_activity);
        if settled == Some(TurnSettleOutcome::NativeWait) {
            return AgentStatus::Waiting;
        }
        if self.budget_park.is_some() && self.status != AgentStatus::Waiting {
            return AgentStatus::Paused;
        }
        if self.status == AgentStatus::Waiting && settled == Some(TurnSettleOutcome::Interrupted) {
            return AgentStatus::Idle;
        }
        if self.status != AgentStatus::Running {
            return self.status;
        }
        if let Some((class, _)) = self.displayed_turn_error() {
            return if class.pauses_turn() {
                AgentStatus::Paused
            } else {
                self.status
            };
        }
        match settled {
            Some(TurnSettleOutcome::PlanProposed) => AgentStatus::Waiting,
            Some(TurnSettleOutcome::Complete) => AgentStatus::Success,
            Some(TurnSettleOutcome::Interrupted) => AgentStatus::Idle,
            // A native wait already returned above. A clean turn end parked on
            // still-in-flight background work reads as success: the verdict was
            // earned, only the chore hums on (mirrors the sidebar's parked settle).
            Some(TurnSettleOutcome::NativeWait) | None => {
                if self.phase == TurnPhase::Parked {
                    AgentStatus::Success
                } else {
                    self.status
                }
            }
        }
    }

    /// True when the row must reserve pane input for a native prompt. Durable
    /// `Waiting` uses its ask timestamp; provider-local input and rollout plan
    /// markers cover native dialogs without inventing a durable ask record.
    /// A keyed open ask waits for its durable completion edge because parallel
    /// sibling tools also touch activity. Newer activity self-clears keyless
    /// and derived asks.
    pub fn is_awaiting_input(&self) -> bool {
        matches!(
            settled_outcome(self.status, self.context.as_ref(), self.last_activity),
            Some(TurnSettleOutcome::NativeWait | TurnSettleOutcome::PlanProposed)
        ) || (self.status == AgentStatus::Waiting
            && (self
                .open_ask
                .as_ref()
                .is_some_and(|ask| ask.native_key.is_some())
                || self
                    .waiting_since
                    .is_some_and(|waiting_since| self.last_activity <= waiting_since)))
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
            .or_else(|| self.usage.input_context_tokens())
    }

    /// Tokens occupying the window for a `--smart-compact <tokens>` threshold: the
    /// precise composition when known, else the carried turn total. The gauge's
    /// `context_used_tokens` withholds a bare total so it never legends a partial
    /// composition; a threshold instead scales against the same numerator the
    /// percent gauge derives from, so `--smart-compact 100000` fires for a
    /// transcript-derived session that reports only a running total — matching
    /// `--smart-compact 70%`, which already reads that total through the gauge.
    pub fn occupied_context_tokens(&self) -> Option<u64> {
        self.context_used_tokens().or(self.usage.total_tokens)
    }

    /// The window denominator: the folded statusline's `context_window_size`,
    /// else the adapter-resolved `context_window`, else the model definition's
    /// default.
    pub fn resolved_context_window(&self) -> Option<u64> {
        self.context
            .as_ref()
            .and_then(|context| context.tokens.as_ref())
            .and_then(|tokens| tokens.context_window_size)
            .or(self.usage.context_window)
            .or_else(|| {
                crate::agents::spec_by_kind(self.kind.as_str())
                    .and_then(|definition| definition.default_context_window)
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
                .or(self.usage.context_pct)
                .map(f64::from),
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
    value
        .chars()
        .any(|ch| !ch.is_whitespace() && !ch.is_control())
        && !looks_like_control_text(value)
}

/// Whether a description candidate is a harness-injected control turn rather
/// than human-authored text.
fn looks_like_control_text(value: &str) -> bool {
    let trimmed = value.trim_start();
    crate::agents::CONTROL_TAG_PREFIXES
        .iter()
        .any(|tag| trimmed.starts_with(tag))
}

fn session_name_is_prompt_prefix(value: &str, prompt: Option<&str>) -> bool {
    let Some(value) = single_line_description(value) else {
        return false;
    };
    prompt
        .and_then(single_line_description)
        .is_some_and(|prompt| prompt.starts_with(&value))
}

pub(crate) fn select_activity_description<'a>(
    context: Option<&'a AgentContext>,
    description: Option<&'a str>,
    task: Option<&'a str>,
    first_prompt: Option<&'a str>,
    prompt: Option<&'a str>,
) -> Option<&'a str> {
    context
        .and_then(|context| context.session_name.as_deref())
        .filter(|value| usable_description(value))
        .filter(|value| !session_name_is_prompt_prefix(value, first_prompt.or(prompt)))
        .or_else(|| {
            context
                .and_then(|context| context.session_preview.as_deref())
                .filter(|value| usable_description(value))
        })
        .or_else(|| description.filter(|value| usable_description(value)))
        .or_else(|| task.filter(|value| usable_description(value)))
        .or_else(|| first_prompt.filter(|value| usable_description(value)))
        .or_else(|| prompt.filter(|value| usable_description(value)))
}

#[cfg(test)]
mod tests;
