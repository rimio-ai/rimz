//! Sidebar view-model assembly: the `Sidebar*` renderer contract and the
//! grouping, ranking, capping, and status projection that fills it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use tracing::debug;

use super::fold::ResumeOutcome;
#[cfg(test)]
use super::fold::agent_rollup_with_carryover;
use super::panes::SidebarOwnView;
use super::row::{PaneAgent, SidebarRow};
use crate::agents::AgentState;
use crate::agents::SpendTally;
use crate::ids::{AgentKind, AgentSessionId, PaneId, WorkspaceId};
use crate::store::agent_context::AgentContextRecord;
#[cfg(test)]
use crate::store::event::EventEnvelope;
use crate::store::event_log::{self};
use crate::store::subagent_context::SubagentContextRecord;
use crate::workspace::RootClass;

mod aggregate;
mod layout;
mod live;
mod model;
mod providers;
mod reap;
mod rows;
mod score;

pub(crate) use providers::{format_plan_label, sort_windows};

pub use layout::{AgentWorktreeGroup, group_live_agents_by_worktree};
use model::cohort_team;
pub use model::{
    DailyBudgetView, PresenceSample, RemoteControlBadge, SidebarCohortEffort, SidebarLinkFreshness,
    SidebarLinkHealth, SidebarPresence, SidebarProviderPanel, SidebarSeatEffort,
    SidebarStatusCount, SidebarWorktreeGroup, SidebarWorktreeKind, WorktreePrCi, WorktreePrState,
    WorktreeTrunkSync, actionable_unread_count, lead_unread_row, triage_key,
};
pub use reap::RuntimeReapInputs;

#[cfg(test)]
pub(super) use aggregate::{attach_sub_agents, sub_agent_from_state};
#[cfg(test)]
pub(crate) use live::row_identity_violations;
#[cfg(test)]
pub(super) use rows::row_from_agent;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TruthNotice {
    pub carried: usize,
    pub since_ms: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pane_ids: Vec<PaneId>,
}

/// Serde default for [`SidebarSnapshot::root_class`]: `Repo` keeps a pre-class
/// snapshot (and the pure reducer path) on the prior repo-room grouping.
fn default_root_class() -> RootClass {
    RootClass::Repo
}

/// Bump when [`SidebarSnapshot`]'s persisted shape changes; old
/// `latest.json` files read as stale instead of accreting one-off guards.
pub const SNAPSHOT_VERSION: u32 = 16;

