//! Sidebar row data model: shared row identity plus the kind-specific card
//! payload the renderer paints.

use std::collections::BTreeMap;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::lifecycle::TurnPhase;
use crate::agents::state::select_activity_description;
use crate::agents::{AgentContext, AgentTokenUsage, AgentUsageSummary};
use crate::agents::{AgentStatus, ContextSeverity};
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::pane::PaneRef;

/// One frame-admitted sidebar row. The base names the row and pane; [`RowCard`]
/// carries the fields that make sense for the row kind. Serde flattens the card
/// so JSON keeps the existing flat `row_kind` key and field names.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SidebarRow {
    pub id: String,
    pub name: String,
    pub pane: Option<PaneRef>,
    pub worktree_path: Option<String>,
    pub worktree_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// Shared pending-look state: the row has an open unread episode not reached
    /// by any read receipt. The producer opens/prunes episodes in `unread.json`;
    /// every snapshot fold derives this bit from that file plus read marks.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unread: bool,
    /// Producer-stamped staleness: this row's latest activity has aged past the
    /// inactive window, sinking it beneath every live row whatever its status.
    /// Unread state does not change this sink.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub inactive: bool,
    /// Producer-stamped archive sink: this row has aged past the archive
    /// window, so it parks below hot and warm rows. Process rows are exempt.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archived: bool,
    /// Producer-stamped fixed-point attention score in milli-units for every
    /// presentation sort after the fold.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub attention_score: u32,
    pub last_activity: Timestamp,
    #[serde(flatten)]
    pub card: RowCard,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

impl SidebarRow {
    pub fn is_agent(&self) -> bool {
        matches!(self.card, RowCard::Agent(_))
    }

    pub fn is_process(&self) -> bool {
        matches!(self.card, RowCard::Process(_))
    }

    pub fn as_agent(&self) -> Option<&AgentCard> {
        match &self.card {
            RowCard::Agent(card) => Some(card.as_ref()),
            RowCard::Process(_) => None,
        }
    }

    pub fn as_agent_mut(&mut self) -> Option<&mut AgentCard> {
        match &mut self.card {
            RowCard::Agent(card) => Some(card.as_mut()),
            RowCard::Process(_) => None,
        }
    }

    pub fn as_process(&self) -> Option<&ProcessCard> {
        match &self.card {
            RowCard::Agent(_) => None,
            RowCard::Process(card) => Some(card),
        }
    }

    pub fn as_process_mut(&mut self) -> Option<&mut ProcessCard> {
        match &mut self.card {
            RowCard::Agent(_) => None,
            RowCard::Process(card) => Some(card),
        }
    }

    pub fn launch_cohort(&self) -> Option<&str> {
        let agent = self.as_agent()?;
        agent.team.as_deref().or(agent.launch_group.as_deref())
    }

    pub fn team(&self) -> Option<&str> {
        self.as_agent()?.team.as_deref()
    }

    pub fn launch_ordinal(&self) -> Option<u32> {
        self.as_agent().and_then(|agent| agent.launch_ordinal)
    }

    /// The name to display on the card and in notifications: the agent's handle
    /// (role, explicit name, or profile) when set, else `name` — the kind for
    /// an agent row, the command for a process row.
    pub fn display_name(&self) -> &str {
        self.as_agent()
            .and_then(|card| card.handle.as_deref())
            .unwrap_or(&self.name)
    }

    pub fn status(&self) -> Option<AgentStatus> {
        self.as_agent().map(|agent| agent.status)
    }

    /// Status that determines row-level attention. A provider child has no pane
    /// of its own, so its actionable state lifts the parent card without
    /// changing the parent's displayed lifecycle status.
    pub fn attention_status(&self) -> Option<AgentStatus> {
        let status = self.status()?;
        if status.is_actionable() {
            return Some(status);
        }
        let children = self.sub_agents();
        if children
            .iter()
            .any(|child| child.provider_native && child.status == AgentStatus::Waiting)
        {
            return Some(AgentStatus::Waiting);
        }
        if children
            .iter()
            .any(|child| child.provider_native && child.status == AgentStatus::Failed)
        {
            return Some(AgentStatus::Failed);
        }
        Some(status)
    }

