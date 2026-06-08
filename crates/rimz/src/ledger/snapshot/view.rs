//! Sidebar view-model assembly: the `Sidebar*` renderer contract and the
//! grouping, ranking, capping, and status projection that fills it.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::fold::agent_rollup_with_carryover;
use super::panes::{
    AgentPaneRow, LazyAgentPairingResult, SidebarOwnView, agent_for_pane, agent_pane_for_pane,
    compute_lazy_agent_pairings, is_daemon_mode_codex, pane_admits_card, pane_start_matches,
    row_from_frame_pane,
};
use super::process::{pane_command_is_known, row_from_process};
use super::row::{AgentCard, RowCard, SidebarResolverState, SidebarRow, SidebarSubAgent};
use crate::agent_activity::AgentActivity;
use crate::agents::TurnErrorClass;
use crate::agents::lifecycle::TurnPhase;
use crate::agents::{AgentAccount, AgentContext, AgentTurnError, RateLimitWindow, SpendTally};
use crate::feed::{
    AgentState, AgentStatus, FeedItem, FeedStatus, PaneRef, ResolverStepState, Surface,
};
use crate::ids::{AgentKind, AgentSessionId, PaneId, WorkspaceId};
use crate::ledger::agent_context::AgentContextRecord;
use crate::ledger::event_log::{self};
use crate::ledger::subagent_context::SubagentContextRecord;
use crate::schema::event::EventEnvelope;
use crate::workspace::RootClass;

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
    /// Whether any supported agent has its Rimz hooks wired. The sidebar's
    /// first-run hint reads it: with no hooks installed, running an agent
    /// registers nothing, so the empty-room hint points at `rimz hooks
    /// install` rather than "run claude or codex". This is environment state,
    /// not ledger truth, so the pure reducer leaves it `false` and the `rimz
    /// sidebar snapshot` CLI fills it; the placeholder/persisted snapshot keeps
    /// `false`, where the renderer suppresses the hint anyway.
    #[serde(default)]
    pub agent_hooks_ready: bool,
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
            agent_hooks_ready: false,
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

    /// Apply best-effort process liveness to agent overlays that published a
    /// PID. Hook protocols do not all expose a session-exit event; when a hook
    /// command can record the agent process identity, the sidebar uses it to
    /// suppress stale ledger overlays without scraping pane contents.
    pub fn drop_dead_agents_with(&mut self, mut is_alive: impl FnMut(u32, Option<&str>) -> bool) {
        self.agents.retain(|agent| {
            if let Some(owner) = &agent.runtime_owner {
                return is_alive(owner.pid, owner.process_start.as_deref());
            }
            agent
                .agent_pid
                .is_none_or(|pid| is_alive(pid, agent.agent_process_start.as_deref()))
        });
    }

    /// Reap daemon-mode Codex sessions the per-user app-server daemon no longer
    /// holds in memory. A daemon-backed session records the shared daemon's pid,
    /// not its own CLI's, so process liveness — which keeps it while the daemon
    /// lives ([`drop_dead_agents_with`]) — can never reap it. Without this a closed
    /// remote-control conversation lingers as a ghost and binds its stale
    /// status, model, tokens, and pending ask onto a live `codex` pane by cwd
    /// ([`agent_pane_for_pane`]).
    ///
    /// Tri-state, and fail-safe by construction (the loaded-thread set is a
    /// liveness improvement, not a perfect pane signal, so it never mass-reaps):
    /// - `loaded` is `None` — the daemon was unreachable or its `thread/loaded/list`
    ///   could not be trusted — keep every session;
    /// - `daemon_pids` is empty — no daemon is running, so every session is
    ///   standalone — keep every session;
    /// - a session is daemon-mode ([`is_daemon_mode_codex`]) and its id is absent
    ///   from `loaded` — reap it;
    /// - anything else — keep it.
    ///
    /// The producer runs this before the live-pane fold, so a reaped session can
    /// neither render a row nor attach stale stats to a live pane.
    pub fn drop_dead_daemon_sessions(
        &mut self,
        daemon_pids: &BTreeSet<u32>,
        loaded: Option<&BTreeSet<String>>,
    ) {
        let Some(loaded) = loaded else { return };
        if daemon_pids.is_empty() {
            return;
        }
        self.agents.retain(|agent| {
            let reapable = is_daemon_mode_codex(agent, daemon_pids)
                && !loaded.contains(agent.agent_id.as_str());
            !reapable
        });
    }

    /// Reap ghost sessions from the agent rollup. This filters the *derived*
    /// rollup only; the append-only event log is untouched, so it complements
    /// the workspace-level `rimz gc`. Two rules, both safe for the
    /// one-pane-one-row invariant:
    ///
    /// (a) a **pidless** session past [`GHOST_SESSION_TTL_SECS`] — it never
    ///     captured a pid, so process liveness can never reap it, yet it has
    ///     not reported in hours. A recent pidless session (a just-launched
    ///     agent) is kept.
    /// (b) an older session **superseded** by a strictly-newer same-kind
    ///     session on the same `(worktree_path, worktree_branch)`, when the
    ///     older holds no live pane the newer doesn't already occupy. This
    ///     collapses relaunch-in-place and shared-pid ghosts to the newest
    ///     while never dropping a concurrent agent that owns its own pane.
    pub fn reap_stale_sessions(&mut self) {
        let now = self.now;
        // Mark each superseded older session by position, borrowing `agents`
        // read-only. Runs on every snapshot rebuild, so the old approach — a
        // `BTreeSet` of owned `(kind, agent_id)` tuples plus a second clone per
        // agent in `retain` — meant up to ~3×N string allocations per call; the
        // parallel `Vec<bool>` keeps it allocation-free per agent.
        //
        // Both reap rules are root-only. A subagent is paneless and pidless by
        // construction and shares no worktree key with its parent, so the
        // supersession rule would collapse two live parallel siblings and the
        // pidless-TTL rule would reap an idle child — both wrong. A subagent
        // `older` therefore maps to `false` (never superseded), and the retain
        // below keeps every subagent outright; they leave the rollup only
        // transitively once the parent is gone.
        let superseded: Vec<bool> = self
            .agents
            .iter()
            .map(|older| {
                older.parent_agent_id.is_none()
                    && self.agents.iter().any(|newer| {
                        newer.parent_agent_id.is_none()
                            && newer.kind == older.kind
                            && newer.agent_id != older.agent_id
                            && newer.last_activity > older.last_activity
                            && newer.worktree_path == older.worktree_path
                            && newer.worktree_branch == older.worktree_branch
                            && older_yields_pane(older, newer)
                    })
            })
            .collect();
        // `Vec::retain` visits each element once, front to back, so a cursor over
        // `superseded` stays aligned with `agents` without a hand-rolled index.
        let mut superseded = superseded.into_iter();
        self.agents.retain(|agent| {
            // Advance the cursor once per agent, before any early return, so it
            // stays aligned with `agents` even when a subagent short-circuits.
            let is_superseded = superseded.next().unwrap_or(false);
            // Subagents are never reaped here — kept until their parent leaves,
            // when the projection's orphan-drop hides them.
            if agent.parent_agent_id.is_some() {
                return true;
            }
            if is_superseded {
                return false;
            }
            !(agent_is_pidless(agent) && session_age_secs(now, agent) > GHOST_SESSION_TTL_SECS)
        });
    }

    /// Whether every live, non-sidebar view in `panes` is the `rimzd` daemon
    /// view — i.e. the user has nothing left but the managed daemon tab. A view
    /// is a *daemon* view iff, after dropping its sidebar pane, it is non-empty
    /// and every remaining pane is a managed host
    /// ([`crate::remote_control::pane_is_host`]); a *working* view iff it holds
    /// any non-sidebar, non-host pane. A sidebar-only view (a working tab
    /// mid-self-close) counts as neither, so it neither trips nor blocks the
    /// signal. Returns `false` for an empty or not-yet-born session.
    ///
    /// Keys on `view_id` + `pane_is_host` (which reads the command marker or
    /// the `rimzd` view name — both backends report the view name), so it
    /// behaves identically on Zellij and tmux.
    pub fn only_daemon_view(panes: &[PaneRef]) -> bool {
        // Per view_id: (host pane count, working pane count). Sidebar panes are
        // dropped but still register the view, so a sidebar-only view exists as
        // an entry with zero of each — counted as neither daemon nor working.
        let mut views: std::collections::BTreeMap<&str, (u32, u32)> =
            std::collections::BTreeMap::new();
        for pane in panes {
            let Some(view_id) = pane.view_id.as_deref() else {
                continue;
            };
            let entry = views.entry(view_id).or_default();
            let is_sidebar = pane
                .command
                .as_deref()
                .is_some_and(super::process::command_is_sidebar_chrome);
            if is_sidebar {
                continue;
            }
            if crate::remote_control::pane_is_host(pane) {
                entry.0 += 1;
            } else {
                entry.1 += 1;
            }
        }
        let mut saw_daemon = false;
        for (hosts, working) in views.values() {
            if *working > 0 {
                return false;
            }
            if *hosts > 0 {
                saw_daemon = true;
            }
        }
        saw_daemon
    }

    /// Fold live multiplexer panes into the sidebar view-model. This reducer is
    /// pure: callers own pane discovery and pass the result in, so snapshot
    /// building stays independent of any backend command.
    pub fn with_live_panes(mut self, panes: Vec<PaneRef>, exclude: Option<&PaneId>) -> Self {
        let panes = Self::card_admitted_live_panes(panes, exclude);
        self.fold_admitted_live_panes(&panes, None);
        self
    }

    pub(crate) fn card_admitted_live_panes(
        panes: Vec<PaneRef>,
        exclude: Option<&PaneId>,
    ) -> Vec<PaneRef> {
        panes
            .into_iter()
            .filter(|pane| pane_admits_card(pane, exclude).admits())
            .collect()
    }

    pub(crate) fn with_admitted_live_panes(
        mut self,
        panes: Vec<PaneRef>,
        lazy_pairings: &LazyAgentPairingResult,
    ) -> Self {
        self.fold_admitted_live_panes(&panes, Some(lazy_pairings));
        self
    }

    fn fold_admitted_live_panes(
        &mut self,
        panes: &[PaneRef],
        lazy_pairings: Option<&LazyAgentPairingResult>,
    ) {
        self.worktree_groups = build_worktree_groups_from_rows(
            rows_from_panes(
                &self.agents,
                &self.needs_attention,
                &self.resolver_working,
                panes,
                LazyAgentPaneProjection {
                    wired_kinds: &self.wired_lazy_kinds,
                    default_models: &self.lazy_agent_default_models,
                    pairings: lazy_pairings,
                },
                self.now,
            ),
            &self.agents,
            self.project_root.as_deref(),
            &self.worktree_roots,
            self.root_class,
            self.now,
            self.sidebar.attention.stalled_after_secs.get(),
        );
    }

    pub(crate) fn remove_pane_rows(&mut self, pane_id: &PaneId) -> bool {
        let mut changed = false;
        for group in &mut self.worktree_groups {
            let before = group.rows.len();
            group.rows.retain(|row| {
                !row.pane
                    .as_ref()
                    .is_some_and(|pane| pane.pane_id == *pane_id)
            });
            changed |= group.rows.len() != before;
            refresh_overlay_group(group);
        }
        self.worktree_groups
            .retain(|group| !group.rows.is_empty() || group.hidden_count > 0);
        if self
            .own_view
            .as_ref()
            .and_then(|view| view.active_pane_id.as_ref())
            .is_some_and(|active| active == pane_id)
            && let Some(view) = &mut self.own_view
        {
            view.active_pane_id = None;
        }
        changed
    }

    pub(crate) fn overlay_pane_command(&mut self, pane_id: &PaneId, command: &str) -> bool {
        let mut changed = false;
        for group in &mut self.worktree_groups {
            for row in &mut group.rows {
                let Some(pane) = row.pane.as_mut() else {
                    continue;
                };
                if pane.pane_id != *pane_id {
                    continue;
                }
                pane.command = Some(command.to_owned());
                pane.pane_process_start = None;
                if let Some(next) = row_from_frame_pane(
                    pane,
                    &self.wired_lazy_kinds,
                    &self.lazy_agent_default_models,
                    self.now,
                ) {
                    let worktree_path = row
                        .worktree_path
                        .clone()
                        .or_else(|| next.worktree_path.clone());
                    *row = next;
                    row.worktree_path = row.worktree_path.clone().or(worktree_path);
                }
                changed = true;
            }
            refresh_overlay_group(group);
        }
        changed
    }

    /// Apply a fused per-view focus patch. Row `is_focused` bits mirror the
    /// patch for every listed pane — per-view marks are session-wide truth the
    /// pull would also report — while the own-view baseline retargets only when
    /// the patch names one of this view's own working panes: a focus move in
    /// another tab is that view's mark, never this renderer's selection
    /// baseline.
    pub(crate) fn overlay_focus(&mut self, focused: &[PaneId], unfocused: &[PaneId]) -> bool {
        if focused.is_empty() && unfocused.is_empty() {
            return false;
        }
        let mut changed = false;
        for group in &mut self.worktree_groups {
            for row in &mut group.rows {
                let Some(pane) = row.pane.as_mut() else {
                    continue;
                };
                if focused.iter().any(|pane_id| pane_id == &pane.pane_id) {
                    changed |= !pane.is_focused;
                    pane.is_focused = true;
                }
                if unfocused.iter().any(|pane_id| pane_id == &pane.pane_id) {
                    changed |= pane.is_focused;
                    pane.is_focused = false;
                }
            }
        }
        if let Some(view) = &mut self.own_view {
            if let Some(own_focused) = focused
                .iter()
                .find(|&pane_id| view.working_pane_ids.contains(pane_id))
            {
                if view.active_pane_id.as_ref() != Some(own_focused) || view.own_is_active {
                    view.active_pane_id = Some(own_focused.clone());
                    view.own_is_active = false;
                    changed = true;
                }
            } else if view
                .active_pane_id
                .as_ref()
                .is_some_and(|active| unfocused.iter().any(|pane_id| pane_id == active))
            {
                view.active_pane_id = None;
                changed = true;
            }
        }
        changed
    }

    /// Fold per-agent activity heartbeats into the rollup. The agent's hook
    /// touches its heartbeat on every progress-proving event, so the freshest
    /// touch is a truer `last_activity` than the turn-grained event log — it
    /// advances per tool call, which is what keeps a busy agent's row animated,
    /// recovers an answered ask, and dates a genuine stall. Latency, not truth:
    /// a missing or older heartbeat leaves the event-log value untouched.
    ///
    /// Apply this before [`Self::with_live_panes`] so age, ranking, the
    /// ask-fold guard, and the stall window all read the accurate value.
    pub fn with_agent_activity(mut self, activity: &[AgentActivity]) -> Self {
        for agent in &mut self.agents {
            let Some(touch) = activity
                .iter()
                .filter(|a| a.kind == agent.kind && a.agent_id == agent.agent_id)
                .max_by_key(|a| a.at)
            else {
                continue;
            };
            if touch.at > agent.last_activity {
                agent.last_activity = touch.at;
            }
        }
        self
    }

    /// Fold the agent rollup into per-provider dashboard blocks — one per agent
    /// kind, plus one for any provider with no active session this run that is
    /// logged in (an account-only block, so the dashboard shows your accounts
    /// and budgets between turns).
    /// Sums each kind's spend, tokens, and edited lines; takes the plan, version,
    /// and rate-limit windows from the freshest session (account state is shared,
    /// so the latest reading is truest). `probed_accounts` carries out-of-band
    /// login facts the context cannot (Claude's `auth status`, Codex's
    /// `auth.json`), preferred only when the freshest context has none — and a kind
    /// whose only signal is such a login still earns a block;
    /// `remote_control` carries the per-kind `⇅ rc` flag. Styling (emblem, color,
    /// name) resolves from `self.sidebar.providers` over the built-in defaults, so
    /// the renderer gets a ready-to-paint block. With no explicit
    /// `provider_list`, the set is capped to `max_provider_blocks` by today's
    /// spend, then ordered stably by kind — the panels are the dashboard's tabs,
    /// so the row never reorders as spend shifts. An explicit `provider_list`
    /// supplies the shown set and order, with `all` expanding the remaining
    /// discovered providers and bypassing the cap. Producer-only: the pure
    /// reducer leaves `providers` empty.
    pub fn with_provider_aggregates(
        mut self,
        probed_accounts: &BTreeMap<String, AgentAccount>,
        remote_control: &BTreeMap<String, bool>,
        provider_spending: &BTreeMap<String, SpendTally>,
    ) -> Self {
        let mut kinds: Vec<String> = Vec::new();
        for agent in &self.agents {
            if agent.parent_agent_id.is_some() {
                continue;
            }
            if !kinds.iter().any(|known| agent.kind == **known) {
                kinds.push(agent.kind.to_string());
            }
        }
        // A provider that is logged in but has no active session this run still
        // earns a block, so the dashboard shows your accounts and budgets between
        // turns — fold in every probed-account kind not already covered.
        for (kind, account) in probed_accounts {
            if account_creates_provider_panel(account) && !kinds.iter().any(|known| known == kind) {
                kinds.push(kind.clone());
            }
        }
        let mut panels: Vec<SidebarProviderPanel> = Vec::new();
        for kind in kinds {
            let sessions: Vec<&AgentState> = self
                .agents
                .iter()
                .filter(|agent| agent.parent_agent_id.is_none() && agent.kind == kind)
                .collect();
            // Nothing to show without a session or a logged-in account. Recorded
            // spend enriches an existing provider block but never creates the
            // provider section by itself.
            if sessions.is_empty()
                && !probed_accounts
                    .get(&kind)
                    .is_some_and(account_creates_provider_panel)
            {
                continue;
            }

            // The freshest context wins the account-scoped facts (plan, version)
            // — every session shares one account.
            let freshest = sessions
                .iter()
                .filter_map(|agent| agent.context.as_ref())
                .max_by_key(|context| context.observed_at);
            // A live session's rich-context version wins; the out-of-band
            // probe's binary read (Pi's `pi --version`) covers a provider whose
            // sessions never report one.
            let version = freshest
                .and_then(|context| context.agent_version.clone())
                .or_else(|| {
                    probed_accounts
                        .get(&kind)
                        .and_then(|account| account.version.clone())
                });
            let account = freshest
                .and_then(|context| context.account.clone())
                .or_else(|| probed_accounts.get(&kind).cloned());

            // The budget windows are account-scoped too, but the *freshest*
            // session is not the truest reading: parallel sessions report the same
            // window at slightly different instants, so "freshest wins" flips
            // between ticks and the bar flickers. Instead, pick each window stably
            // across every session, grouped by duration — drop readings whose reset
            // already passed (stale), then keep the most-drained survivor (most
            // conservative). Same inputs always yield the same bars, regardless of
            // which session reported last. A provider whose descriptor declares no
            // rate-limit windows renders the absence deliberately — its panel
            // never grows budget bars even if a stray reading lands in a session
            // context; an unregistered kind keeps whatever it reports.
            let now = self.now;
            let windows_for = |of_kind: &str| {
                stable_windows(
                    self.agents
                        .iter()
                        .filter(|agent| agent.parent_agent_id.is_none() && agent.kind == *of_kind)
                        .filter_map(|agent| agent.context.as_ref()?.rate_limits.as_ref())
                        .flat_map(|limits| limits.windows.iter().cloned()),
                    now,
                )
            };
            let declares_windows = crate::agents::descriptor_by_kind(&kind)
                .is_none_or(|descriptor| descriptor.capabilities.rate_limit_windows);
            let windows = if declares_windows {
                windows_for(&kind)
            } else {
                // A provider with no window surface of its own (Pi) running on a
                // metered sibling subscription shares that account's budget, so
                // its block borrows the sibling kind's windows — same account,
                // same bars. No metered sub, no mapped sibling, or no readings
                // → bar-less, exactly as before.
                account
                    .as_ref()
                    .filter(|account| account.metered == Some(true))
                    .and_then(|account| account.sub_provider.as_deref())
                    .and_then(crate::agents::kind_for_sub_provider)
                    .map(windows_for)
                    .unwrap_or_default()
            };
            let has_windows = !windows.is_empty();

            let metered = account
                .as_ref()
                .and_then(|account| account.metered)
                .unwrap_or(has_windows);
            let plan = account
                .and_then(|account| account.plan)
                .filter(|plan| !plan.is_empty())
                .map(|raw| format_plan_label(&kind, &raw));

            let (default_name, default_art, default_color) = default_provider_style(&kind);
            let style = self.sidebar.providers.get(&kind);
            let product_name = style
                .and_then(|style| style.product_name.clone())
                .filter(|name| !name.is_empty())
                .unwrap_or(default_name);
            let art = style
                .and_then(|style| style.ascii_art.as_deref())
                .filter(|art| !art.is_empty())
                .map(|art| art.lines().map(ToOwned::to_owned).collect())
                .unwrap_or(default_art);
            let color = style.and_then(|style| style.color).unwrap_or(default_color);
            let remote_control = remote_control.get(&kind).copied().unwrap_or(false);
            let spending = provider_spending.get(&kind).cloned();

            panels.push(SidebarProviderPanel {
                kind,
                product_name,
                art,
                color,
                version,
                plan,
                metered,
                remote_control,
                spending,
                windows,
            });
        }

        self.providers = resolve_provider_panels(
            panels,
            &self.sidebar.provider_list,
            self.sidebar.max_provider_blocks,
        );
        self
    }
}