/// Sidebar view-model. The pane frame admits every rendered card; store,
/// sidecars, and realtime events only enrich rows admitted from live panes.
/// Worktree groups are the renderer contract: grouping, attention ranking,
/// caps, status tallies, and row metadata are resolved here so renderers only
/// paint semantics into glyphs.
///
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SidebarSnapshot {
    #[serde(default)]
    pub snapshot_version: u32,
    pub workspace_id: WorkspaceId,
    pub display_name: String,
    pub generated_at: Timestamp,
    /// Producer timestamp of the pane frame folded into this snapshot. Realtime
    /// events older than this baseline are superseded by pulled truth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panes_produced_at_ms: Option<u64>,
    /// Pane-source observation timestamp folded into this snapshot. When a
    /// fold is frameless, fusion falls back to `panes_produced_at_ms`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panes_observed_at_ms: Option<u64>,
    /// Panes attached clients are currently viewing (global focus, one per
    /// client), folded from the pane frame. Drives the focused-worktree fast
    /// tick; the pure reducer leaves it empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub viewed_panes: Vec<PaneId>,
    /// Full native attached-client observations for focus action fencing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub client_views: Vec<crate::mux::ClientPaneView>,
    /// Mux session that produced the pane/client frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_session_name: Option<String>,
    /// Session-global latest focused pane, folded from the pane frame and
    /// latency-updated by single-pane focus events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_pane: Option<PaneId>,
    /// Whether the user is currently present in this mux session. The producer
    /// fills it from the same per-client mux sample that populates
    /// `viewed_panes`; the pure reducer leaves it `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<SidebarPresence>,
    /// The pane frame is painting from carried prior-pane truth because the
    /// latest mux pane source omitted panes whose processes are still alive.
    /// Display-only and renderer-local: the store state stays unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truth_degraded: Option<TruthNotice>,
    /// The single instant every time-window verdict in this projection reads —
    /// the compaction head, the stall escalation, rate-limit resets, and
    /// subagent retention all agree on one clock, captured once at
    /// construction. Never serialized: a reader re-stamps it at its own read
    /// instant (`read_fresh_latest`), so a deserialized snapshot's enrichment
    /// rebuilds against the read, not the long-gone produce.
    #[serde(skip, default = "Timestamp::now")]
    pub now: Timestamp,
    pub worktree_groups: Vec<SidebarWorktreeGroup>,
    pub agents: Vec<AgentState>,
    /// The agent kinds with an active observation path: installed hooks or
    /// declared local-session discovery. Gates the idle synthesis in
    /// `rows_from_panes`: a launched-but-unbound pane for an observable agent
    /// has a row RimZ can later enrich, while a pane with no active integration
    /// stays a process row. Cwd binding for an existing paneless session is
    /// separate pairing logic and does not read this idle-synthesis set.
    /// Environment, not store — the pure reducer leaves it empty; the `rimz sidebar snapshot` CLI and consumer enrichment fill it before folding live panes.
    /// The placeholder/persisted snapshot keeps it empty (a process row).
    #[serde(default)]
    pub wired_kinds: Vec<String>,
    /// Per-kind launch model defaults for idle synthesized agent rows, filled
    /// from adapter-owned config reads before the live-pane fold. The pure
    /// reducer leaves it empty and falls back to definition defaults.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub wired_default_models: BTreeMap<String, String>,
    /// Every live agent pane the producer bound during the pane fold, built at
    /// the binding site — the authoritative source for command resolution
    /// (`message --steer`), so a target reaches exactly the agent panes the
    /// producer saw. Holds bound sessions (with their pane, even when the session's own
    /// `agent_id` carries no stamped pane) and wired panes with no session yet.
    /// Frame-derived: the pure rollup leaves it empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_panes: Vec<PaneAgent>,
    /// The calling sidebar's own-view summary: how many sibling panes share its
    /// tab/window and which non-sidebar siblings are working panes. The renderer's
    /// self-close, notification targeting, and stranded-focus repair read it
    /// instead of spawning a second `pane list` per tick. Computed by the `rimz sidebar
    /// snapshot` CLI from the live pane list when `--exclude-pane-id` names the
    /// caller's pane; the pure reducer and the placeholder/persisted snapshot
    /// leave it `None` (meaning "unknown" — the renderer never self-closes on a
    /// `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub own_view: Option<SidebarOwnView>,
    /// True iff every live, non-sidebar view in the session is the `rimzd`
    /// daemon view — the user has closed every working tab and only the managed
    /// daemon tab remains. Like `own_view`, this is live-pane state the pure
    /// reducer can't read, so the reducer and the placeholder/persisted snapshot
    /// leave it `false`; the producer fills it from the live pane list.
    #[serde(default)]
    pub only_daemon_view_remains: bool,
    /// The project's canonical root. Grouping uses it to tell a project
    /// worktree (the main checkout, or `<root>/.claude/worktrees/*`) from a
    /// pane whose cwd sits outside the project entirely (a home shell, `/tmp`),
    /// which folds into the `external` catch-all instead of minting its own
    /// pod. Like `display_name`, this is workspace identity the reducer can't
    /// read from the store, so the pure path leaves it `None` (every cwd keeps
    /// per-path grouping) and the `rimz sidebar snapshot` CLI fills it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root: Option<PathBuf>,
    /// The repo's enumerated worktree checkout roots, as `git worktree list`
    /// reports them. A pane whose cwd is inside any of these is one of the
    /// project's worktrees and earns its own pod — *including a worktree parked
    /// outside `project_root`*, which the `project_root` prefix test alone would
    /// miss. Git-backed rows also contribute their own resolved worktree root
    /// during grouping, so directory rooms do not scan children. Like
    /// `project_root`, this is workspace identity the reducer can't read from
    /// the store, so the pure path leaves it empty (row-derived roots and the
    /// `project_root` prefix test then stand alone) and the `rimz sidebar
    /// snapshot` CLI fills it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub worktree_roots: Vec<PathBuf>,
    /// The repo's durable worktree-home directory — the resolved `[agents.worktree]
    /// dir` template (default `…/<repo>-worktrees`). It widens the cockpit
    /// spend scope: a session recorded under a worktree that cleanup has since
    /// removed still counts toward the room's headline figure, because the home
    /// is a stable path prefix while `worktree_roots` tracks only the live `git
    /// worktree list`. It also folds unstamped rows inside RimZ-owned worktrees
    /// into their `#channel` pod so grouping agrees with message addressing.
    /// Like `project_root`, the `rimz sidebar snapshot` enrichment fills it from
    /// `MachineConfig`; the pure path leaves it `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_home: Option<PathBuf>,
    /// The room root's class from the workspace record. Grouping reads it to
    /// give a non-repo room's root pod the name-only [`SidebarWorktreeKind::Root`]
    /// kind while a repo room's own checkout keeps per-path pods. Like
    /// `project_root`, workspace identity the reducer can't read from the
    /// store: the pure path and any pre-class snapshot default to `Repo`
    /// (the prior behavior) and the producing reads fill it from the record.
    #[serde(default = "default_root_class")]
    pub root_class: RootClass,
    /// Per-machine sidebar behavior preferences. Like `project_root`, this is
    /// machine state the pure reducer can't read, so the reducer leaves it
    /// default and the `rimz sidebar snapshot` CLI fills it from `MachineConfig`.
    #[serde(default)]
    pub sidebar: crate::config::SidebarConfig,
    /// Per-machine appearance preferences: palette, glyphs, providers, and
    /// animations. Filled beside [`Self::sidebar`] from `MachineConfig`.
    #[serde(default)]
    pub theme: crate::config::ThemeConfig,
    /// Per-machine attention timing preferences.
    #[serde(default)]
    pub attention: crate::config::AttentionConfig,
    /// Per-provider dashboard blocks pinned to the bottom of the sidebar — the
    /// account-scoped budgets, aggregate spend/tokens, and brand emblem.
    /// One block folds every session of a kind. Built by
    /// [`Self::with_provider_aggregates`] on the producer (it needs config and an
    /// account probe the pure reducer can't read), so the placeholder/persisted
    /// snapshot leaves it empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<SidebarProviderPanel>,
    /// Account-global JSONL-computed spend and token tally — configured
    /// headline / week / month / trailing-year — summing every provider across
    /// every workspace.
    /// Attached by the sidebar enrichment spine
    /// (`sidebar::enrich::enrich`) from the producer's fleet spending walk
    /// (`sidebar::produce`, via [`crate::agents::spending::SpendingWalker`]);
    /// `None` until the cache is seeded (the first producer tick after startup)
    /// or when nothing has been recorded. The fleet store reads the trailing
    /// `week` and `month` rows; provider dashboard panels read the per-provider
    /// entries produced alongside this total.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_tally: Option<SpendTally>,
    /// Workspace-scoped spend and token tally for the cockpit, limited to the
    /// room's project root plus grouped worktree roots. Unknown-origin
    /// transcript entries are omitted. This is cached under the workspace
    /// runtime root and folded beside [`Self::value_tally`] so the provider
    /// dashboard and fleet store stay account-global.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_value_tally: Option<SpendTally>,
    /// The cockpit's live headline spend: walked workspace headline USD with
    /// active live-card sessions excluded, plus those cards' current costs.
    /// This keeps the headline aligned with visible cards while the W/M store
    /// rows keep reading the exact walked record.
    /// Stamped where the spending cache folds onto the snapshot — the
    /// producing CLI and the consumer fold alike; `None` on the pure-reducer
    /// path and any pre-overlay snapshot, where the cockpit falls back to the
    /// tally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub today_spend_live_usd: Option<f64>,
    /// Headline-window epoch for [`Self::today_spend_live_usd`]. The renderer's
    /// within-session ratchet resets when this cutoff changes. `None` on older
    /// producer frames leaves the ratchet inert.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub today_spend_epoch_secs: Option<u64>,
    /// Room-local calendar-day spend, independent of the configured headline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fleet_day_spend_usd: Option<f64>,
    /// Local-day epoch for [`Self::fleet_day_spend_usd`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fleet_day_spend_epoch_secs: Option<u64>,
    /// Effective room-fleet local-day cap and current enforcement state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fleet_budget: Option<DailyBudgetView>,
    /// Remote SSH link health published by `rimz remote connect` through the
    /// remote-side `link-stats.json` sidecar. Local rooms and old remotes carry
    /// `None`, keeping the footer byte-identical to the pre-link-health render.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<SidebarLinkHealth>,
    /// The active event-log extent this rollup reflects — the freshness stamp
    /// `read_fresh_latest` compares against the live log. Stamped by
    /// `build_from` under the producing fold; `None` on the pure-reducer
    /// path, the renderer placeholder, and any pre-stamp snapshot, all of
    /// which read as stale so a fresh fold replaces them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reflects_log: Option<event_log::LogExtent>,
    /// Sessions fenced from local-session fresh-fallback binding: durably
    /// ended (`ended_at`, including every reap variant and graceful end) or
    /// expelled from the runtime projection for a known-dead owner process.
    /// Stamped by `assemble_snapshot`; the pure reducer path leaves it empty.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub fenced_sessions: BTreeSet<(AgentKind, AgentSessionId)>,
    /// Latest terminal resume-gated prompt outcome per folded agent card.
    /// `None` means a pre-outcome snapshot and forces a fresh fold before the
    /// auto-continue producer reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_outcomes: Option<Vec<ResumeOutcome>>,
}

