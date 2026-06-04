//! Sidebar view-model assembly: the `Sidebar*` renderer contract and the
//! grouping, ranking, capping, and status projection that fills it.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::fold::agent_rollup_with_carryover;
use super::panes::{
    LazyAgentRow, SidebarOwnView, agent_for_pane, is_daemon_mode_codex, lazy_agent_for_pane,
};
use super::process::{program_label, row_from_process};
use crate::agent_activity::AgentActivity;
use crate::agents::{AgentAccount, AgentContext, RateLimitWindow, SpendTally};
use crate::feed::{
    AgentState, AgentStatus, FeedItem, FeedKind, FeedStatus, PaneRef, ResolverStepState, Surface,
};
use crate::ids::{PaneId, RequestId, ResolverId, WorkspaceId};
use crate::ledger::agent_context::AgentContextRecord;
use crate::ledger::event_log::{self};
use crate::ledger::subagent_context::SubagentContextRecord;
use crate::schema::event::EventEnvelope;

/// Sidebar view-model. The worktree groups are the renderer contract:
/// grouping, attention ranking, caps, status tallies, and row metadata are
/// resolved here so renderers only paint semantics into glyphs.
///
/// `needs_attention` and `resolver_working` are load-bearing: they are the
/// reducer inputs the group rebuild reads when panes are folded in
/// (`with_live_panes`) or dead agents are reaped (`drop_dead_agents_with`).
/// The sidebar renderer reads `worktree_groups`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SidebarSnapshot {
    pub workspace_id: WorkspaceId,
    pub display_name: String,
    pub generated_at: Timestamp,
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
    /// ([`crate::agents::registers_session_lazily`] ∩ installed). Gates the
    /// idle-instance synthesis in `rows_from_panes`: a launched-but-unbound pane of
    /// such an agent has no ledger session yet (it registers lazily on the first
    /// turn), and only a wired agent can ever report status, so only a wired lazy
    /// agent's bare pane is promoted from a process row to an idle agent. Codex is
    /// the only such agent today. Environment, not ledger — the pure reducer leaves
    /// it empty; the `rimz sidebar snapshot` CLI and consumer enrichment fill it
    /// before folding live panes. The placeholder/persisted snapshot keeps it empty
    /// (a process row).
    #[serde(default)]
    pub wired_lazy_kinds: Vec<String>,
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
    /// daemon tab remains. The daemon view's own sidebar reads it (gated by
    /// `SidebarOwnView::own_view_is_daemon` and a latch) to detach the client,
    /// leaving the background session and its daemons alive. Like `own_view`,
    /// this is live-pane state the pure reducer can't read, so the reducer and
    /// the placeholder/persisted snapshot leave it `false`; the `rimz sidebar
    /// snapshot` CLI fills it from the live pane list.
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
    /// Codex and Pi fleet-wide). Built on the producer by the `rimz sidebar
    /// snapshot` spending enrichment (`cli::sidebar::compute_fleet_spending` then
    /// `apply_spending`, via [`crate::agents::spending::compute_spending`]);
    /// `None` until the cache is seeded (the first producer tick after startup)
    /// or when nothing has been recorded. The cockpit reads `today` (sessions,
    /// the token split, and the count-up `$`); the fleet ledger reads the
    /// trailing `week` and `month` rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_tally: Option<SpendTally>,
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
    Worktree,
    Workspace,
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
    /// default; `origin/` stripped for display). Names the `≡` landed marker —
    /// a worktree with zero commits ahead and a zero diff renders `≡ <trunk>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trunk: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SidebarStatusCount {
    pub status: AgentStatus,
    pub count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidebarRowKind {
    /// An agent session: a live pane it stamped, or — in the no-pane rollup — a
    /// session row. A standalone script/bridge ask reuses this kind; it renders
    /// the same single line when no capability fields are set.
    Agent,
    /// A live pane with no agent bound to it: a shell, an editor, `git`.
    Process,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SidebarRow {
    pub row_kind: SidebarRowKind,
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<AgentStatus>,
    /// The running turn is still in its pre-edit reasoning phase: the renderer
    /// paints the thinking sparkle instead of the working spinner. A transient
    /// head like `compacting`, never a status bucket of its own. Always `false`
    /// for process rows and outside `Running`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub thinking: bool,
    pub pane: Option<PaneRef>,
    pub request_id: Option<RequestId>,
    pub surface: Option<Surface>,
    pub task: Option<String>,
    /// The session's latest user prompt, carried forward from `AgentState`. The
    /// renderer labels an unnamed session by it once `task` clears on idle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Context-window % gauge value (0..=100). Agent rows default this to
    /// `Some(0)` so renderers always draw the started-session gauge; transcript
    /// usage only upgrades the meter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_pct: Option<u8>,
    /// The model's context window in tokens, the identity line's `258k`/`1M`
    /// token. Hook-derived fallback; the renderer prefers the fresher
    /// `context.tokens.context_window_size` when an out-of-band source reports
    /// it. `None` for process rows and before any source has named it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub todo_done: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub todo_total: Option<u32>,
    /// The session's rich enrichment (cost, token breakdown, rate-limit windows,
    /// session name), copied from `AgentState.context` so the renderer reads one
    /// struct instead of cross-referencing `agents[]`. Source-agnostic: Claude
    /// fills it from its statusline, Codex from the app-server (rate-limit
    /// windows, model display name, effort, version). Display-only; `None` for
    /// process rows and any agent with no out-of-band source, where the scalar
    /// `model`/`effort`/`context_pct`/`total_tokens` are the fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<AgentContext>,
    pub worktree_path: Option<String>,
    pub worktree_branch: Option<String>,
    pub last_activity: Timestamp,
    pub resolver: Option<SidebarResolverState>,
    pub options: Vec<String>,
    /// Subagents this agent spawned this turn (Claude Task children, Codex
    /// threads), nested under the parent at projection time. Paneless, so they
    /// never render as their own row; the sidebar lists them inside the parent's
    /// expanded card. Empty for every non-parent row.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sub_agents: Vec<SidebarSubAgent>,
    /// A `process` row whose foreground command is genuine work (a build, a
    /// test, a script) rather than a bare shell or interactive TUI — so the
    /// renderer can give it the running spinner in a dim tone. Always `false`
    /// for agent rows and for idle shells/editors; never enters `status_counts`
    /// (a process row keeps `status: None`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub process_active: bool,
    /// The full foreground command of an *active* `process` row (`sudo npm install
    /// -g @openai/codex`, `cargo build --release`), shown dim on the row's second
    /// line beneath the shell anchor on `name`. `None` for idle process rows —
    /// where the single label already says everything — and for every agent row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_detail: Option<String>,
    /// The agent is condensing its context window right now (Claude `PreCompact`,
    /// Codex `SessionStart:compact`). A short-lived transient the renderer paints
    /// as a pulsing head over the base status; never a status bucket of its own.
    /// Always `false` for process rows.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub compacting: bool,
    /// The agent parked its last turn on still-in-flight background work
    /// (Claude Code v2.1.145+) rather than ending it. The row stays `Running`;
    /// the renderer paints a distinct secondary marker so it reads as "working
    /// in the background" without a false `success` and without overwriting the
    /// activity description. Always `false` for process rows.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub parked_on_background: bool,
    /// Why the displayed status escalated to `failed` when the agent's latest
    /// turn died on a provider API error with no `Stop` hook — the upstream
    /// error text ("API Error: Overloaded") from the transcript-tail marker.
    /// Set only by the turn-death projection (`project_display_status`), so it
    /// is present exactly while that escalation holds; the renderer paints it
    /// as the card's dim line-2 body. Always `None` for process rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_error_label: Option<String>,
}

/// A compact summary of a child agent, nested under its parent's row. The
/// expanded list paints its identity and live status, and — when Claude's
/// `subagentStatusLine` has reported them — what the parent asked it to do, what
/// it has spent, and how long it has run. The enrichment fields stay `None` for a
/// Codex child or before the first render, and the card degrades to the bare
/// type line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SidebarSubAgent {
    pub id: String,
    /// The subagent's type (`Explore`, `review`, …), from the `SubagentStart`
    /// task descriptor; falls back to a short degraded id when none was
    /// reported.
    pub name: String,
    pub status: AgentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// What the parent asked this child to do (`subagentStatusLine` description),
    /// painted after the type on the first row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Cumulative tokens the child has spent (`subagentStatusLine` `tokenCount`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    /// Wall-clock seconds the child has worked: `now − started_at` while running,
    /// frozen at `last_activity − started_at` once it finishes. `None` when no
    /// start time was reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_secs: Option<i64>,
    pub last_activity: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SidebarResolverState {
    pub resolver_id: ResolverId,
    pub display_name: Option<String>,
    pub budget_until: Option<Timestamp>,
}

impl SidebarSnapshot {
    pub fn build(
        workspace_id: WorkspaceId,
        items: Vec<FeedItem>,
        events: Vec<EventEnvelope>,
    ) -> Self {
        Self::build_with_carryover(workspace_id, items, events, Vec::new())
    }

    /// Build a snapshot, folding `carryover_agents` into the agent rollup so
    /// pre-rotation observations survive event-log archiving. Live events
    /// with a newer `last_seen` override the carryover.
    pub fn build_with_carryover(
        workspace_id: WorkspaceId,
        items: Vec<FeedItem>,
        events: Vec<EventEnvelope>,
        carryover_agents: Vec<AgentState>,
    ) -> Self {
        let agents = agent_rollup_with_carryover(&events, carryover_agents);
        Self::build_with_agents(workspace_id, items, agents)
    }

    pub fn build_with_agents(
        workspace_id: WorkspaceId,
        mut items: Vec<FeedItem>,
        agents: Vec<AgentState>,
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
        // The pure reducer has no project root or worktree set, so every cwd
        // keeps per-path grouping here; callers that know them re-fold via
        // `with_project_root` / `with_worktree_roots`.
        let worktree_groups =
            build_worktree_groups(&agents, &needs_attention, &resolver_working, None, &[]);

        Self {
            workspace_id,
            display_name,
            generated_at: Timestamp::now(),
            worktree_groups,
            needs_attention,
            resolver_working,
            agents,
            agent_hooks_ready: false,
            wired_lazy_kinds: Vec::new(),
            own_view: None,
            only_daemon_view_remains: false,
            project_root: None,
            worktree_roots: Vec::new(),
            sidebar: crate::config::SidebarConfig::default(),
            providers: Vec::new(),
            value_tally: None,
            reflects_log: None,
        }
    }

    /// Re-fold the worktree groups from the current agents, attention/working
    /// sets, and project root. Called after any mutation of `self.agents`.
    fn rebuild_groups(&mut self) {
        self.worktree_groups = build_worktree_groups(
            &self.agents,
            &self.needs_attention,
            &self.resolver_working,
            self.project_root.as_deref(),
            &self.worktree_roots,
        );
    }

    /// Record the project root and re-fold groups so a cwd that is neither under
    /// it nor inside one of the repo's worktrees lands in the `external`
    /// catch-all instead of its own pod. Callers set this from the workspace
    /// record after construction (the reducer can't read it), mirroring how
    /// `display_name` is filled.
    pub fn with_project_root(mut self, project_root: Option<PathBuf>) -> Self {
        self.project_root = project_root;
        self.rebuild_groups();
        self
    }

    /// Record the repo's worktree checkout roots and re-fold groups so a
    /// worktree parked *outside* `project_root` still earns its own pod rather
    /// than folding into `external`. Like `with_project_root`, the
    /// `rimz sidebar snapshot` CLI fills this from `git worktree list` after
    /// construction; the pure path leaves it empty.
    pub fn with_worktree_roots(mut self, worktree_roots: Vec<PathBuf>) -> Self {
        self.worktree_roots = worktree_roots;
        self.rebuild_groups();
        self
    }

    /// Attach each session's rich statusline context to its `AgentState` by
    /// `(kind, agent_id)`, then re-fold groups so the rows carry it for the
    /// renderer (`SidebarRow.context`). Context is display-only — it never
    /// changes ranking, since `last_activity` is untouched — but rows are built
    /// from the agents, so a rebuild is what moves the enrichment onto them. The
    /// live path also rebuilds again under the pane overlay; this rebuild is
    /// what carries context in the no-pane fallback. A context whose session is
    /// absent from the (already reaped) rollup is dropped — the session is gone,
    /// so its context is just history. Records carry no identity of their own;
    /// the key they're filed under is authority.
    pub fn with_agent_context(mut self, records: Vec<AgentContextRecord>) -> Self {
        if records.is_empty() {
            return self;
        }
        let mut by_key: BTreeMap<(String, String), _> = records
            .into_iter()
            .map(|record| ((record.kind, record.agent_id), record.context))
            .collect();
        let mut changed = false;
        for agent in &mut self.agents {
            if let Some(context) = by_key.remove(&(agent.kind.clone(), agent.agent_id.clone())) {
                agent.context = Some(context);
                changed = true;
            }
        }
        if changed {
            self.rebuild_groups();
        }
        self
    }

    /// Attach each child's `subagentStatusLine` enrichment (description, token
    /// count, start time) to its `AgentState` by `(kind, agent_id)`, then rebuild
    /// so the projection picks it up. It must land on the `AgentState`, not the
    /// already-projected `SidebarSubAgent`: the rebuild re-runs `attach_sub_agents`
    /// → `sub_agent_from_state`, which would discard anything written on the
    /// projection. `token_count` claims the otherwise-unused `total_tokens` slot
    /// (a paneless child reads no transcript). Display-only, like
    /// [`with_agent_context`](Self::with_agent_context) — it never touches
    /// `last_activity`, so ranking is untouched. A record whose child is absent
    /// from the rollup is dropped; the key it is filed under is authority.
    pub fn with_subagent_context(mut self, records: Vec<SubagentContextRecord>) -> Self {
        if records.is_empty() {
            return self;
        }
        let mut by_key: BTreeMap<(String, String), _> = records
            .into_iter()
            .map(|record| ((record.kind, record.agent_id), record.context))
            .collect();
        let mut changed = false;
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
                changed = true;
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
        if changed {
            self.rebuild_groups();
        }
        self
    }

    /// Apply best-effort process liveness to agent overlays that published a
    /// PID. Hook protocols do not all expose a session-exit event; when a hook
    /// command can record the agent process identity, the sidebar uses it to
    /// suppress stale ledger overlays without scraping pane contents.
    pub fn drop_dead_agents_with(&mut self, mut is_alive: impl FnMut(u32, Option<&str>) -> bool) {
        let previous_len = self.agents.len();
        self.agents.retain(|agent| {
            if let Some(owner) = &agent.runtime_owner {
                return is_alive(owner.pid, owner.process_start.as_deref());
            }
            agent
                .agent_pid
                .is_none_or(|pid| is_alive(pid, agent.agent_process_start.as_deref()))
        });
        if self.agents.len() != previous_len {
            self.rebuild_groups();
        }
    }

    /// Reap daemon-mode Codex sessions the per-user app-server daemon no longer
    /// holds in memory. A daemon-backed session records the shared daemon's pid,
    /// not its own CLI's, so process liveness — which keeps it while the daemon
    /// lives ([`drop_dead_agents_with`]) — can never reap it. Without this a closed
    /// remote-control conversation lingers as a ghost and binds its stale status,
    /// model, tokens, and pending ask onto a live `codex` pane by cwd
    /// ([`lazy_agent_for_pane`]).
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
        let previous_len = self.agents.len();
        self.agents.retain(|agent| {
            let reapable =
                is_daemon_mode_codex(agent, daemon_pids) && !loaded.contains(&agent.agent_id);
            !reapable
        });
        if self.agents.len() != previous_len {
            self.rebuild_groups();
        }
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
    pub fn reap_stale_sessions(&mut self, now: Timestamp) {
        let previous_len = self.agents.len();
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
        if self.agents.len() != previous_len {
            self.rebuild_groups();
        }
    }

    /// Whether every live, non-sidebar view in `panes` is the `rimzd` daemon
    /// view — i.e. the user has nothing left but the managed daemon tab. A view
    /// is a *daemon* view iff, after dropping its sidebar pane, it is non-empty
    /// and every remaining pane is a managed host
    /// ([`crate::remote_control::pane_is_host`]); a *working* view iff it holds
    /// any non-sidebar, non-host pane. A sidebar-only view (a working tab
    /// mid-self-close) counts as neither, so it neither trips nor blocks the
    /// signal. Returns `false` for an empty or not-yet-born session (no daemon
    /// view), so the renderer never detaches at startup.
    ///
    /// Keys on `view_id` + `pane_is_host`, never on `view_name`, so it behaves
    /// identically on Zellij (where `list_panes` leaves `view_name` `None`) and
    /// tmux (where it carries the window name).
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
                .is_some_and(|command| program_label(command) == "rimz-sidebar");
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
        let panes = panes
            .into_iter()
            .filter(|pane| exclude.is_none_or(|excluded| pane.pane_id != *excluded))
            .filter(|pane| {
                pane.command
                    .as_deref()
                    .is_none_or(|command| program_label(command) != "rimz-sidebar")
            })
            // The remote-control host is ambient infrastructure, not work: it no
            // longer renders as a row. Its presence surfaces as the `⇅ rc` flag
            // on the provider dashboard block instead, so drop its pane here.
            .filter(|pane| !crate::remote_control::pane_is_host(pane))
            .collect::<Vec<_>>();
        self.worktree_groups = build_worktree_groups_with_panes(
            &self.agents,
            &self.needs_attention,
            &self.resolver_working,
            &panes,
            self.project_root.as_deref(),
            &self.worktree_roots,
            &self.wired_lazy_kinds,
        );
        self
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
        let mut changed = false;
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
                changed = true;
            }
        }
        if changed {
            self.rebuild_groups();
        }
        self
    }

    /// Fold the agent rollup into per-provider dashboard blocks — one per agent
    /// kind, plus one for any provider with no active session this run that is
    /// either logged in or has recorded spend (an account-only block, so the
    /// dashboard shows your accounts, budgets, and fleet history between turns).
    /// Sums each kind's spend, tokens, and edited lines; takes the plan, version,
    /// and rate-limit windows from the freshest session (account state is shared,
    /// so the latest reading is truest). `probed_accounts` carries out-of-band
    /// login facts the context cannot (Claude's `auth status`, Codex's
    /// `auth.json`), preferred only when the freshest context has none — and a kind
    /// whose only signal is such a login, or whose only signal is recorded spend in
    /// `provider_spending`, still earns a block;
    /// `remote_control` carries the per-kind `⇅ rc` flag. Styling (emblem, color,
    /// name) resolves from `self.sidebar.providers` over the built-in defaults, so
    /// the renderer gets a ready-to-paint block. Capped to `max_provider_blocks`,
    /// ordered by spend. Producer-only: the pure reducer leaves `providers` empty.
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
            if !kinds.iter().any(|known| known == &agent.kind) {
                kinds.push(agent.kind.clone());
            }
        }
        // A provider that is logged in but has no active session this run still
        // earns a block, so the dashboard shows your accounts and budgets between
        // turns — fold in every probed-account kind not already covered.
        for kind in probed_accounts.keys() {
            if !kinds.iter().any(|known| known == kind) {
                kinds.push(kind.clone());
            }
        }
        // A provider with recorded spend earns a block too, so its fleet history
        // shows even with no live session and no probed login this run.
        for (kind, tally) in provider_spending {
            if !tally.is_zero() && !kinds.iter().any(|known| known == kind) {
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
            // Nothing to show without a session, a logged-in account, or recorded
            // spend; an idle provider with any of the three falls through to a
            // minimal block.
            let has_spend = provider_spending
                .get(&kind)
                .is_some_and(|tally| !tally.is_zero());
            if sessions.is_empty() && !probed_accounts.contains_key(&kind) && !has_spend {
                continue;
            }

            // The freshest context wins the account-scoped facts (plan, version)
            // — every session shares one account.
            let freshest = sessions
                .iter()
                .filter_map(|agent| agent.context.as_ref())
                .max_by_key(|context| context.observed_at);
            let version = freshest.and_then(|context| context.agent_version.clone());
            // The budget windows are account-scoped too, but the *freshest*
            // session is not the truest reading: parallel sessions report the same
            // window at slightly different instants, so "freshest wins" flips
            // between ticks and the bar flickers. Instead, pick each window stably
            // across every session, grouped by duration — drop readings whose reset
            // already passed (stale), then keep the most-drained survivor (most
            // conservative). Same inputs always yield the same bars, regardless of
            // which session reported last.
            let now = Timestamp::now();
            let windows = stable_windows(
                sessions
                    .iter()
                    .filter_map(|agent| agent.context.as_ref()?.rate_limits.as_ref())
                    .flat_map(|limits| limits.windows.iter().cloned()),
                now,
            );
            let has_windows = !windows.is_empty();

            let account = freshest
                .and_then(|context| context.account.clone())
                .or_else(|| probed_accounts.get(&kind).cloned());
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

        // Most spend first, then kind for a stable order; cap the panel height.
        // Rank by today's JSONL spend so the provider you are actively spending
        // on floats up, and a token-only provider (Codex) ranks on the same
        // transcript-derived footing as a live-cost one.
        panels.sort_by(|left, right| {
            right
                .rank_cost()
                .partial_cmp(&left.rank_cost())
                .unwrap_or(Ordering::Equal)
                .then_with(|| left.kind.cmp(&right.kind))
        });
        panels.truncate(self.sidebar.max_provider_blocks);
        self.providers = panels;
        self
    }
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

/// Built-in `(product_name, art lines, color)` for a provider kind, used when
/// the per-machine config overrides none of them. Ships the brand emblems and
/// colors: Claude clay (173), Codex blue (32), Pi forest green (28).
fn default_provider_style(kind: &str) -> (String, Vec<String>, u8) {
    let lines = |art: &str| art.lines().map(ToOwned::to_owned).collect::<Vec<_>>();
    match kind {
        "claude" => (
            "Claude".to_owned(),
            lines(" ▐▛███▜▌\n▝▜█████▛▘\n  ▘▘ ▝▝"),
            173,
        ),
        "codex" => (
            "Codex".to_owned(),
            lines(" ▗▛███▜▖\n ▜▌ ▚ ▐▛\n ▝▀▀▀▀▀▘"),
            38,
        ),
        "pi" => ("Pi".to_owned(), lines(" ▗▛████▜▖\n  ▐▌  ▐▌\n  ▝▘  ▝▘"), 28),
        other => (provider_title_case(other), Vec::new(), 244),
    }
}

/// Format a raw provider plan tier into its brand label: Claude's tiers prefix
/// `Claude` (`max` → `Claude Max`), Codex's prefix `ChatGPT` (`pro` → `ChatGPT
/// Pro`); any other provider just title-cases the tier.
fn format_plan_label(kind: &str, raw: &str) -> String {
    let tier = provider_title_case(raw);
    match kind {
        "claude" => format!("Claude {tier}"),
        "codex" => format!("ChatGPT {tier}"),
        _ => tier,
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

/// Age in seconds after which a *finished* (idle/success) subagent clears from
/// its parent's expanded list even when no fresh parent turn boundary has
/// arrived. The turn boundary is the primary signal, but a parent that never
/// re-prompts would otherwise pin a finished child forever — this is the
/// backstop that closes that gap. Short: a finished child is pure history.
const SUBAGENT_FINISHED_TTL_SECS: i64 = 5 * 60;

fn agent_is_pidless(agent: &AgentState) -> bool {
    agent.runtime_owner.is_none() && agent.agent_pid.is_none()
}

fn session_age_secs(now: Timestamp, agent: &AgentState) -> i64 {
    now.duration_since(agent.last_activity).as_secs()
}

/// True when reaping `older` cannot drop a concurrently-live agent: either it
/// never stamped a pane, or it stamped the very pane `newer` now occupies (a
/// relaunch in place). An older session holding its own distinct pane is a
/// separate live agent and is kept.
fn older_yields_pane(older: &AgentState, newer: &AgentState) -> bool {
    match older.pane.as_ref() {
        None => true,
        Some(older_pane) => newer
            .pane
            .as_ref()
            .is_some_and(|newer_pane| newer_pane.pane_id == older_pane.pane_id),
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

fn build_worktree_groups(
    agents: &[AgentState],
    needs_attention: &[FeedItem],
    resolver_working: &[FeedItem],
    project_root: Option<&Path>,
    worktree_roots: &[PathBuf],
) -> Vec<SidebarWorktreeGroup> {
    let rows = rows_from_ledger(agents, needs_attention, resolver_working);
    build_worktree_groups_from_rows(rows, agents, project_root, worktree_roots)
}

/// One pane = one row, by construction. Every live pane anchors exactly one
/// row: it binds the unique agent that stamped this pane id — rendering that
/// agent with its single most-relevant pending ask folded in — or, with no such
/// agent, renders as a plain process row. Agents with no live pane (ghosts,
/// sub-agents, a relaunch the reaper has not yet collapsed) do not render, so a
/// dead session can never resurrect a row or latch onto a stranger's pane. The
/// one exception is a pane-less lazy-registering agent (Codex) whose session
/// arrives unstamped from the app-server daemon: it binds the live agent pane in
/// its own worktree (`lazy_agent_for_pane`), and a wired such pane with no session
/// yet renders idle rather than as a process row. The only truly paneless rows are
/// standalone script/bridge asks, which no agent session raised. `wired_lazy_kinds`
/// gates the idle-instance synthesis (see `lazy_agent_for_pane`).
fn build_worktree_groups_with_panes(
    agents: &[AgentState],
    needs_attention: &[FeedItem],
    resolver_working: &[FeedItem],
    panes: &[PaneRef],
    project_root: Option<&Path>,
    worktree_roots: &[PathBuf],
    wired_lazy_kinds: &[String],
) -> Vec<SidebarWorktreeGroup> {
    build_worktree_groups_from_rows(
        rows_from_panes(
            agents,
            needs_attention,
            resolver_working,
            panes,
            wired_lazy_kinds,
        ),
        agents,
        project_root,
        worktree_roots,
    )
}

fn rows_from_panes(
    agents: &[AgentState],
    needs_attention: &[FeedItem],
    resolver_working: &[FeedItem],
    panes: &[PaneRef],
    wired_lazy_kinds: &[String],
) -> Vec<SidebarRow> {
    let mut rows = Vec::new();
    let mut bound_agents: BTreeSet<(String, String)> = BTreeSet::new();

    for pane in panes {
        if let Some(agent) = agent_for_pane(pane, agents, &bound_agents) {
            push_agent_row(
                &mut rows,
                &mut bound_agents,
                agent,
                pane,
                needs_attention,
                resolver_working,
            );
        } else if let Some(bind) =
            lazy_agent_for_pane(pane, agents, &bound_agents, wired_lazy_kinds)
        {
            // The lazy-agent relaxation of stamped-id binding. A lazy-registering
            // agent (Codex) can be present without a stamped session — it registers
            // lazily and routes hooks through the pane-less app-server — so it can't
            // bind through `agent_for_pane`. `lazy_agent_for_pane` owns the whole
            // case: an unstamped session binds the live agent pane in its worktree
            // by cwd, and a wired-but-unbound pane (no session yet) renders as an
            // idle agent rather than a bare process row. Remote-control and
            // app-server broker host panes are filtered out upstream
            // (`with_live_panes`), so they never reach here.
            match bind {
                LazyAgentRow::Agent(agent) => push_agent_row(
                    &mut rows,
                    &mut bound_agents,
                    agent,
                    pane,
                    needs_attention,
                    resolver_working,
                ),
                LazyAgentRow::Idle(row) => rows.push(*row),
            }
        } else {
            rows.push(row_from_process(pane));
        }
    }

    // Script/bridge asks raised outside an agent session have no pane to anchor
    // to, so they keep a standalone attention row. Agent-hook asks never do:
    // they fold onto their pane above, or do not render at all.
    for item in needs_attention.iter().chain(resolver_working.iter()) {
        if item.source_kind != "agent-hook"
            && let Some(row) = row_from_item(item, agents)
        {
            rows.push(row);
        }
    }

    rows
}

/// Render `agent` on `pane`: mark it bound, project its row, overlay the live
/// pane cwd as the worktree fallback, attach the pane, and fold the session's
/// single most-relevant pending ask. Shared by the two binds — the stamped-id
/// match and the Codex daemon's cwd fallback — so both render identically.
fn push_agent_row(
    rows: &mut Vec<SidebarRow>,
    bound: &mut BTreeSet<(String, String)>,
    agent: &AgentState,
    pane: &PaneRef,
    needs_attention: &[FeedItem],
    resolver_working: &[FeedItem],
) {
    bound.insert((agent.kind.clone(), agent.agent_id.clone()));
    let mut row = row_from_agent(agent);
    row.worktree_path = row.worktree_path.or_else(|| pane.cwd.clone());
    row.pane = Some(pane.clone());
    if let Some(ask) = most_relevant_ask(agent, needs_attention, resolver_working) {
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
fn agent_moved_past_ask(agent: &AgentState, ask: &FeedItem) -> bool {
    agent.last_activity > ask.updated_at
}

/// Overlay a pending ask onto its agent's pane row: the row keeps the agent's
/// identity and capability line but takes the ask's waiting status, request,
/// surface, resolver, options, and age.
fn fold_ask_onto_row(row: &mut SidebarRow, ask: &FeedItem) {
    row.status = Some(AgentStatus::Waiting);
    row.request_id = Some(ask.request_id.clone());
    row.surface = Some(ask.surface);
    row.resolver = active_resolver_state(ask);
    row.options = ask.options.clone();
    row.last_activity = ask.updated_at;
    if row.task.is_none() {
        row.task = Some(feed_kind_task(ask.kind).to_owned());
    }
}

fn rows_from_ledger(
    agents: &[AgentState],
    needs_attention: &[FeedItem],
    resolver_working: &[FeedItem],
) -> Vec<SidebarRow> {
    let mut rows = Vec::new();
    let mut replaced_agents = BTreeSet::new();

    for item in needs_attention.iter().chain(resolver_working.iter()) {
        // Re-check liveness here too: `drop_dead_agents_with` can reap a
        // process-dead agent after classification, and this rebuild runs
        // against the reduced set.
        if agent_hook_session_stale(item, agents) {
            continue;
        }
        // The agent kept working after raising this ask (answered in its own
        // UI), so it has un-blocked — don't re-raise its calm row to waiting.
        // Mirrors the pane path's `most_relevant_ask` guard; a bridge ask keeps
        // the hook blocked, so no progress is recorded and this never fires for
        // one mid-flight.
        if let Some(matched) = matching_agent(item, agents)
            && agent_moved_past_ask(matched, item)
        {
            continue;
        }
        // One row per session. Items arrive newest-first, so the first ask for
        // a session wins and any later ask (a sequential permission/question
        // pair, or a stale duplicate that outran expiry) folds onto it instead
        // of stacking an identical row. The same set then suppresses the
        // session's calm agent-rollup row below.
        if let Some(agent_id) = agent_id_from_item(item)
            && !replaced_agents.insert((item.source.clone(), agent_id))
        {
            continue;
        }
        if let Some(row) = row_from_item(item, agents) {
            rows.push(row);
        }
    }

    for agent in agents {
        // A subagent is paneless and nests inside its parent's card; it must
        // never become a standalone top-level row. `attach_sub_agents` folds it
        // onto the parent later.
        if agent.parent_agent_id.is_some() {
            continue;
        }
        let key = (agent.kind.clone(), agent.agent_id.clone());
        if replaced_agents.contains(&key) {
            continue;
        }
        rows.push(row_from_agent(agent));
    }

    rows
}

fn build_worktree_groups_from_rows(
    mut rows: Vec<SidebarRow>,
    agents: &[AgentState],
    project_root: Option<&Path>,
    worktree_roots: &[PathBuf],
) -> Vec<SidebarWorktreeGroup> {
    // Nest each subagent under its parent root row before grouping. This is the
    // one chokepoint both the live (`rows_from_panes`) and no-pane
    // (`rows_from_ledger`) builders share, so nesting behaves identically on
    // either path.
    let now = Timestamp::now();
    attach_sub_agents(&mut rows, agents, now);
    // Project the displayed status now that each row knows its subagents (the
    // delegated-wait exemption) and the full agent set is in hand (the account
    // rate-limit verdict). The one place display state diverges from the rollup.
    project_display_status(&mut rows, agents, now);
    // A worktree dir holds one branch at a time, so rows under one path
    // normally share a branch and group together — the agent and its shell
    // panes alike. Only when stale ledger rows put two distinct branches under
    // one path do we split that path by branch, so a mislabeled cross-branch
    // section can't form while the common "agent + its shell" case stays whole.
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
            // label.
            let label = group_branch_label(&rows).unwrap_or(label);
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
            }
        })
        .collect::<Vec<_>>();
    groups.sort_by(compare_groups);
    groups
}

/// Nest each subagent under its parent root row. A subagent is a reduced
/// `AgentState` carrying `parent_agent_id`; it is paneless, so it built no row
/// of its own (`rows_from_panes` binds only stamped panes, `rows_from_ledger`
/// skips it). This pass matches each child to its parent row by
/// `(kind, parent_agent_id)` and pushes a compact summary onto it.
///
/// Retention reaps a child three ways: a finished (idle/success) child whose
/// work predates the parent's *current* turn (`turn_started_at`, advanced only
/// by `UserPromptSubmit`) belongs to a past turn and is dropped; a finished
/// child sat idle past [`SUBAGENT_FINISHED_TTL_SECS`] is dropped even when no
/// fresh parent turn arrived (the gap that let ghosts linger); and a *running*
/// child superseded by a newer parent turn, or silent past the generous
/// [`GHOST_SESSION_TTL_SECS`] backstop, is a ghost that never sent `Stop` —
/// reaped so it can't freeze the parent's delegated-wait head. A child whose
/// parent row is absent (parent ended, reaped, or has no live pane) is an
/// orphan and never renders. Survivors are deduped by child id so a child can
/// never appear twice, then ordered running-first for a deterministic list.
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
        let superseded = parent_turn_start(&child.kind, parent_id)
            .is_some_and(|started| started > child.last_activity);
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
            // Finished: history once the parent moved on, or once idle past the TTL.
            !superseded && idle_secs(child) < SUBAGENT_FINISHED_TTL_SECS
        };
        if !keep {
            continue;
        }
        // Attach to the parent row when one is present; an orphan (no parent
        // row) never renders — but log it, since a child that names a parent
        // with no row is an anomaly worth tracing.
        if let Some(parent) = rows.iter_mut().find(|row| {
            row.row_kind == SidebarRowKind::Agent && row.name == child.kind && row.id == parent_id
        }) {
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
    for row in rows.iter_mut().filter(|r| !r.sub_agents.is_empty()) {
        // Dedup by child id (freshest activity wins) so the same logical child
        // can never appear twice and the `subagents (N)` count stays honest.
        row.sub_agents
            .sort_by(|a, b| a.id.cmp(&b.id).then(b.last_activity.cmp(&a.last_activity)));
        row.sub_agents.dedup_by(|a, b| a.id == b.id);
        // Display order: running first, then most-recent work, then a stable id
        // tiebreak so the list is deterministic.
        row.sub_agents.sort_by(|a, b| {
            let running = |status: AgentStatus| status == AgentStatus::Running;
            running(b.status)
                .cmp(&running(a.status))
                .then(b.last_activity.cmp(&a.last_activity))
                .then(a.id.cmp(&b.id))
        });
    }
}

/// Project each agent row's *displayed* status from its raw lifecycle status,
/// its liveness, its live subagents, and its account's rate-limit budget. This
/// is the one place display state diverges from the rollup truth kept in
/// `snapshot.agents`; a pending ask already folded `waiting` onto the row
/// upstream and always wins.
///
/// - A `running` agent with a live subagent is *waiting on its children*, not
///   wedged — its own heartbeat is silent because the work is theirs — so it
///   keeps `running` (the renderer paints the delegated-wait head) and is exempt
///   from the stall escalation.
/// - A `running` agent whose latest turn died on a provider API error with no
///   `Stop` hook (the transcript-tail marker postdates its activity) is
///   projected to `failed` at once — the explicit death certificate beats the
///   stall window — and the row carries the upstream error text as
///   `turn_error_label`.
/// - A `running` agent silent past the stall window is projected to `failed`, so
///   a wedge becomes actionable instead of a frozen spinner.
/// - A resting (`idle`/`success`) agent on a rate-limited account is projected
///   to `rate_limited` — parked until the window resets, auto-resumable.
fn project_display_status(rows: &mut [SidebarRow], agents: &[AgentState], now: Timestamp) {
    let limited_kinds = rate_limited_kinds(agents, now);
    for row in rows.iter_mut() {
        if row.row_kind != SidebarRowKind::Agent {
            continue;
        }
        let Some(status) = row.status else {
            continue;
        };
        // A human-blocked `waiting` ask outranks every derived state.
        if status == AgentStatus::Waiting {
            continue;
        }
        let has_live_child = row
            .sub_agents
            .iter()
            .any(|child| child.status == AgentStatus::Running);
        // `row.name` is the agent kind for an agent row (see `row_from_agent`),
        // and the rate-limit verdict is account- (kind-) scoped.
        let projected = if status == AgentStatus::Running && has_live_child {
            AgentStatus::Running
        } else if crate::feed::is_turn_dead(status, row.context.as_ref(), row.last_activity) {
            // The turn died on a provider API error with no `Stop` hook — the
            // transcript marker is explicit, so escalate now rather than
            // waiting out the stall window, and surface the upstream text.
            row.turn_error_label = row
                .context
                .as_ref()
                .and_then(|context| context.turn_error.as_ref())
                .and_then(|error| error.label.clone());
            AgentStatus::Failed
        } else if crate::feed::is_stalled(status, row.last_activity, now) {
            AgentStatus::Failed
        } else if crate::feed::is_rate_limited(status, limited_kinds.contains(&row.name)) {
            AgentStatus::RateLimited
        } else {
            status
        };
        row.status = Some(projected);
    }
}

/// The set of provider kinds whose account rate-limit budget is spent: a live
/// session of the kind reports any window used to the cap whose reset has not yet
/// passed. Account-scoped — every session of a kind shares the
/// budget — so the verdict flips *every* resting agent of the kind, including one
/// that launched straight into a spent account. Reads the same window source as
/// the provider dashboard (`agent.context.rate_limits`), so the cockpit tally and
/// the dashboard bars never disagree.
fn rate_limited_kinds(agents: &[AgentState], now: Timestamp) -> BTreeSet<String> {
    let mut limited = BTreeSet::new();
    for agent in agents {
        if agent.parent_agent_id.is_some() || limited.contains(&agent.kind) {
            continue;
        }
        let Some(limits) = agent
            .context
            .as_ref()
            .and_then(|ctx| ctx.rate_limits.as_ref())
        else {
            continue;
        };
        if limits
            .windows
            .iter()
            .any(|window| window_spent_unreset(window, now))
        {
            limited.insert(agent.kind.clone());
        }
    }
    limited
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
        id: child.agent_id.clone(),
        name,
        status: child.status,
        task: child.task.clone(),
        description: child.subagent_description.clone(),
        total_tokens: child.total_tokens,
        elapsed_secs,
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

pub(super) fn row_from_agent(agent: &AgentState) -> SidebarRow {
    // `SidebarRow.status` is the *displayed* status. It starts as the raw rollup
    // value and is projected in `project_display_status` once the row knows its
    // subagents and its account's rate-limit budget (stall → `failed`,
    // spent-budget → `rate_limited`); a pending ask folds `waiting` on upstream.
    // The rollup in `snapshot.agents` always keeps the true status.
    SidebarRow {
        row_kind: SidebarRowKind::Agent,
        id: agent.agent_id.clone(),
        name: agent.kind.clone(),
        status: Some(agent.status),
        thinking: agent.thinking,
        pane: agent.pane.clone(),
        request_id: None,
        surface: None,
        task: agent.task.clone(),
        prompt: agent.prompt.clone(),
        model: agent.model.clone(),
        effort: agent.effort.clone(),
        context_pct: Some(agent.context_pct.unwrap_or(0)),
        context_window: agent.context_window,
        total_tokens: agent.total_tokens,
        todo_done: agent.todo_done,
        todo_total: agent.todo_total,
        context: agent.context.clone(),
        worktree_path: agent.worktree_path.clone(),
        worktree_branch: agent.worktree_branch.clone(),
        last_activity: agent.last_activity,
        resolver: None,
        options: Vec::new(),
        sub_agents: Vec::new(),
        process_active: false,
        command_detail: None,
        compacting: is_compacting(agent, Timestamp::now()),
        parked_on_background: agent.parked_on_background,
        // Filled by the turn-death projection (`project_display_status`) when
        // the escalation holds; never carried from the rollup.
        turn_error_label: None,
    }
}

/// Whether the agent is mid-compaction: it stamped `compacting_since` and the
/// marker is still fresh. The next lifecycle event clears the stamp; this window
/// is the crash backstop so a missed terminator can't pulse the head forever.
fn is_compacting(agent: &AgentState, now: Timestamp) -> bool {
    agent.compacting_since.is_some_and(|since| {
        now.duration_since(since).as_secs() < crate::feed::COMPACTING_WINDOW_SECS
    })
}

/// A standalone attention row for a pending ask. Two callers, two shapes: an
/// agent-hook ask in the no-pane rollup, enriched from its live session; or a
/// script/bridge ask raised outside any agent, titled by the ask itself.
/// Pane-bound agent asks never reach here — they fold onto their pane row in
/// `rows_from_panes`.
fn row_from_item(item: &FeedItem, agents: &[AgentState]) -> Option<SidebarRow> {
    if item.status != FeedStatus::Pending {
        return None;
    }
    let is_agent_hook = item.source_kind == "agent-hook";
    // A non-agent ask has no session to enrich from; leave it bare and titled.
    let matched = is_agent_hook
        .then(|| matching_agent(item, agents))
        .flatten();
    let task = if is_agent_hook {
        matched
            .and_then(|agent| agent.task.clone())
            .or_else(|| Some(feed_kind_task(item.kind).to_owned()))
    } else {
        Some(item.title.clone())
    };
    let id = agent_id_from_item(item).unwrap_or_else(|| item.request_id.to_string());
    Some(SidebarRow {
        row_kind: SidebarRowKind::Agent,
        id,
        name: item.source.clone(),
        status: Some(AgentStatus::Waiting),
        // A waiting row is blocked on the human, not reasoning — no thinking head.
        thinking: false,
        pane: item
            .pane
            .clone()
            .or_else(|| matched.and_then(|agent| agent.pane.clone())),
        request_id: Some(item.request_id.clone()),
        surface: Some(item.surface),
        task,
        prompt: matched.and_then(|agent| agent.prompt.clone()),
        model: matched.and_then(|agent| agent.model.clone()),
        effort: matched.and_then(|agent| agent.effort.clone()),
        context_pct: if is_agent_hook {
            Some(matched.and_then(|agent| agent.context_pct).unwrap_or(0))
        } else {
            None
        },
        context_window: matched.and_then(|agent| agent.context_window),
        total_tokens: matched.and_then(|agent| agent.total_tokens),
        todo_done: matched.and_then(|agent| agent.todo_done),
        todo_total: matched.and_then(|agent| agent.todo_total),
        context: matched.and_then(|agent| agent.context.clone()),
        worktree_path: item
            .worktree_path
            .clone()
            .or_else(|| matched.and_then(|agent| agent.worktree_path.clone())),
        worktree_branch: item
            .worktree_branch
            .clone()
            .or_else(|| matched.and_then(|agent| agent.worktree_branch.clone())),
        last_activity: item.updated_at,
        resolver: active_resolver_state(item),
        options: item.options.clone(),
        sub_agents: Vec::new(),
        process_active: false,
        command_detail: None,
        compacting: false,
        parked_on_background: false,
        turn_error_label: None,
    })
}

fn matching_agent<'a>(item: &FeedItem, agents: &'a [AgentState]) -> Option<&'a AgentState> {
    let item_agent_id = agent_id_from_item(item);
    if let Some(agent_id) = item_agent_id.as_deref() {
        return agents
            .iter()
            .find(|agent| agent.kind == item.source && agent.agent_id == agent_id);
    }

    let candidates = agents
        .iter()
        .filter(|agent| {
            agent.kind == item.source
                && item
                    .worktree_path
                    .as_ref()
                    .is_none_or(|path| agent.worktree_path.as_ref() == Some(path))
        })
        .collect::<Vec<_>>();
    if candidates.len() == 1 {
        Some(candidates[0])
    } else {
        None
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

fn feed_kind_task(kind: FeedKind) -> &'static str {
    match kind {
        FeedKind::Permission => "permission",
        FeedKind::PlanApproval => "plan approval",
        FeedKind::Question => "question",
        FeedKind::NeedsInput => "needs input",
        FeedKind::Completion => "completion",
        FeedKind::Failure => "failure",
        FeedKind::ToolEvent => "tool",
        FeedKind::SubAgentStarted => "sub-agent started",
        FeedKind::SubAgentStopped => "sub-agent stopped",
        FeedKind::Generic => "activity",
    }
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
) -> (SidebarWorktreeKind, String, String) {
    let branch = branch.filter(|branch| !branch.is_empty());
    if let Some(path) = path.filter(|path| !path.is_empty()) {
        // A cwd is one of the project's worktrees when it is under the main
        // checkout *or* inside any worktree `git worktree list` reported —
        // including a worktree parked outside `project_root`. Only a cwd that is
        // neither (a home shell, `/tmp`, CI) folds into the `external` catch-all
        // rather than minting its own pod. With no known root and no enumerated
        // worktrees, every path keeps per-path grouping.
        let cwd = Path::new(path);
        let in_project = match project_root {
            Some(root) => is_within(root, cwd) || worktree_roots.iter().any(|w| is_within(w, cwd)),
            None => worktree_roots.is_empty() || worktree_roots.iter().any(|w| is_within(w, cwd)),
        };
        if in_project {
            let label = branch.map(ToOwned::to_owned).unwrap_or_else(|| {
                Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .filter(|name| !name.is_empty())
                    .unwrap_or(path)
                    .to_owned()
            });
            // Disambiguate the key by branch only for a path that holds more
            // than one — a newline can appear in neither a path nor a branch, so
            // it is an unambiguous separator. `enrich_worktree_groups` recovers
            // the bare path from the rows, not the key, so the split never
            // breaks git reads.
            let key = match branch.filter(|_| split_by_branch) {
                Some(branch) => format!("{path}\n{branch}"),
                None => path.to_owned(),
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
    // Catch-all: untethered scripts/CI and out-of-project shells. The stable
    // grouping key stays `workspace`; the header reads `external` so it reads
    // as "outside the project."
    (
        SidebarWorktreeKind::Workspace,
        "workspace".to_owned(),
        "external".to_owned(),
    )
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
        AgentStatus::RateLimited,
        AgentStatus::Running,
        AgentStatus::Idle,
        AgentStatus::Success,
    ]
    .into_iter()
    .filter_map(|status| {
        let count = rows
            .iter()
            .filter(|row| row.row_kind != SidebarRowKind::Process && row.status == Some(status))
            .count();
        (count > 0).then_some(SidebarStatusCount { status, count })
    })
    .collect()
}

/// Trim a group's calm tail to `WORKTREE_ROW_CAP`, always keeping the rows that
/// need you (`waiting`/`failed`) and the focused pane. Because `running` now
/// ranks last among agents, it is the first calm bucket trimmed behind
/// `+K more` — by design: a working agent is the least attention-hungry.
fn capped_rows(rows: Vec<SidebarRow>) -> Vec<SidebarRow> {
    let mut visible = Vec::new();
    for row in rows {
        if row.status == Some(AgentStatus::Waiting)
            || row.status == Some(AgentStatus::Failed)
            || row.pane.as_ref().is_some_and(|pane| pane.is_focused)
            || visible.len() < WORKTREE_ROW_CAP
        {
            visible.push(row);
        }
    }
    visible
}

fn compare_rows(left: &SidebarRow, right: &SidebarRow) -> Ordering {
    row_rank(left)
        .cmp(&row_rank(right))
        .then_with(|| within_bucket(left, right))
        .then_with(|| left.name.cmp(&right.name))
        .then_with(|| left.id.cmp(&right.id))
}

/// Tiebreak two rows that share a status bucket (their ranks already tied).
///
/// Attention rows (`waiting`/`failed`) sort longest-overdue-first: a blocked or
/// failed agent's `last_activity` is frozen, so this is both stable and the
/// triage order the `␣` "next attention" key promises. Calm rows (`idle`,
/// `success`, `running`) and bare process rows hold a stable spawn order keyed
/// on `pane_process_start` — untouched by the activity heartbeat — so a working
/// agent never jumps just because it finished a tool, and new agents append at
/// the bottom of their bucket.
fn within_bucket(left: &SidebarRow, right: &SidebarRow) -> Ordering {
    if is_attention(left.status) {
        left.last_activity.cmp(&right.last_activity)
    } else {
        cmp_start_asc(pane_start(left), pane_start(right))
    }
}

fn is_attention(status: Option<AgentStatus>) -> bool {
    matches!(
        status,
        Some(AgentStatus::Waiting | AgentStatus::Failed | AgentStatus::RateLimited)
    )
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
    // stable order keyed on the earliest spawned pane, then label. The external
    // group therefore only rises out of the tail when it holds a `waiting` or
    // `failed` agent — the tier carries that, no separate predicate needed.
    group_tier(left)
        .cmp(&group_tier(right))
        .then_with(|| group_is_external(left).cmp(&group_is_external(right)))
        .then_with(|| cmp_start_asc(group_earliest_start(left), group_earliest_start(right)))
        .then_with(|| left.label.cmp(&right.label))
}

/// The most-urgent member's rank. `rows` is already sorted by `compare_rows`
/// and the cap never hides `waiting`/`failed`, so `rows.first()` is the true
/// top; an empty group ranks last.
fn group_tier(group: &SidebarWorktreeGroup) -> u8 {
    group.rows.first().map_or(u8::MAX, row_rank)
}

fn group_is_external(group: &SidebarWorktreeGroup) -> bool {
    group.kind == SidebarWorktreeKind::Workspace
}

fn group_earliest_start(group: &SidebarWorktreeGroup) -> Option<Timestamp> {
    group.rows.iter().filter_map(pane_start).min()
}

fn row_rank(row: &SidebarRow) -> u8 {
    match row.status {
        Some(status) => status_rank(status),
        None => 7,
    }
}

fn status_rank(status: AgentStatus) -> u8 {
    // Working agents are the least attention-hungry, so `running` ranks below the
    // calm-but-settled `idle`/`success`. Actionable attention (`waiting`/`failed`)
    // leads; `rate_limited` sits just under it — attention-class, but parked with
    // nothing to do but wait, so it ranks below a real failure and above calm.
    match status {
        AgentStatus::Waiting => 0,
        AgentStatus::Failed => 1,
        AgentStatus::RateLimited => 2,
        AgentStatus::Idle => 3,
        AgentStatus::Success => 4,
        AgentStatus::Running => 5,
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::super::panes::pane_ref_from_id;
    use super::super::project::reduce_agent_states;
    use super::*;
    use crate::agents::SpendWindow;
    use crate::feed::FeedKind;
    use crate::feed::{RuntimeOwner, RuntimeOwnerKind};
    use crate::ids::{MuxName, PaneId, WorkspaceId};
    use crate::ledger::snapshot::testkit::*;

    #[test]
    fn build_groups_by_surface_and_status() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut native = FeedItem::new(
            workspace.clone(),
            Surface::NativeUi,
            FeedKind::Permission,
            "n",
            "claude",
            "agent-hook",
        );
        let bridge = FeedItem::new(
            workspace.clone(),
            Surface::Bridge,
            FeedKind::Permission,
            "b",
            "rimz",
            "cli",
        );
        let mut answered = FeedItem::new(
            workspace.clone(),
            Surface::Bridge,
            FeedKind::Permission,
            "a",
            "rimz",
            "cli",
        );
        answered.status = FeedStatus::Resolved;
        let mut timed = FeedItem::new(
            workspace,
            Surface::Bridge,
            FeedKind::Permission,
            "t",
            "rimz",
            "cli",
        );
        timed.status = FeedStatus::TimedOut;
        native.updated_at += std::time::Duration::from_secs(1);

        let snap = SidebarSnapshot::build(
            WorkspaceId::from_project_root(Path::new("/tmp/x")),
            vec![native, bridge, answered, timed],
            Vec::new(),
        );
        // Pending native + bridge asks surface as attention/working; the
        // resolved and timed-out items are history, so they are dropped — they
        // never become rows.
        assert_eq!(snap.needs_attention.len(), 1);
        assert_eq!(snap.resolver_working.len(), 1);
        assert_eq!(snap.worktree_groups.len(), 1);
        assert_eq!(snap.worktree_groups[0].kind, SidebarWorktreeKind::Workspace);
        assert_eq!(snap.worktree_groups[0].label, "external");
        assert_eq!(snap.worktree_groups[0].rows.len(), 2);
    }

    #[test]
    fn activity_heartbeat_updates_last_activity_not_thinking() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut agent = agent("claude", "sess-1", AgentStatus::Running, 50_000);
        agent.thinking = true;
        let original_seen = agent.last_seen;
        let at = original_seen + std::time::Duration::from_secs(10);
        let touch = AgentActivity {
            kind: agent.kind.clone(),
            agent_id: agent.agent_id.clone(),
            at,
        };
        let snap = SidebarSnapshot::build_with_agents(workspace, Vec::new(), vec![agent])
            .with_agent_activity(&[touch]);

        // The heartbeat is latency, not a lifecycle signal — it advances
        // `last_activity` only, never the turn-phase head.
        assert!(snap.agents[0].thinking);
        assert_eq!(snap.agents[0].last_activity, at);
        assert_eq!(snap.agents[0].last_seen, original_seen);
    }

    #[test]
    fn provider_panel_spending_is_attached_and_ranks_panels() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let claude = agent("claude", "c1", AgentStatus::Idle, 10);
        let codex = agent("codex", "x1", AgentStatus::Idle, 20);
        let snapshot =
            SidebarSnapshot::build_with_agents(workspace, Vec::new(), vec![claude, codex]);

        let today_tally = |usd: f64| SpendTally {
            today: SpendWindow {
                usd,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut by_provider: BTreeMap<String, SpendTally> = BTreeMap::new();
        by_provider.insert("claude".to_owned(), today_tally(1.0));
        by_provider.insert("codex".to_owned(), today_tally(5.0));

        let snapshot =
            snapshot.with_provider_aggregates(&BTreeMap::new(), &BTreeMap::new(), &by_provider);

        // Codex's today spend (5.0) outranks Claude's (1.0), so it sorts first —
        // even though Codex has no live `total_cost_usd`.
        assert_eq!(snapshot.providers[0].kind, "codex");
        assert_eq!(
            snapshot.providers[0].spending.as_ref().unwrap().today.usd,
            5.0
        );
        let claude_panel = snapshot
            .providers
            .iter()
            .find(|panel| panel.kind == "claude")
            .expect("claude panel present");
        assert_eq!(claude_panel.spending.as_ref().unwrap().today.usd, 1.0);
    }

    #[test]
    fn provider_with_recorded_spend_earns_a_panel_without_a_session() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        // No live agents and no probed accounts — only recorded fleet spend for
        // Claude. Its history alone must still surface a panel, so the dashboard
        // never reads zero for a provider you spent on earlier.
        let snapshot = SidebarSnapshot::build_with_agents(workspace, Vec::new(), Vec::new());

        let mut by_provider: BTreeMap<String, SpendTally> = BTreeMap::new();
        by_provider.insert(
            "claude".to_owned(),
            SpendTally {
                today: SpendWindow {
                    usd: 2.0,
                    tokens: 100,
                    ..Default::default()
                },
                year: SpendWindow {
                    usd: 9.0,
                    tokens: 900,
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        let snapshot =
            snapshot.with_provider_aggregates(&BTreeMap::new(), &BTreeMap::new(), &by_provider);

        let claude = snapshot
            .providers
            .iter()
            .find(|panel| panel.kind == "claude")
            .expect("claude panel from recorded spend alone");
        assert_eq!(claude.spending.as_ref().unwrap().year.usd, 9.0);
    }

    #[test]
    fn pending_cli_native_items_do_not_become_sidebar_attention() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let item = FeedItem::new(
            workspace.clone(),
            Surface::NativeUi,
            FeedKind::Generic,
            "Should I proceed?",
            "rimz",
            "cli",
        );

        let snap = SidebarSnapshot::build(workspace, vec![item], Vec::new());

        assert!(snap.needs_attention.is_empty());
        assert!(snap.worktree_groups.is_empty());
    }

    #[test]
    fn pending_script_items_use_worktree_branch_label() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut item = FeedItem::new(
            workspace.clone(),
            Surface::Script,
            FeedKind::Question,
            "Should I proceed?",
            "rimz",
            "cli",
        );
        item.worktree_path = Some("/repo/rimz".to_owned());
        item.worktree_branch = Some("main".to_owned());

        let snap = SidebarSnapshot::build(workspace, vec![item], Vec::new());

        assert_eq!(snap.worktree_groups.len(), 1);
        assert_eq!(snap.worktree_groups[0].label, "main");
        assert_eq!(
            snap.worktree_groups[0].rows[0].task.as_deref(),
            Some("Should I proceed?")
        );
    }

    #[test]
    fn multiple_pending_asks_for_one_session_render_one_row() {
        // The live pile-up: a session held several pending native_ui asks, and
        // the no-panes rollup emitted one row each. Read-time dedup collapses
        // them to a single row keyed by `(source, agent_id)`.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut session = agent("claude", "sess-1", AgentStatus::Idle, 1_000);
        session.worktree_path = Some("/repo/main".to_owned());

        let mk = |kind: FeedKind| {
            let mut item = FeedItem::new(
                workspace.clone(),
                Surface::NativeUi,
                kind,
                "claude needs attention",
                "claude",
                "agent-hook",
            );
            item.worktree_path = Some("/repo/main".to_owned());
            item.payload = serde_json::json!({ "session_id": "sess-1" });
            item
        };

        let items = vec![mk(FeedKind::Permission), mk(FeedKind::Question)];
        let snapshot =
            SidebarSnapshot::build_with_carryover(workspace, items, Vec::new(), vec![session]);

        let rows = &snapshot.worktree_groups[0].rows;
        let agent_rows: Vec<_> = rows
            .iter()
            .filter(|row| row.row_kind == SidebarRowKind::Agent)
            .collect();
        assert_eq!(
            agent_rows.len(),
            1,
            "two pending asks for one session collapse to one row: {rows:?}"
        );
        assert_eq!(agent_rows[0].status, Some(AgentStatus::Waiting));
    }

    #[test]
    fn agents_on_different_branches_in_one_path_form_two_groups() {
        // Root cause 5: stale rows put two branches under one path, collapsing
        // into a single mislabeled section. Keying on branch splits them into
        // two correctly-labeled groups.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut feature = agent("claude", "sess-a", AgentStatus::Idle, 1_000);
        feature.worktree_path = Some("/repo/shared".to_owned());
        feature.worktree_branch = Some("feature".to_owned());
        let mut main = agent("claude", "sess-b", AgentStatus::Idle, 1_100);
        main.worktree_path = Some("/repo/shared".to_owned());
        main.worktree_branch = Some("main".to_owned());

        let snapshot = SidebarSnapshot::build_with_carryover(
            workspace,
            Vec::new(),
            Vec::new(),
            vec![feature, main],
        );

        assert_eq!(
            snapshot.worktree_groups.len(),
            2,
            "two branches under one path split into two groups"
        );
        for group in &snapshot.worktree_groups {
            assert_eq!(group.rows.len(), 1);
            assert_eq!(
                group.rows[0].worktree_branch.as_deref(),
                Some(group.label.as_str()),
                "each group's label matches its branch"
            );
        }
    }

    #[test]
    fn one_branch_path_keeps_agent_and_shell_in_one_group() {
        // The common case must not fragment: a process/shell row carries no
        // branch, so it stays with the single-branch agent in its worktree.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut claude = agent("claude", "sess-a", AgentStatus::Running, 1_000);
        claude.worktree_path = Some("/repo/main".to_owned());
        claude.worktree_branch = Some("main".to_owned());
        claude.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));

        let snapshot =
            SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![claude])
                .with_live_panes(
                    vec![
                        pane("%1", "claude", "/repo/main"),
                        pane("%2", "zsh", "/repo/main"),
                    ],
                    None,
                );

        assert_eq!(
            snapshot.worktree_groups.len(),
            1,
            "agent and its shell share one worktree group: {:?}",
            snapshot.worktree_groups,
        );
        assert_eq!(snapshot.worktree_groups[0].label, "main");
        let rows = &snapshot.worktree_groups[0].rows;
        assert!(rows.iter().any(|row| row.row_kind == SidebarRowKind::Agent));
        assert!(
            rows.iter()
                .any(|row| row.row_kind == SidebarRowKind::Process && row.name == "zsh")
        );
    }

    #[test]
    fn remote_control_host_pane_renders_no_row() {
        // A `claude remote-control` pane (Zellij reports the full command line)
        // is ambient infrastructure: it no longer renders as any row — its
        // presence surfaces as the provider dashboard's `⇅ rc` flag instead.
        // Only the shell pane beside it remains a row.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new()).with_live_panes(
            vec![
                pane("%1", "zsh", "/repo/main"),
                pane("%2", "claude remote-control --spawn worktree", "/repo/main"),
            ],
            None,
        );

        let rows = &snapshot.worktree_groups[0].rows;
        assert_eq!(rows.len(), 1, "only the shell pane is a row: {rows:?}");
        assert_eq!(rows[0].row_kind, SidebarRowKind::Process);
        assert_eq!(rows[0].name, "zsh");
        assert!(
            rows.iter().all(|row| row.name != "claude"),
            "the host pane must not produce a claude row: {rows:?}",
        );
    }

    #[test]
    fn remote_control_host_pane_filtered_when_detected_by_view_name() {
        // tmux reports only the `claude` basename, but names the window — so the
        // view name marks the host, and that pane is filtered out the same way.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut rc_pane = pane("%2", "claude", "/repo/main");
        rc_pane.view_name = Some(crate::remote_control::VIEW_NAME.to_owned());
        let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new())
            .with_live_panes(vec![rc_pane], None);

        let rows: Vec<_> = snapshot
            .worktree_groups
            .iter()
            .flat_map(|group| &group.rows)
            .collect();
        assert!(
            rows.is_empty(),
            "a host-only pane set produces no rows: {rows:?}",
        );
    }

    #[test]
    fn sub_agent_nests_under_parent_and_never_top_level() {
        let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
        let child = child_state("sess-root", "child-1", AgentStatus::Running, 5);
        // Only the parent built a row; the paneless child attaches onto it.
        let mut rows = vec![row_from_agent(&parent)];
        attach_sub_agents(&mut rows, &[parent.clone(), child], Timestamp::now());
        assert_eq!(rows.len(), 1, "the child is never its own top-level row");
        assert_eq!(rows[0].sub_agents.len(), 1);
        assert_eq!(rows[0].sub_agents[0].id, "child-1");
        assert_eq!(rows[0].sub_agents[0].name, "Explore");
    }

    #[test]
    fn orphan_sub_agent_is_dropped() {
        let child = child_state("missing-parent", "child-1", AgentStatus::Running, 5);
        let mut rows: Vec<SidebarRow> = Vec::new();
        attach_sub_agents(&mut rows, &[child], Timestamp::now());
        assert!(rows.is_empty(), "a child with no parent row never renders");
    }

    #[test]
    fn with_subagent_context_folds_onto_child_by_key() {
        use crate::agents::context::SubagentContext;
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
        let child = child_state("sess-root", "child-1", AgentStatus::Running, 5);
        let started = Timestamp::from_second(1_700_000_000).unwrap();
        let snapshot =
            SidebarSnapshot::build_with_agents(workspace, Vec::new(), vec![parent, child]);

        let record = SubagentContextRecord {
            kind: "claude".to_owned(),
            agent_id: "child-1".to_owned(),
            context: SubagentContext {
                agent_type: None,
                description: Some("locate the render seam".to_owned()),
                token_count: Some(12_400),
                started_at: Some(started),
                observed_at: Timestamp::now(),
            },
        };
        let folded = snapshot.with_subagent_context(vec![record]);
        let child = folded
            .agents
            .iter()
            .find(|a| a.agent_id == "child-1")
            .expect("child in rollup");
        assert_eq!(
            child.subagent_description.as_deref(),
            Some("locate the render seam")
        );
        assert_eq!(child.total_tokens, Some(12_400));
        assert_eq!(child.subagent_started_at, Some(started));

        // A record whose child is absent from the rollup is dropped — the key it
        // is filed under is authority.
        let absent = SubagentContextRecord {
            kind: "claude".to_owned(),
            agent_id: "ghost".to_owned(),
            context: SubagentContext {
                agent_type: None,
                description: Some("nowhere".to_owned()),
                token_count: None,
                started_at: None,
                observed_at: Timestamp::now(),
            },
        };
        let folded = folded.with_subagent_context(vec![absent]);
        assert!(folded.agents.iter().all(|a| a.agent_id != "ghost"));
    }

    #[test]
    fn with_subagent_context_back_fills_task_from_agent_type() {
        use crate::agents::context::SubagentContext;
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
        // A fork child: parent_agent_id set, task None (no agent_type in SubagentStart).
        let mut fork = child_state("sess-root", "fork-1", AgentStatus::Running, 5);
        fork.task = None;
        let snapshot =
            SidebarSnapshot::build_with_agents(workspace, Vec::new(), vec![parent, fork]);

        let record = SubagentContextRecord {
            kind: "claude".to_owned(),
            agent_id: "fork-1".to_owned(),
            context: SubagentContext {
                agent_type: Some("Explore".to_owned()),
                description: Some("search the ledger".to_owned()),
                token_count: Some(5_000),
                started_at: None,
                observed_at: Timestamp::now(),
            },
        };
        let folded = snapshot.with_subagent_context(vec![record]);
        let fork = folded
            .agents
            .iter()
            .find(|a| a.agent_id == "fork-1")
            .expect("fork in rollup");
        assert_eq!(
            fork.task.as_deref(),
            Some("Explore"),
            "agent_type back-fills task"
        );
        assert_eq!(
            fork.subagent_description.as_deref(),
            Some("search the ledger")
        );
    }

    #[test]
    fn with_subagent_context_does_not_overwrite_existing_task() {
        use crate::agents::context::SubagentContext;
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
        // Typed child: task already set by SubagentStart.
        let mut typed = child_state("sess-root", "child-1", AgentStatus::Running, 5);
        typed.task = Some("review".to_owned());
        let snapshot =
            SidebarSnapshot::build_with_agents(workspace, Vec::new(), vec![parent, typed]);

        let record = SubagentContextRecord {
            kind: "claude".to_owned(),
            agent_id: "child-1".to_owned(),
            context: SubagentContext {
                agent_type: Some("SomethingElse".to_owned()),
                description: None,
                token_count: None,
                started_at: None,
                observed_at: Timestamp::now(),
            },
        };
        let folded = snapshot.with_subagent_context(vec![record]);
        let typed = folded
            .agents
            .iter()
            .find(|a| a.agent_id == "child-1")
            .expect("child in rollup");
        assert_eq!(
            typed.task.as_deref(),
            Some("review"),
            "lifecycle-established task must not be overwritten by enrichment",
        );
    }

    #[test]
    fn sub_agent_projection_carries_enrichment_and_freezes_finished_elapsed() {
        let now = Timestamp::from_second(1_700_000_100).unwrap();
        let started = Timestamp::from_second(1_700_000_000).unwrap();

        // Running: elapsed counts to `now` (100s), enrichment projects through.
        let mut running = child_state("sess-root", "child-1", AgentStatus::Running, 5);
        running.subagent_description = Some("locate the render seam".to_owned());
        running.subagent_started_at = Some(started);
        running.total_tokens = Some(12_400);
        let sub = sub_agent_from_state(&running, now);
        assert_eq!(sub.description.as_deref(), Some("locate the render seam"));
        assert_eq!(sub.total_tokens, Some(12_400));
        assert_eq!(sub.elapsed_secs, Some(100));

        // Finished: elapsed freezes at `last_activity` (40s after start), never `now`.
        let mut finished = child_state("sess-root", "child-2", AgentStatus::Success, 0);
        finished.last_activity = Timestamp::from_second(1_700_000_040).unwrap();
        finished.subagent_started_at = Some(started);
        let sub = sub_agent_from_state(&finished, now);
        assert_eq!(sub.elapsed_secs, Some(40));

        // A child with no enrichment (Codex, or pre-first-render) degrades cleanly.
        let bare = child_state("sess-root", "child-3", AgentStatus::Running, 5);
        let sub = sub_agent_from_state(&bare, now);
        assert_eq!(sub.description, None);
        assert_eq!(sub.total_tokens, None);
        assert_eq!(sub.elapsed_secs, None);
    }

    #[test]
    fn finished_sub_agent_drops_once_parent_starts_next_turn() {
        let now = Timestamp::now();
        let mut parent = agent("claude", "sess-root", AgentStatus::Running, 100);
        // The current turn began AFTER the child finished — a past-turn child.
        parent.turn_started_at = Some(Timestamp::from_second(now.as_second() - 30).unwrap());
        let child = child_state("sess-root", "child-1", AgentStatus::Idle, 60);
        let mut rows = vec![row_from_agent(&parent)];
        attach_sub_agents(&mut rows, &[parent.clone(), child], now);
        assert!(rows[0].sub_agents.is_empty());
    }

    #[test]
    fn running_sub_agent_of_current_turn_is_kept() {
        let now = Timestamp::now();
        let mut parent = agent("claude", "sess-root", AgentStatus::Running, 100);
        // The turn began BEFORE the child's activity — live work of this turn.
        parent.turn_started_at = Some(Timestamp::from_second(now.as_second() - 90).unwrap());
        let child = child_state("sess-root", "child-1", AgentStatus::Running, 30);
        let mut rows = vec![row_from_agent(&parent)];
        attach_sub_agents(&mut rows, &[parent.clone(), child], now);
        assert_eq!(
            rows[0].sub_agents.len(),
            1,
            "a live child of the current turn is kept"
        );
    }

    #[test]
    fn superseded_running_sub_agent_is_reaped_as_ghost() {
        let now = Timestamp::now();
        let mut parent = agent("claude", "sess-root", AgentStatus::Running, 100);
        // The parent moved to a newer turn than the child's last activity: the
        // child never sent `SubagentStop` and is a leftover ghost — reaped so it
        // can't freeze the parent's delegated-wait head.
        parent.turn_started_at = Some(Timestamp::from_second(now.as_second() - 30).unwrap());
        let child = child_state("sess-root", "child-1", AgentStatus::Running, 60);
        let mut rows = vec![row_from_agent(&parent)];
        attach_sub_agents(&mut rows, &[parent.clone(), child], now);
        assert!(
            rows[0].sub_agents.is_empty(),
            "a running child from a past turn is a ghost"
        );
    }

    #[test]
    fn finished_sub_agent_of_current_turn_is_kept() {
        let now = Timestamp::now();
        let mut parent = agent("claude", "sess-root", AgentStatus::Running, 100);
        // The turn began BEFORE the child finished — same-turn, so it stays.
        parent.turn_started_at = Some(Timestamp::from_second(now.as_second() - 90).unwrap());
        let child = child_state("sess-root", "child-1", AgentStatus::Idle, 30);
        let mut rows = vec![row_from_agent(&parent)];
        attach_sub_agents(&mut rows, &[parent.clone(), child], now);
        assert_eq!(rows[0].sub_agents.len(), 1);
    }

    #[test]
    fn sub_agents_sort_running_before_finished() {
        let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
        // The idle child is more recent, the running child older — running leads.
        let idle = child_state("sess-root", "c-idle", AgentStatus::Idle, 2);
        let running = child_state("sess-root", "c-run", AgentStatus::Running, 30);
        let mut rows = vec![row_from_agent(&parent)];
        attach_sub_agents(
            &mut rows,
            &[parent.clone(), idle, running],
            Timestamp::now(),
        );
        let ids: Vec<&str> = rows[0].sub_agents.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["c-run", "c-idle"]);
    }

    #[test]
    fn duplicate_children_collapse_to_one_row() {
        // Two reduced states aliasing the same child id must render as one row,
        // so `subagents (N)` never double-counts. Freshest activity wins.
        let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
        let stale = child_state("sess-root", "child-dup", AgentStatus::Running, 50);
        let fresh = child_state("sess-root", "child-dup", AgentStatus::Running, 5);
        let mut rows = vec![row_from_agent(&parent)];
        attach_sub_agents(&mut rows, &[parent.clone(), stale, fresh], Timestamp::now());
        assert_eq!(
            rows[0].sub_agents.len(),
            1,
            "the same child can't appear twice"
        );
        assert_eq!(rows[0].sub_agents[0].id, "child-dup");
    }

    #[test]
    fn typeless_child_renders_degraded_label_never_the_kind() {
        // A child with no type label must not borrow the provider kind, which
        // would render as a phantom `claude` row. This is the "3 Explore + 3
        // claude" regression.
        let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
        let mut child = child_state("sess-root", "child-1", AgentStatus::Running, 5);
        child.task = None;
        let mut rows = vec![row_from_agent(&parent)];
        attach_sub_agents(&mut rows, &[parent.clone(), child], Timestamp::now());
        let name = &rows[0].sub_agents[0].name;
        assert!(name.starts_with("subagent"), "got {name}");
        assert_ne!(name, "claude");
    }

    #[test]
    fn finished_child_drops_past_ttl_without_a_turn_boundary() {
        // The parent never took a fresh turn (`turn_started_at` stays None), so
        // only the TTL backstop can clear a long-finished child — without it the
        // ghost would linger forever.
        let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
        assert!(parent.turn_started_at.is_none());
        let child = child_state(
            "sess-root",
            "child-1",
            AgentStatus::Idle,
            SUBAGENT_FINISHED_TTL_SECS + 10,
        );
        let mut rows = vec![row_from_agent(&parent)];
        attach_sub_agents(&mut rows, &[parent.clone(), child], Timestamp::now());
        assert!(
            rows[0].sub_agents.is_empty(),
            "a long-finished child clears on the TTL"
        );
    }

    #[test]
    fn reaper_never_drops_a_subagent() {
        let now = Timestamp::now();
        let parent = agent("claude", "sess-root", AgentStatus::Running, 0);
        // A pidless idle child well past the ghost TTL, plus a same-type sibling
        // that would "supersede" it under the root rule — both survive, because
        // children are exempt and leave only when the parent does.
        let old_child = child_state(
            "sess-root",
            "child-old",
            AgentStatus::Idle,
            GHOST_SESSION_TTL_SECS + 600,
        );
        let new_child = child_state("sess-root", "child-new", AgentStatus::Running, 5);
        assert_eq!(
            reap_survivors(now, vec![parent, old_child, new_child]),
            vec![
                "child-new".to_owned(),
                "child-old".to_owned(),
                "sess-root".to_owned()
            ],
        );
    }

    #[test]
    fn live_panes_add_process_rows_without_attention_counts() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new())
            .with_live_panes(vec![pane("%1", "zsh", "/repo/main")], None);

        assert_eq!(snapshot.worktree_groups.len(), 1);
        let row = &snapshot.worktree_groups[0].rows[0];
        assert_eq!(row.row_kind, SidebarRowKind::Process);
        assert_eq!(row.name, "zsh");
        assert_eq!(row.status, None);
        assert!(snapshot.worktree_groups[0].status_counts.is_empty());
    }

    #[test]
    fn is_within_compares_path_components() {
        let root = Path::new("/home/marvin");
        assert!(is_within(root, root));
        assert!(is_within(root, Path::new("/home/marvin/")));
        assert!(is_within(root, Path::new("/home/marvin/sub/dir")));
        // A shared string prefix that is not a component boundary is outside.
        assert!(!is_within(root, Path::new("/home/marvinX")));
        assert!(!is_within(root, Path::new("/home/other")));
        assert!(!is_within(root, Path::new("/")));
    }

    #[test]
    fn out_of_project_process_folds_into_external_catch_all() {
        let root = "/home/marvin/workspace/project-rimz/rimz";
        let workspace = WorkspaceId::from_project_root(Path::new(root));
        let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new())
            .with_project_root(Some(PathBuf::from(root)))
            .with_live_panes(vec![pane("%1", "zsh", "/home/marvin")], None);

        assert_eq!(snapshot.worktree_groups.len(), 1);
        let group = &snapshot.worktree_groups[0];
        assert_eq!(group.kind, SidebarWorktreeKind::Workspace);
        assert_eq!(group.key, "workspace");
        assert_eq!(group.label, "external");
        assert_eq!(group.rows[0].name, "zsh");
    }

    #[test]
    fn in_project_worktree_pane_keeps_its_own_group() {
        let root = "/repo/rimz";
        let workspace = WorkspaceId::from_project_root(Path::new(root));
        let worktree = "/repo/rimz/.claude/worktrees/featureX";
        let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new())
            .with_project_root(Some(PathBuf::from(root)))
            .with_live_panes(vec![pane("%1", "zsh", worktree)], None);

        assert_eq!(snapshot.worktree_groups.len(), 1);
        let group = &snapshot.worktree_groups[0];
        assert_eq!(group.kind, SidebarWorktreeKind::Worktree);
        assert_eq!(group.key, worktree);
        assert_eq!(group.label, "featureX");
    }

    #[test]
    fn main_checkout_pane_is_in_project() {
        let root = "/repo/rimz";
        let workspace = WorkspaceId::from_project_root(Path::new(root));
        let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new())
            .with_project_root(Some(PathBuf::from(root)))
            .with_live_panes(vec![pane("%1", "zsh", root)], None);

        let group = &snapshot.worktree_groups[0];
        assert_eq!(group.kind, SidebarWorktreeKind::Worktree);
        assert_eq!(group.label, "rimz");
    }

    #[test]
    fn component_boundary_pane_is_external() {
        // cwd shares a string prefix with the root but not a component boundary.
        let workspace = WorkspaceId::from_project_root(Path::new("/home/marvin"));
        let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new())
            .with_project_root(Some(PathBuf::from("/home/marvin")))
            .with_live_panes(vec![pane("%1", "zsh", "/home/marvinX/repo")], None);

        let group = &snapshot.worktree_groups[0];
        assert_eq!(group.kind, SidebarWorktreeKind::Workspace);
        assert_eq!(group.label, "external");
    }

    #[test]
    fn external_worktree_pane_gets_its_own_pod() {
        // A worktree parked outside the project root — captured by `git worktree
        // list` — is project-related and earns its own pod, not the `external`
        // catch-all the `project_root` prefix test alone would give it.
        let root = "/repo/rimz";
        let external = "/elsewhere/feature-wt";
        let workspace = WorkspaceId::from_project_root(Path::new(root));
        let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new())
            .with_project_root(Some(PathBuf::from(root)))
            .with_worktree_roots(vec![PathBuf::from(root), PathBuf::from(external)])
            .with_live_panes(vec![pane("%1", "zsh", external)], None);

        assert_eq!(snapshot.worktree_groups.len(), 1);
        let group = &snapshot.worktree_groups[0];
        assert_eq!(group.kind, SidebarWorktreeKind::Worktree);
        assert_eq!(group.key, external);
        assert_eq!(group.label, "feature-wt");
    }

    #[test]
    fn external_worktree_subdir_stays_with_its_worktree() {
        // A cwd nested under an external worktree root is still that worktree's,
        // never `external`.
        let root = "/repo/rimz";
        let external = "/elsewhere/feature-wt";
        let workspace = WorkspaceId::from_project_root(Path::new(root));
        let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new())
            .with_project_root(Some(PathBuf::from(root)))
            .with_worktree_roots(vec![PathBuf::from(root), PathBuf::from(external)])
            .with_live_panes(vec![pane("%1", "zsh", "/elsewhere/feature-wt/src")], None);

        let group = &snapshot.worktree_groups[0];
        assert_eq!(group.kind, SidebarWorktreeKind::Worktree);
    }

    #[test]
    fn non_worktree_path_is_the_only_external() {
        // With the worktree set known, a cwd that is neither under the project
        // root nor inside any worktree (a home shell) is all that's left as
        // `external`.
        let root = "/repo/rimz";
        let external = "/elsewhere/feature-wt";
        let workspace = WorkspaceId::from_project_root(Path::new(root));
        let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new())
            .with_project_root(Some(PathBuf::from(root)))
            .with_worktree_roots(vec![PathBuf::from(root), PathBuf::from(external)])
            .with_live_panes(vec![pane("%1", "zsh", "/home/marvin")], None);

        let group = &snapshot.worktree_groups[0];
        assert_eq!(group.kind, SidebarWorktreeKind::Workspace);
        assert_eq!(group.label, "external");
    }

    #[test]
    fn no_project_root_preserves_per_path_grouping() {
        // With no known root, an outside cwd still gets its own worktree group —
        // the prior behavior, preserved as the safe default.
        let workspace = WorkspaceId::from_project_root(Path::new("/repo/rimz"));
        let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new())
            .with_live_panes(vec![pane("%1", "zsh", "/home/marvin")], None);

        let group = &snapshot.worktree_groups[0];
        assert_eq!(group.kind, SidebarWorktreeKind::Worktree);
        assert_eq!(group.key, "/home/marvin");
        assert_eq!(group.label, "marvin");
    }

    #[test]
    fn live_panes_overlay_matching_agent_rows() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut codex = agent("codex", "sess-1", AgentStatus::Running, 1_000);
        codex.worktree_path = Some("/repo/main".to_owned());
        codex.worktree_branch = Some("main".to_owned());
        codex.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
        let snapshot =
            SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![codex])
                .with_live_panes(vec![pane("%1", "codex", "/repo/main")], None);

        assert_eq!(snapshot.worktree_groups.len(), 1);
        assert_eq!(snapshot.worktree_groups[0].rows.len(), 1);
        let row = &snapshot.worktree_groups[0].rows[0];
        assert_eq!(row.row_kind, SidebarRowKind::Agent);
        assert_eq!(row.pane.as_ref().unwrap().pane_id.raw(), "%1");
    }

    #[test]
    fn live_panes_do_not_render_unmatched_ledger_agents() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut codex = agent("codex", "sess-1", AgentStatus::Running, 1_000);
        codex.worktree_path = Some("/repo/main".to_owned());

        let snapshot =
            SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![codex])
                .with_live_panes(vec![pane("%1", "zsh", "/repo/main")], None);

        assert_eq!(snapshot.worktree_groups.len(), 1);
        assert!(
            snapshot.worktree_groups[0]
                .rows
                .iter()
                .all(|row| row.row_kind != SidebarRowKind::Agent),
            "non-attention agent rows must come from live pane presence"
        );
        assert!(
            snapshot.worktree_groups[0]
                .rows
                .iter()
                .any(|row| row.row_kind == SidebarRowKind::Process && row.name == "zsh"),
            "the live shell pane remains a process row"
        );
    }

    #[test]
    fn live_panes_suppress_stale_agent_attention_without_process() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut item = FeedItem::new(
            workspace.clone(),
            Surface::NativeUi,
            FeedKind::Permission,
            "claude needs attention",
            "claude",
            "agent-hook",
        );
        item.worktree_path = Some("/repo/main".to_owned());
        item.payload = serde_json::json!({ "session_id": "stale-claude" });

        let snapshot = SidebarSnapshot::build(workspace, vec![item], Vec::new()).with_live_panes(
            vec![
                pane(
                    "%0",
                    "/home/me/.cargo/bin/rimz-sidebar serve --workspace-id ws_x",
                    "/repo/main",
                ),
                pane("%1", "zsh", "/repo/main"),
            ],
            None,
        );

        assert_eq!(snapshot.worktree_groups.len(), 1);
        assert!(
            snapshot.worktree_groups[0]
                .rows
                .iter()
                .all(|row| row.row_kind == SidebarRowKind::Process && row.name == "zsh"),
            "a stale agent prompt must not claim the sidebar pane or outlive its agent process: {:?}",
            snapshot.worktree_groups[0].rows,
        );
        assert!(snapshot.worktree_groups[0].status_counts.is_empty());
    }

    #[test]
    fn live_panes_keep_agent_attention_with_process() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut item = FeedItem::new(
            workspace.clone(),
            Surface::NativeUi,
            FeedKind::Permission,
            "claude needs attention",
            "claude",
            "agent-hook",
        );
        item.worktree_path = Some("/repo/main".to_owned());
        item.payload = serde_json::json!({ "session_id": "live-claude" });
        // The ask's session is live in the rollup, so it binds to that
        // session's pane and renders as attention.
        let mut session = agent("claude", "live-claude", AgentStatus::Idle, 1_000);
        session.worktree_path = Some("/repo/main".to_owned());
        session.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));

        // The pane runs under a `node` wrapper, not a `claude` foreground — the
        // bind is by the session's stamped pane id, so the command is moot.
        let snapshot =
            SidebarSnapshot::build_with_carryover(workspace, vec![item], Vec::new(), vec![session])
                .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

        assert_eq!(snapshot.worktree_groups.len(), 1);
        let row = &snapshot.worktree_groups[0].rows[0];
        assert_eq!(row.row_kind, SidebarRowKind::Agent);
        assert_eq!(row.name, "claude");
        assert_eq!(row.status, Some(AgentStatus::Waiting));
        assert_eq!(row.pane.as_ref().unwrap().pane_id.raw(), "%1");
    }

    #[test]
    fn newer_subagent_does_not_expire_parent_attention() {
        // A child shares the parent's pane and worktree, so it can be newer than
        // the parent without superseding the parent's human decision surface.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut item = FeedItem::new(
            workspace.clone(),
            Surface::NativeUi,
            FeedKind::Permission,
            "claude needs attention",
            "claude",
            "agent-hook",
        );
        item.worktree_path = Some("/repo/main".to_owned());
        item.payload = serde_json::json!({ "session_id": "parent-claude" });

        let mut parent = agent("claude", "parent-claude", AgentStatus::Running, 1_000);
        parent.worktree_path = Some("/repo/main".to_owned());
        parent.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
        let mut child = agent("claude", "child-claude", AgentStatus::Idle, 2_000);
        child.parent_agent_id = Some("parent-claude".to_owned());
        child.worktree_path = Some("/repo/main".to_owned());
        child.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));

        let snapshot = SidebarSnapshot::build_with_carryover(
            workspace,
            vec![item.clone()],
            Vec::new(),
            vec![parent, child],
        )
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

        assert_eq!(
            snapshot.needs_attention[0].request_id, item.request_id,
            "the child must not make the parent's ask stale"
        );
        let row = &snapshot.worktree_groups[0].rows[0];
        assert_eq!(row.id, "parent-claude");
        assert_eq!(row.status, Some(AgentStatus::Waiting));
        assert_eq!(row.request_id, Some(item.request_id));
    }

    #[test]
    fn answered_native_ui_ask_returns_to_running() {
        // The live bug: a native_ui ask is answered in the agent's own UI and
        // the agent keeps working the same turn. The ask stays pending in the
        // ledger, but the activity heartbeat has advanced `last_activity` past
        // the ask, so the row must read `running`, not stay folded to `waiting`.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut item = FeedItem::new(
            workspace.clone(),
            Surface::NativeUi,
            FeedKind::Question,
            "claude needs attention",
            "claude",
            "agent-hook",
        );
        item.worktree_path = Some("/repo/main".to_owned());
        item.payload = serde_json::json!({ "session_id": "live-claude" });
        // Ask raised at t=1000.
        item.updated_at = Timestamp::from_second(1_000).unwrap();

        // The agent recorded progress at t=2000 — after the ask — so it has
        // un-blocked and moved on.
        let mut session = agent("claude", "live-claude", AgentStatus::Running, 2_000);
        session.worktree_path = Some("/repo/main".to_owned());
        session.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));

        let snapshot =
            SidebarSnapshot::build_with_carryover(workspace, vec![item], Vec::new(), vec![session])
                .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

        let row = &snapshot.worktree_groups[0].rows[0];
        assert_eq!(row.row_kind, SidebarRowKind::Agent);
        assert_eq!(
            row.status,
            Some(AgentStatus::Running),
            "an answered ask the agent moved past must not pin the row to waiting"
        );
    }

    #[test]
    fn answered_native_ui_ask_returns_to_running_without_panes() {
        // The same recovery as the pane path, but on the ledger-rollup fallback
        // (`rimz sidebar snapshot` with no live mux). The moved-past guard must
        // apply here too, or the answered ask falsely pins the row to waiting.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut item = FeedItem::new(
            workspace.clone(),
            Surface::NativeUi,
            FeedKind::Question,
            "claude needs attention",
            "claude",
            "agent-hook",
        );
        item.worktree_path = Some("/repo/main".to_owned());
        item.payload = serde_json::json!({ "session_id": "live-claude" });
        // Ask raised long ago; the agent recorded progress since (recent
        // `last_activity` via the `agent` helper), so it has moved past it.
        item.updated_at = Timestamp::from_second(1_000).unwrap();
        let mut session = agent("claude", "live-claude", AgentStatus::Running, 2_000);
        session.worktree_path = Some("/repo/main".to_owned());

        // No `with_live_panes`: the snapshot stays on the ledger-rollup path.
        let snapshot =
            SidebarSnapshot::build_with_carryover(workspace, vec![item], Vec::new(), vec![session]);

        let row = &snapshot.worktree_groups[0].rows[0];
        assert_eq!(
            row.status,
            Some(AgentStatus::Running),
            "the moved-past recovery must also apply on the no-pane ledger fallback"
        );
    }

    #[test]
    fn stalled_running_agent_recovers_when_activity_resumes() {
        // The stall escalation is self-healing: once the agent's next completed
        // tool touches the activity heartbeat, the fold readvances
        // `last_activity`, `is_stalled` goes false, and the row drops back out
        // of attention with no human action.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut session = agent("claude", "live-claude", AgentStatus::Running, 0);
        session.worktree_path = Some("/repo/main".to_owned());
        session.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
        // Silent past the stall window.
        session.last_activity = Timestamp::now()
            - std::time::Duration::from_secs(crate::feed::STALL_WINDOW_SECS as u64 + 60);

        // A fresh heartbeat lands (the agent's next tool completed).
        let touch = AgentActivity {
            kind: "claude".to_owned(),
            agent_id: "live-claude".to_owned(),
            at: Timestamp::now(),
        };
        let snapshot =
            SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![session])
                .with_agent_activity(&[touch])
                .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

        let row = &snapshot.worktree_groups[0].rows[0];
        assert_eq!(
            row.status,
            Some(AgentStatus::Running),
            "a fresh heartbeat readvances last_activity, so the stalled row recovers"
        );
    }

    #[test]
    fn stalled_running_agent_escalates_to_attention() {
        // A running agent that records no activity past the stall window is
        // likely wedged; the displayed row escalates to the attention bucket
        // (`!`) and the rollup keeps the true `running` status.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut session = agent("claude", "live-claude", AgentStatus::Running, 0);
        session.worktree_path = Some("/repo/main".to_owned());
        session.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
        session.last_activity = Timestamp::now()
            - std::time::Duration::from_secs(crate::feed::STALL_WINDOW_SECS as u64 + 60);

        let snapshot =
            SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![session])
                .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

        let row = &snapshot.worktree_groups[0].rows[0];
        assert_eq!(
            row.status,
            Some(AgentStatus::Failed),
            "a long-silent running agent escalates to the attention bucket"
        );
        assert!(
            snapshot.worktree_groups[0]
                .status_counts
                .iter()
                .any(|count| count.status == AgentStatus::Failed && count.count == 1),
            "the stalled agent counts in the attention tally"
        );
        let rolled_up = snapshot
            .agents
            .iter()
            .find(|a| a.agent_id == "live-claude")
            .expect("agent in rollup");
        assert_eq!(
            rolled_up.status,
            AgentStatus::Running,
            "the rollup keeps the true running status; only the display row escalates"
        );
    }

    fn ctx_with_limits(windows: Vec<RateLimitWindow>) -> AgentContext {
        AgentContext {
            source: "claude".to_owned(),
            session_name: None,
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
            rate_limits: Some(crate::agents::AgentRateLimits { windows }),
            pr: None,
            account: None,
            turn_error: None,
            observed_at: Timestamp::now(),
        }
    }

    #[test]
    fn spent_account_parks_every_resting_agent_of_the_kind() {
        // Account-scoped: one claude session reports a spent 5-hour window, so
        // the whole kind is rate-limited — including a *fresh* idle session that
        // carries no context of its own yet (the "launched into a spent account"
        // case). A working session is left alone; its turn finishes before it can
        // park.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut reporter = agent("claude", "sess-spent", AgentStatus::Success, 1_000);
        reporter.worktree_path = Some("/repo/main".to_owned());
        reporter.context = Some(ctx_with_limits(vec![window(100, 3_600)]));
        let mut fresh = agent("claude", "sess-fresh", AgentStatus::Idle, 1_100);
        fresh.worktree_path = Some("/repo/main".to_owned());
        let mut working = agent("claude", "sess-busy", AgentStatus::Running, 1_200);
        working.worktree_path = Some("/repo/main".to_owned());

        let snapshot = SidebarSnapshot::build_with_agents(
            workspace,
            Vec::new(),
            vec![reporter, fresh, working],
        );
        let status_of = |id: &str| {
            snapshot
                .worktree_groups
                .iter()
                .flat_map(|group| &group.rows)
                .find(|row| row.id == id)
                .unwrap_or_else(|| panic!("row {id} present"))
                .status
        };
        assert_eq!(status_of("sess-spent"), Some(AgentStatus::RateLimited));
        assert_eq!(
            status_of("sess-fresh"),
            Some(AgentStatus::RateLimited),
            "a fresh idle session inherits the account verdict"
        );
        assert_eq!(
            status_of("sess-busy"),
            Some(AgentStatus::Running),
            "a working session is never parked"
        );
        // The rollup keeps the true lifecycle status; only the display projects.
        assert_eq!(
            snapshot
                .agents
                .iter()
                .find(|a| a.agent_id == "sess-fresh")
                .unwrap()
                .status,
            AgentStatus::Idle
        );
    }

    #[test]
    fn a_window_spent_but_already_reset_does_not_park() {
        // A spent reading whose reset has passed is stale, not limiting — the
        // budget has refilled, so a resting agent reads idle, not parked.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut idle = agent("claude", "sess-1", AgentStatus::Idle, 1_000);
        idle.worktree_path = Some("/repo/main".to_owned());
        idle.context = Some(ctx_with_limits(vec![window(100, -60)]));

        let snapshot = SidebarSnapshot::build_with_agents(workspace, Vec::new(), vec![idle]);
        assert_eq!(
            snapshot.worktree_groups[0].rows[0].status,
            Some(AgentStatus::Idle),
            "a passed reset means the budget refilled — not rate-limited"
        );
    }

    #[test]
    fn running_parent_with_a_live_subagent_waits_instead_of_stalling() {
        // A running parent that has delegated to a live child shows no heartbeat
        // of its own, so the stall window would falsely escalate it. The
        // delegated-wait exemption keeps it `running` while a child runs; the
        // renderer paints the waiting-on-subagents head from `sub_agents`.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut parent = agent("claude", "root", AgentStatus::Running, 1_000);
        parent.worktree_path = Some("/repo/main".to_owned());
        parent.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
        // Silent past the stall window — its heartbeat is quiet because the work
        // is the child's, not a wedge.
        parent.last_activity = Timestamp::now()
            - std::time::Duration::from_secs(crate::feed::STALL_WINDOW_SECS as u64 + 60);
        let mut child = child_state("root", "child-1", AgentStatus::Running, 5);
        child.kind = "claude".to_owned();

        let snapshot = SidebarSnapshot::build_with_carryover(
            workspace,
            Vec::new(),
            Vec::new(),
            vec![parent, child],
        )
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

        let row = &snapshot.worktree_groups[0].rows[0];
        assert_eq!(
            row.status,
            Some(AgentStatus::Running),
            "a parent delegating to a live child is waiting on it, not stalled"
        );
        assert!(
            row.sub_agents
                .iter()
                .any(|child| child.status == AgentStatus::Running),
            "the live child is nested so the renderer can paint the wait head"
        );
    }

    fn ctx_with_turn_error(at: Timestamp, label: &str) -> AgentContext {
        AgentContext {
            source: "claude".to_owned(),
            session_name: None,
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
            turn_error: Some(crate::agents::AgentTurnError {
                at,
                label: Some(label.to_owned()),
            }),
            observed_at: Timestamp::now(),
        }
    }

    #[test]
    fn api_error_turn_escalates_running_to_attention() {
        // A turn that died on a provider API error fires no Stop hook, so the
        // rollup keeps `running` — but the transcript marker postdates the
        // agent's own activity, and the projection escalates at once. The
        // headline: the agent is *inside* the stall window (silent only a
        // minute), so this beats the 10-minute backstop.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut session = agent("claude", "live-claude", AgentStatus::Running, 0);
        session.worktree_path = Some("/repo/main".to_owned());
        session.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
        session.last_activity = Timestamp::now() - std::time::Duration::from_secs(60);
        session.context = Some(ctx_with_turn_error(
            Timestamp::now() - std::time::Duration::from_secs(10),
            "API Error: Overloaded",
        ));

        let snapshot =
            SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![session])
                .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

        let row = &snapshot.worktree_groups[0].rows[0];
        assert_eq!(
            row.status,
            Some(AgentStatus::Failed),
            "the explicit death certificate escalates without waiting out the stall window"
        );
        assert_eq!(
            row.turn_error_label.as_deref(),
            Some("API Error: Overloaded"),
            "the row carries the upstream error text for the card's line 2"
        );
        assert!(
            snapshot.worktree_groups[0]
                .status_counts
                .iter()
                .any(|count| count.status == AgentStatus::Failed && count.count == 1),
            "the dead turn counts in the attention tally"
        );
        let rolled_up = snapshot
            .agents
            .iter()
            .find(|a| a.agent_id == "live-claude")
            .expect("agent in rollup");
        assert_eq!(
            rolled_up.status,
            AgentStatus::Running,
            "the rollup keeps the agent-owned status; only the display row escalates"
        );
    }

    #[test]
    fn api_error_self_clears_when_activity_resumes() {
        // Any newer hook event (a prompt, a resume, a rewind) advances
        // `last_activity` past the stale marker and the escalation drops with
        // no human action — the self-clear guard.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut session = agent("claude", "live-claude", AgentStatus::Running, 0);
        session.worktree_path = Some("/repo/main".to_owned());
        session.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
        session.last_activity = Timestamp::now() - std::time::Duration::from_secs(30);
        session.context = Some(ctx_with_turn_error(
            Timestamp::now() - std::time::Duration::from_secs(120),
            "API Error: Overloaded",
        ));

        let snapshot =
            SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![session])
                .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

        let row = &snapshot.worktree_groups[0].rows[0];
        assert_eq!(
            row.status,
            Some(AgentStatus::Running),
            "activity newer than the marker means the session moved on"
        );
        assert!(
            row.turn_error_label.is_none(),
            "a cleared escalation leaves no stale reason label"
        );
    }

    #[test]
    fn api_error_does_not_override_waiting() {
        // A human-blocked ask outranks every derived state, the dead-turn
        // escalation included.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut session = agent("claude", "live-claude", AgentStatus::Waiting, 0);
        session.worktree_path = Some("/repo/main".to_owned());
        session.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
        session.last_activity = Timestamp::now() - std::time::Duration::from_secs(60);
        session.context = Some(ctx_with_turn_error(
            Timestamp::now() - std::time::Duration::from_secs(10),
            "API Error: Overloaded",
        ));

        let snapshot =
            SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![session])
                .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

        let row = &snapshot.worktree_groups[0].rows[0];
        assert_eq!(row.status, Some(AgentStatus::Waiting));
        assert!(row.turn_error_label.is_none());
    }

    #[test]
    fn dead_parent_with_live_child_keeps_running() {
        // The delegated-wait exemption wins: a live child's heartbeats are the
        // parent's work, so a stale parent marker never escalates over it. If
        // the children also die, the stall window remains the backstop.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut parent = agent("claude", "root", AgentStatus::Running, 1_000);
        parent.worktree_path = Some("/repo/main".to_owned());
        parent.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
        parent.last_activity = Timestamp::now() - std::time::Duration::from_secs(60);
        parent.context = Some(ctx_with_turn_error(
            Timestamp::now() - std::time::Duration::from_secs(10),
            "API Error: Overloaded",
        ));
        let mut child = child_state("root", "child-1", AgentStatus::Running, 5);
        child.kind = "claude".to_owned();

        let snapshot = SidebarSnapshot::build_with_carryover(
            workspace,
            Vec::new(),
            Vec::new(),
            vec![parent, child],
        )
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

        let row = &snapshot.worktree_groups[0].rows[0];
        assert_eq!(row.status, Some(AgentStatus::Running));
        assert!(row.turn_error_label.is_none());
    }

    #[test]
    fn compacting_marker_lights_the_head_then_expires() {
        // A fresh compaction marker pulses the head; one older than the window
        // has expired (the crash backstop), so the head returns to its base.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut fresh = agent("claude", "compacting-now", AgentStatus::Running, 1_000);
        fresh.worktree_path = Some("/repo/main".to_owned());
        fresh.compacting_since = Some(Timestamp::now());
        let mut stale = agent("claude", "compacted-long-ago", AgentStatus::Idle, 1_100);
        stale.worktree_path = Some("/repo/main".to_owned());
        stale.compacting_since = Some(
            Timestamp::now()
                - std::time::Duration::from_secs(crate::feed::COMPACTING_WINDOW_SECS as u64 + 10),
        );

        let snapshot =
            SidebarSnapshot::build_with_agents(workspace, Vec::new(), vec![fresh, stale]);
        let row = |id: &str| {
            snapshot
                .worktree_groups
                .iter()
                .flat_map(|group| &group.rows)
                .find(|row| row.id == id)
                .unwrap_or_else(|| panic!("row {id} present"))
        };
        assert!(row("compacting-now").compacting, "a fresh marker pulses");
        assert!(
            !row("compacted-long-ago").compacting,
            "a marker past the window has expired"
        );
    }

    #[test]
    fn compaction_event_stamps_then_a_later_event_clears_the_marker() {
        // The reducer treats a `compacting` event as a transient: it stamps
        // `compacting_since` and keeps the prior status (not a transition); the
        // next lifecycle event means compaction is done and clears the marker.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let lifecycle = |params: serde_json::Value| {
            EventEnvelope::new(
                workspace.clone(),
                "session",
                "claude",
                "agent-hook",
                "agent.lifecycle",
                params,
            )
        };
        let prompt = lifecycle(serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-1",
            "status": "running",
        }));
        let compact = lifecycle(serde_json::json!({
            "event_name": "PreCompact",
            "agent_id": "sess-1",
            "compacting": true,
        }));
        let after_compact = reduce_agent_states(&[prompt.clone(), compact.clone()]);
        assert!(
            after_compact[0].compacting_since.is_some(),
            "the compaction marker is stamped"
        );
        assert_eq!(
            after_compact[0].status,
            AgentStatus::Running,
            "compaction keeps the prior status — it is not a transition"
        );

        let stop = lifecycle(serde_json::json!({
            "event_name": "Stop",
            "agent_id": "sess-1",
            "status": "success",
        }));
        let after_stop = reduce_agent_states(&[prompt, compact, stop]);
        assert!(
            after_stop[0].compacting_since.is_none(),
            "the next lifecycle event clears the marker"
        );
        assert_eq!(after_stop[0].status, AgentStatus::Success);
    }

    #[test]
    fn two_same_kind_agents_bind_to_their_stamped_panes() {
        // Two claude sessions in one worktree are indistinguishable by name and
        // cwd alone; binding is by the hook-stamped pane id, so each session
        // lands on exactly its own pane instead of cross-wiring the rows.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut older = agent("claude", "sess-a", AgentStatus::Idle, 1_000);
        older.worktree_path = Some("/repo/main".to_owned());
        older.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
        let mut newer = agent("claude", "sess-b", AgentStatus::Running, 2_000);
        newer.worktree_path = Some("/repo/main".to_owned());
        newer.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%2")));

        let snapshot = SidebarSnapshot::build_with_carryover(
            workspace,
            Vec::new(),
            Vec::new(),
            vec![older, newer],
        )
        .with_live_panes(
            vec![
                pane("%1", "claude", "/repo/main"),
                pane("%2", "claude", "/repo/main"),
            ],
            None,
        );

        assert_eq!(snapshot.worktree_groups.len(), 1);
        let rows = &snapshot.worktree_groups[0].rows;
        let by_id = |id: &str| {
            rows.iter()
                .find(|row| row.id == id)
                .unwrap_or_else(|| panic!("row {id} missing from {rows:?}"))
        };
        assert_eq!(by_id("sess-a").pane.as_ref().unwrap().pane_id.raw(), "%1");
        assert_eq!(by_id("sess-b").pane.as_ref().unwrap().pane_id.raw(), "%2");
    }

    #[test]
    fn agent_binds_only_by_stamped_pane_id() {
        // The pane-keyed invariant: an agent stamped `%2`, but only `%1` is
        // live. `%1`'s command and cwd both match the agent — under the old
        // command/cwd fallback it would have bound. Stamped-id binding refuses
        // it, so `%1` stays a process row and the agent simply does not render.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut claude = agent("claude", "sess-1", AgentStatus::Running, 1_000);
        claude.worktree_path = Some("/repo/main".to_owned());
        claude.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%2")));

        let snapshot =
            SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![claude])
                .with_live_panes(vec![pane("%1", "claude", "/repo/main")], None);

        let rows = &snapshot.worktree_groups[0].rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].row_kind, SidebarRowKind::Process);
        assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "%1");
    }

    #[test]
    fn subagent_never_steals_its_parents_pane() {
        // A subagent runs in its parent's pane, so its lifecycle hooks stamp the
        // parent's pane id — parent and child both claim `%1`. The child here is
        // strictly more recently active than the parked parent, which would let
        // `max_by_key(last_activity)` bind the pane to the child. Panes bind root
        // agents only: `%1` stays the parent's row and the child nests under it.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut parent = agent("claude", "sess-root", AgentStatus::Running, 1_000);
        parent.worktree_path = Some("/repo/main".to_owned());
        parent.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
        // Newer activity than the parent (5s ago vs ~99s ago) — the flip trigger.
        let mut child = child_state("sess-root", "child-1", AgentStatus::Running, 5);
        child.worktree_path = Some("/repo/main".to_owned());
        child.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));

        let snapshot = SidebarSnapshot::build_with_carryover(
            workspace,
            Vec::new(),
            Vec::new(),
            vec![parent, child],
        )
        .with_live_panes(vec![pane("%1", "claude", "/repo/main")], None);

        let rows = &snapshot.worktree_groups[0].rows;
        assert_eq!(rows.len(), 1, "one pane binds exactly one top-level row");
        assert_eq!(
            rows[0].id, "sess-root",
            "the pane binds the root, not the child"
        );
        assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "%1");
        assert_eq!(
            rows[0].sub_agents.len(),
            1,
            "the child nests under the parent"
        );
        assert_eq!(rows[0].sub_agents[0].id, "child-1");
        assert_eq!(rows[0].sub_agents[0].name, "Explore");
    }

    #[test]
    fn each_live_pane_yields_exactly_one_row() {
        // One pane = one row, by construction: every live pane produces exactly
        // one row — agent or process — and no pane id is ever duplicated.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let stamped = |id, raw| {
            let mut a = agent("claude", id, AgentStatus::Running, 1_000);
            a.worktree_path = Some("/repo/main".to_owned());
            a.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, raw)));
            a
        };

        let snapshot = SidebarSnapshot::build_with_carryover(
            workspace,
            Vec::new(),
            Vec::new(),
            vec![stamped("sess-a", "%1"), stamped("sess-b", "%2")],
        )
        .with_live_panes(
            vec![
                pane("%1", "claude", "/repo/main"),
                pane("%2", "claude", "/repo/main"),
                pane("%3", "zsh", "/repo/main"),
            ],
            None,
        );

        let rows: Vec<_> = snapshot
            .worktree_groups
            .iter()
            .flat_map(|group| &group.rows)
            .collect();
        assert_eq!(rows.len(), 3, "three panes render three rows: {rows:?}");
        let mut pane_ids: Vec<&str> = rows
            .iter()
            .map(|row| row.pane.as_ref().unwrap().pane_id.raw())
            .collect();
        pane_ids.sort_unstable();
        assert_eq!(pane_ids, vec!["%1", "%2", "%3"], "no pane id is duplicated");
        let agents = rows
            .iter()
            .filter(|row| row.row_kind == SidebarRowKind::Agent)
            .count();
        assert_eq!(agents, 2, "the two stamped panes bound their agents");
    }

    fn paneless_codex(id: &str, worktree: &str, rank: i64) -> AgentState {
        let mut codex = agent("codex", id, AgentStatus::Running, rank);
        // The app-server daemon fires the hook with no mux pane env, so the
        // agent carries its worktree but never stamps a pane.
        codex.worktree_path = Some(worktree.to_owned());
        codex
    }

    #[test]
    fn paneless_codex_agent_binds_to_its_worktree_pane() {
        // The daemon exception: a Codex agent the app-server daemon registered
        // has no stamped pane, but its worktree matches the live `codex` pane's
        // cwd, so the cwd fallback binds it as an agent row — not a process row.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let snapshot = SidebarSnapshot::build_with_carryover(
            workspace,
            Vec::new(),
            Vec::new(),
            vec![paneless_codex("sess-1", "/repo/main", 1_000)],
        )
        .with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

        let rows = &snapshot.worktree_groups[0].rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].row_kind, SidebarRowKind::Agent);
        assert_eq!(rows[0].name, "codex");
        assert_eq!(rows[0].id, "sess-1");
        assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "term1");
    }

    #[test]
    fn paneless_codex_agent_in_other_worktree_stays_a_process_row() {
        // The cwd fallback never crosses worktrees: a pane-less Codex agent in a
        // different worktree leaves the live `codex` pane a process row.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let snapshot = SidebarSnapshot::build_with_carryover(
            workspace,
            Vec::new(),
            Vec::new(),
            vec![paneless_codex("sess-1", "/repo/other", 1_000)],
        )
        .with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

        let rows = &snapshot.worktree_groups[0].rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].row_kind, SidebarRowKind::Process);
        assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "term1");
    }

    #[test]
    fn paneless_codex_agent_does_not_capture_a_nested_worktree_pane() {
        // Worktree match is exact, not containment: a session checked out at the
        // parent `/repo` must not capture a `codex` pane running in a nested
        // worktree under it (this repo nests worktrees under `.claude/`).
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let snapshot = SidebarSnapshot::build_with_carryover(
            workspace,
            Vec::new(),
            Vec::new(),
            vec![paneless_codex("sess-1", "/repo", 1_000)],
        )
        .with_live_panes(vec![pane("term1", "codex", "/repo/sub")], None);

        let rows = &snapshot.worktree_groups[0].rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].row_kind, SidebarRowKind::Process);
        assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "term1");
    }

    #[test]
    fn paneless_codex_does_not_bind_a_non_codex_pane() {
        // The pane's own command gates the fallback: a shell the session dropped
        // back to in the worktree stays a process row, never an agent.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let snapshot = SidebarSnapshot::build_with_carryover(
            workspace,
            Vec::new(),
            Vec::new(),
            vec![paneless_codex("sess-1", "/repo/main", 1_000)],
        )
        .with_live_panes(vec![pane("term1", "zsh", "/repo/main")], None);

        let rows = &snapshot.worktree_groups[0].rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].row_kind, SidebarRowKind::Process);
    }

    #[test]
    fn paneless_claude_agent_is_never_rescued_by_cwd() {
        // Only Codex is daemon-backed and pane-less by construction. A pane-less
        // Claude agent is genuinely gone (Claude always stamps a live pane), so
        // the fallback must leave a matching `claude` pane a process row.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut claude = agent("claude", "sess-1", AgentStatus::Running, 1_000);
        claude.worktree_path = Some("/repo/main".to_owned());
        let snapshot =
            SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![claude])
                .with_live_panes(vec![pane("term1", "claude", "/repo/main")], None);

        let rows = &snapshot.worktree_groups[0].rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].row_kind, SidebarRowKind::Process);
    }

    #[test]
    fn two_paneless_codex_in_one_worktree_bind_most_recent() {
        // When two pane-less Codex sessions claim one worktree — a lingering
        // closed session and a live one — the most-recently-active binds the
        // single live pane; the stale session does not render.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let snapshot = SidebarSnapshot::build_with_carryover(
            workspace,
            Vec::new(),
            Vec::new(),
            vec![
                paneless_codex("sess-old", "/repo/main", 1_000),
                paneless_codex("sess-new", "/repo/main", 2_000),
            ],
        )
        .with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

        let rows = &snapshot.worktree_groups[0].rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].row_kind, SidebarRowKind::Agent);
        assert_eq!(rows[0].id, "sess-new");
    }

    #[test]
    fn paneless_codex_predating_pane_start_does_not_bind() {
        // The defensive guard on the cwd fallback: when the backend reports the
        // pane's process start, a pane-less Codex session whose last activity
        // predates it belongs to an older instance that once ran in this worktree,
        // not the process now in the pane. A daemon-mode session records the shared
        // daemon pid, so process liveness can't tell the stale one from the live
        // one — so the bind is refused and the fresh pane stays a process row until
        // its own session reports.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let pane_start = Timestamp::now();
        let mut stale = paneless_codex("sess-old", "/repo/main", 1_000);
        stale.last_activity = pane_start - std::time::Duration::from_secs(60);
        let fresh_pane = PaneRef {
            pane_process_start: Some(pane_start),
            ..pane("term1", "codex", "/repo/main")
        };
        let snapshot =
            SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![stale])
                .with_live_panes(vec![fresh_pane], None);

        let rows = &snapshot.worktree_groups[0].rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].row_kind,
            SidebarRowKind::Process,
            "a session predating the pane start must not bind it",
        );
    }

    #[test]
    fn paneless_codex_active_after_pane_start_binds() {
        // The guard never over-blocks: a session whose last activity is at or after
        // the pane's process start is the live occupant and binds normally.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let pane_start = Timestamp::now();
        let mut live = paneless_codex("sess-1", "/repo/main", 1_000);
        live.last_activity = pane_start + std::time::Duration::from_secs(5);
        let started_pane = PaneRef {
            pane_process_start: Some(pane_start),
            ..pane("term1", "codex", "/repo/main")
        };
        let snapshot =
            SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![live])
                .with_live_panes(vec![started_pane], None);

        let rows = &snapshot.worktree_groups[0].rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].row_kind, SidebarRowKind::Agent);
        assert_eq!(rows[0].id, "sess-1");
        assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "term1");
    }

    #[test]
    fn fresh_codex_pane_with_proc_start_shows_idle_not_ghost() {
        // The ghost-stats regression. A completed daemon-mode Codex session lingers
        // in the rollup — its owner is the shared, still-alive app-server daemon, so
        // process liveness can never reap it, and the daemon still holds the thread
        // loaded so the loaded-set reap keeps it too. A fresh `codex` then starts in
        // the same worktree. On Zellij the backend reports no pane process start, so
        // the producer stamps the in-pane CLI's `/proc` start; fed that, the guard
        // refuses the stale session and the wired pane renders the synthesized idle
        // row (`○ codex`) — not yesterday's `success` stats — until its own first
        // turn binds a new session.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let pane_start = Timestamp::now();
        let mut ghost = paneless_codex("sess-old", "/repo/main", 1_000);
        ghost.status = AgentStatus::Success;
        ghost.total_tokens = Some(126_621);
        ghost.model = Some("gpt-5.5".to_owned());
        ghost.last_activity = pane_start - std::time::Duration::from_secs(12 * 60 * 60);
        let mut snapshot =
            SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![ghost]);
        snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
        let fresh_pane = PaneRef {
            pane_process_start: Some(pane_start),
            ..pane("term1", "codex", "/repo/main")
        };
        let snapshot = snapshot.with_live_panes(vec![fresh_pane], None);

        let rows = &snapshot.worktree_groups[0].rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].row_kind, SidebarRowKind::Agent);
        assert_eq!(rows[0].status, Some(AgentStatus::Idle));
        // The synthesized idle row keys on the pane id, never the stale session, and
        // carries none of its stats.
        assert_eq!(rows[0].id, "tmux:term1");
        assert_ne!(rows[0].id, "sess-old");
        assert_eq!(
            rows[0].total_tokens, None,
            "no ghost tokens on a fresh pane"
        );
        assert_eq!(rows[0].model, None, "no ghost model on a fresh pane");
    }

    fn daemon_codex(id: &str, worktree: &str, owner_pid: u32) -> AgentState {
        let mut codex = paneless_codex(id, worktree, 1_000);
        codex.runtime_owner = Some(RuntimeOwner::new(
            RuntimeOwnerKind::Agent,
            id,
            owner_pid,
            None,
        ));
        codex.agent_pid = Some(owner_pid);
        codex
    }

    fn daemon_snapshot(agents: Vec<AgentState>) -> SidebarSnapshot {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), agents)
    }

    fn rollup_ids(snapshot: &SidebarSnapshot) -> Vec<String> {
        let mut ids: Vec<String> = snapshot.agents.iter().map(|a| a.agent_id.clone()).collect();
        ids.sort();
        ids
    }

    #[test]
    fn daemon_session_absent_from_loaded_is_reaped() {
        // The shared daemon pid is alive, so process liveness keeps the ghost; the
        // app-server no longer holds the thread, so the loaded-set filter reaps it
        // while keeping the session it still holds.
        let daemon_pids = BTreeSet::from([7]);
        let loaded = BTreeSet::from(["t-live".to_owned()]);
        let mut snapshot = daemon_snapshot(vec![
            daemon_codex("t-live", "/repo/a", 7),
            daemon_codex("t-gone", "/repo/b", 7),
        ]);
        snapshot.drop_dead_daemon_sessions(&daemon_pids, Some(&loaded));
        assert_eq!(rollup_ids(&snapshot), vec!["t-live"]);
    }

    #[test]
    fn unknown_loaded_set_keeps_every_session() {
        // `None` means the daemon was unreachable or its list untrusted — never
        // mass-reap.
        let daemon_pids = BTreeSet::from([7]);
        let mut snapshot = daemon_snapshot(vec![daemon_codex("t-gone", "/repo/b", 7)]);
        snapshot.drop_dead_daemon_sessions(&daemon_pids, None);
        assert_eq!(rollup_ids(&snapshot), vec!["t-gone"]);
    }

    #[test]
    fn empty_daemon_pids_keeps_every_session() {
        // No daemon is running, so every session is standalone — process liveness
        // governs them, not the loaded-thread set.
        let loaded = BTreeSet::new();
        let mut snapshot = daemon_snapshot(vec![daemon_codex("t-gone", "/repo/b", 7)]);
        snapshot.drop_dead_daemon_sessions(&BTreeSet::new(), Some(&loaded));
        assert_eq!(rollup_ids(&snapshot), vec!["t-gone"]);
    }

    #[test]
    fn standalone_codex_is_not_reaped_by_the_loaded_set() {
        // A session whose owner pid is its own in-pane CLI (not a daemon pid) is not
        // daemon-mode, so its absence from the daemon's loaded set means nothing.
        let daemon_pids = BTreeSet::from([7]);
        let loaded = BTreeSet::new();
        let mut snapshot = daemon_snapshot(vec![daemon_codex("t-standalone", "/repo/b", 99)]);
        snapshot.drop_dead_daemon_sessions(&daemon_pids, Some(&loaded));
        assert_eq!(rollup_ids(&snapshot), vec!["t-standalone"]);
    }

    #[test]
    fn daemon_filter_spares_subagents_and_other_kinds() {
        // A codex subagent id is never a root thread, and a non-codex agent is never
        // daemon-mode — neither is reaped even sharing the daemon pid and absent from
        // the loaded set.
        let daemon_pids = BTreeSet::from([7]);
        let loaded = BTreeSet::new();
        let mut sub = daemon_codex("sub-1", "/repo/a", 7);
        sub.parent_agent_id = Some("root-1".to_owned());
        let mut claude = daemon_codex("claude-1", "/repo/c", 7);
        claude.kind = "claude".to_owned();
        let mut snapshot = daemon_snapshot(vec![sub, claude]);
        snapshot.drop_dead_daemon_sessions(&daemon_pids, Some(&loaded));
        assert_eq!(rollup_ids(&snapshot), vec!["claude-1", "sub-1"]);
    }

    #[test]
    fn wired_unprompted_codex_pane_renders_as_idle_agent() {
        // Codex registers its session lazily — `SessionStart` rides in with the
        // first prompt — so a launched-but-never-prompted `codex` pane has no
        // agent state. When Codex is wired it must read as an idle agent (`○ codex`
        // with its gauge and a cockpit tally), not a bare, dim process row, the
        // moment it opens.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut snapshot =
            SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), Vec::new());
        snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
        let snapshot = snapshot.with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

        let rows = &snapshot.worktree_groups[0].rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].row_kind, SidebarRowKind::Agent);
        assert_eq!(rows[0].name, "codex");
        assert_eq!(rows[0].status, Some(AgentStatus::Idle));
        // No session id exists yet, so the row keys on the pane id (its full
        // mux-qualified form, as `row_from_process` does).
        assert_eq!(rows[0].id, "tmux:term1");
        assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "term1");
        assert_eq!(
            rows[0].model, None,
            "no model until the first turn enriches it"
        );
    }

    #[test]
    fn non_lazy_agent_pane_is_never_idle_synthesized() {
        // The idle-instance synthesis is gated on the agent registering lazily
        // (`registers_session_lazily`), not merely on being wired. Claude stamps a
        // pane on every session, so an unbound `claude` pane stays a process row
        // even when the producer is told claude is a wired lazy kind — the static
        // trait gate refuses it. This is what keeps the lifecycle agent-agnostic
        // (a new lazy agent slots in by overriding the trait) without changing how
        // Claude renders.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut snapshot =
            SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), Vec::new());
        snapshot.wired_lazy_kinds = vec!["claude".to_owned(), "codex".to_owned()];
        let snapshot = snapshot.with_live_panes(vec![pane("term1", "claude", "/repo/main")], None);

        let rows = &snapshot.worktree_groups[0].rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].row_kind, SidebarRowKind::Process);
    }

    #[test]
    fn unwired_codex_pane_stays_a_process_row() {
        // The consent invariant: an unwired Codex can report no status, so its
        // live pane stays a process row (agents are invisible until their hooks
        // are wired). `wired_lazy_kinds` left empty reproduces an un-onboarded
        // Codex.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let snapshot =
            SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), Vec::new())
                .with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

        let rows = &snapshot.worktree_groups[0].rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].row_kind, SidebarRowKind::Process);
        assert_eq!(rows[0].name, "codex");
    }

    #[test]
    fn bound_codex_pane_keeps_its_real_agent_over_idle_synthesis() {
        // The idle synthesis is a last resort: a `codex` pane that binds a real
        // (pane-less, cwd-matched) agent keeps that agent's identity and status,
        // never the synthesized idle row — even with Codex wired.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut snapshot = SidebarSnapshot::build_with_carryover(
            workspace,
            Vec::new(),
            Vec::new(),
            vec![paneless_codex("sess-1", "/repo/main", 1_000)],
        );
        snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
        let snapshot = snapshot.with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

        let rows = &snapshot.worktree_groups[0].rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].row_kind, SidebarRowKind::Agent);
        assert_eq!(
            rows[0].id, "sess-1",
            "the real agent binds, not a synthesis"
        );
        assert_eq!(rows[0].status, Some(AgentStatus::Running));
    }

    #[test]
    fn two_codex_panes_one_agent_yields_one_real_one_idle() {
        // The multi-codex-per-worktree case: one prompted (pane-less) agent plus a
        // second still-unprompted `codex` pane in the same worktree. The agent
        // binds the first codex pane by cwd; the second synthesizes an idle row —
        // no codex pane is ever left as a process row.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut snapshot = SidebarSnapshot::build_with_carryover(
            workspace,
            Vec::new(),
            Vec::new(),
            vec![paneless_codex("sess-1", "/repo/main", 1_000)],
        );
        snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
        let snapshot = snapshot.with_live_panes(
            vec![
                pane("term1", "codex", "/repo/main"),
                pane("term2", "codex", "/repo/main"),
            ],
            None,
        );

        let rows = &snapshot.worktree_groups[0].rows;
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter().all(|row| row.row_kind == SidebarRowKind::Agent),
            "neither codex pane is a process row",
        );
        assert!(
            rows.iter().any(|row| row.id == "sess-1"),
            "the prompted session binds one pane",
        );
        assert!(
            rows.iter().any(|row| row.status == Some(AgentStatus::Idle)),
            "the unprompted pane synthesizes an idle row",
        );
    }

    #[test]
    fn unbound_claude_pane_stays_a_process_row_even_when_codex_wired() {
        // The synthesis is Codex-only: Claude always stamps a live pane, so a
        // `claude` pane with no bound agent is a genuinely-ended session and must
        // read as a process row, never an idle agent — regardless of Codex wiring.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut snapshot =
            SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), Vec::new());
        snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
        let snapshot = snapshot.with_live_panes(vec![pane("term1", "claude", "/repo/main")], None);

        let rows = &snapshot.worktree_groups[0].rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].row_kind, SidebarRowKind::Process);
        assert_eq!(rows[0].name, "claude");
    }

    #[test]
    fn stale_session_ask_does_not_render_or_steal_a_pane() {
        // Reproduces the live bug: a pending permission ask whose claude
        // session has ended must not become attention, and must not latch onto
        // a freshly launched codex sharing the worktree.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut stale = FeedItem::new(
            workspace.clone(),
            Surface::NativeUi,
            FeedKind::Permission,
            "claude needs attention",
            "claude",
            "agent-hook",
        );
        stale.worktree_path = Some("/repo/main".to_owned());
        stale.payload = serde_json::json!({ "session_id": "ended-claude" });

        // Only a live codex session remains in the rollup.
        let mut codex = agent("codex", "sess-codex", AgentStatus::Idle, 2_000);
        codex.worktree_path = Some("/repo/main".to_owned());
        codex.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));

        let snapshot =
            SidebarSnapshot::build_with_carryover(workspace, vec![stale], Vec::new(), vec![codex])
                .with_live_panes(vec![pane("%1", "codex", "/repo/main")], None);

        assert!(
            snapshot.needs_attention.is_empty(),
            "stale ask is not attention"
        );
        assert_eq!(snapshot.worktree_groups.len(), 1);
        let rows = &snapshot.worktree_groups[0].rows;
        assert_eq!(rows.len(), 1, "only the live codex renders");
        assert_eq!(rows[0].name, "codex");
        assert_eq!(rows[0].status, Some(AgentStatus::Idle));
        assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "%1");
    }

    #[test]
    fn superseded_zombie_ask_yields_pane_to_the_fresh_session() {
        // Live reproduction: a pidless `SessionStart`-only claude never ends and
        // never gets reaped, so it lingers in the rollup with an old pending
        // ask. A freshly launched claude shares the worktree. The ask must not
        // render as attention or pin the dead session's "permission" task and
        // stale timestamp onto the live pane — the fresh session binds it idle.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut stale = FeedItem::new(
            workspace.clone(),
            Surface::NativeUi,
            FeedKind::Permission,
            "claude needs attention",
            "claude",
            "agent-hook",
        );
        stale.worktree_path = Some("/repo/main".to_owned());
        stale.payload = serde_json::json!({ "session_id": "zombie-claude" });

        let mut zombie = agent("claude", "zombie-claude", AgentStatus::Idle, 1_000);
        zombie.worktree_path = Some("/repo/main".to_owned());
        let mut fresh = agent("claude", "fresh-claude", AgentStatus::Idle, 2_000);
        fresh.worktree_path = Some("/repo/main".to_owned());
        // Only the fresh session stamped the live pane; the zombie holds none.
        fresh.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));

        let snapshot = SidebarSnapshot::build_with_carryover(
            workspace,
            vec![stale],
            Vec::new(),
            vec![zombie, fresh],
        )
        .with_live_panes(vec![pane("%1", "claude", "/repo/main")], None);

        assert!(
            snapshot.needs_attention.is_empty(),
            "the superseded session's ask is not attention"
        );
        assert_eq!(snapshot.worktree_groups.len(), 1);
        let rows = &snapshot.worktree_groups[0].rows;
        assert_eq!(rows.len(), 1, "only the fresh session renders");
        assert_eq!(rows[0].id, "fresh-claude");
        assert_eq!(rows[0].status, Some(AgentStatus::Idle));
        assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "%1");
    }

    #[test]
    fn live_codex_command_does_not_corroborate_claude_attention() {
        // Live reproduction: an old Claude ask still has a ledger session, but
        // the only live pane in the worktree is `node /usr/bin/codex`. The
        // pane must remain Codex-shaped instead of inheriting Claude's model
        // and stale ask age.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut stale = FeedItem::new(
            workspace.clone(),
            Surface::NativeUi,
            FeedKind::Permission,
            "claude needs attention",
            "claude",
            "agent-hook",
        );
        stale.worktree_path = Some("/repo/main".to_owned());
        stale.payload = serde_json::json!({ "session_id": "stale-claude" });

        let mut claude = agent("claude", "stale-claude", AgentStatus::Idle, 1_000);
        claude.worktree_path = Some("/repo/main".to_owned());
        claude.model = Some("claude-opus-4-7".to_owned());

        let snapshot =
            SidebarSnapshot::build_with_carryover(workspace, vec![stale], Vec::new(), vec![claude])
                .with_live_panes(vec![pane("%1", "node /usr/bin/codex", "/repo/main")], None);

        assert_eq!(snapshot.worktree_groups.len(), 1);
        let rows = &snapshot.worktree_groups[0].rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].row_kind, SidebarRowKind::Process);
        assert_eq!(rows[0].name, "codex");
        assert!(snapshot.worktree_groups[0].status_counts.is_empty());
    }

    /// User's reported scenario: ledger carries a pile of stale claude
    /// observations from killed sessions (no SessionEnd ever fired), all
    /// claiming the same worktree path. A fresh claude pane lands. The fresh
    /// agent must still bind to its pane — stale count does not block live
    /// presence.
    #[test]
    fn live_claude_pane_binds_despite_pile_of_stale_ledger_ghosts() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let stale_a = {
            let mut a = agent("claude", "stale-a", AgentStatus::Idle, 1_000);
            a.worktree_path = Some("/repo/main".to_owned());
            a
        };
        let stale_b = {
            let mut a = agent("claude", "stale-b", AgentStatus::Idle, 1_001);
            a.worktree_path = Some("/repo/main".to_owned());
            a
        };
        let stale_c = {
            let mut a = agent("claude", "stale-c", AgentStatus::Idle, 1_002);
            a.worktree_path = Some("/repo/main".to_owned());
            a
        };
        let live = {
            let mut a = agent("claude", "live", AgentStatus::Running, i64::from(u32::MAX));
            a.worktree_path = Some("/repo/main".to_owned());
            a.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
            a
        };

        let snapshot = SidebarSnapshot::build_with_carryover(
            workspace,
            Vec::new(),
            Vec::new(),
            vec![stale_a, stale_b, stale_c, live],
        )
        .with_live_panes(vec![pane("%1", "claude", "/repo/main")], None);

        let rows = &snapshot.worktree_groups[0].rows;
        let agent_rows: Vec<_> = rows
            .iter()
            .filter(|r| r.row_kind == SidebarRowKind::Agent)
            .collect();
        assert_eq!(agent_rows.len(), 1, "only the live claude renders");
        assert_eq!(agent_rows[0].id, "live");
    }

    #[test]
    fn pending_attention_survives_without_pane_fold_in() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let item = FeedItem::new(
            workspace.clone(),
            Surface::Script,
            FeedKind::Question,
            "approve deploy?",
            "deploy",
            "script",
        );

        let snapshot = SidebarSnapshot::build(workspace, vec![item], Vec::new());

        assert_eq!(snapshot.worktree_groups.len(), 1);
        assert_eq!(
            snapshot.worktree_groups[0].rows[0].status,
            Some(AgentStatus::Waiting)
        );
        assert_eq!(
            snapshot.worktree_groups[0].rows[0].task.as_deref(),
            Some("approve deploy?")
        );
    }

    #[test]
    fn calm_tail_cap_never_hides_attention_rows() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut agents = (0..8)
            .map(|i| {
                let mut agent = agent(
                    "codex",
                    &format!("sess-{i}"),
                    AgentStatus::Running,
                    1_000 + i,
                );
                agent.worktree_path = Some("/repo/main".to_owned());
                agent
            })
            .collect::<Vec<_>>();
        let mut failed = agent("claude", "failed", AgentStatus::Failed, 2_000);
        failed.worktree_path = Some("/repo/main".to_owned());
        agents.push(failed);

        let snapshot =
            SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), agents);

        assert!(
            snapshot.worktree_groups[0]
                .rows
                .iter()
                .any(|row| row.status == Some(AgentStatus::Failed)),
            "attention rows remain visible past the calm-row cap"
        );
        assert!(snapshot.worktree_groups[0].hidden_count > 0);
    }

    #[test]
    fn calm_tail_cap_never_hides_focused_rows() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let agents = (0..8)
            .map(|i| {
                let mut agent = agent(
                    "codex",
                    &format!("sess-{i}"),
                    AgentStatus::Running,
                    1_000 + i,
                );
                agent.worktree_path = Some("/repo/main".to_owned());
                if i == 0 {
                    agent.pane = Some(PaneRef {
                        pane_id: PaneId::from_parts(MuxName::Tmux, "%99"),
                        session_name: "rimz-test".to_owned(),
                        view_id: Some("@0".to_owned()),
                        view_kind: Some(crate::ids::ViewKind::Window),
                        view_name: None,
                        is_focused: true,
                        command: Some("codex".to_owned()),
                        cwd: Some("/repo/main".to_owned()),
                        pane_pid: None,
                        pane_process_start: None,
                    });
                }
                agent
            })
            .collect::<Vec<_>>();

        let snapshot =
            SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), agents);

        assert!(
            snapshot.worktree_groups[0]
                .rows
                .iter()
                .any(|row| row.id == "sess-0"),
            "the focused running pane remains visible even past the calm-row cap"
        );
        assert!(snapshot.worktree_groups[0].hidden_count > 0);
    }

    #[test]
    fn bucket_order_puts_attention_first_and_running_last() {
        // Scrambled input proves the sort, not the insertion order.
        let agents = [
            AgentStatus::Running,
            AgentStatus::Success,
            AgentStatus::Idle,
            AgentStatus::Failed,
            AgentStatus::Waiting,
        ]
        .into_iter()
        .enumerate()
        .map(|(i, status)| agent_in(&format!("sess-{i}"), "/repo/main", status, 1_000 + i as i64))
        .collect::<Vec<_>>();

        let snapshot = SidebarSnapshot::build_with_carryover(
            WorkspaceId::from_project_root(Path::new("/tmp/x")),
            Vec::new(),
            Vec::new(),
            agents,
        );

        let order = snapshot.worktree_groups[0]
            .rows
            .iter()
            .map(|row| row.status)
            .collect::<Vec<_>>();
        assert_eq!(
            order,
            vec![
                Some(AgentStatus::Waiting),
                Some(AgentStatus::Failed),
                Some(AgentStatus::Idle),
                Some(AgentStatus::Success),
                Some(AgentStatus::Running),
            ],
            "attention leads; working agents sink to the bottom of the group"
        );
    }

    #[test]
    fn calm_bucket_holds_stable_spawn_order() {
        // Idle agents with distinct spawn times (and one with no pane). The
        // bucket holds spawn order — oldest first — regardless of activity.
        let specs: [(&str, Option<u64>); 4] = [
            ("late", Some(100)),
            ("nopane", None),
            ("early", Some(300)),
            ("mid", Some(200)),
        ];
        let agents = specs
            .into_iter()
            .enumerate()
            .map(|(i, (id, ago_secs))| {
                let mut agent = agent_in(id, "/repo/main", AgentStatus::Idle, 1_000 + i as i64);
                agent.pane = ago_secs.map(|secs| {
                    pane_started(
                        &format!("%{i}"),
                        "/repo/main",
                        Timestamp::now() - std::time::Duration::from_secs(secs),
                    )
                });
                agent
            })
            .collect::<Vec<_>>();

        let snapshot = SidebarSnapshot::build_with_carryover(
            WorkspaceId::from_project_root(Path::new("/tmp/x")),
            Vec::new(),
            Vec::new(),
            agents,
        );

        let order = snapshot.worktree_groups[0]
            .rows
            .iter()
            .map(|row| row.id.clone())
            .collect::<Vec<_>>();
        // Oldest pane first; the paneless row falls to the bucket tail.
        assert_eq!(order, vec!["early", "mid", "late", "nopane"]);
    }

    #[test]
    fn attention_bucket_sorts_longest_overdue_first() {
        // Scrambled input; a higher rank means more recent activity.
        let agents = vec![
            ("wait-new", AgentStatus::Waiting, 9_000),
            ("wait-old", AgentStatus::Waiting, 1_000),
            ("fail-new", AgentStatus::Failed, 8_000),
            ("fail-old", AgentStatus::Failed, 2_000),
        ]
        .into_iter()
        .map(|(id, status, rank)| agent_in(id, "/repo/main", status, rank))
        .collect::<Vec<_>>();

        let snapshot = SidebarSnapshot::build_with_carryover(
            WorkspaceId::from_project_root(Path::new("/tmp/x")),
            Vec::new(),
            Vec::new(),
            agents,
        );

        let order = snapshot.worktree_groups[0]
            .rows
            .iter()
            .map(|row| row.id.clone())
            .collect::<Vec<_>>();
        // Waiting leads failed; within each, the longest-overdue (oldest activity) rises.
        assert_eq!(order, vec!["wait-old", "wait-new", "fail-old", "fail-new"]);
    }

    #[test]
    fn group_tiering_floats_attention_and_tails_external() {
        let labels_for = |agents: Vec<AgentState>| {
            SidebarSnapshot::build_with_carryover(
                WorkspaceId::from_project_root(Path::new("/tmp/x")),
                Vec::new(),
                Vec::new(),
                agents,
            )
            .worktree_groups
            .iter()
            .map(|group| group.label.clone())
            .collect::<Vec<_>>()
        };
        let external = |id: &str, status: AgentStatus| agent("claude", id, status, 1_000);

        // A calm external sinks below calm project worktrees; an attention
        // worktree leads regardless of its name.
        assert_eq!(
            labels_for(vec![
                agent_in("a1", "/repo/alpha", AgentStatus::Failed, 1_000),
                agent_in("a2", "/repo/alpha", AgentStatus::Idle, 1_000),
                agent_in("b1", "/repo/beta", AgentStatus::Idle, 1_000),
                agent_in("b2", "/repo/beta", AgentStatus::Idle, 1_000),
                external("e1", AgentStatus::Idle),
            ]),
            vec!["alpha", "beta", "external"]
        );

        // The external catch-all rises out of the tail only when it holds an
        // attention agent (waiting or failed).
        assert_eq!(
            labels_for(vec![
                agent_in("b1", "/repo/beta", AgentStatus::Idle, 1_000),
                agent_in("b2", "/repo/beta", AgentStatus::Idle, 1_000),
                external("e1", AgentStatus::Failed),
            ]),
            vec!["external", "beta"]
        );
    }

    #[test]
    fn liveness_drops_dead_agent_pid_and_rebuilds_groups() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut codex = agent("codex", "sess-1", AgentStatus::Running, 1_000);
        codex.agent_pid = Some(424_242);
        codex.agent_process_start = Some("12345".to_owned());
        codex.worktree_branch = Some("main".to_owned());

        let mut snapshot =
            SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![codex]);
        assert_eq!(
            snapshot.worktree_groups[0].rows[0].status,
            Some(AgentStatus::Running)
        );

        snapshot.drop_dead_agents_with(|pid, start| {
            assert_eq!(pid, 424_242);
            assert_eq!(start, Some("12345"));
            false
        });

        assert!(snapshot.agents.is_empty());
        assert!(snapshot.worktree_groups.is_empty());
    }

    /// Build a single-agent rollup, run the reap, and return the surviving
    /// agent ids. Timestamps are stamped relative to `now` so the TTL rules are
    /// exercised deterministically.
    fn reap_survivors(now: Timestamp, agents: Vec<AgentState>) -> Vec<String> {
        let mut snapshot = SidebarSnapshot::build_with_carryover(
            WorkspaceId::from_project_root(Path::new("/tmp/x")),
            Vec::new(),
            Vec::new(),
            agents,
        );
        snapshot.reap_stale_sessions(now);
        let mut ids: Vec<String> = snapshot.agents.iter().map(|a| a.agent_id.clone()).collect();
        ids.sort();
        ids
    }

    fn aged(mut agent: AgentState, now: Timestamp, secs_ago: i64) -> AgentState {
        let at = Timestamp::from_second(now.as_second() - secs_ago).unwrap();
        agent.last_activity = at;
        agent.last_seen = at;
        agent
    }

    #[test]
    fn reap_drops_pidless_session_past_ttl_but_keeps_recent_and_pidful() {
        let now = Timestamp::now();
        let mut stale = aged(
            agent("claude", "stale", AgentStatus::Idle, 0),
            now,
            GHOST_SESSION_TTL_SECS + 60,
        );
        stale.worktree_path = Some("/repo/stale".to_owned());
        let mut recent = aged(agent("claude", "recent", AgentStatus::Idle, 0), now, 60);
        recent.worktree_path = Some("/repo/recent".to_owned());
        // Old but pid-bearing: TTL reaping is for pidless ghosts only.
        let mut pidful = aged(
            agent("codex", "pidful", AgentStatus::Idle, 0),
            now,
            GHOST_SESSION_TTL_SECS * 10,
        );
        pidful.worktree_path = Some("/repo/pidful".to_owned());
        pidful.agent_pid = Some(4242);

        assert_eq!(
            reap_survivors(now, vec![stale, recent, pidful]),
            vec!["pidful".to_owned(), "recent".to_owned()],
            "only the pidless, past-TTL ghost is reaped"
        );
    }

    #[test]
    fn reap_collapses_superseded_paneless_session_to_the_newest() {
        let now = Timestamp::now();
        let mut older = aged(agent("codex", "older", AgentStatus::Idle, 0), now, 120);
        older.worktree_path = Some("/repo/a".to_owned());
        older.worktree_branch = Some("main".to_owned());
        let mut newer = aged(agent("codex", "newer", AgentStatus::Idle, 0), now, 60);
        newer.worktree_path = Some("/repo/a".to_owned());
        newer.worktree_branch = Some("main".to_owned());

        assert_eq!(
            reap_survivors(now, vec![older, newer]),
            vec!["newer".to_owned()],
            "the older paneless session on the same path+branch is reaped"
        );
    }

    #[test]
    fn reap_keeps_concurrent_agents_each_holding_a_distinct_pane() {
        // The one-pane-one-row safety property: two same-branch agents in
        // distinct panes are both live and must both survive supersession.
        let now = Timestamp::now();
        let mut older = aged(agent("claude", "older", AgentStatus::Running, 0), now, 120);
        older.worktree_path = Some("/repo/a".to_owned());
        older.worktree_branch = Some("main".to_owned());
        older.agent_pid = Some(111);
        older.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
        let mut newer = aged(agent("claude", "newer", AgentStatus::Running, 0), now, 60);
        newer.worktree_path = Some("/repo/a".to_owned());
        newer.worktree_branch = Some("main".to_owned());
        newer.agent_pid = Some(222);
        newer.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%2")));

        assert_eq!(
            reap_survivors(now, vec![older, newer]),
            vec!["newer".to_owned(), "older".to_owned()],
            "an agent holding its own distinct pane is never reaped"
        );
    }

    fn window(used: u8, resets_in_secs: i64) -> RateLimitWindow {
        let now = Timestamp::now();
        let resets_at = if resets_in_secs >= 0 {
            now + std::time::Duration::from_secs(resets_in_secs as u64)
        } else {
            now - std::time::Duration::from_secs((-resets_in_secs) as u64)
        };
        RateLimitWindow {
            used_percentage: Some(used),
            resets_at: Some(resets_at),
            duration_mins: Some(300),
        }
    }

    #[test]
    fn stable_window_ignores_passed_resets_and_keeps_the_most_drained() {
        let now = Timestamp::now();
        // A stale window (reset already passed) reads low; two live windows
        // report 50% and 80%. The stale one is dropped, and the most-drained
        // live survivor (80%) wins — never over-promising remaining budget.
        let live_50 = window(50, 3_600);
        let live_80 = window(80, 1_800);
        let stale_10 = window(10, -60);

        let pick = stable_window(
            [live_50.clone(), live_80.clone(), stale_10.clone()].into_iter(),
            now,
        )
        .expect("a live window survives");
        assert_eq!(pick.used_percentage, Some(80));

        // Order-independent: the producer must not flicker with session order.
        let reversed = stable_window([stale_10, live_80, live_50].into_iter(), now)
            .expect("a live window survives");
        assert_eq!(reversed.used_percentage, Some(80));
    }

    #[test]
    fn stable_window_is_none_when_every_reading_is_stale() {
        let now = Timestamp::now();
        assert!(stable_window([window(90, -10), window(40, -3_600)].into_iter(), now).is_none());
    }

    #[test]
    fn stable_window_falls_back_to_an_undated_reading() {
        // A window with no reset instant can't be aged out; it is the last-resort
        // reading only when nothing with a live reset survives.
        let now = Timestamp::now();
        let undated = RateLimitWindow {
            used_percentage: Some(33),
            resets_at: None,
            duration_mins: Some(300),
        };
        let pick = stable_window([window(90, -10), undated].into_iter(), now)
            .expect("the undated reading backstops the stale one");
        assert_eq!(pick.used_percentage, Some(33));
    }

    #[test]
    fn stable_windows_picks_one_per_duration_sorted_short_to_long() {
        let now = Timestamp::now();
        let mk = |used: u8, mins: u32| RateLimitWindow {
            used_percentage: Some(used),
            resets_at: Some(now + std::time::Duration::from_secs(3_600)),
            duration_mins: Some(mins),
        };
        // Two sessions, each reporting a 5h and a 30d window at different drains.
        let readings = [mk(10, 43_800), mk(20, 300), mk(40, 43_800), mk(5, 300)];
        let stable = stable_windows(readings.into_iter(), now);
        assert_eq!(stable.len(), 2, "one bar per duration");
        assert_eq!(
            stable[0].duration_mins,
            Some(300),
            "short window sorts first"
        );
        assert_eq!(stable[0].used_percentage, Some(20), "most-drained 5h kept");
        assert_eq!(
            stable[1].duration_mins,
            Some(43_800),
            "long window sorts last"
        );
        assert_eq!(stable[1].used_percentage, Some(40), "most-drained 30d kept");
    }
}
