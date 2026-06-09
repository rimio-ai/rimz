//! Sidebar view-model assembly: the `Sidebar*` renderer contract and the
//! grouping, ranking, capping, and status projection that fills it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use tracing::debug;

use super::fold::agent_rollup_with_carryover;
use super::panes::SidebarOwnView;
use super::row::SidebarRow;
use crate::agents::{RateLimitWindow, SpendTally};
use crate::feed::{AgentState, AgentStatus, FeedItem, FeedStatus, Surface};
use crate::ids::{AgentKind, AgentSessionId, WorkspaceId};
use crate::ledger::agent_context::AgentContextRecord;
use crate::ledger::event_log::{self};
use crate::ledger::subagent_context::SubagentContextRecord;
use crate::schema::event::EventEnvelope;
use crate::workspace::RootClass;

mod aggregate;
mod layout;
mod live;
mod providers;
mod reap;
mod rows;

use reap::{agent_hook_session_stale, is_agent_native_item};

#[cfg(test)]
pub(super) use aggregate::{attach_sub_agents, sub_agent_from_state};
#[cfg(test)]
pub(super) use rows::row_from_agent;

/// Serde default for [`SidebarSnapshot::root_class`]: `Repo` keeps a pre-class
/// snapshot (and the pure reducer path) on the prior repo-room grouping.
fn default_root_class() -> RootClass {
    RootClass::Repo
}