impl SidebarSnapshot {
    pub(crate) fn live_agent_pane(
        &self,
        kind: &AgentKind,
        agent_id: &AgentSessionId,
    ) -> Option<PaneId> {
        self.agent_panes
            .iter()
            .find(|pane| &pane.kind == kind && pane.agent_id.as_ref() == Some(agent_id))
            .map(|pane| pane.pane_id.clone())
    }

    #[cfg(test)]
    pub fn build(workspace_id: WorkspaceId, events: Vec<EventEnvelope>, now: Timestamp) -> Self {
        Self::build_with_carryover(workspace_id, events, Vec::new(), now)
    }

    /// Build a snapshot, folding `carryover_agents` into the agent rollup so
    /// pre-rotation observations survive event-log archiving. Live events
    /// with a newer `last_seen` override the carryover.
    #[cfg(test)]
    pub fn build_with_carryover(
        workspace_id: WorkspaceId,
        events: Vec<EventEnvelope>,
        carryover_agents: Vec<AgentState>,
        now: Timestamp,
    ) -> Self {
        let agents = agent_rollup_with_carryover(&events, carryover_agents);
        Self::build_with_agents(workspace_id, agents, now)
    }

    pub fn build_with_agents(
        workspace_id: WorkspaceId,
        agents: Vec<AgentState>,
        now: Timestamp,
    ) -> Self {
        let display_name = workspace_id.as_str().to_owned();
        Self {
            snapshot_version: SNAPSHOT_VERSION,
            workspace_id,
            display_name,
            generated_at: now,
            panes_produced_at_ms: None,
            panes_observed_at_ms: None,
            viewed_panes: Vec::new(),
            client_views: Vec::new(),
            pane_session_name: None,
            focused_pane: None,
            presence: None,
            truth_degraded: None,
            now,
            worktree_groups: Vec::new(),
            agents,
            wired_kinds: Vec::new(),
            wired_default_models: BTreeMap::new(),
            agent_panes: Vec::new(),
            own_view: None,
            only_daemon_view_remains: false,
            project_root: None,
            worktree_roots: Vec::new(),
            worktree_home: None,
            root_class: default_root_class(),
            sidebar: crate::config::SidebarConfig::default(),
            theme: crate::config::ThemeConfig::default(),
            attention: crate::config::AttentionConfig::default(),
            providers: Vec::new(),
            value_tally: None,
            workspace_value_tally: None,
            today_spend_live_usd: None,
            today_spend_epoch_secs: None,
            fleet_day_spend_usd: None,
            fleet_day_spend_epoch_secs: None,
            fleet_budget: None,
            link: None,
            reflects_log: None,
            fenced_sessions: BTreeSet::new(),
            resume_outcomes: Some(Vec::new()),
        }
    }

