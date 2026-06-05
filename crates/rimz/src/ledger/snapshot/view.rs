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
use super::process::{pane_command_is_known, program_label, row_from_process};
use crate::agent_activity::AgentActivity;
use crate::agents::lifecycle::TurnPhase;
use crate::agents::{AgentAccount, AgentContext, RateLimitWindow, SpendTally};
use crate::feed::{
    AgentState, AgentStatus, FeedItem, FeedKind, FeedStatus, PaneRef, ResolverStepState, Surface,
};
use crate::ids::{AgentKind, AgentSessionId, PaneId, RequestId, ResolverId, WorkspaceId};
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
    /// a non-trunk worktree with zero commits ahead and a zero diff renders
    /// `≡ <trunk>`; the trunk worktree itself (`label == trunk`) never wears
    /// it, since "landed on itself" carries no information.
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
    /// The running turn's shape, copied from the rollup: `reasoning` paints the
    /// thinking sparkle, `acting` the working spinner, `parked` the secondary
    /// "background" marker. A transient axis like `compacting`, never a status
    /// bucket of its own. Always `idle` for process rows and outside `Running`.
    #[serde(default, skip_serializing_if = "turn_phase_is_idle")]
    pub phase: TurnPhase,
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
    /// The context meter's severity verdict —
    /// [`ContextSeverity::classify`](crate::feed::ContextSeverity::classify)
    /// over this row's gauge inputs and the `[sidebar.context]` bands — stamped
    /// once where the machine config is folded onto the snapshot, so the
    /// renderer's color ramp and any future signal emitter read one authority.
    /// Display + signal; never a status bucket. `None` for process rows and on
    /// a snapshot that predates the config fold (the renderer then classifies
    /// locally from the same bands).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_severity: Option<crate::feed::ContextSeverity>,
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
    /// Why the displayed status escalated to `failed` when the agent's latest
    /// turn died on a provider API error with no `Stop` hook — the upstream
    /// error text ("API Error: Overloaded") from the transcript-tail marker.
    /// Set only by the turn-death projection (`project_display_status`), so it
    /// is present exactly while that escalation holds; the renderer paints it
    /// as the card's dim line-2 body. Always `None` for process rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_error_label: Option<String>,
    /// Resident set size of the row's pane process in kibibytes, from the
    /// producer's per-tick `/proc` read. Display-only; set for process rows
    /// only — an agent card spends its right slots on cost and age instead.
    /// `None` on non-Linux or when the process was unreadable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rss_kb: Option<u64>,
    /// CPU utilisation of the row's pane process in integer percent, from two
    /// consecutive `/proc/<pid>/stat` readings. Set for process rows only.
    /// `None` on the first tick (no prior sample), on non-Linux, or when the
    /// process was unreadable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_pct: Option<u16>,
    /// Combined VFS I/O rate (rchar + wchar bytes/s) of the row's pane
    /// process, from two consecutive `/proc/<pid>/io` readings. Set for
    /// process rows only. `None` on the first tick, on non-Linux, or when the
    /// file was unreadable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub io_bps: Option<u64>,
}

impl SidebarRow {
    /// The context gauge's value (0..=100): the statusline's authoritative
    /// `used_percentage` when present, else the transcript-derived scalar.
    /// One of the two inputs `context_severity` is classified from; the
    /// renderer also reads it for the bar fill.
    pub fn context_gauge_percent(&self) -> Option<u8> {
        self.context
            .as_ref()
            .and_then(|context| context.tokens.as_ref())
            .and_then(|tokens| tokens.used_percentage)
            .or(self.context_pct)
    }

    /// Tokens currently occupying the context window — the current message's
    /// `input + cache_creation + cache_read`, exactly the numerator the gauge
    /// percent scales. The severity classification's absolute-token axis.
    /// `None` when no per-message breakdown was reported.
    pub fn context_used_tokens(&self) -> Option<u64> {
        let usage = self
            .context
            .as_ref()?
            .tokens
            .as_ref()?
            .current_usage
            .as_ref()?;
        Some(
            usage.input_tokens.unwrap_or(0)
                + usage.cache_creation_input_tokens.unwrap_or(0)
                + usage.cache_read_input_tokens.unwrap_or(0),
        )
    }
}