/// Sidebar view-model. The pane frame admits every rendered card; ledger,
/// sidecars, and realtime events only enrich rows admitted from live panes.
/// Worktree groups are the renderer contract: grouping, attention ranking,
/// caps, status tallies, and row metadata are resolved here so renderers only
/// paint semantics into glyphs.
///
/// `needs_attention` and `resolver_working` are load-bearing: they are the
/// reducer inputs the live-pane fold reads when panes are folded in
/// (`with_live_panes`). The sidebar renderer reads `worktree_groups`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SidebarSnapshot {
    pub workspace_id: WorkspaceId,
    pub display_name: String,
    pub generated_at: Timestamp,
    /// Producer timestamp of the pane frame folded into this snapshot. Realtime
    /// events older than this baseline are superseded by pulled truth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panes_produced_at_ms: Option<u64>,
    /// The single instant every time-window verdict in this projection reads —
    /// the compaction head, the stall escalation, rate-limit resets, and
    /// subagent retention all agree on one clock, captured once at
    /// construction. Never serialized: a reader re-stamps it at its own read
    /// instant (`read_fresh_latest`), so a deserialized snapshot's enrichment
    /// rebuilds against the read, not the long-gone produce.
    #[serde(skip, default = "Timestamp::now")]
    pub now: Timestamp,
    pub worktree_groups: Vec<SidebarWorktreeGroup>,
    pub needs_attention: Vec<FeedItem>,
    pub resolver_working: Vec<FeedItem>,
    pub agents: Vec<AgentState>,
    /// The lazy-registering agent kinds whose Rimz hooks are wired
    /// ([`crate::agents::Capabilities::registers_lazily`] ∩ installed). Gates the
    /// idle-instance synthesis in `rows_from_panes`: a launched-but-unbound pane
    /// of such an agent has no ledger session yet (it registers lazily on the
    /// first turn), and only a wired agent can ever report status, so only a
    /// wired lazy agent's bare pane is promoted from a process row to an idle
    /// agent. Cwd binding for an existing paneless session is separate and can
    /// also recover a non-lazy agent after a mux rebirth clears pane stamps.
    /// Codex is the only lazy-registering agent today. Environment, not ledger
    /// — the pure reducer leaves it empty; the `rimz sidebar snapshot` CLI and
    /// consumer enrichment fill it before folding live panes. The
    /// placeholder/persisted snapshot keeps it empty (a process row).
    #[serde(default)]
    pub wired_lazy_kinds: Vec<String>,
    /// Per-kind launch model defaults for idle synthesized lazy-agent rows,
    /// filled from adapter-owned config reads before the live-pane fold. Codex
    /// uses this to show the configured model beside the context window before
    /// its first session event; the pure reducer leaves it empty and falls
    /// back to descriptor defaults.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lazy_agent_default_models: BTreeMap<String, String>,
    /// The calling sidebar's own-view summary: how many sibling panes share its
    /// tab/window, whether its own pane holds focus, and which sibling is
    /// focused. The renderer's self-close and selection-sync read it instead of
    /// spawning a second `pane list` per tick. Computed by the `rimz sidebar
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
    /// read from the ledger, so the pure path leaves it `None` (every cwd keeps
    /// per-path grouping) and the `rimz sidebar snapshot` CLI fills it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root: Option<PathBuf>,
    /// The repo's worktree checkout roots, as `git worktree list` reports them.
    /// A pane whose cwd is inside any of these is one of the project's
    /// worktrees and earns its own pod — *including a worktree parked outside
    /// `project_root`*, which the `project_root` prefix test alone would miss.
    /// Like `project_root`, this is workspace identity the reducer can't read
    /// from the ledger, so the pure path leaves it empty (the `project_root`
    /// prefix test then stands alone) and the `rimz sidebar snapshot` CLI fills
    /// it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub worktree_roots: Vec<PathBuf>,
    /// The room root's class from the workspace record. Grouping reads it to
    /// give a non-repo room's root pod the name-only [`SidebarWorktreeKind::Root`]
    /// kind while a repo room's own checkout keeps per-path pods. Like
    /// `project_root`, workspace identity the reducer can't read from the
    /// ledger: the pure path and any pre-class snapshot default to `Repo`
    /// (the prior behavior) and the producing reads fill it from the record.
    #[serde(default = "default_root_class")]
    pub root_class: RootClass,
    /// Per-machine sidebar display preferences (the attention-redden window and
    /// the per-provider dashboard styling). Like `project_root`, this is machine
    /// state the pure reducer can't read, so the reducer leaves it default and the
    /// `rimz sidebar snapshot` CLI fills it from `MachineConfig`. The renderer — a
    /// pure snapshot consumer — reads it to tune the cards and the dashboard.
    #[serde(default)]
    pub sidebar: crate::config::SidebarConfig,
    /// Per-provider dashboard blocks pinned to the bottom of the sidebar — the
    /// account-scoped budgets, aggregate spend/tokens, and brand emblem.
    /// One block folds every session of a kind. Built by
    /// [`Self::with_provider_aggregates`] on the producer (it needs config and an
    /// account probe the pure reducer can't read), so the placeholder/persisted
    /// snapshot leaves it empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<SidebarProviderPanel>,
    /// Fleet-wide JSONL-computed spend and token tally — today / week / month /
    /// all-time — summing every provider (Claude scoped to the visible worktrees,
    /// Codex and Pi fleet-wide). Attached by the sidebar enrichment spine
    /// (`sidebar::enrich::enrich`) from the producer's fleet spending walk
    /// (`sidebar::produce`, via [`crate::agents::spending::compute_spending`]);
    /// `None` until the cache is seeded (the first producer tick after startup)
    /// or when nothing has been recorded. The cockpit reads `today` (sessions,
    /// the token split, and the count-up `$`); the fleet ledger reads the
    /// trailing `week` and `month` rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_tally: Option<SpendTally>,
    /// The cockpit's live today-spend: the walked `value_tally.today` figure
    /// plus each live session's overshoot over the baseline captured at the
    /// walk's publish ([`crate::agents::spending::today_spend_live_usd`]), so
    /// the headline climbs the instant a session's statusline cost moves while
    /// the tally stays the exact walked record the W/M ledger rows read.
    /// Stamped where the spending cache folds onto the snapshot — the
    /// producing CLI and the consumer fold alike; `None` on the pure-reducer
    /// path and any pre-overlay snapshot, where the cockpit falls back to the
    /// tally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub today_spend_live_usd: Option<f64>,
    /// The active event-log extent this rollup reflects — the freshness stamp
    /// `read_fresh_latest` compares against the live log. Stamped by
    /// `build_from` under the producing fold; `None` on the pure-reducer
    /// path, the renderer placeholder, and any pre-stamp snapshot, all of
    /// which read as stale so a fresh fold replaces them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reflects_log: Option<event_log::LogExtent>,
}