    /// Top-level agent sessions in this snapshot; children stay attached to
    /// their parent card rather than producing their own row.
    pub fn root_agents(&self) -> impl Iterator<Item = &AgentState> {
        self.agents
            .iter()
            .filter(|agent| agent.parent_agent_id.is_none())
    }

    /// Top-level agent sessions bound to one of this frame's live agent panes.
    /// Historical rollup roots stay out of peer-set handle disambiguation.
    /// Only meaningful after [`Self::with_live_panes`] populates `agent_panes`.
    pub fn pane_bound_roots(&self) -> impl Iterator<Item = &AgentState> {
        self.root_agents().filter(|agent| {
            self.agent_panes
                .iter()
                .any(|pane| pane.agent_id.as_ref() == Some(&agent.agent_id))
        })
    }

    /// Every row across every worktree group, in group order.
    pub fn rows(&self) -> impl Iterator<Item = &SidebarRow> {
        self.worktree_groups
            .iter()
            .flat_map(|group| group.rows.iter())
    }

    /// Every mutable row across every worktree group, in group order.
    pub fn rows_mut(&mut self) -> impl Iterator<Item = &mut SidebarRow> {
        self.worktree_groups
            .iter_mut()
            .flat_map(|group| group.rows.iter_mut())
    }

    /// Record the project root so a frame-admitted row whose cwd is neither
    /// under it nor inside one of the repo's worktrees lands in the `external`
    /// catch-all instead of its own pod. Callers set this from the workspace
    /// record after construction (the reducer can't read it), mirroring how
    /// `display_name` is filled.
    pub fn with_project_root(mut self, project_root: Option<PathBuf>) -> Self {
        self.project_root = project_root;
        self
    }