    pub fn phase(&self) -> TurnPhase {
        self.as_agent().map_or(TurnPhase::Idle, |agent| agent.phase)
    }

    pub fn task(&self) -> Option<&str> {
        self.as_agent().and_then(|agent| agent.task.as_deref())
    }

    pub fn model(&self) -> Option<&str> {
        self.as_agent().and_then(|agent| agent.model.as_deref())
    }

    pub fn total_tokens(&self) -> Option<u64> {
        self.as_agent().and_then(|agent| agent.usage.total_tokens)
    }

    pub fn context_window(&self) -> Option<u64> {
        self.as_agent().and_then(|agent| agent.usage.context_window)
    }

    pub fn turn_error_label(&self) -> Option<&str> {
        self.as_agent()
            .and_then(|agent| agent.turn_error_label.as_deref())
    }

    pub fn compacting(&self) -> bool {
        self.as_agent().is_some_and(|agent| agent.compacting)
    }

    pub fn sub_agents(&self) -> &[SidebarSubAgent] {
        self.as_agent()
            .map_or(&[], |agent| agent.sub_agents.as_slice())
    }

    pub fn process_state(&self) -> Option<ProcessState> {
        self.as_process().map(|process| process.state)
    }

    pub fn process_is_busy(&self) -> bool {
        self.as_process()
            .is_some_and(|process| process.state == ProcessState::Busy)
    }

    /// The context gauge's value (0..=100): the statusline's authoritative
    /// `used_percentage` when present, else the transcript-derived scalar.
    pub fn context_gauge_percent(&self) -> Option<u8> {
        self.as_agent().and_then(AgentCard::context_gauge_percent)
    }

    /// Tokens currently occupying the context window.
    pub fn context_used_tokens(&self) -> Option<u64> {
        self.as_agent().and_then(AgentCard::context_used_tokens)
    }

    /// The latest call's composition when the rich token blob is absent.
    pub fn call_split(&self) -> Option<RowCallSplit> {
        self.as_agent().and_then(AgentCard::call_split)
    }
}

/// A live agent pane the producer bound during the pane fold: a running agent
/// CLI and the pane it occupies. Built at the binding site, so command
/// resolution (`message --steer`) addresses exactly the live agent panes the
/// producer saw. A bound session carries its `agent_id`, pet name, and ordinal;
/// a wired pane before its session binds carries only its kind and pane.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PaneAgent {
    pub kind: AgentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind_ordinal: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Whether `name` is a user-chosen launch handle rather than a minted
    /// petname. Bound panes use it to keep delivery resolution aligned with
    /// rendered handles.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub name_explicit: bool,
    /// The `[agents.profiles]` profile the bound session launched as, copied from the
    /// rollup so a bound agent answers to `@<profile>` through its pane. `None`
    /// for a sessionless pane or a bare-kind launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// The `[agents.teams]` role the bound session launched as, copied from the
    /// rollup so a bound agent answers to `@<role>` through its pane. `None`
    /// for a sessionless pane or a roleless launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Routing lane copied from the bound session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// The bound session, or `None` for a wired pane with no session yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<AgentSessionId>,
    pub pane_id: PaneId,
    /// Live pane root process id from the producer's pane frame, when the
    /// backend probe reported one. Best-effort advisory metadata for
    /// process-tree metrics; never a correctness signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_branch: Option<String>,
}