/// One provider's aggregate dashboard block, pinned to the bottom of the
/// sidebar. Account-scoped: every session of one agent kind folds into one
/// block — summed spend and tokens, plus the freshest session's plan, version,
/// and rate-limit windows — so the budgets render once per account, never
/// per row. Resolved on the producer into a ready-to-paint shape: the renderer
/// reads art, color, and plan straight off it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SidebarProviderPanel {
    pub kind: String,
    /// Header display name (`Claude`, `Codex`, …).
    pub product_name: String,
    /// Multi-line ASCII emblem, painted brand-colored at the block's left.
    pub art: Vec<String>,
    /// 256-color index for the emblem.
    pub color: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Brand plan label (`Claude Max`, `ChatGPT Pro`); `None` when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// Whether the account is metered by rate-limit windows. `false` paints the
    /// "infinite power" bar in place of draining budget bars.
    pub metered: bool,
    /// Whether remote control is enabled for this provider (the `⇅ rc` flag).
    pub remote_control: bool,
    /// JSONL-computed today / week / month / all-time spend and tokens for this
    /// provider, summed across all of its sessions' transcript history — the one
    /// source for the panel's `$` and `◇` figures, and the only cost source for
    /// token-only providers like Codex. `None` until the producer's spending
    /// enrichment runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spending: Option<SpendTally>,
    /// The account-scoped budget windows, ordered short→long by duration. A
    /// metered account drains one mana bar per window; the persisted cache folds
    /// in so an idle account still paints its last-known bars.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<RateLimitWindow>,
}