    /// Record the repo's worktree checkout roots so a worktree parked *outside*
    /// `project_root` still earns its own pod rather than folding into
    /// `external`. Like `with_project_root`, the `rimz sidebar snapshot` CLI
    /// fills this from `git worktree list` after construction; git-backed rows
    /// add their own roots during grouping, and the pure path leaves this empty.
    pub fn with_worktree_roots(mut self, worktree_roots: Vec<PathBuf>) -> Self {
        self.worktree_roots = worktree_roots;
        self
    }

    /// Record the repo's durable worktree-home directory so the cockpit spend
    /// scope counts sessions from worktrees cleanup has since removed and
    /// unstamped RimZ-owned worktree rows fold into their `#channel` pod.
    /// Filled from `MachineConfig`'s `[agents.worktree] dir` after construction,
    /// like `with_worktree_roots`; the pure path leaves it `None`.
    pub fn with_worktree_home(mut self, worktree_home: Option<PathBuf>) -> Self {
        self.worktree_home =
            worktree_home.map(|path| crate::worktree::normalize_path_lexical(&path));
        self
    }

    /// Record the room root's class so a non-repo room's
    /// root pod takes the name-only [`SidebarWorktreeKind::Root`] kind. Like
    /// `with_project_root`, filled from the workspace record after
    /// construction (the reducer can't read it); the pure path keeps the
    /// `Repo` default.
    pub fn with_root_class(mut self, root_class: RootClass) -> Self {
        self.root_class = root_class;
        self
    }