fn account_creates_provider_panel(account: &AgentAccount) -> bool {
    account.plan.as_deref().is_some_and(|plan| !plan.is_empty())
        || account.metered.is_some()
        || account
            .sub_provider
            .as_deref()
            .is_some_and(|provider| !provider.is_empty())
}

fn resolve_provider_panels(
    mut panels: Vec<SidebarProviderPanel>,
    provider_list: &[String],
    max_provider_blocks: usize,
) -> Vec<SidebarProviderPanel> {
    if provider_list.is_empty() {
        // Today's JSONL spend decides only *which* panels survive the cap — the
        // provider you are actively spending on always earns its block, and a
        // token-only provider (Codex) ranks on the same transcript-derived
        // footing as a live-cost one. The retained set then orders stably by
        // kind: the panels are the dashboard's tabs, and a tab row must not
        // reorder as today's spend shifts between providers.
        panels.sort_by(|left, right| {
            right
                .rank_cost()
                .partial_cmp(&left.rank_cost())
                .unwrap_or(Ordering::Equal)
                .then_with(|| left.kind.cmp(&right.kind))
        });
        panels.truncate(max_provider_blocks);
        panels.sort_by(|left, right| left.kind.cmp(&right.kind));
        return panels;
    }

    let explicitly_named: BTreeSet<&str> = provider_list
        .iter()
        .filter_map(|kind| (kind != "all").then_some(kind.as_str()))
        .collect();
    let by_kind: BTreeMap<String, SidebarProviderPanel> = panels
        .into_iter()
        .map(|panel| (panel.kind.clone(), panel))
        .collect();
    let mut resolved = Vec::new();
    let mut emitted_named = BTreeSet::new();
    let mut emitted_all = false;
    for kind in provider_list {
        if kind == "all" {
            if !emitted_all {
                resolved.extend(by_kind.iter().filter_map(|(kind, panel)| {
                    (!explicitly_named.contains(kind.as_str())).then_some(panel.clone())
                }));
                emitted_all = true;
            }
            continue;
        }
        if emitted_named.insert(kind.as_str())
            && let Some(panel) = by_kind.get(kind)
        {
            resolved.push(panel.clone());
        }
    }
    resolved
}