impl PaneAgent {
    /// A human handle: role, explicit name, profile, pet name, `kind-ordinal`,
    /// else kind.
    pub fn label(&self) -> String {
        if let Some(role) = self.role.as_deref().filter(|role| !role.is_empty()) {
            return role.to_owned();
        }
        if self.name_explicit
            && let Some(name) = self.name.as_deref().filter(|name| !name.is_empty())
        {
            return name.to_owned();
        }
        if let Some(profile) = self
            .profile
            .as_deref()
            .filter(|profile| !profile.is_empty())
        {
            return profile.to_owned();
        }
        if let Some(name) = self.name.as_deref().filter(|name| !name.is_empty()) {
            return name.to_owned();
        }
        match self.kind_ordinal {
            Some(ordinal) => format!("{}-{}", self.kind, ordinal),
            None => self.kind.to_string(),
        }
    }

    /// The channel this pane participates in: stamped lane, else worktree
    /// directory basename. `None` means the pane is outside a known worktree.
    pub fn channel(&self) -> Option<String> {
        compose_channel(
            self.channel.as_deref(),
            self.worktree_path
                .as_deref()
                .and_then(|path| path.rsplit('/').next()),
        )
    }
}

/// Compose a routing channel for read-side fallback. A launch-stamped lane
/// wins; otherwise the worktree directory basename is the fallback for agents
/// not launched by this RimZ binary.
pub fn compose_channel(explicit: Option<&str>, dir_basename: Option<&str>) -> Option<String> {
    if let Some(channel) = explicit.filter(|channel| !channel.is_empty()) {
        return Some(channel.to_owned());
    }
    dir_basename
        .filter(|dir| !dir.is_empty())
        .map(ToOwned::to_owned)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "row_kind", rename_all = "snake_case")]