/// `skip_serializing_if` helper: the resting phase is the default and stays off
/// the wire.
fn turn_phase_is_idle(phase: &TurnPhase) -> bool {
    *phase == TurnPhase::Idle
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
    /// When the child began (its `subagentStatusLine` `startTime`) — the
    /// expanded list's sort key, so siblings hold their spawn order across
    /// refreshes. `None` for a Codex child or before the first status report.
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
        // The pure reducer has no project root or worktree set, so every cwd
        // keeps per-path grouping here; callers that know them re-fold via
        // `with_project_root` / `with_worktree_roots`.
        let worktree_groups =
            build_worktree_groups(&agents, &needs_attention, &resolver_working, None, &[], now);

        Self {
            workspace_id,
            display_name,
            generated_at: now,
            now,
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
            self.now,
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
        let mut by_key: BTreeMap<(AgentKind, AgentSessionId), _> = records
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
        let mut by_key: BTreeMap<(AgentKind, AgentSessionId), _> = records
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
            let reapable = is_daemon_mode_codex(agent, daemon_pids)
                && !loaded.contains(agent.agent_id.as_str());
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
    pub fn reap_stale_sessions(&mut self) {
        let now = self.now;
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
        self.worktree_groups = build_worktree_groups_from_rows(
            rows_from_panes(
                &self.agents,
                &self.needs_attention,
                &self.resolver_working,
                &panes,
                &self.wired_lazy_kinds,
                self.now,
            ),
            &self.agents,
            self.project_root.as_deref(),
            &self.worktree_roots,
            self.now,
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
            if !kinds.iter().any(|known| agent.kind == **known) {
                kinds.push(agent.kind.to_string());
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
            // A live session's rich-context version wins; the out-of-band
            // probe's binary read (Pi's `pi -v`) covers a provider whose
            // sessions never report one.
            let version = freshest
                .and_then(|context| context.agent_version.clone())
                .or_else(|| {
                    probed_accounts
                        .get(&kind)
                        .and_then(|account| account.version.clone())
                });
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
            let declares_windows = crate::agents::descriptor_by_kind(&kind)
                .is_none_or(|descriptor| descriptor.capabilities.rate_limit_windows);
            let windows = if declares_windows {
                stable_windows(
                    sessions
                        .iter()
                        .filter_map(|agent| agent.context.as_ref()?.rate_limits.as_ref())
                        .flat_map(|limits| limits.windows.iter().cloned()),
                    now,
                )
            } else {
                Vec::new()
            };
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
                .iter()
                .map(|line| (*line).to_owned())
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
    now: Timestamp,
) -> Vec<SidebarWorktreeGroup> {
    let rows = rows_from_ledger(agents, needs_attention, resolver_working, now);
    build_worktree_groups_from_rows(rows, agents, project_root, worktree_roots, now)
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
fn rows_from_panes(
    agents: &[AgentState],
    needs_attention: &[FeedItem],
    resolver_working: &[FeedItem],
    panes: &[PaneRef],
    wired_lazy_kinds: &[String],
    now: Timestamp,
) -> Vec<SidebarRow> {
    let mut rows = Vec::new();
    let mut bound_agents: BTreeSet<(AgentKind, AgentSessionId)> = BTreeSet::new();

    for pane in panes {
        if let Some(agent) = agent_for_pane(pane, agents, &bound_agents) {
            push_agent_row(
                &mut rows,
                &mut bound_agents,
                agent,
                pane,
                needs_attention,
                resolver_working,
                now,
            );
        } else if let Some(bind) =
            lazy_agent_for_pane(pane, agents, &bound_agents, wired_lazy_kinds, now)
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
                    now,
                ),
                LazyAgentRow::Idle(row) => rows.push(*row),
            }
        } else if pane_command_is_known(pane) {
            rows.push(row_from_process(pane, now));
        }
        // else: a brand-new or raced pane whose command is still unknown after
        // carry-forward — the third honest-read guard. Presence without identity
        // folds no row until a read names it; the pane stays in the published
        // pane list, so the sibling count and selection baseline see it.
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
    bound: &mut BTreeSet<(AgentKind, AgentSessionId)>,
    agent: &AgentState,
    pane: &PaneRef,
    needs_attention: &[FeedItem],
    resolver_working: &[FeedItem],
    now: Timestamp,
) {
    bound.insert((agent.kind.clone(), agent.agent_id.clone()));
    let mut row = row_from_agent(agent, now);
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
    row.status = Some(AgentStatus::Waiting);
    // Phase is a head on Running — the reduced state's invariant — so the
    // waiting overlay drops it rather than carrying a stale Reasoning/Acting.
    row.phase = TurnPhase::Idle;
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
    now: Timestamp,
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
            && !replaced_agents.insert((
                AgentKind::new_unchecked(item.source.clone()),
                AgentSessionId::from(agent_id),
            ))
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
        rows.push(row_from_agent(agent, now));
    }

    rows
}

fn build_worktree_groups_from_rows(
    mut rows: Vec<SidebarRow>,
    agents: &[AgentState],
    project_root: Option<&Path>,
    worktree_roots: &[PathBuf],
    now: Timestamp,
) -> Vec<SidebarWorktreeGroup> {
    // Nest each subagent under its parent root row before grouping. This is the
    // one chokepoint both the live (`rows_from_panes`) and no-pane
    // (`rows_from_ledger`) builders share, so nesting behaves identically on
    // either path.
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
        // Display order: creation time ascending — the spawn order the parent
        // launched them in, stable across refreshes (an activity-keyed sort
        // reshuffled the list on every tick). A child with no reported start
        // time sorts after the dated ones; the id tiebreak keeps the whole
        // order deterministic.
        row.sub_agents.sort_by(|a, b| {
            cmp_start_asc(a.started_at, b.started_at).then_with(|| a.id.cmp(&b.id))
        });
    }
}

/// Project each agent row's *displayed* status from its raw lifecycle status,
/// its liveness, its live subagents, and its account's rate-limit budget. This
/// is the one place display state diverges from the rollup truth kept in
/// `snapshot.agents`; a pending ask already folded `waiting` onto the row
/// upstream and always wins.
///
/// - An agent on an account whose rate-limit budget is spent is projected to
///   `rate_limited` — parked until the window resets, auto-resumable. The park
///   leads the derived states because the spent account explains them all: a
///   rate-limited turn dies on the same transcript marker the turn-death check
///   reads, its retry loop is silent enough to stall, and its delegated
///   children share the spent budget. Once the window resets, the kind leaves
///   the limited set and the checks below escalate an agent that failed to
///   resume.
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
        let projected =
            if crate::feed::is_rate_limited(status, limited_kinds.contains(row.name.as_str())) {
                // The spent-account park leads the chain: a rate-limited turn dies
                // with the same `isApiErrorMessage` marker the turn-death check
                // reads (which would mislabel the park as a failure), its retry
                // loop is silent by design (the stall window would escalate it),
                // and a delegated child shares the spent account (the delegated
                // wait would spin forever). Self-healing: once the window resets,
                // the kind leaves the limited set and the checks below escalate an
                // agent that failed to resume.
                AgentStatus::RateLimited
            } else if status == AgentStatus::Running && has_live_child {
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
            } else {
                status
            };
        row.status = Some(projected);
        if projected != AgentStatus::Running {
            // Phase is a head on Running — the reduced state's invariant —
            // so a Failed/RateLimited override drops it rather than carrying
            // a stale Reasoning/Acting onto a resting row.
            row.phase = TurnPhase::Idle;
        }
    }
}