impl SidebarProviderPanel {
    /// The figure the dashboard ranks panels by: today's JSONL spend, so the
    /// provider you are spending on right now floats to the top.
    fn rank_cost(&self) -> f64 {
        self.spending.as_ref().map_or(0.0, |s| s.today.usd)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidebarWorktreeKind {
    /// A group root with a git story: a repo room's worktree checkout or a
    /// directory room's child repo. Carries the header's diff/commit cluster.
    Worktree,
    /// A non-repo room's own pod — panes at the root and in non-repo subdirs.
    /// Name-only header; excluded from every git read.
    Root,
    /// The out-of-project catch-all: untethered scripts/CI and shells whose
    /// cwd is outside every group root. Renders as the dim `external` divider.
    External,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SidebarWorktreeGroup {
    pub key: String,
    pub label: String,
    pub kind: SidebarWorktreeKind,
    pub status_counts: Vec<SidebarStatusCount>,
    pub rows: Vec<SidebarRow>,
    pub hidden_count: usize,
    /// The worktree's total insertions and deletions relative to the trunk —
    /// committed, staged, and unstaged folded into one `+/-` by diffing the
    /// working tree against the merge-base with `main`. Projected by the
    /// `rimz sidebar snapshot` CLI (the reducer stays pure). Lives on the
    /// group header — never on a per-agent row — so the shared-worktree
    /// "whose diff?" ambiguity is resolved by belonging to the worktree, not
    /// the agent. `None` when no git read was attempted or the worktree is
    /// not a git repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_added: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_removed: Option<u32>,
    /// Commits this worktree carries ahead of the trunk — `git rev-list --count
    /// <merge-base>..HEAD`, the committed work waiting to land (the `+/-` diff
    /// also folds in staged/unstaged change). Like the diff, it is a property of
    /// the worktree path, projected by the `rimz sidebar snapshot` CLI; `None`
    /// when no git read was attempted or the worktree is not a git repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commits_ahead: Option<u32>,
    /// Commits the trunk has advanced past this worktree's fork point — `git
    /// rev-list --count <merge-base>..<trunk>`, the work the branch would pick
    /// up by rebasing. Projected alongside `commits_ahead`; `None` on the same
    /// terms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commits_behind: Option<u32>,
    /// The resolved trunk name the diff and commit delta compare against
    /// (configured `[sidebar] trunk`, else detected `main`/`master`/remote
    /// default; `origin/` stripped for display). Names the landed markers — a
    /// non-trunk worktree holding no work of its own (zero ahead, zero diff,
    /// clean tree) renders `≡ <trunk>` at zero behind and `✓ <trunk>` once the
    /// trunk has moved on; the trunk worktree itself (`label == trunk`) never
    /// wears either, since "landed on itself" carries no information.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trunk: Option<String>,
    /// Whether the working tree is clean — `git status --porcelain` emptiness,
    /// untracked files included — the safe-to-remove verdict both landed
    /// markers require. Untracked content also folds into `diff_added` as line
    /// counts, so an untracked-only worktree reads `+N` rather than landed.
    /// `None` when no status read was attempted or an old producer wrote the
    /// cache; the renderer treats that as not proven clean.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clean: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SidebarStatusCount {
    pub status: AgentStatus,
    pub count: usize,
}

impl SidebarSnapshot {
    pub fn build(
        workspace_id: WorkspaceId,
        items: Vec<FeedItem>,
        events: Vec<EventEnvelope>,
        now: Timestamp,
    ) -> Self {
        Self::build_with_carryover(workspace_id, items, events, Vec::new(), now)
    }

    /// Build a snapshot, folding `carryover_agents` into the agent rollup so
    /// pre-rotation observations survive event-log archiving. Live events
    /// with a newer `last_seen` override the carryover.
    pub fn build_with_carryover(
        workspace_id: WorkspaceId,
        items: Vec<FeedItem>,
        events: Vec<EventEnvelope>,
        carryover_agents: Vec<AgentState>,
        now: Timestamp,
    ) -> Self {
        let agents = agent_rollup_with_carryover(&events, carryover_agents);
        Self::build_with_agents(workspace_id, items, agents, now)
    }

    pub fn build_with_agents(
        workspace_id: WorkspaceId,
        mut items: Vec<FeedItem>,
        agents: Vec<AgentState>,
        now: Timestamp,
    ) -> Self {
        items.sort_by_key(|item| std::cmp::Reverse(item.updated_at));

        let mut needs_attention = Vec::new();
        let mut resolver_working = Vec::new();

        for item in items {
            // A pending agent-hook ask outlives its agent only as data: once the
            // session that raised it is gone from the live rollup it is no longer
            // attention, just history. This is what stops a fresh agent from
            // inheriting a dead session's 18h-old permission prompt.
            let stale =
                item.status == FeedStatus::Pending && agent_hook_session_stale(&item, &agents);
            match (item.status, item.surface) {
                (FeedStatus::Pending, Surface::NativeUi)
                    if is_agent_native_item(&item) && !stale =>
                {
                    needs_attention.push(item);
                }
                (FeedStatus::Pending, Surface::Script) => {
                    needs_attention.push(item);
                }
                (FeedStatus::Pending, Surface::Bridge) if !stale => resolver_working.push(item),
                // Resolved and otherwise-inactive items are history, not
                // presence or attention: the sidebar never renders them, so they
                // are dropped here rather than carried in the view-model.
                _ => {}
            }
        }

        let display_name = workspace_id.as_str().to_owned();
        Self {
            workspace_id,
            display_name,
            generated_at: now,
            panes_produced_at_ms: None,
            now,
            worktree_groups: Vec::new(),
            needs_attention,
            resolver_working,
            agents,
            wired_lazy_kinds: Vec::new(),
            lazy_agent_default_models: BTreeMap::new(),
            own_view: None,
            only_daemon_view_remains: false,
            project_root: None,
            worktree_roots: Vec::new(),
            root_class: default_root_class(),
            sidebar: crate::config::SidebarConfig::default(),
            providers: Vec::new(),
            value_tally: None,
            today_spend_live_usd: None,
            reflects_log: None,
        }
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

    /// Record the repo's worktree checkout roots so a
    /// worktree parked *outside* `project_root` still earns its own pod rather
    /// than folding into `external`. Like `with_project_root`, the
    /// `rimz sidebar snapshot` CLI fills this from `git worktree list` after
    /// construction; the pure path leaves it empty.
    pub fn with_worktree_roots(mut self, worktree_roots: Vec<PathBuf>) -> Self {
        self.worktree_roots = worktree_roots;
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

    /// Attach each session's rich context sidecar to its `AgentState` by
    /// `(kind, agent_id)`. Context is display-only — it never changes ranking,
    /// since `last_activity` is untouched — and reaches rows only through the
    /// live-pane fold. A context whose session is absent from the (already
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
    /// count, start time) to its `AgentState` by `(kind, agent_id)`. It must land
    /// on the `AgentState`, not the already-projected `SidebarSubAgent`: the
    /// live-pane fold re-runs `attach_sub_agents` → `sub_agent_from_state`.
    /// `token_count` claims the otherwise-unused `total_tokens` slot (a paneless
    /// child reads no transcript). Display-only, like
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
                agent.subagent_description = context.description;
                agent.subagent_started_at = context.started_at;
                if context.token_count.is_some() {
                    agent.total_tokens = context.token_count;
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