pub enum RowCard {
    Agent(Box<AgentCard>),
    Process(ProcessCard),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentCard {
    pub status: AgentStatus,
    /// The running turn's shape, copied from the rollup: `reasoning` paints the
    /// thinking head, `acting` the working spinner, `parked` the secondary
    /// "background" marker.
    #[serde(default, skip_serializing_if = "turn_phase_is_idle")]
    pub phase: TurnPhase,
    pub task: Option<String>,
    /// The session's first usable user prompt, copied from `AgentState` as the
    /// stable unnamed-session label ahead of the latest prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_prompt: Option<String>,
    /// The session's latest user prompt, carried forward from `AgentState`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Launch-seeded card label, shown until richer session naming arrives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// The agent's display handle — the team role it launched as (`planner`,
    /// `coder`), else its explicit name, else its profile, copied from the
    /// rollup. The card and notification labels render this in the provider
    /// brand color instead of the bare kind; `None` (a bare-kind launch) falls
    /// back to the kind. The kind stays on `SidebarRow::name` for brand lookup,
    /// spend attribution, and subagent nesting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    /// The launch team copied from the rollup. Row sorting treats it as the
    /// cohort key ahead of `launch_group`; a finished pod receipt leads with
    /// the name when every stamped member shares it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// Inline multi-agent launch cohort copied from the rollup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_group: Option<String>,
    /// Stable order inside the launch cohort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_ordinal: Option<u32>,
    #[serde(default, flatten)]
    pub usage: AgentUsageSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<AgentContext>,
    /// Cost of every pane-backed child this session launched, across all turns.
    /// The turn-scoped `sub_agents` list can be shorter than this lifetime sum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_severity: Option<ContextSeverity>,
    /// Estimated working time for this root session, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_active_secs: Option<u64>,
    /// The session's registration instant, copied from
    /// `AgentState.registered_at`. The live-spend enrichment reads it to date a
    /// session's first cost; row ordering keys on pane creation, not this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registered_at: Option<Timestamp>,
    /// Subagents this agent spawned this turn, nested under the parent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sub_agents: Vec<SidebarSubAgent>,
    /// The agent is condensing its context window right now.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub compacting: bool,
    /// Lifetime count of completed context compactions, copied from the rollup;
    /// the context line renders `↻ N` from the first completed compaction.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub compaction_count: u32,
    /// Named tool calls observed for this session.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_calls: BTreeMap<String, u32>,
    /// Open run of consecutive identical tool calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_repeat: Option<crate::agent_activity::ToolRepeat>,
    /// Label explaining why the row projected to failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_error_label: Option<String>,
}

impl Default for AgentCard {
    fn default() -> Self {
        Self {
            status: AgentStatus::Idle,
            phase: TurnPhase::Idle,
            task: None,
            first_prompt: None,
            prompt: None,
            description: None,
            model: None,
            effort: None,
            handle: None,
            team: None,
            launch_group: None,
            launch_ordinal: None,
            usage: AgentUsageSummary::default(),
            context: None,
            delegated_cost_usd: None,
            context_severity: None,
            estimated_active_secs: None,
            registered_at: None,
            sub_agents: Vec::new(),
            compacting: false,
            compaction_count: 0,
            tool_calls: BTreeMap::new(),
            tool_repeat: None,
            turn_error_label: None,
        }
    }
}

impl AgentCard {
    /// Session cost plus the cost of pane-backed children this session launched.
    pub fn cost_usd(&self) -> Option<f64> {
        crate::agents::spending::sum_optional_cost(
            self.context
                .as_ref()
                .and_then(|context| context.cost.as_ref())
                .and_then(|cost| cost.total_cost_usd),
            self.delegated_cost_usd,
        )
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

    /// The context gauge's value (0..=100): the statusline's authoritative
    /// `used_percentage` when it is paired with its own window, else the
    /// statusline's token composition over that same window, else the
    /// fold-derived scalar. The pairing matters — the identity line shows
    /// `context_window_size` when present, so trusting a percentage only
    /// alongside that window keeps the bar and the window label on one
    /// denominator. A nonzero rich composition with no window is explicitly
    /// unknown rather than falling through to a synthetic lifecycle 0%; other
    /// untethered context falls through to the fold-derived `context_pct`.
    pub fn context_gauge_percent(&self) -> Option<u8> {
        let context_tokens = self
            .context
            .as_ref()
            .and_then(|context| context.tokens.as_ref());
        if context_tokens.is_some_and(|tokens| {
            tokens.context_window_size.is_none()
                && tokens.used_tokens().is_some_and(|used| used > 0)
        }) {
            return None;
        }
        let sidecar_tokens = context_tokens.filter(|tokens| tokens.context_window_size.is_some());

        sidecar_tokens
            .and_then(|tokens| tokens.used_percentage)
            .or_else(|| {
                let tokens = sidecar_tokens?;
                derive_percent(tokens.used_tokens()?, tokens.context_window_size?)
            })
            .or(self.usage.context_pct)
    }

    /// Tokens currently occupying the context window — the authoritative live
    /// scalar when present, else the current message's categorized input.
    pub fn context_used_tokens(&self) -> Option<u64> {
        self.context
            .as_ref()
            .and_then(|context| context.tokens.as_ref())
            .and_then(AgentTokenUsage::used_tokens)
            .or_else(|| self.call_split().map(|split| split.filled()))
    }

    /// The latest call's composition when the rich `context.tokens.
    /// current_usage` blob is absent.
    pub fn call_split(&self) -> Option<RowCallSplit> {
        let fresh_input = self.usage.fresh_input_tokens?;
        let split = RowCallSplit {
            cache_read: self.usage.cache_read_input_tokens.unwrap_or(0),
            cache_write: self.usage.cache_write_input_tokens.unwrap_or(0),
            fresh_input,
            output: self.usage.output_tokens.unwrap_or(0),
        };
        let authoritative = self
            .context
            .as_ref()
            .and_then(|context| context.tokens.as_ref())
            .and_then(|tokens| tokens.current_context_tokens);
        (authoritative.is_none() || authoritative == Some(split.filled())).then_some(split)
    }

    /// Evidence that the session has already done work. A compacted session may
    /// rest at a 0% context gauge, but token, tool, compaction, or spend history
    /// keeps it distinct from a never-started agent.
    pub fn has_session_history(&self) -> bool {
        self.usage.total_tokens.is_some_and(|total| total > 0)
            || self.compaction_count > 0
            || !self.tool_calls.is_empty()
            || self
                .context
                .as_ref()
                .and_then(|context| context.tokens.as_ref())
                .and_then(AgentTokenUsage::used_tokens)
                .is_some_and(|tokens| tokens > 0)
            || self
                .context
                .as_ref()
                .and_then(|context| context.tokens.as_ref())
                .and_then(|tokens| tokens.session_usage.as_ref())
                .is_some_and(|usage| !usage.is_zero())
            || self
                .context
                .as_ref()
                .and_then(|context| context.cost.as_ref())
                .and_then(|cost| cost.total_cost_usd)
                .is_some_and(|cost| cost > 0.0)
            || self.delegated_cost_usd.is_some_and(|cost| cost > 0.0)
    }
}

pub(crate) fn derive_percent(used: u64, window: u64) -> Option<u8> {
    (window > 0).then(|| (used.saturating_mul(100) / window).min(100) as u8)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    #[default]
    Idle,
    Busy,
    Stuck,
}

impl ProcessState {
    pub fn is_idle(&self) -> bool {
        *self == Self::Idle
    }