/// The set of provider kinds whose account rate-limit budget is spent: a live
/// session of the kind reports any window used to the cap whose reset has not yet
/// passed. Account-scoped — every session of a kind shares the
/// budget — so the verdict parks *every* agent of the kind (idle, success, or
/// still `running`), including one that launched straight into a spent account.
/// Reads the same window source as the provider dashboard
/// (`agent.context.rate_limits`), so the cockpit tally and the dashboard bars
/// never disagree.
fn rate_limited_kinds(agents: &[AgentState], now: Timestamp) -> BTreeSet<AgentKind> {
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
        id: child.agent_id.to_string(),
        name,
        status: child.status,
        task: child.task.clone(),
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
    // spent-budget → `rate_limited`); a pending ask folds `waiting` on upstream.
    // The rollup in `snapshot.agents` always keeps the true status.
    SidebarRow {
        row_kind: SidebarRowKind::Agent,
        id: agent.agent_id.to_string(),
        name: agent.kind.to_string(),
        status: Some(agent.status),
        phase: agent.phase,
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
        context_severity: None,
        worktree_path: agent.worktree_path.clone(),
        worktree_branch: agent.worktree_branch.clone(),
        last_activity: agent.last_activity,
        resolver: None,
        options: Vec::new(),
        sub_agents: Vec::new(),
        process_active: false,
        command_detail: None,
        compacting: is_compacting(agent, now),
        // Filled by the turn-death projection (`project_display_status`) when
        // the escalation holds; never carried from the rollup.
        turn_error_label: None,
        // Filled by `push_agent_row` from the live pane; the rollup carries no
        // per-process metrics.
        rss_kb: None,
        cpu_pct: None,
        io_bps: None,
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
        // A waiting row is blocked on the human, not reasoning — no turn phase.
        phase: TurnPhase::Idle,
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
        context_severity: None,
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
        turn_error_label: None,
        rss_kb: None,
        cpu_pct: None,
        io_bps: None,
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
        if row.status.is_some_and(AgentStatus::is_actionable)
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
mod tests;