    /// Re-sort the already-built worktree groups after a renderer-local
    /// presentation flag such as `SidebarRow::unread` changes. This preserves
    /// the producer's row cap and status counts; it only changes visible order.
    pub fn sort_groups_for_presentation(&mut self) {
        layout::sort_groups_for_presentation(&mut self.worktree_groups);
    }

    /// Attach each session's rich context sidecar to its `AgentState` by
    /// `(kind, agent_id)`. Context is display-only and never changes durable
    /// lifecycle truth or `last_activity`; explicit context markers may refine
    /// a displayed status and its attention rank. Context reaches rows only
    /// through the live-pane fold. A context whose session is absent from the (already
    /// reaped) rollup is dropped — the session is gone, so its context is just
    /// history. Records carry no identity of their own; the key they're filed
    /// under is authority.
    pub fn with_agent_context(mut self, records: Vec<AgentContextRecord>) -> Self {
        if records.is_empty() {
            return self;
        }
        let mut by_key: BTreeMap<(AgentKind, AgentSessionId), _> = records
            .into_iter()
            .map(|record| ((record.kind, record.agent_id), record.context))
            .collect();
        for agent in &mut self.agents {
            if let Some(context) = by_key.remove(&(agent.kind.clone(), agent.agent_id.clone())) {
                agent.context = Some(context);
            }
        }
        self
    }

    /// Attach each child's `subagentStatusLine` enrichment (description, token
    /// count, exact cost, start time) to its `AgentState` by `(kind, agent_id)`.
    /// It must land on the `AgentState`, not the already-projected
    /// `SidebarSubAgent`: the live-pane fold re-runs `attach_sub_agents` →
    /// `sub_agent_from_state`. `token_count` claims the otherwise-unused
    /// `total_tokens` slot (a paneless child reads no transcript). Display-only, like
    /// [`with_agent_context`](Self::with_agent_context) — it never touches
    /// `last_activity`, so ranking is untouched. A record whose child is absent
    /// from the rollup is dropped; the key it is filed under is authority.
    pub fn with_subagent_context(mut self, records: Vec<SubagentContextRecord>) -> Self {
        if records.is_empty() {
            return self;
        }
        let mut by_key: BTreeMap<(AgentKind, AgentSessionId), _> = records
            .into_iter()
            .map(|record| ((record.kind, record.agent_id), record.context))
            .collect();
        for agent in &mut self.agents {
            if let Some(context) = by_key.remove(&(agent.kind.clone(), agent.agent_id.clone())) {
                // Back-fill the type label when the lifecycle hook never provided one.
                // Fork agents carry no `agent_type` in `SubagentStart`, so `task`
                // stays `None` until the first `subagentStatusLine` render. Never
                // overwrite a type the lifecycle already established.
                if agent.task.is_none() {
                    agent.task = context.agent_type;
                }
                // Lifecycle learns Claude's child model only at SubagentStop;
                // until then, paint the transcript-harvested model without
                // replacing lifecycle-established truth.
                if agent.model.is_none() {
                    agent.model = context.model;
                }
                if agent.effort.is_none() {
                    agent.effort = context.effort;
                }
                agent.subagent_description = context.description;
                agent.subagent_cost_usd = context.cost_usd;
                agent.subagent_started_at = context.started_at;
                if context.token_count.is_some() {
                    agent.usage.total_tokens = context.token_count;
                }
            }
        }
        // Unmatched sidecars mean the task `id` from `subagentStatusLine` doesn't
        // match the lifecycle `agent_id`. Log at debug so the mismatch is visible
        // without polluting production output.
        for key in by_key.keys() {
            debug!(
                target: "rimz::sidebar::subagent",
                kind = %key.0,
                agent_id = %key.1,
                "subagent context sidecar has no matching agent row — possible id mismatch",
            );
        }
        self
    }
}

#[cfg(test)]
mod tests;