    pub fn is_busy(&self) -> bool {
        *self == Self::Busy
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProcessCard {
    #[serde(default, skip_serializing_if = "ProcessState::is_idle")]
    pub state: ProcessState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreign_user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rss_kb: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_pct: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub io_bps: Option<u64>,
}

/// A row-level per-call token composition — the fallback the renderer legends
/// when no rich per-call blob exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowCallSplit {
    /// Tokens read back from cache (`◌`).
    pub cache_read: u64,
    /// Tokens newly written into cache (`◍`).
    pub cache_write: u64,
    /// Fresh, uncached input (`↘`).
    pub fresh_input: u64,
    /// Output generated (`↗`) — it joins the window next turn.
    pub output: u64,
}

impl RowCallSplit {
    /// The window numerator — everything occupying the window after this call.
    pub fn filled(&self) -> u64 {
        self.cache_read + self.cache_write + self.fresh_input
    }
}

/// `skip_serializing_if` helper: the resting phase is the default and stays off
/// the wire.
pub(crate) fn turn_phase_is_idle(phase: &TurnPhase) -> bool {
    *phase == TurnPhase::Idle
}

fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// A compact summary of a child agent, nested under its parent's row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SidebarSubAgent {
    pub id: String,
    /// The subagent's type (`Explore`, `review`, …), from the `SubagentStart`
    /// task definition; falls back to a short degraded id when none was
    /// reported.
    pub name: String,
    /// Whether this is a provider-native paneless child. The neutral marker
    /// survives publication so consumers can lift the child's attention state.
    #[serde(default, skip_serializing_if = "is_false")]
    pub provider_native: bool,
    pub status: AgentStatus,
    /// The running turn's shape (reasoning / acting), the child's own lifecycle
    /// machine output.
    #[serde(default, skip_serializing_if = "turn_phase_is_idle")]
    pub phase: TurnPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// The `[subagents.profiles]` profile a pane-backed child launched as;
    /// absent for provider-native children and bare-kind launches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    /// Exact cumulative cost from a provider-native child feed, or from a
    /// pane-backed child's own session sidecar. Provider-native cost is already
    /// included in the parent transcript; pane-backed cost is added separately.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_secs: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<Timestamp>,
    pub last_activity: Timestamp,
    /// The child's registration instant, copied from `AgentState.registered_at`.
    /// This is the spawn-order sort key: present from first observation and
    /// stable across refreshes, unlike the enrichment-fed `started_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registered_at: Option<Timestamp>,
}

#[cfg(test)]
mod tests;