/// The account-stable *set* of budget windows across every session of a
/// provider, grouped by [`duration_mins`](RateLimitWindow::duration_mins).
/// Readings of the same duration run through [`stable_window`] independently, so
/// two sessions reporting one budget at different instants converge to a single
/// bar per duration. Output sorted short→long for a stable paint order; windows
/// of unknown duration sort last.
fn stable_windows(
    windows: impl Iterator<Item = RateLimitWindow>,
    now: Timestamp,
) -> Vec<RateLimitWindow> {
    let mut groups: BTreeMap<Option<u32>, Vec<RateLimitWindow>> = BTreeMap::new();
    for window in windows {
        groups.entry(window.duration_mins).or_default().push(window);
    }
    let mut stable: Vec<RateLimitWindow> = groups
        .into_values()
        .filter_map(|group| stable_window(group.into_iter(), now))
        .collect();
    stable.sort_by_key(|window| window.duration_mins.unwrap_or(u32::MAX));
    stable
}

/// The account-stable reading of one rate-limit window (one duration) across
/// every session of a provider. Parallel sessions report the same shared budget
/// at different instants, so a "freshest wins" pick flickers; this is
/// deterministic instead.
///
/// Drop any reading whose `resets_at` has already passed — that window reset, so
/// its `used_percentage` is stale — then, among the survivors, keep the most
/// drained (highest `used_percentage`, so the bar never over-promises remaining
/// budget). A window with no reset instant can't be aged out, so it is kept as a
/// last-resort reading only when nothing with a live reset survives.
fn stable_window(
    windows: impl Iterator<Item = RateLimitWindow>,
    now: Timestamp,
) -> Option<RateLimitWindow> {
    let mut live: Option<RateLimitWindow> = None;
    let mut undated: Option<RateLimitWindow> = None;
    for window in windows {
        if window.used_percentage.is_none() {
            continue;
        }
        match window.resets_at {
            Some(resets_at) if resets_at <= now => continue, // reset already passed — stale
            Some(_) => {
                if live
                    .as_ref()
                    .is_none_or(|best| window.used_percentage > best.used_percentage)
                {
                    live = Some(window);
                }
            }
            None => {
                if undated
                    .as_ref()
                    .is_none_or(|best| window.used_percentage > best.used_percentage)
                {
                    undated = Some(window);
                }
            }
        }
    }
    live.or(undated)
}

/// Built-in `(product_name, art lines, color)` for a provider kind, read from
/// the adapter's brand descriptor ([`crate::agents::Brand`]); used when the
/// per-machine config overrides none of them. An unregistered kind renders
/// title-cased with no emblem in neutral grey (244).
fn default_provider_style(kind: &str) -> (String, Vec<String>, u8) {
    if let Some(descriptor) = crate::agents::descriptor_by_kind(kind) {
        return (
            descriptor.display_name.to_owned(),
            descriptor
                .brand
                .emblem
                .trim_matches('\n')
                .lines()
                .map(ToOwned::to_owned)
                .collect(),
            descriptor.brand.color,
        );
    }
    (provider_title_case(kind), Vec::new(), 244)
}

/// Format a raw provider plan tier into its brand label, per the adapter's
/// [`crate::agents::PlanLabel`]: Claude's tiers prefix `Claude` (`max` →
/// `Claude Max`), Codex's prefix `ChatGPT` (`pro` → `ChatGPT Pro`); any other
/// provider just title-cases the tier.
fn format_plan_label(kind: &str, raw: &str) -> String {
    let tier = provider_title_case(raw);
    match crate::agents::descriptor_by_kind(kind).map(|descriptor| &descriptor.plan_label) {
        Some(crate::agents::PlanLabel::Prefixed { prefix }) => format!("{prefix} {tier}"),
        Some(crate::agents::PlanLabel::TitleCaseOnly) | None => tier,
    }
}

