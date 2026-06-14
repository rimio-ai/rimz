//! Sidebar row data model: shared row identity plus the kind-specific card
//! payload the renderer paints.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::lifecycle::TurnPhase;
use crate::agents::{AgentContext, AgentTokenUsage};
use crate::feed::{AgentStatus, ContextSeverity, PaneRef, Surface};
use crate::ids::{AgentKind, AgentSessionId, PaneId, RequestId, ResolverId};

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
    /// Renderer-local presentation state: this row changed since this sidebar
    /// view last focused its pane. Producer snapshots leave it false; the
    /// native sidebar stamps it in memory before painting.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unread: bool,
    /// Producer-stamped staleness: this row's latest activity has aged past the
    /// inactive window, sinking it beneath every live row whatever its status.
    /// Renderer-local `unread` still outranks this sink.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub inactive: bool,
    pub last_activity: Timestamp,
    #[serde(flatten)]
    pub card: RowCard,
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

    pub fn status(&self) -> Option<AgentStatus> {
        self.as_agent().and_then(|agent| agent.status)
    }

    pub fn phase(&self) -> TurnPhase {
        self.as_agent().map_or(TurnPhase::Idle, |agent| agent.phase)
    }

    pub fn resolver(&self) -> Option<&SidebarResolverState> {
        self.as_agent().and_then(|agent| agent.resolver.as_ref())
    }

    pub fn request_id(&self) -> Option<&RequestId> {
        self.as_agent().and_then(|agent| agent.request_id.as_ref())
    }

    pub fn surface(&self) -> Option<Surface> {
        self.as_agent().and_then(|agent| agent.surface)
    }

    pub fn task(&self) -> Option<&str> {
        self.as_agent().and_then(|agent| agent.task.as_deref())
    }

    pub fn model(&self) -> Option<&str> {
        self.as_agent().and_then(|agent| agent.model.as_deref())
    }

    pub fn total_tokens(&self) -> Option<u64> {
        self.as_agent().and_then(|agent| agent.total_tokens)
    }

    pub fn context_window(&self) -> Option<u64> {
        self.as_agent().and_then(|agent| agent.context_window)
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
/// CLI and the pane it occupies. Built uncapped at the binding site, so command
/// resolution (`steer`) addresses exactly the live agent panes the producer saw
/// — not the capped, display-shaped [`SidebarRow`]s. A bound session carries its
/// `agent_id`, pet name, and ordinal; a lazy-registering agent before its first
/// turn carries only its kind and pane.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PaneAgent {
    pub kind: AgentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind_ordinal: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The bound session, or `None` for a lazy pane with no session yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<AgentSessionId>,
    pub pane_id: PaneId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_branch: Option<String>,
}

impl PaneAgent {
    /// A human handle: pet name, else `kind-ordinal`, else kind.
    pub fn label(&self) -> String {
        match &self.name {
            Some(name) => name.clone(),
            None => match self.kind_ordinal {
                Some(ordinal) => format!("{}-{}", self.kind, ordinal),
                None => self.kind.to_string(),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "row_kind", rename_all = "snake_case")]
pub enum RowCard {
    Agent(Box<AgentCard>),
    Process(ProcessCard),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentCard {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<AgentStatus>,
    /// The running turn's shape, copied from the rollup: `reasoning` paints the
    /// thinking head, `acting` the working spinner, `parked` the secondary
    /// "background" marker.
    #[serde(default, skip_serializing_if = "turn_phase_is_idle")]
    pub phase: TurnPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<RequestId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<Surface>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// The session's latest user prompt, carried forward from `AgentState`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Context-window % gauge value (0..=100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_pct: Option<u8>,
    /// The model's context window in tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub todo_done: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub todo_total: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<AgentContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_severity: Option<ContextSeverity>,
    /// The session's registration instant, copied from
    /// `AgentState.registered_at`. The live-spend enrichment reads it to date a
    /// session's first cost; row ordering keys on pane creation, not this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registered_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolver: Option<SidebarResolverState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
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
    /// Provider error label projected while a dead turn escalates to failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_error_label: Option<String>,
}

impl AgentCard {
    /// The context gauge's value (0..=100): the statusline's authoritative
    /// `used_percentage` when it is paired with its own window, else the
    /// fold-derived scalar. The pairing matters — the identity line shows
    /// `context_window_size` when present, so trusting a percentage only
    /// alongside that window keeps the bar and the window label on one
    /// denominator. A percentage with no window would otherwise be drawn
    /// against the fold's window (the original mismatch), so it falls through to
    /// `context_pct`, which the fold derived against that same window.
    pub fn context_gauge_percent(&self) -> Option<u8> {
        self.context
            .as_ref()
            .and_then(|context| context.tokens.as_ref())
            .filter(|tokens| tokens.context_window_size.is_some())
            .and_then(|tokens| tokens.used_percentage)
            .or(self.context_pct)
    }

    /// Tokens currently occupying the context window — the current message's
    /// `input + cache_creation + cache_read`, exactly the numerator the gauge
    /// percent scales.
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
        let fresh_input = self.fresh_input_tokens?;
        Some(RowCallSplit {
            cache_read: self.cache_read_input_tokens.unwrap_or(0),
            fresh_input,
            output: self.output_tokens.unwrap_or(0),
        })
    }

    /// Evidence that the session has already done work. A compacted session may
    /// rest at a 0% context gauge, but token, compaction, or spend history keeps
    /// it distinct from a never-started agent.
    pub fn has_session_history(&self) -> bool {
        self.total_tokens.is_some_and(|total| total > 0)
            || self.compaction_count > 0
            || self
                .context
                .as_ref()
                .and_then(|context| context.cost.as_ref())
                .and_then(|cost| cost.total_cost_usd)
                .is_some_and(|cost| cost > 0.0)
    }
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

    pub fn is_stuck(&self) -> bool {
        *self == Self::Stuck
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
    /// Fresh, uncached input (`↘`).
    pub fresh_input: u64,
    /// Output generated (`↗`) — it joins the window next turn.
    pub output: u64,
}

impl RowCallSplit {
    /// The window numerator — everything occupying the window after this call.
    pub fn filled(&self) -> u64 {
        self.cache_read + self.fresh_input
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

/// A compact summary of a child agent, nested under its parent's row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SidebarSubAgent {
    pub id: String,
    /// The subagent's type (`Explore`, `review`, …), from the `SubagentStart`
    /// task descriptor; falls back to a short degraded id when none was
    /// reported.
    pub name: String,
    pub status: AgentStatus,
    /// The running turn's shape (reasoning / acting), the child's own lifecycle
    /// machine output.
    #[serde(default, skip_serializing_if = "turn_phase_is_idle")]
    pub phase: TurnPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_secs: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<Timestamp>,
    pub last_activity: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SidebarResolverState {
    pub resolver_id: ResolverId,
    pub display_name: Option<String>,
    pub budget_until: Option<Timestamp>,
}

#[cfg(test)]
mod tests;