/// Title-case a `-`/`_`/space-delimited token (`gpt-5` → `Gpt 5`, `max` →
/// `Max`). ASCII-oriented; a non-ASCII leading char is uppercased as Unicode.
fn provider_title_case(value: &str) -> String {
    value
        .split(['-', '_', ' '])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

const WORKTREE_ROW_CAP: usize = 6;

/// Age in seconds after which a pidless agent session is reaped as a ghost.
/// A session that never captured a pid can't be reaped by process liveness, so
/// without a TTL it would linger forever; a few hours is long enough that a
/// genuinely live but pidless session (rare) survives, short enough that an
/// abandoned one clears on its own.
const GHOST_SESSION_TTL_SECS: i64 = 3 * 60 * 60;

fn agent_is_pidless(agent: &AgentState) -> bool {
    agent.runtime_owner.is_none() && agent.agent_pid.is_none()
}

fn session_age_secs(now: Timestamp, agent: &AgentState) -> i64 {
    now.duration_since(agent.last_activity).as_secs()
}

/// True when reaping `older` cannot drop a concurrently-live agent: either it
/// is paneless and the newer session is paneless too (indistinguishable daemon
/// remnants), or it stamped the very pane `newer` now occupies (a relaunch in
/// place). An older paneless session does not yield to a newer distinctly
/// stamped pane: it may still be the occupant of another same-cwd lazy agent
/// pane that only the projection can bind.
fn older_yields_pane(older: &AgentState, newer: &AgentState) -> bool {
    match (older.pane.as_ref(), newer.pane.as_ref()) {
        (None, None) => true,
        (None, Some(_)) => false,
        (Some(older_pane), Some(newer_pane)) => newer_pane.pane_id == older_pane.pane_id,
        (Some(_), None) => false,
    }
}

fn is_agent_native_item(item: &FeedItem) -> bool {
    item.source_kind == "agent-hook"
}

/// True when an agent-hook ask names a session (`agent_id`/`session_id`) that is
/// no longer the live occupant of its pane. The rollup is the liveness source of
/// truth — gated by `SessionEnd` and process-liveness — so an ask is stale when
/// either its session has left the rollup entirely, or a strictly-newer root
/// session of the same kind has taken over the worktree. The latter reaps the
/// zombie case: a pidless `SessionStart`-only session never ends and never gets
/// reaped by process liveness, so without supersession its old permission prompt
/// pins itself onto the freshly launched session sharing the pane. Subagents
/// never supersede their parent: they share the parent's pane and worktree but do
/// not own the human decision surface. Asks with no session id can't be proven
/// stale and are kept.
fn agent_hook_session_stale(item: &FeedItem, agents: &[AgentState]) -> bool {
    if item.source_kind != "agent-hook" {
        return false;
    }
    let Some(agent_id) = agent_id_from_item(item) else {
        return false;
    };
    let Some(session) = agents
        .iter()
        .find(|agent| agent.kind == item.source && agent.agent_id == agent_id)
    else {
        return true;
    };
    if session.parent_agent_id.is_some() {
        return false;
    }
    agents.iter().any(|other| {
        other.parent_agent_id.is_none()
            && other.kind == session.kind
            && other.agent_id != session.agent_id
            && other.worktree_path == session.worktree_path
            && other.last_activity > session.last_activity
    })
}

/// One pane = one row, by construction. Every live pane anchors exactly one
/// row: it binds the unique agent that stamped this pane id — rendering that
/// agent with its single most-relevant pending ask folded in — or, with no such
/// agent, renders as a plain process row. Agents with no live pane (ghosts,
/// sub-agents, a relaunch the reaper has not yet collapsed) do not render, so a
/// dead session can never resurrect a row or latch onto a stranger's pane. The
/// one exception is an unstamped live agent command in its own worktree
/// (`agent_pane_for_pane`): lazy-registering agents bind their session by cwd,
/// and non-lazy agents use the same guarded bind to recover after a mux rebirth
/// clears pane stamps while the process keeps running. A wired lazy pane with
/// no session yet renders idle rather than as a process row. Standalone
/// script/bridge asks render only when they name a pane in this frame, and
/// refresh their pane reference from that frame: on a pane that resolves to an
/// agent row the ask folds onto that row, and only an agent-less pane renders
/// the bare ask card. `wired_lazy_kinds` gates the idle-instance synthesis (see
/// `agent_pane_for_pane`).
struct LazyAgentPaneProjection<'a> {
    wired_kinds: &'a [String],
    default_models: &'a BTreeMap<String, String>,
    pairings: Option<&'a LazyAgentPairingResult>,
}

fn rows_from_panes(
    agents: &[AgentState],
    needs_attention: &[FeedItem],
    resolver_working: &[FeedItem],
    panes: &[PaneRef],
    lazy_agents: LazyAgentPaneProjection<'_>,
    now: Timestamp,
) -> Vec<SidebarRow> {
    let mut rows = Vec::new();
    let mut bound_agents: BTreeSet<(AgentKind, AgentSessionId)> = BTreeSet::new();
    let standalone_items = standalone_items_by_pane(needs_attention, resolver_working, panes);
    let computed_pairings;
    let lazy_pairings = if let Some(pairings) = lazy_agents.pairings {
        pairings
    } else {
        computed_pairings = compute_lazy_agent_pairings(panes, agents);
        &computed_pairings
    };

    for pane in panes {
        let standalone_ask = standalone_items.get(&pane.pane_id).copied();
        if let Some(agent) = agent_for_pane(pane, agents, &bound_agents) {
            push_agent_row(
                &mut rows,
                &mut bound_agents,
                agent,
                pane,
                pane_ask(agent, standalone_ask, needs_attention, resolver_working),
                now,
            );
        } else if let Some(bind) = agent_pane_for_pane(
            pane,
            agents,
            lazy_pairings,
            &bound_agents,
            lazy_agents.wired_kinds,
            lazy_agents.default_models,
            now,
        ) {
            // The cwd relaxation of stamped-id binding. A lazy-registering
            // agent (Codex) can be present without a stamped session, and a
            // non-lazy agent can lose its stamp across a mux rebirth while its
            // process keeps running. `agent_pane_for_pane` owns the whole case:
            // an unstamped session binds the live agent pane in its worktree by
            // cwd, and a wired-but-unbound lazy pane (no session yet) renders as
            // an idle agent rather than a bare process row. Remote-control and
            // app-server broker host panes are filtered out upstream
            // (`with_live_panes`), so they never reach here.
            match bind {
                AgentPaneRow::Agent(agent) => push_agent_row(
                    &mut rows,
                    &mut bound_agents,
                    agent,
                    pane,
                    pane_ask(agent, standalone_ask, needs_attention, resolver_working),
                    now,
                ),
                AgentPaneRow::Idle(row) => {
                    // The synthesized idle row is the pane's card, so a frame-
                    // admitted standalone ask folds onto it exactly as it folds
                    // onto a bound agent's row.
                    let mut row = *row;
                    if let Some(ask) = standalone_ask {
                        fold_ask_onto_row(&mut row, ask);
                    }
                    rows.push(row);
                }
            }
        } else if let Some(item) = standalone_ask {
            rows.push(row_from_standalone_item(item, pane));
        } else if pane_command_is_known(pane) {
            rows.push(row_from_process(pane, now));
        }
        // else: a brand-new or raced pane whose command is still unknown after
        // frame rotation — the third honest-read guard. Presence without
        // identity folds no row until a read names it; the pane stays in the
        // published pane frame, so the sibling count and selection baseline see
        // it.
    }

    rows
}

/// The newest pending standalone (non-agent-hook) ask per frame-admitted pane.
/// Pane-keyed because the ask's card is the pane's card: one pane renders one
/// row, so two scripts asking from one pane collapse to the newest while the
/// older stays rollup metadata until that one resolves. An ask naming no pane,
/// or a pane absent from the frame, is dropped here — no live pane, no card.
fn standalone_items_by_pane<'a>(
    needs_attention: &'a [FeedItem],
    resolver_working: &'a [FeedItem],
    panes: &[PaneRef],
) -> HashMap<PaneId, &'a FeedItem> {
    let mut by_pane = HashMap::new();
    for item in needs_attention.iter().chain(resolver_working.iter()) {
        if item.source_kind == "agent-hook" {
            continue;
        }
        let Some(pane) = frame_pane_for_item(item, panes) else {
            continue;
        };
        by_pane
            .entry(pane.pane_id.clone())
            .and_modify(|current: &mut &'a FeedItem| {
                if item.updated_at > current.updated_at {
                    *current = item;
                }
            })
            .or_insert(item);
    }
    by_pane
}

fn frame_pane_for_item<'a>(item: &FeedItem, panes: &'a [PaneRef]) -> Option<&'a PaneRef> {
    let requested = item.pane.as_ref()?;
    panes
        .iter()
        .find(|pane| pane.pane_id == requested.pane_id && pane_start_matches(requested, pane))
}

/// The single pending ask folded onto an agent's pane row. A frame-admitted
/// standalone script/bridge ask naming the pane outranks the session's own
/// agent-hook ask: it blocks the pane's foreground right now, and the agent's
/// activity never settles it — unlike a native ask it clears only when the
/// request resolves. Without one, the session's most-relevant agent-hook ask
/// stands ([`most_relevant_ask`]).
fn pane_ask<'a>(
    agent: &AgentState,
    standalone_ask: Option<&'a FeedItem>,
    needs_attention: &'a [FeedItem],
    resolver_working: &'a [FeedItem],
) -> Option<&'a FeedItem> {
    standalone_ask.or_else(|| most_relevant_ask(agent, needs_attention, resolver_working))
}

/// Render `agent` on `pane`: mark it bound, project its row, overlay the live
/// pane cwd as the worktree fallback, attach the pane, and fold the caller-
/// resolved pending ask ([`pane_ask`]) — keeping the agent's identity and
/// capability line on the row instead of swapping in a bare ask card. Shared
/// by the two binds — the stamped-id match and the Codex daemon's cwd
/// fallback — so both render identically.
fn push_agent_row(
    rows: &mut Vec<SidebarRow>,
    bound: &mut BTreeSet<(AgentKind, AgentSessionId)>,
    agent: &AgentState,
    pane: &PaneRef,
    ask: Option<&FeedItem>,
    now: Timestamp,
) {
    bound.insert((agent.kind.clone(), agent.agent_id.clone()));
    let mut row = row_from_agent(agent, now);
    row.worktree_path = row.worktree_path.or_else(|| pane.cwd.clone());
    row.pane = Some(pane.clone());
    if let Some(ask) = ask {
        fold_ask_onto_row(&mut row, ask);
    }
    rows.push(row);
}

/// The agent's single most-relevant pending ask: the newest agent-hook ask that
/// names this session and that the agent has not already moved past. Asks
/// arrive newest-first, so the first match wins. Folding only one ask onto the
/// row is the read-side guarantee that a session never stacks more than one
/// attention row.
fn most_relevant_ask<'a>(
    agent: &AgentState,
    needs_attention: &'a [FeedItem],
    resolver_working: &'a [FeedItem],
) -> Option<&'a FeedItem> {
    needs_attention
        .iter()
        .chain(resolver_working.iter())
        .find(|item| {
            item.source_kind == "agent-hook"
                && item.source == agent.kind
                && agent_id_from_item(item).as_deref() == Some(agent.agent_id.as_str())
                && !agent_moved_past_ask(agent, item)
        })
}

/// True when the agent recorded progress activity *after* raising this ask — it
/// answered in its own UI and kept working, so the ask is settled and must not
/// re-raise the row to `waiting`. This is the read-side recovery for a native_ui
/// ask the agent never reports back through Rimz: the per-tool activity
/// heartbeat advances `last_activity` past the ask's `updated_at` as soon as the
/// agent runs its next tool. A bridge ask keeps the hook blocked, so the agent
/// emits no progress while it waits and this never fires for one mid-flight.
/// Sound only because a blocked agent's `last_activity` is its *own*: a
/// backgrounded subagent keeps emitting child-stamped events while the parent
/// blocks, and the adapters drop those from the lifecycle channel
/// (`resolve_root_identity`) — folded onto the parent they would advance it
/// past a pending ask and misfire this recovery.
fn agent_moved_past_ask(agent: &AgentState, ask: &FeedItem) -> bool {
    agent.last_activity > ask.updated_at
}

/// Overlay a pending ask onto its agent's pane row: the row keeps the agent's
/// identity and capability line but takes the ask's waiting status, request,
/// surface, resolver, options, and age.
fn fold_ask_onto_row(row: &mut SidebarRow, ask: &FeedItem) {
    row.last_activity = ask.updated_at;
    let Some(agent) = row.as_agent_mut() else {
        return;
    };
    agent.status = Some(AgentStatus::Waiting);
    // Phase is a head on Running — the reduced state's invariant — so the
    // waiting overlay drops it rather than carrying a stale Reasoning/Acting.
    agent.phase = TurnPhase::Idle;
    agent.request_id = Some(ask.request_id.clone());
    agent.surface = Some(ask.surface);
    agent.resolver = active_resolver_state(ask);
    agent.options = ask.options.clone();
}

fn build_worktree_groups_from_rows(
    mut rows: Vec<SidebarRow>,
    agents: &[AgentState],
    project_root: Option<&Path>,
    worktree_roots: &[PathBuf],
    root_class: RootClass,
    now: Timestamp,
    stalled_after_secs: u32,
) -> Vec<SidebarWorktreeGroup> {
    // Nest each subagent under its parent root row before grouping. This is the
    // one chokepoint every live (`rows_from_panes`) card flows through, so
    // nesting behaves identically for process, agent, and attention rows.
    attach_sub_agents(&mut rows, agents, now);
    // A delegating parent's work is its children's, so their activity advances
    // the parent row's displayed clock — before the projection below, so the
    // stall check reads the folded value too.
    fold_child_activity_onto_parents(&mut rows);
    // Project the displayed status now that each row knows its subagents (the
    // delegated-wait exemption) and the full agent set is in hand (the account
    // rate-limit verdict). The one place display state diverges from the rollup.
    project_display_status(&mut rows, agents, now, stalled_after_secs);
    // A worktree dir holds one branch at a time, so rows under one path
    // normally share a branch and group together — the agent and its shell
    // panes alike. Only when two live-admitted rows carry distinct branches
    // under one path do we split that path by branch, so a mislabeled
    // cross-branch section can't form while the common "agent + its shell" case
    // stays whole.
    let mut branches_per_path: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for row in &rows {
        if let (Some(path), Some(branch)) = (
            row.worktree_path.as_deref().filter(|path| !path.is_empty()),
            row.worktree_branch
                .as_deref()
                .filter(|branch| !branch.is_empty()),
        ) {
            branches_per_path.entry(path).or_default().insert(branch);
        }
    }
    let multi_branch_paths: BTreeSet<String> = branches_per_path
        .into_iter()
        .filter(|(_, branches)| branches.len() > 1)
        .map(|(path, _)| path.to_owned())
        .collect();

    let mut by_group: BTreeMap<String, (String, SidebarWorktreeKind, Vec<SidebarRow>)> =
        BTreeMap::new();
    for row in rows {
        let split_by_branch = row
            .worktree_path
            .as_deref()
            .is_some_and(|path| multi_branch_paths.contains(path));
        let (kind, key, label) = worktree_group_key(
            row.worktree_path.as_deref(),
            row.worktree_branch.as_deref(),
            split_by_branch,
            project_root,
            worktree_roots,
            root_class,
        );
        by_group
            .entry(key)
            .and_modify(|(_, _, rows)| rows.push(row.clone()))
            .or_insert_with(|| (label, kind, vec![row]));
    }

    let mut groups = by_group
        .into_iter()
        .map(|(key, (label, kind, mut rows))| {
            rows.sort_by(compare_rows);
            // Prefer a branch label over the path-basename seed: a group can mix
            // a branched agent row with a branchless process/attention row, and
            // every branched row in a group shares one branch (a path with two
            // is split above), so any branch is the right, order-independent
            // label. The root pod keeps its directory name — a non-repo root
            // has no branch, so a stale branched row must not rename the room.
            let label = if kind == SidebarWorktreeKind::Root {
                label
            } else {
                group_branch_label(&rows).unwrap_or(label)
            };
            let status_counts = status_counts(&rows);
            let total = rows.len();
            rows = capped_rows(rows);
            SidebarWorktreeGroup {
                key,
                label,
                kind,
                status_counts,
                hidden_count: total.saturating_sub(rows.len()),
                rows,
                diff_added: None,
                diff_removed: None,
                commits_ahead: None,
                commits_behind: None,
                trunk: None,
                clean: None,
            }
        })
        .collect::<Vec<_>>();
    groups.sort_by(compare_groups);
    groups
}

/// Nest each subagent under its parent root row. A subagent is a reduced
/// `AgentState` carrying `parent_agent_id`; it is paneless, so it built no row
/// of its own (`rows_from_panes` binds only stamped panes). This pass matches
/// each child to its parent row by
/// `(kind, parent_agent_id)` and pushes a compact summary onto it.
///
/// Retention is turn-scoped: a finished (success/failed) child stays listed
/// until its work predates the parent's *current* turn (`turn_started_at`,
/// advanced only by a turn start, the `TurnStarted` signal, never a turn end),
/// when it belongs to a past turn and is dropped. The generous
/// [`GHOST_SESSION_TTL_SECS`] backstop covers the no-turn-boundary case, so a
/// finished child cannot linger forever when the parent never recorded the next
/// turn start. A *running* child superseded by a newer parent turn, or silent
/// past that same backstop, is a ghost that never sent `Stop` — reaped so it
/// can't freeze the parent's delegated-wait head. A child whose parent row is
/// absent (parent ended, reaped, or has no live pane) is an orphan and never
/// renders. Survivors are deduped by child id so a child can never appear
/// twice, then ordered by creation time for a deterministic list.
pub(super) fn attach_sub_agents(rows: &mut [SidebarRow], agents: &[AgentState], now: Timestamp) {
    let parent_turn_start = |kind: &str, id: &str| -> Option<Timestamp> {
        agents
            .iter()
            .find(|a| a.kind == kind && a.agent_id == id)
            .and_then(|a| a.turn_started_at)
    };
    let idle_secs = |child: &AgentState| now.duration_since(child.last_activity).as_secs();
    for child in agents.iter().filter(|a| a.parent_agent_id.is_some()) {
        let Some(parent_id) = child.parent_agent_id.as_deref() else {
            continue;
        };
        let parent_turn_started_at = parent_turn_start(&child.kind, parent_id);
        let parent_has_turn_boundary = parent_turn_started_at.is_some();
        let superseded =
            parent_turn_started_at.is_some_and(|started| started > child.last_activity);
        let keep = if child.status == AgentStatus::Running {
            if superseded {
                warn!(
                    target: "rimz::agent::lifecycle",
                    kind = %child.kind,
                    parent = parent_id,
                    child = %child.agent_id,
                    "running subagent superseded by a newer parent turn — reaped",
                );
                false
            } else if idle_secs(child) >= GHOST_SESSION_TTL_SECS {
                warn!(
                    target: "rimz::agent::lifecycle",
                    kind = %child.kind,
                    parent = parent_id,
                    child = %child.agent_id,
                    "subagent stuck running with no Stop past the ghost TTL — reaped",
                );
                false
            } else {
                true
            }
        } else {
            // Finished: turn-scoped — kept until the parent's next turn
            // supersedes it (its work predates `turn_started_at`). The
            // generous ghost TTL is the backstop for a parent that never
            // recorded a turn boundary, so a finished child can never linger
            // forever in the gap.
            !superseded && (parent_has_turn_boundary || idle_secs(child) < GHOST_SESSION_TTL_SECS)
        };
        if !keep {
            continue;
        }
        // Attach to the parent row when one is present; an orphan (no parent
        // row) never renders — but log it, since a child that names a parent
        // with no row is an anomaly worth tracing.
        let parent = rows
            .iter_mut()
            .filter(|row| row.name == child.kind && row.id == parent_id)
            .find_map(SidebarRow::as_agent_mut);
        if let Some(parent) = parent {
            parent.sub_agents.push(sub_agent_from_state(child, now));
        } else {
            warn!(
                target: "rimz::agent::lifecycle",
                kind = %child.kind,
                parent = parent_id,
                child = %child.agent_id,
                "subagent names a parent with no row — orphan, not rendered",
            );
        }
    }
    for agent in rows.iter_mut().filter_map(SidebarRow::as_agent_mut) {
        if agent.sub_agents.is_empty() {
            continue;
        }
        // Dedup by child id (freshest activity wins) so the same logical child
        // can never appear twice and the `subagents (N)` count stays honest.
        agent
            .sub_agents
            .sort_by(|a, b| a.id.cmp(&b.id).then(b.last_activity.cmp(&a.last_activity)));
        agent.sub_agents.dedup_by(|a, b| a.id == b.id);
        // Display order: creation time ascending — the spawn order the parent
        // launched them in, stable across refreshes (an activity-keyed sort
        // reshuffled the list on every tick). A child with no reported start
        // time sorts after the dated ones; the id tiebreak keeps the whole
        // order deterministic.
        agent.sub_agents.sort_by(|a, b| {
            cmp_start_asc(a.started_at, b.started_at).then_with(|| a.id.cmp(&b.id))
        });
    }
}

/// Advance each parent row's *displayed* `last_activity` to its freshest
/// child's: a delegating parent is quiet because the work is its children's,
/// so the age clock stays honest while they tick and a parent whose child just
/// finished never false-stalls (the stall check reads the folded clock).
/// Display-only — the rollup's own `last_activity` is untouched, so
/// `agent_moved_past_ask` keeps reading the agent's own clock and a blocked
/// parent stays waiting. Two guards keep the frozen clocks frozen: an
/// attention row (`waiting`/`failed`) measures how long it has needed a human,
/// and a turn that died on a provider error keeps its own clock so a
/// still-ticking child can never mask the death certificate.
fn fold_child_activity_onto_parents(rows: &mut [SidebarRow]) {
    for row in rows.iter_mut() {
        let Some(agent) = row.as_agent() else {
            continue;
        };
        if agent.sub_agents.is_empty() {
            continue;
        }
        let Some(status) = agent.status else {
            continue;
        };
        if matches!(status, AgentStatus::Waiting | AgentStatus::Failed) {
            continue;
        }
        if crate::feed::is_turn_dead(status, agent.context.as_ref(), row.last_activity) {
            continue;
        }
        if let Some(freshest) = agent
            .sub_agents
            .iter()
            .map(|child| child.last_activity)
            .max()
        {
            row.last_activity = row.last_activity.max(freshest);
        }
    }
}

/// Project each agent row's *displayed* status from its raw lifecycle status,
/// liveness, live subagents, turn-error marker, and provider budget windows.
/// This is the one place display state diverges from the rollup truth kept in
/// `snapshot.agents`; a pending ask already folded `waiting` onto the row
/// upstream and always wins.
///
/// Rows reaching this projection have already been admitted through a live mux
/// pane by `rows_from_panes`/`with_live_panes`, so no second liveness check is
/// needed here.
///
/// - A paused-class turn-error marker means the agent actually stopped
///   mid-turn on a provider limit. It projects to `paused`; a rate-limit marker
///   whose spent windows have provably reset escalates to `failed` so the row
///   asks for a resume nudge.
/// - A `running` agent with a live subagent is *waiting on its children*, not
///   wedged — unless a paused marker above says the provider stopped the turn.
/// - A failed-class turn-error marker projects to `failed` at once and carries
///   the upstream error text as `turn_error_label`.
/// - A stalled `running` agent whose kind still has a spent, unreset window
///   projects to `paused`; any other stall projects to `failed`.
fn project_display_status(
    rows: &mut [SidebarRow],
    agents: &[AgentState],
    now: Timestamp,
    stalled_after_secs: u32,
) {
    let rate_limit_kinds = rate_limit_window_kinds(agents, now);
    for row in rows.iter_mut() {
        let row_name = row.name.clone();
        let last_activity = row.last_activity;
        let Some(agent) = row.as_agent_mut() else {
            continue;
        };
        let Some(status) = agent.status else {
            continue;
        };
        // A human-blocked `waiting` ask outranks every derived state.
        if status == AgentStatus::Waiting {
            continue;
        }
        let has_live_child = agent
            .sub_agents
            .iter()
            .any(|child| child.status == AgentStatus::Running);
        let active_error = active_turn_error(status, agent.context.as_ref(), last_activity);
        let projected = if let Some(error) = active_error.filter(|error| {
            matches!(
                error.class,
                TurnErrorClass::PausedRateLimit | TurnErrorClass::PausedOverloaded
            )
        }) {
            if error.class == TurnErrorClass::PausedRateLimit
                && rate_limit_kinds.reset.contains(row_name.as_str())
                && !rate_limit_kinds.spent.contains(row_name.as_str())
            {
                agent.turn_error_label = error.label.clone();
                AgentStatus::Failed
            } else {
                AgentStatus::Paused
            }
        } else if status == AgentStatus::Running && has_live_child {
            AgentStatus::Running
        } else if let Some(error) =
            active_error.filter(|error| error.class == TurnErrorClass::Failed)
        {
            agent.turn_error_label = error.label.clone();
            AgentStatus::Failed
        } else {
            let stalled = crate::feed::is_stalled(status, last_activity, now, stalled_after_secs);
            if stalled && rate_limit_kinds.spent.contains(row_name.as_str()) {
                AgentStatus::Paused
            } else if stalled {
                AgentStatus::Failed
            } else {
                status
            }
        };
        agent.status = Some(projected);
        if projected != AgentStatus::Running {
            // Phase is a head on Running — the reduced state's invariant —
            // so a Failed/Paused override drops it rather than carrying
            // a stale Reasoning/Acting onto a resting row.
            agent.phase = TurnPhase::Idle;
        }
    }
}

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

#[derive(Default)]
struct RateLimitKindSummary {
    /// Provider kinds with a currently-spent budget window. This is not a
    /// parking verdict by itself; it only powers the stalled-running fallback.
    spent: BTreeSet<AgentKind>,
    /// Provider kinds whose known spent windows have passed their reset
    /// instant. A rate-limit pause marker uses this as proof that at least one
    /// wait ended; projection still requires no unreset spent window before
    /// lifting the pause.
    reset: BTreeSet<AgentKind>,
}

fn rate_limit_window_kinds(agents: &[AgentState], now: Timestamp) -> RateLimitKindSummary {
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

/// Whether a window is spent and has not yet reset — the budget is gone *now*. A
/// spent window whose `resets_at` has already passed is stale, not limiting.
fn window_spent_unreset(window: &RateLimitWindow, now: Timestamp) -> bool {
    window.is_spent() && window.resets_at.is_none_or(|reset| reset > now)
}

/// A child `AgentState` projected to the compact summary the parent's expanded
/// card paints. The subagent's type rode in as its `task` on `SubagentStart`,
/// carried forward as identity by the reducer, so it stays labeled after it
/// finishes even when its `SubagentStop` omits the type. A child that somehow
/// reaches projection without a type is named by a short id placeholder, never
/// the provider `kind` (which would render as a phantom `claude`/`codex` row
/// indistinguishable from a real subagent), and the anomaly is logged. Elapsed
/// work is frozen at projection: a running child counts to `now`, a finished
/// one to its `last_activity` (which stops advancing), so the figure settles
/// when it ends.
pub(super) fn sub_agent_from_state(child: &AgentState, now: Timestamp) -> SidebarSubAgent {
    let name = child
        .task
        .clone()
        .filter(|task| !task.is_empty())
        .unwrap_or_else(|| {
            warn!(
                target: "rimz::agent::lifecycle",
                kind = %child.kind,
                child = %child.agent_id,
                "subagent has no type label — rendering a degraded placeholder",
            );
            degraded_subagent_label(&child.agent_id)
        });
    let elapsed_secs = child.subagent_started_at.map(|started| {
        let until = if child.status == AgentStatus::Running {
            now
        } else {
            child.last_activity
        };
        until.duration_since(started).as_secs().max(0)
    });
    SidebarSubAgent {
        id: child.agent_id.to_string(),
        name,
        status: child.status,
        phase: child.phase,
        task: child.task.clone(),
        model: child.model.clone(),
        effort: child.effort.clone(),
        description: child.subagent_description.clone(),
        total_tokens: child.total_tokens,
        elapsed_secs,
        started_at: child.subagent_started_at,
        last_activity: child.last_activity,
    }
}

/// A placeholder label for a subagent that reported no type — a short id prefix
/// so it reads as a distinct, traceable child rather than the provider kind.
fn degraded_subagent_label(agent_id: &str) -> String {
    let short = agent_id.split('-').next().unwrap_or(agent_id);
    let short = short.get(..8).unwrap_or(short);
    if short.is_empty() {
        "subagent".to_owned()
    } else {
        format!("subagent {short}")
    }
}

pub(super) fn row_from_agent(agent: &AgentState, now: Timestamp) -> SidebarRow {
    // `SidebarRow.status` is the *displayed* status. It starts as the raw rollup
    // value and is projected in `project_display_status` once the row knows its
    // subagents and its account's rate-limit budget (stall → `failed`,
    // spent-budget → `paused`); a pending ask folds `waiting` on upstream.
    // The rollup in `snapshot.agents` always keeps the true status.
    SidebarRow {
        id: agent.agent_id.to_string(),
        name: agent.kind.to_string(),
        pane: agent.pane.clone(),
        worktree_path: agent.worktree_path.clone(),
        worktree_branch: agent.worktree_branch.clone(),
        last_activity: agent.last_activity,
        card: RowCard::Agent(Box::new(AgentCard {
            status: Some(agent.status),
            phase: agent.phase,
            request_id: None,
            surface: None,
            task: agent.task.clone(),
            prompt: agent.prompt.clone(),
            model: agent.model.clone(),
            effort: agent.effort.clone(),
            context_pct: Some(agent.context_pct.unwrap_or(0)),
            context_window: agent_context_window(agent),
            total_tokens: agent.total_tokens,
            cache_read_input_tokens: agent.cache_read_input_tokens,
            fresh_input_tokens: agent.fresh_input_tokens,
            output_tokens: agent.output_tokens,
            todo_done: agent.todo_done,
            todo_total: agent.todo_total,
            context: agent.context.clone(),
            context_severity: None,
            registered_at: agent.registered_at,
            resolver: None,
            options: Vec::new(),
            sub_agents: Vec::new(),
            compacting: is_compacting(agent, now),
            compaction_count: agent.compaction_count,
            turn_error_label: None,
        })),
    }
}

/// Whether the agent is mid-compaction: it stamped `compacting_since` and the
/// marker is still fresh. The trailing compaction hook clears the stamp; this
/// window is the crash backstop so a missed terminator can't pulse the head
/// forever.
fn is_compacting(agent: &AgentState, now: Timestamp) -> bool {
    agent.compacting_since.is_some_and(|since| {
        now.duration_since(since).as_secs() < crate::feed::COMPACTING_WINDOW_SECS
    })
}

fn agent_context_window(agent: &AgentState) -> Option<u64> {
    agent.context_window.or_else(|| {
        crate::agents::descriptor_by_kind(agent.kind.as_str())
            .and_then(|descriptor| descriptor.default_context_window)
    })
}

/// A standalone attention row for a pending script/bridge ask on a pane no
/// agent row claims. The caller has already proven `pane` is present in the
/// current frame; the row refreshes its pane reference from that frame so
/// jumps, focus, view id, command, cwd, and process start all read from live
/// mux truth. Infallible by construction: both attention lists hold only
/// pending items (`build_with_agents` filters), and agent-hook asks fold onto
/// their session's row instead of standing alone.
fn row_from_standalone_item(item: &FeedItem, pane: &PaneRef) -> SidebarRow {
    debug_assert_eq!(item.status, FeedStatus::Pending);
    debug_assert_ne!(item.source_kind, "agent-hook");
    let id = agent_id_from_item(item).unwrap_or_else(|| item.request_id.to_string());
    SidebarRow {
        id,
        name: item.source.clone(),
        pane: Some(pane.clone()),
        worktree_path: item.worktree_path.clone().or_else(|| pane.cwd.clone()),
        worktree_branch: item.worktree_branch.clone(),
        last_activity: item.updated_at,
        card: RowCard::Agent(Box::new(AgentCard {
            status: Some(AgentStatus::Waiting),
            // A waiting row is blocked on the human, not reasoning — no turn phase.
            phase: TurnPhase::Idle,
            request_id: Some(item.request_id.clone()),
            surface: Some(item.surface),
            task: Some(item.title.clone()),
            prompt: None,
            model: None,
            effort: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            cache_read_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            todo_done: None,
            todo_total: None,
            context: None,
            context_severity: None,
            registered_at: None,
            resolver: active_resolver_state(item),
            options: item.options.clone(),
            sub_agents: Vec::new(),
            compacting: false,
            compaction_count: 0,
            turn_error_label: None,
        })),
    }
}

fn agent_id_from_item(item: &FeedItem) -> Option<String> {
    item.agent_session_id().map(ToOwned::to_owned)
}

fn active_resolver_state(item: &FeedItem) -> Option<SidebarResolverState> {
    if item.surface != Surface::Bridge || item.status != FeedStatus::Pending {
        return None;
    }
    let resolver_id = item.chain_active_resolver.clone().or_else(|| {
        item.chain
            .iter()
            .find(|step| step.state == ResolverStepState::Active)
            .map(|step| step.resolver_id.clone())
    })?;
    let display_name = item
        .chain
        .iter()
        .find(|step| step.resolver_id == resolver_id)
        .and_then(|step| step.display_name.clone());
    Some(SidebarResolverState {
        resolver_id,
        display_name,
        budget_until: item.chain_active_until,
    })
}

/// The branch shared by a group's branched rows, if any. Returns `None` for a
/// group with no branch information, leaving the caller's path-basename seed.
fn group_branch_label(rows: &[SidebarRow]) -> Option<String> {
    rows.iter()
        .find_map(|row| row.worktree_branch.as_deref().filter(|b| !b.is_empty()))
        .map(ToOwned::to_owned)
}

fn worktree_group_key(
    path: Option<&str>,
    branch: Option<&str>,
    split_by_branch: bool,
    project_root: Option<&Path>,
    worktree_roots: &[PathBuf],
    root_class: RootClass,
) -> (SidebarWorktreeKind, String, String) {
    let branch = branch.filter(|branch| !branch.is_empty());
    if let Some(path) = path.filter(|path| !path.is_empty()) {
        // A cwd belongs to the *deepest* group root that contains it: the room
        // root or any enumerated group root — a repo room's worktree checkouts
        // (`git worktree list`, including one parked outside `project_root`)
        // or a directory room's child repos. Keying on the matched root is
        // what folds every pane of one checkout into one pod. Two cases keep
        // per-path pods: a repo room's own checkout (so a nested worktree the
        // enumeration hasn't caught up with never folds into the main pod),
        // and a snapshot with no known root and no enumerated roots. A cwd
        // outside every root (a home shell, `/tmp`, CI) falls through to the
        // `external` catch-all.
        let cwd = Path::new(path);
        let matched = worktree_roots
            .iter()
            .map(PathBuf::as_path)
            .chain(project_root)
            .filter(|root| is_within(root, cwd))
            .max_by_key(|root| root.components().count());
        let per_path = match matched {
            Some(root) => project_root == Some(root) && root_class == RootClass::Repo,
            None => project_root.is_none() && worktree_roots.is_empty(),
        };
        if per_path {
            let label = branch
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| path_basename(cwd));
            // Disambiguate the key by branch only for a path that holds more
            // than one — a newline can appear in neither a path nor a branch, so
            // it is an unambiguous separator. `enrich_worktree_groups` recovers
            // the bare path from the key's first line, so the split never
            // breaks git reads.
            let key = match branch.filter(|_| split_by_branch) {
                Some(branch) => format!("{path}\n{branch}"),
                None => path.to_owned(),
            };
            return (SidebarWorktreeKind::Worktree, key, label);
        }
        if let Some(root) = matched {
            let root_key = root.to_string_lossy().into_owned();
            // The room root of a non-repo room: one name-only pod for panes at
            // the root and in non-repo subdirs. Branches never split or label
            // it — a non-repo root has no git story to disagree about.
            if project_root == Some(root) {
                let label = path_basename(root);
                return (SidebarWorktreeKind::Root, root_key, label);
            }
            let label = branch
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| path_basename(root));
            let key = match branch.filter(|_| split_by_branch) {
                Some(branch) => format!("{root_key}\n{branch}"),
                None => root_key,
            };
            return (SidebarWorktreeKind::Worktree, key, label);
        }
    }
    if let Some(branch) = branch {
        return (
            SidebarWorktreeKind::Worktree,
            format!("branch:{branch}"),
            branch.to_owned(),
        );
    }
    // Catch-all: untethered scripts/CI and out-of-project shells. `external`
    // is both the stable grouping key and the header label, so it reads as
    // "outside the project."
    (
        SidebarWorktreeKind::External,
        "external".to_owned(),
        "external".to_owned(),
    )
}

/// The display basename of a group root, falling back to the full path for a
/// root with no final component (`/`).
fn path_basename(root: &Path) -> String {
    root.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| root.to_string_lossy().into_owned())
}

/// True when `path` is `root` itself or nested under it, compared by path
/// components so `/home/marvinX` is not treated as under `/home/marvin`. This
/// is a lexical test on the raw cwd the mux reported — no filesystem
/// canonicalization — keeping the reducer pure. Used against both the project
/// root and each enumerated worktree root to decide a cwd's pod.
fn is_within(root: &Path, path: &Path) -> bool {
    let mut root_components = root.components();
    let mut path_components = path.components();
    loop {
        match (root_components.next(), path_components.next()) {
            (Some(r), Some(p)) if r == p => continue,
            (Some(_), _) => return false,
            (None, _) => return true,
        }
    }
}

fn status_counts(rows: &[SidebarRow]) -> Vec<SidebarStatusCount> {
    [
        AgentStatus::Waiting,
        AgentStatus::Failed,
        AgentStatus::Paused,
        AgentStatus::Success,
        AgentStatus::Running,
        AgentStatus::Idle,
    ]
    .into_iter()
    .filter_map(|status| {
        let count = rows
            .iter()
            .filter(|row| row.status() == Some(status))
            .count();
        (count > 0).then_some(SidebarStatusCount { status, count })
    })
    .collect()
}

fn refresh_overlay_group(group: &mut SidebarWorktreeGroup) {
    group.rows.sort_by(compare_rows);
    group.status_counts = status_counts(&group.rows);
    let total = group.rows.len().saturating_add(group.hidden_count);
    let rows = std::mem::take(&mut group.rows);
    group.rows = capped_rows(rows);
    group.hidden_count = total.saturating_sub(group.rows.len());
}

/// Trim a group's calm tail to `WORKTREE_ROW_CAP`, always keeping the rows that
/// need you (`waiting`/`failed`) and the focused pane. Because `idle` ranks
/// last among agents, it is the first calm bucket trimmed behind `+K more` —
/// by design: a parked, work-less agent is the least attention-hungry, and a
/// finished or working agent stays visible longer.
fn capped_rows(rows: Vec<SidebarRow>) -> Vec<SidebarRow> {
    let mut visible = Vec::new();
    for row in rows {
        if row.status().is_some_and(AgentStatus::is_actionable)
            || row.pane.as_ref().is_some_and(|pane| pane.is_focused)
            || visible.len() < WORKTREE_ROW_CAP
        {
            visible.push(row);
        }
    }
    visible
}

fn compare_rows(left: &SidebarRow, right: &SidebarRow) -> Ordering {
    // The final tiebreak is the stable `id` alone — never `name`, which mutates
    // through the session-name → task → prompt label ladder and would reshuffle
    // a bucket on every rename.
    row_rank(left)
        .cmp(&row_rank(right))
        .then_with(|| within_bucket(left, right))
        .then_with(|| left.id.cmp(&right.id))
}

/// Tiebreak two rows that share a status bucket (their ranks already tied).
///
/// Attention rows (`waiting`/`failed`/`paused`) sort longest-overdue-first:
/// a blocked or failed agent's `last_activity` is frozen, so this is both stable
/// and the triage order the `␣` "next attention" key promises. Calm rows
/// (`success`, `running`, `idle`) and bare process rows hold a stable spawn
/// order keyed on [`spawn_key`] — set-once and untouched by the activity
/// heartbeat — so a working agent never jumps just because it finished a tool,
/// and new agents append at the bottom of their bucket.
fn within_bucket(left: &SidebarRow, right: &SidebarRow) -> Ordering {
    if is_attention(left.status()) {
        left.last_activity.cmp(&right.last_activity)
    } else {
        cmp_start_asc(spawn_key(left), spawn_key(right))
    }
}

/// The row's durable spawn instant: the pane's process start when the backend
/// reports it (tmux always, Zellij only via the `/proc` agent-pane derivation),
/// else the session's `registered_at`. Both are set-once and immune to the
/// activity heartbeat, so the calm order is stable across refreshes and a
/// renamed session never reorders.
fn spawn_key(row: &SidebarRow) -> Option<Timestamp> {
    pane_start(row).or_else(|| row.as_agent().and_then(|agent| agent.registered_at))
}

fn is_attention(status: Option<AgentStatus>) -> bool {
    status.is_some_and(AgentStatus::is_attention)
}

fn pane_start(row: &SidebarRow) -> Option<Timestamp> {
    row.pane.as_ref().and_then(|pane| pane.pane_process_start)
}

/// Ascending by start time, but a missing start sorts *last* — the opposite of
/// `Option::cmp`, which would float paneless rows (script asks, detached
/// sessions) to the top of their bucket.
fn cmp_start_asc(left: Option<Timestamp>, right: Option<Timestamp>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn compare_groups(left: &SidebarWorktreeGroup, right: &SidebarWorktreeGroup) -> Ordering {
    // A worktree floats by its most-urgent member: a `waiting`-topped group sits
    // above a `failed`-topped one, above the calm groups. Among same-tier groups
    // the `external` catch-all sorts after project worktrees; then both hold a
    // stable order keyed on the earliest-spawned member, then label. The external
    // group therefore only rises out of the tail when it holds a `waiting` or
    // `failed` agent — the tier carries that, no separate predicate needed.
    group_tier(left)
        .cmp(&group_tier(right))
        .then_with(|| group_is_external(left).cmp(&group_is_external(right)))
        .then_with(|| cmp_start_asc(group_earliest_spawn(left), group_earliest_spawn(right)))
        .then_with(|| left.label.cmp(&right.label))
}

/// The most-urgent member's *group* tier. `rows` is already sorted by
/// `compare_rows` and the cap never hides `waiting`/`failed`, so `rows.first()`
/// is the true top; an empty group ranks last. Unlike `row_rank`, every calm
/// status collapses to one tier: a calm group's position must not leapfrog a
/// sibling just because its top row flipped success↔running↔idle — calm groups
/// hold the stable earliest-pane order, and only genuine attention reorders.
fn group_tier(group: &SidebarWorktreeGroup) -> u8 {
    match group.rows.first().map(SidebarRow::status) {
        Some(Some(AgentStatus::Waiting)) => 0,
        Some(Some(AgentStatus::Failed)) => 1,
        Some(Some(AgentStatus::Paused)) => 2,
        // success / running / idle — one calm tier.
        Some(Some(_)) => 3,
        // Process-only group.
        Some(None) => 4,
        None => u8::MAX,
    }
}

fn group_is_external(group: &SidebarWorktreeGroup) -> bool {
    group.kind == SidebarWorktreeKind::External
}

/// The group's earliest member [`spawn_key`] — the same durable key the
/// within-bucket calm tiebreak uses, so group order survives a backend that
/// reports no pane starts (Zellij) instead of degrading to the label.
fn group_earliest_spawn(group: &SidebarWorktreeGroup) -> Option<Timestamp> {
    group.rows.iter().filter_map(spawn_key).min()
}

fn row_rank(row: &SidebarRow) -> u8 {
    match row.status() {
        Some(status) => status_rank(status),
        None => 7,
    }
}

fn status_rank(status: AgentStatus) -> u8 {
    // Actionable attention (`waiting`/`failed`) leads; `paused` sits just
    // under it — attention-class, but parked with nothing to do but wait, so it
    // ranks below a real failure and above calm. Among the calm states `idle`
    // ranks *last*: a fresh agent registers idle, so idle-at-the-bottom makes a
    // new card append at the bottom of the calm region every time — it never
    // lands above finished or working agents only to drop on its first prompt.
    // Finished work (`success`) reads first — it has a result for you — then
    // live work, then the parked idle tail the per-worktree cap trims first.
    match status {
        AgentStatus::Waiting => 0,
        AgentStatus::Failed => 1,
        AgentStatus::Paused => 2,
        AgentStatus::Success => 3,
        AgentStatus::Running => 4,
        AgentStatus::Idle => 5,
    }
}

#[cfg(test)]
mod tests;
