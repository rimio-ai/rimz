//! Reduced workspace snapshot. The sidebar consumes this via
//! `rimz sidebar snapshot --json`; correctness lives in the feed files and
//! event log this is derived from.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agent_activity::AgentActivity;
use crate::feed::{
    AgentState, AgentStatus, FeedItem, FeedKind, FeedStatus, PaneRef, PermissionPosture,
    ResolverStepState, RuntimeOwner, RuntimeOwnerKind, Surface,
};
use crate::ids::{PaneId, RequestId, ResolverId, WorkspaceId};
use crate::ledger::agent_context::AgentContextRecord;
use crate::ledger::atomic::{self, write_temp_then_rename};
use crate::ledger::event_log::{self, EventLogErr};
use crate::ledger::feed_store::{self, FeedStoreErr};
use crate::ledger::paths::StatePaths;
use crate::ledger::workspace_record;
use crate::schema::event::EventEnvelope;

#[derive(Debug, thiserror::Error)]
pub enum SnapshotErr {
    #[error(transparent)]
    FeedStore(#[from] FeedStoreErr),
    #[error(transparent)]
    EventLog(#[from] EventLogErr),
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("json parse error on {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

pub type Result<T> = std::result::Result<T, SnapshotErr>;

/// Sidebar view-model. The worktree groups are the renderer contract:
/// grouping, attention ranking, caps, status tallies, and row metadata are
/// resolved here so renderers only paint semantics into glyphs.
///
/// `needs_attention` and `resolver_working` are load-bearing: they are the
/// reducer inputs the group rebuild reads when panes are folded in
/// (`with_live_panes`) or dead agents are reaped (`drop_dead_agents_with`).
/// `recently_answered` and `recent_activity` are retained only to keep the
/// `sidebar snapshot --json` wire shape stable; no renderer consumes them, and
/// they are candidates to drop once the shape can change (see
/// `docs/internals/sidebar.md`). The sidebar renderer reads `worktree_groups`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SidebarSnapshot {
    pub workspace_id: WorkspaceId,
    pub display_name: String,
    pub generated_at: Timestamp,
    pub worktree_groups: Vec<SidebarWorktreeGroup>,
    pub needs_attention: Vec<FeedItem>,
    pub resolver_working: Vec<FeedItem>,
    pub recently_answered: Vec<FeedItem>,
    pub recent_activity: Vec<SidebarActivity>,
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
    /// The project's canonical root. Grouping uses it to tell a project
    /// worktree (the main checkout, or `<root>/.claude/worktrees/*`) from a
    /// pane whose cwd sits outside the project entirely (a home shell, `/tmp`),
    /// which folds into the `external` catch-all instead of minting its own
    /// pod. Like `display_name`, this is workspace identity the reducer can't
    /// read from the ledger, so the pure path leaves it `None` (every cwd keeps
    /// per-path grouping) and the `rimz sidebar snapshot` CLI fills it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root: Option<PathBuf>,
}

/// One sidebar's view of the panes sharing its tab/window. `None` on the
/// snapshot means the count could not be determined (no `--exclude-pane-id`, or
/// the caller's pane was absent from the live list); the renderer treats that
/// as "never self-close".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidebarOwnView {
    pub sibling_count: usize,
    pub own_is_focused: bool,
    pub focused_pane_id: Option<PaneId>,
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
    /// Worktree-level `git diff` insertions and deletions, projected by the
    /// `rimz sidebar snapshot` CLI (the reducer stays pure). Lives on the
    /// group header — never on a per-agent row — so the shared-worktree
    /// "whose diff?" ambiguity is resolved by belonging to the worktree, not
    /// the agent. `None` when no git read was attempted or the worktree is
    /// not a git repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_added: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_removed: Option<u32>,
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
    /// Permission posture pill (`auto`/`yolo`; `default` and `unknown` omit).
    /// Replaces the older `mode` field — mode/posture were conflated in a way
    /// that didn't cross backends cleanly.
    pub permission_posture: Option<PermissionPosture>,
    /// True while the agent is in read-only plan mode. With `status == Running`
    /// the renderer paints the "thinking" state instead of the working spinner.
    #[serde(default)]
    pub plan_mode: bool,
    pub pane: Option<PaneRef>,
    pub request_id: Option<RequestId>,
    pub surface: Option<Surface>,
    pub task: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Context-window % gauge value (0..=100). Agent rows default this to
    /// `Some(0)` so renderers always draw the started-session gauge; transcript
    /// usage only upgrades the meter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_pct: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub todo_done: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub todo_total: Option<u32>,
    pub worktree_path: Option<String>,
    pub worktree_branch: Option<String>,
    pub last_activity: Timestamp,
    pub resolver: Option<SidebarResolverState>,
    pub options: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SidebarResolverState {
    pub resolver_id: ResolverId,
    pub display_name: Option<String>,
    pub budget_until: Option<Timestamp>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SidebarActivity {
    Feed { item: Box<FeedItem> },
    Event { event: Box<EventEnvelope> },
}

impl SidebarActivity {
    pub fn timestamp(&self) -> Timestamp {
        match self {
            Self::Feed { item } => item.updated_at,
            Self::Event { event } => event.timestamp,
        }
    }
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
        Self::build_with_agents(workspace_id, items, events, agents)
    }

    pub fn build_with_agents(
        workspace_id: WorkspaceId,
        mut items: Vec<FeedItem>,
        events: Vec<EventEnvelope>,
        agents: Vec<AgentState>,
    ) -> Self {
        items.sort_by_key(|item| std::cmp::Reverse(item.updated_at));

        let mut needs_attention = Vec::new();
        let mut resolver_working = Vec::new();
        let mut recently_answered = Vec::new();
        let mut recent_activity = Vec::new();

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
                (FeedStatus::Resolved, _) => recently_answered.push(item),
                _ => recent_activity.push(SidebarActivity::Feed {
                    item: Box::new(item),
                }),
            }
        }

        recent_activity.extend(
            events
                .into_iter()
                .filter(|event| !event.method.starts_with("feed."))
                .map(|event| SidebarActivity::Event {
                    event: Box::new(event),
                }),
        );
        recent_activity.sort_by_key(|activity| std::cmp::Reverse(activity.timestamp()));

        let display_name = workspace_id.as_str().to_owned();
        // The pure reducer has no project root, so every cwd keeps per-path
        // grouping here; callers that know the root re-fold via `with_project_root`.
        let worktree_groups =
            build_worktree_groups(&agents, &needs_attention, &resolver_working, None);

        Self {
            workspace_id,
            display_name,
            generated_at: Timestamp::now(),
            worktree_groups,
            needs_attention,
            resolver_working,
            recently_answered,
            recent_activity,
            agents,
            agent_hooks_ready: false,
            own_view: None,
            project_root: None,
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
        );
    }

    /// Record the project root and re-fold groups so a cwd outside it lands in
    /// the `external` catch-all instead of its own pod. Callers set this from
    /// the workspace record after construction (the reducer can't read it),
    /// mirroring how `display_name` is filled.
    pub fn with_project_root(mut self, project_root: Option<PathBuf>) -> Self {
        self.project_root = project_root;
        self.rebuild_groups();
        self
    }

    /// Attach each session's rich statusline context to its `AgentState` by
    /// `(kind, agent_id)`. Capture/store-only: this enriches `agents[]` for
    /// `sidebar snapshot --json` consumers (and future renderers) without
    /// changing row rendering, so worktree groups are not rebuilt. A context
    /// whose session is absent from the (already reaped) rollup is dropped — the
    /// session is gone, so its context is just history. Records carry no
    /// identity of their own; the key they're filed under is authority.
    pub fn with_agent_context(mut self, records: Vec<AgentContextRecord>) -> Self {
        if records.is_empty() {
            return self;
        }
        let mut by_key: BTreeMap<(String, String), _> = records
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
        let superseded: BTreeSet<(String, String)> = self
            .agents
            .iter()
            .filter(|older| {
                self.agents.iter().any(|newer| {
                    newer.kind == older.kind
                        && newer.agent_id != older.agent_id
                        && newer.last_activity > older.last_activity
                        && newer.worktree_path == older.worktree_path
                        && newer.worktree_branch == older.worktree_branch
                        && older_yields_pane(older, newer)
                })
            })
            .map(|agent| (agent.kind.clone(), agent.agent_id.clone()))
            .collect();
        self.agents.retain(|agent| {
            if superseded.contains(&(agent.kind.clone(), agent.agent_id.clone())) {
                return false;
            }
            !(agent_is_pidless(agent) && session_age_secs(now, agent) > GHOST_SESSION_TTL_SECS)
        });
        if self.agents.len() != previous_len {
            self.rebuild_groups();
        }
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
                    .is_none_or(|command| command_label(command) != "rimz-sidebar")
            })
            .collect::<Vec<_>>();
        self.worktree_groups = build_worktree_groups_with_panes(
            &self.agents,
            &self.needs_attention,
            &self.resolver_working,
            &panes,
            self.project_root.as_deref(),
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
            if let Some(touch) = activity
                .iter()
                .filter(|a| a.kind == agent.kind && a.agent_id == agent.agent_id)
                .max_by_key(|a| a.at)
                && touch.at > agent.last_activity
            {
                agent.last_activity = touch.at;
                changed = true;
            }
        }
        if changed {
            self.rebuild_groups();
        }
        self
    }
}

impl SidebarOwnView {
    /// Summarize the panes sharing `own`'s view (tab/window) from a live pane
    /// list. Pure and backend-agnostic: callers own pane discovery and pass the
    /// result in. Returns `None` when `own` is absent from `panes` — the caller
    /// cannot reason about a view it cannot find itself in, so it must not
    /// self-close.
    pub fn from_panes(own: &PaneId, panes: &[PaneRef]) -> Option<Self> {
        let own_pane = panes.iter().find(|pane| pane.pane_id == *own)?;
        let own_view = own_pane.view_id.as_deref();
        let siblings = panes
            .iter()
            .filter(|pane| pane.pane_id != *own && pane.view_id.as_deref() == own_view)
            .collect::<Vec<_>>();
        let focused_pane_id = siblings
            .iter()
            .find(|pane| pane.is_focused)
            .map(|pane| pane.pane_id.clone());
        Some(Self {
            sibling_count: siblings.len(),
            own_is_focused: own_pane.is_focused,
            focused_pane_id,
        })
    }
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
/// either its session has left the rollup entirely, or a strictly-newer session
/// of the same kind has taken over the worktree. The latter reaps the zombie
/// case: a pidless `SessionStart`-only session never ends and never gets reaped
/// by process liveness, so without supersession its old permission prompt pins
/// itself onto the freshly launched session sharing the pane. Asks with no
/// session id can't be proven stale and are kept.
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
    agents.iter().any(|other| {
        other.kind == session.kind
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
) -> Vec<SidebarWorktreeGroup> {
    let rows = rows_from_ledger(agents, needs_attention, resolver_working);
    build_worktree_groups_from_rows(rows, project_root)
}

/// One pane = one row, by construction. Every live pane anchors exactly one
/// row: it binds the unique agent that stamped this pane id — rendering that
/// agent with its single most-relevant pending ask folded in — or, with no such
/// agent, renders as a plain process row. Agents with no live pane (ghosts,
/// sub-agents, a relaunch the reaper has not yet collapsed) do not render, so a
/// dead session can never resurrect a row or latch onto a stranger's pane. The
/// only paneless rows are standalone script/bridge asks, which no agent session
/// raised.
fn build_worktree_groups_with_panes(
    agents: &[AgentState],
    needs_attention: &[FeedItem],
    resolver_working: &[FeedItem],
    panes: &[PaneRef],
    project_root: Option<&Path>,
) -> Vec<SidebarWorktreeGroup> {
    build_worktree_groups_from_rows(
        rows_from_panes(agents, needs_attention, resolver_working, panes),
        project_root,
    )
}

fn rows_from_panes(
    agents: &[AgentState],
    needs_attention: &[FeedItem],
    resolver_working: &[FeedItem],
    panes: &[PaneRef],
) -> Vec<SidebarRow> {
    let mut rows = Vec::new();
    let mut bound_agents: BTreeSet<(String, String)> = BTreeSet::new();

    for pane in panes {
        match agent_for_pane(pane, agents, &bound_agents) {
            Some(agent) => {
                bound_agents.insert((agent.kind.clone(), agent.agent_id.clone()));
                let mut row = row_from_agent(agent);
                row.worktree_path = row.worktree_path.or_else(|| pane.cwd.clone());
                row.pane = Some(pane.clone());
                if let Some(ask) = most_relevant_ask(agent, needs_attention, resolver_working) {
                    fold_ask_onto_row(&mut row, ask);
                }
                rows.push(row);
            }
            None => rows.push(row_from_process(pane)),
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

/// The agent that stamped this exact pane id, if one is still unbound. Binding
/// is by stamped pane id alone — never by foreground command or cwd — so a pane
/// can only ever host the agent that ran in it (`agent_binds_only_by_stamped_
/// pane_id` pins this). When a stale rollup holds more than one claimant for a
/// pane id (a relaunch the reaper has not yet collapsed), the most-recently-
/// active wins, keeping the bind deterministic.
fn agent_for_pane<'a>(
    pane: &PaneRef,
    agents: &'a [AgentState],
    bound: &BTreeSet<(String, String)>,
) -> Option<&'a AgentState> {
    agents
        .iter()
        // Cheap pane match first: only agents stamped on this exact pane reach
        // the allocating `bound` lookup, so the common miss costs no clones.
        .filter(|agent| {
            agent.pane.as_ref().is_some_and(|stamped| {
                stamped.pane_id == pane.pane_id && pane_start_matches(stamped, pane)
            })
        })
        .filter(|agent| !bound.contains(&(agent.kind.clone(), agent.agent_id.clone())))
        .max_by_key(|agent| agent.last_activity)
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
        let key = (agent.kind.clone(), agent.agent_id.clone());
        if replaced_agents.contains(&key) {
            continue;
        }
        rows.push(row_from_agent(agent));
    }

    rows
}

fn build_worktree_groups_from_rows(
    rows: Vec<SidebarRow>,
    project_root: Option<&Path>,
) -> Vec<SidebarWorktreeGroup> {
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
            }
        })
        .collect::<Vec<_>>();
    groups.sort_by(compare_groups);
    groups
}

fn row_from_agent(agent: &AgentState) -> SidebarRow {
    // `SidebarRow.status` is the *displayed* status, not the raw rollup (a
    // pending ask folds it to `waiting` below). A `running` agent silent past
    // the stall window is likely wedged, so project it to the attention bucket
    // here: it then surfaces as `!`, joins the attention tally, and rises in the
    // ranking. The rollup in `snapshot.agents` keeps the true `running` status.
    let status = if crate::feed::is_stalled(agent.status, agent.last_activity, Timestamp::now()) {
        AgentStatus::Failed
    } else {
        agent.status
    };
    SidebarRow {
        row_kind: SidebarRowKind::Agent,
        id: agent.agent_id.clone(),
        name: agent.kind.clone(),
        status: Some(status),
        permission_posture: Some(agent.permission_posture),
        plan_mode: agent.plan_mode,
        pane: agent.pane.clone(),
        request_id: None,
        surface: None,
        task: agent.task.clone(),
        model: agent.model.clone(),
        effort: agent.effort.clone(),
        context_pct: Some(agent.context_pct.unwrap_or(0)),
        total_tokens: agent.total_tokens,
        todo_done: agent.todo_done,
        todo_total: agent.todo_total,
        worktree_path: agent.worktree_path.clone(),
        worktree_branch: agent.worktree_branch.clone(),
        last_activity: agent.last_activity,
        resolver: None,
        options: Vec::new(),
    }
}

fn row_from_process(pane: &PaneRef) -> SidebarRow {
    let name = pane
        .command
        .as_deref()
        .filter(|command| !command.is_empty())
        .map(process_label)
        .unwrap_or_else(|| "process".to_owned());
    SidebarRow {
        row_kind: SidebarRowKind::Process,
        id: pane.pane_id.to_string(),
        name,
        status: None,
        permission_posture: None,
        plan_mode: false,
        pane: Some(pane.clone()),
        request_id: None,
        surface: None,
        task: None,
        model: None,
        effort: None,
        context_pct: None,
        total_tokens: None,
        todo_done: None,
        todo_total: None,
        worktree_path: pane.cwd.clone(),
        worktree_branch: None,
        last_activity: pane.pane_process_start.unwrap_or_else(Timestamp::now),
        resolver: None,
        options: Vec::new(),
    }
}

fn process_label(command: &str) -> String {
    command_agent_kind(command)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| command_label(command))
}

fn command_label(command: &str) -> String {
    let command = command
        .split_whitespace()
        .next()
        .filter(|part| !part.is_empty())
        .unwrap_or(command);
    std::path::Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(command)
        .to_owned()
}

fn command_agent_kind(command: &str) -> Option<&'static str> {
    command.split_whitespace().find_map(|part| {
        let label = command_label(part);
        crate::agents::KNOWN_AGENTS
            .iter()
            .copied()
            .find(|agent| label == *agent)
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
        permission_posture: matched.map(|agent| agent.permission_posture),
        plan_mode: matched.is_some_and(|agent| agent.plan_mode),
        pane: item
            .pane
            .clone()
            .or_else(|| matched.and_then(|agent| agent.pane.clone())),
        request_id: Some(item.request_id.clone()),
        surface: Some(item.surface),
        task,
        model: matched.and_then(|agent| agent.model.clone()),
        effort: matched.and_then(|agent| agent.effort.clone()),
        context_pct: if is_agent_hook {
            Some(matched.and_then(|agent| agent.context_pct).unwrap_or(0))
        } else {
            None
        },
        total_tokens: matched.and_then(|agent| agent.total_tokens),
        todo_done: matched.and_then(|agent| agent.todo_done),
        todo_total: matched.and_then(|agent| agent.todo_total),
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

fn pane_start_matches(expected: &PaneRef, actual: &PaneRef) -> bool {
    match (expected.pane_process_start, actual.pane_process_start) {
        (Some(expected), Some(actual)) => expected == actual,
        _ => true,
    }
}

fn agent_id_from_item(item: &FeedItem) -> Option<String> {
    item.agent_session_id().map(ToOwned::to_owned)
}

/// Build a minimal `PaneRef` carrying just the normalized pane id. The reducer
/// only needs identity for binding an agent to its live pane; the live
/// multiplexer overlay fills in command/cwd/focus when it joins.
fn pane_ref_from_id(pane_id: PaneId) -> PaneRef {
    PaneRef {
        pane_id,
        session_name: String::new(),
        view_id: None,
        view_kind: None,
        is_focused: false,
        command: None,
        cwd: None,
        pane_pid: None,
        pane_process_start: None,
    }
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
) -> (SidebarWorktreeKind, String, String) {
    let branch = branch.filter(|branch| !branch.is_empty());
    if let Some(path) = path.filter(|path| !path.is_empty()) {
        // A cwd outside the project root is not one of the project's worktrees
        // (a home shell, `/tmp`), so it folds into the `external` catch-all
        // rather than minting its own pod. With no known root, every path keeps
        // per-path grouping.
        let in_project = project_root.is_none_or(|root| is_within(root, Path::new(path)));
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
/// canonicalization — keeping the reducer pure, consistent with `worktree_matches`.
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
        .then_with(|| compare_activity(left.status, left.last_activity, right.last_activity))
        .then_with(|| left.name.cmp(&right.name))
        .then_with(|| left.id.cmp(&right.id))
}

fn compare_activity(status: Option<AgentStatus>, left: Timestamp, right: Timestamp) -> Ordering {
    if matches!(status, Some(AgentStatus::Waiting | AgentStatus::Failed)) {
        left.cmp(&right)
    } else {
        right.cmp(&left)
    }
}

fn compare_groups(left: &SidebarWorktreeGroup, right: &SidebarWorktreeGroup) -> Ordering {
    workspace_tail(left)
        .cmp(&workspace_tail(right))
        .then_with(|| compare_group_top(left, right))
        .then_with(|| left.label.cmp(&right.label))
}

fn workspace_tail(group: &SidebarWorktreeGroup) -> bool {
    group.kind == SidebarWorktreeKind::Workspace
        && !group
            .rows
            .iter()
            .any(|row| row.status == Some(AgentStatus::Waiting))
}

fn compare_group_top(left: &SidebarWorktreeGroup, right: &SidebarWorktreeGroup) -> Ordering {
    match (left.rows.first(), right.rows.first()) {
        (Some(left), Some(right)) => compare_rows(left, right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn row_rank(row: &SidebarRow) -> u8 {
    match row.status {
        Some(status) => status_rank(status),
        None => 6,
    }
}

fn status_rank(status: AgentStatus) -> u8 {
    match status {
        AgentStatus::Waiting => 0,
        AgentStatus::Failed => 1,
        AgentStatus::Running => 2,
        AgentStatus::Idle => 3,
        AgentStatus::Success => 4,
    }
}

/// Carryover state preserved across event-log rotation. Today this is the
/// agent rollup; other reductions can join when they appear.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EventCarryover {
    #[serde(default)]
    pub agents: Vec<AgentState>,
}

pub fn read_carryover(path: &Path) -> Result<EventCarryover> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|source| SnapshotErr::Json {
            path: path.to_path_buf(),
            source,
        }),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(EventCarryover::default()),
        Err(source) => Err(SnapshotErr::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[must_use = "durability barrier; check the result"]
pub fn write_carryover(path: &Path, carryover: &EventCarryover) -> Result<()> {
    write_temp_then_rename(path, carryover)?;
    Ok(())
}

/// Walk `events` and project the agent rollup that should outlive a
/// rotation. Exposed so the ledger can capture state just before archiving.
pub fn agent_rollup_for_events(events: &[EventEnvelope]) -> Vec<AgentState> {
    reduce_agent_states(events)
}

pub fn agent_rollup_with_carryover(
    events: &[EventEnvelope],
    carryover_agents: Vec<AgentState>,
) -> Vec<AgentState> {
    let live = reduce_agent_states(events);
    let tombstones = agent_tombstones_for_events(events);
    merge_agent_rollups_with_tombstones(&carryover_agents, &live, &tombstones)
}

/// Merge two agent rollups, preferring the newer `last_seen` per
/// `(agent_kind, agent_id)` pair. `live` wins on ties so a same-second
/// observation in the active log overrides carryover.
pub fn merge_agent_rollups(base: &[AgentState], live: &[AgentState]) -> Vec<AgentState> {
    merge_agent_rollups_with_tombstones(base, live, &BTreeSet::new())
}

fn merge_agent_rollups_with_tombstones(
    base: &[AgentState],
    live: &[AgentState],
    tombstones: &BTreeSet<(String, String)>,
) -> Vec<AgentState> {
    let mut map: BTreeMap<(String, String), AgentState> = BTreeMap::new();
    for entry in base {
        let key = (entry.kind.clone(), entry.agent_id.clone());
        if !tombstones.contains(&key) {
            map.insert(key, entry.clone());
        }
    }
    for entry in live {
        let key = (entry.kind.clone(), entry.agent_id.clone());
        match map.get(&key) {
            Some(existing) if existing.last_seen > entry.last_seen => {}
            _ => {
                map.insert(key, entry.clone());
            }
        }
    }
    map.into_values().collect()
}

fn agent_tombstones_for_events(events: &[EventEnvelope]) -> BTreeSet<(String, String)> {
    let mut tombstones = BTreeSet::new();
    for event in events {
        if event.method != "agent.lifecycle" {
            continue;
        }
        let event_name = event.params.get("event_name").and_then(|v| v.as_str());
        let status = event.params.get("status").and_then(|v| v.as_str());
        if event_name != Some("SessionEnd") && status != Some("offline") {
            continue;
        }
        let kind = event.source.clone();
        let agent_id = event
            .params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("{kind}:anonymous"));
        tombstones.insert((kind, agent_id));
    }
    tombstones
}

/// Strip a trailing capability tag (`claude-opus-4-8[1m]` → `claude-opus-4-8`)
/// so the sidebar shows one stable model id per agent. The tag rides only on a
/// fresh-launch SessionStart payload — it is absent after `/clear`, the
/// transcript records the bare id, and no model env var exposes it — so it can
/// never be shown reliably. Idempotent on an already-bare id.
fn canonical_model(model: &str) -> String {
    match model.split_once('[') {
        Some((base, _)) => base.trim_end().to_owned(),
        None => model.to_owned(),
    }
}

/// Fold `agent.lifecycle` events into the latest [`AgentState`] per
/// agent_id, keyed by `(agent_kind, agent_id)`. Anonymous lifecycle events
/// (no agent_id) collapse to a single rollup keyed by `agent_kind`. Events
/// are walked in log order, so the newest observation wins.
///
/// Each event is a *partial* update: `status` always comes from the event,
/// but the stable capability/identity fields (`permission_posture`, `model`,
/// `effort`, worktree, pane) carry forward from the prior state when the event
/// omits them. A `UserPromptSubmit` therefore moves the agent to running
/// without erasing its model line or demoting a `yolo` posture.
fn reduce_agent_states(events: &[EventEnvelope]) -> Vec<AgentState> {
    let mut map: BTreeMap<(String, String), AgentState> = BTreeMap::new();
    for event in events {
        if event.method != "agent.lifecycle" {
            continue;
        }
        let kind = event.source.clone();
        let agent_id = event
            .params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("{kind}:anonymous"));
        let event_name = event.params.get("event_name").and_then(|v| v.as_str());
        if event_name == Some("SessionEnd")
            || event.params.get("status").and_then(|v| v.as_str()) == Some("offline")
        {
            map.remove(&(kind, agent_id));
            continue;
        }
        let prior = map.get(&(kind.clone(), agent_id.clone()));
        let status: AgentStatus = event
            .params
            .get("status")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or(AgentStatus::Idle);
        let permission_posture: PermissionPosture = event
            .params
            .get("permission_posture")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .or_else(|| prior.map(|p| p.permission_posture))
            .unwrap_or(PermissionPosture::Default);
        // Plan mode carries forward like posture: an event that omits it (the
        // prompt/stop turn boundaries report no mode) keeps the prior value, so
        // toggling plan mode mid-session persists until the agent reports a new
        // one.
        let plan_mode: bool = event
            .params
            .get("plan_mode")
            .and_then(serde_json::Value::as_bool)
            .or_else(|| prior.map(|p| p.plan_mode))
            .unwrap_or(false);
        let param_string = |key: &str| {
            event
                .params
                .get(key)
                .and_then(|v| v.as_str())
                .map(ToOwned::to_owned)
        };
        let param_number = |key: &str| event.params.get(key).and_then(|v| v.as_u64());
        // Enrichment fields carry forward when an event omits them.
        let context_pct = param_number("context_pct")
            .map(|v| v.min(100) as u8)
            .or_else(|| prior.and_then(|p| p.context_pct));
        let total_tokens =
            param_number("total_tokens").or_else(|| prior.and_then(|p| p.total_tokens));
        let todo_done = param_number("todo_done")
            .map(|v| v.min(u32::MAX as u64) as u32)
            .or_else(|| prior.and_then(|p| p.todo_done));
        let todo_total = param_number("todo_total")
            .map(|v| v.min(u32::MAX as u64) as u32)
            .or_else(|| prior.and_then(|p| p.todo_total));
        let establishes_identity = matches!(event_name, Some("SessionStart" | "SubagentStart"));
        let event_worktree_path = param_string("worktree_path");
        let event_worktree_branch = param_string("worktree_branch");
        let prior_worktree_path = prior.and_then(|p| p.worktree_path.clone());
        let prior_worktree_branch = prior.and_then(|p| p.worktree_branch.clone());
        let worktree_path = if establishes_identity || event_name.is_none() {
            event_worktree_path.or(prior_worktree_path)
        } else {
            prior_worktree_path.or(event_worktree_path)
        };
        let worktree_branch = if establishes_identity || event_name.is_none() {
            event_worktree_branch.or(prior_worktree_branch)
        } else {
            prior_worktree_branch.or(event_worktree_branch)
        };
        let agent_pid = event
            .params
            .get("agent_pid")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .or_else(|| prior.and_then(|p| p.agent_pid));
        let agent_process_start = param_string("agent_process_start")
            .or_else(|| prior.and_then(|p| p.agent_process_start.clone()));
        let runtime_owner = event
            .params
            .get("runtime_owner")
            .and_then(|v| serde_json::from_value::<RuntimeOwner>(v.clone()).ok())
            .or_else(|| {
                agent_pid.map(|pid| {
                    RuntimeOwner::new(
                        RuntimeOwnerKind::Agent,
                        agent_id.clone(),
                        pid,
                        agent_process_start.clone(),
                    )
                })
            })
            .or_else(|| prior.and_then(|p| p.runtime_owner.clone()));
        // Task is activity-bound, not identity: a fresh event replaces it
        // (idle clears it back to "—"); only capability fields persist.
        let task = param_string("task");
        // Always store the canonical model id. The agent reports a suffixed id
        // (`claude-opus-4-8[1m]`) only on a fresh-launch SessionStart; every
        // other event (and the transcript fallback) carries the bare id, so the
        // `.or(prior)` carry-forward would otherwise flip the label the first
        // time a suffix-less event arrived. Canonicalizing at reduce time pins
        // the label and keeps the event log faithful to the raw payload.
        let model = param_string("model")
            .map(|raw| canonical_model(&raw))
            .or_else(|| prior.and_then(|p| p.model.clone()));
        let effort = param_string("effort").or_else(|| prior.and_then(|p| p.effort.clone()));
        // The hook stamps the mux pane id it ran inside on every lifecycle
        // event; carry it forward when an event omits it so a `Stop` doesn't
        // unbind the agent from its pane. Only the pane id is reduced — the
        // rest of `PaneRef` is filled by the live `pane list` overlay.
        let pane = param_string("pane_id")
            .and_then(|raw| PaneId::parse(&raw).ok())
            .map(pane_ref_from_id)
            .or_else(|| prior.and_then(|p| p.pane.clone()));
        let state = AgentState {
            agent_id: agent_id.clone(),
            kind: kind.clone(),
            status,
            permission_posture,
            plan_mode,
            pane,
            agent_pid,
            agent_process_start,
            runtime_owner,
            worktree_path,
            worktree_branch,
            task,
            model,
            effort,
            context_pct,
            total_tokens,
            todo_done,
            todo_total,
            // Never reduced from events — the snapshot CLI folds the latest
            // statusline context in via `with_agent_context`.
            context: None,
            last_seen: event.timestamp,
            last_activity: event.timestamp,
        };
        map.insert((kind, agent_id), state);
    }
    map.into_values().collect()
}

/// Rebuild the snapshot from the active event log, the agent carryover, and
/// the feed dir, then persist it atomically. The resulting JSON is what
/// `rimz sidebar snapshot --json` reads on attach.
///
/// Cost is O(active-events + items) per call. Archived event logs are never
/// rescanned; rotation pre-projects the agent rollup into
/// `agents.carryover.json` so the reducer stays bounded.
pub fn rebuild(paths: &StatePaths) -> Result<SidebarSnapshot> {
    let snapshot = build_from(paths)?;
    write_temp_then_rename(&paths.latest_snapshot, &snapshot)?;
    Ok(snapshot)
}

pub fn build_from(paths: &StatePaths) -> Result<SidebarSnapshot> {
    let items = feed_store::list(&paths.feed_dir)?;
    let events = event_log::read_all(&paths.events_log)?;
    let carryover = read_carryover(&paths.agents_carryover)?;
    let mut snapshot = SidebarSnapshot::build_with_carryover(
        paths.workspace_id.clone(),
        items,
        events,
        carryover.agents,
    );
    snapshot.reap_stale_sessions(Timestamp::now());
    snapshot.display_name = display_name_for(paths);
    let snapshot = snapshot.with_project_root(project_root_for(paths));
    Ok(snapshot)
}

pub(crate) fn display_name_for(paths: &StatePaths) -> String {
    workspace_record::read(&paths.workspace_record)
        .ok()
        .and_then(|record| {
            record
                .project_root
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| paths.workspace_id.as_str().to_owned())
}

pub(crate) fn project_root_for(paths: &StatePaths) -> Option<PathBuf> {
    workspace_record::read(&paths.workspace_record)
        .ok()
        .map(|record| record.project_root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::FeedKind;
    use crate::ids::{MuxName, PaneId, WorkspaceId};
    use std::path::{Path, PathBuf};

    fn agent(
        kind: &str,
        id: &str,
        status: AgentStatus,
        posture: PermissionPosture,
        last_seen: i64,
    ) -> AgentState {
        // The `last_seen` arg is a recency rank, not an absolute epoch: anchor it
        // to recent wall-clock (larger rank = more recent, all within ~100s of
        // now) so a `running` test agent is never falsely flagged stalled by the
        // real-time stall window. Tests that exercise the stall/ghost windows
        // override `last_activity` explicitly after construction.
        let offset_ms = (100_000 - last_seen).max(0) as u64;
        let timestamp = Timestamp::now() - std::time::Duration::from_millis(offset_ms);
        AgentState {
            agent_id: id.into(),
            kind: kind.into(),
            status,
            permission_posture: posture,
            plan_mode: false,
            pane: None,
            agent_pid: None,
            agent_process_start: None,
            runtime_owner: None,
            worktree_path: None,
            worktree_branch: None,
            task: None,
            model: None,
            effort: None,
            context_pct: None,
            total_tokens: None,
            todo_done: None,
            todo_total: None,
            context: None,
            last_seen: timestamp,
            last_activity: timestamp,
        }
    }

    fn pane(raw: &str, command: &str, cwd: &str) -> PaneRef {
        PaneRef {
            pane_id: PaneId::from_parts(MuxName::Tmux, raw),
            session_name: "rimz-test".to_owned(),
            view_id: Some("@0".to_owned()),
            view_kind: Some(crate::ids::ViewKind::Window),
            is_focused: false,
            command: Some(command.to_owned()),
            cwd: Some(cwd.to_owned()),
            pane_pid: None,
            pane_process_start: None,
        }
    }

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
        assert_eq!(snap.needs_attention.len(), 1);
        assert_eq!(snap.resolver_working.len(), 1);
        assert_eq!(snap.recently_answered.len(), 1);
        assert_eq!(snap.recent_activity.len(), 1);
        assert_eq!(snap.worktree_groups.len(), 1);
        assert_eq!(snap.worktree_groups[0].kind, SidebarWorktreeKind::Workspace);
        assert_eq!(snap.worktree_groups[0].label, "external");
        assert_eq!(snap.worktree_groups[0].rows.len(), 2);
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
        assert_eq!(snap.recent_activity.len(), 1);
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
        let mut session = agent(
            "claude",
            "sess-1",
            AgentStatus::Idle,
            PermissionPosture::Default,
            1_000,
        );
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
        let mut feature = agent(
            "claude",
            "sess-a",
            AgentStatus::Idle,
            PermissionPosture::Default,
            1_000,
        );
        feature.worktree_path = Some("/repo/shared".to_owned());
        feature.worktree_branch = Some("feature".to_owned());
        let mut main = agent(
            "claude",
            "sess-b",
            AgentStatus::Idle,
            PermissionPosture::Default,
            1_100,
        );
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
        let mut claude = agent(
            "claude",
            "sess-a",
            AgentStatus::Running,
            PermissionPosture::Default,
            1_000,
        );
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
    fn build_includes_non_feed_events_in_recent_activity() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let event = EventEnvelope::new(
            workspace.clone(),
            "session",
            "rimz",
            "cli",
            "event.emit",
            serde_json::json!({ "kind": "build.started", "title": "Building web" }),
        );

        let snap = SidebarSnapshot::build(workspace, Vec::new(), vec![event]);

        assert_eq!(snap.recent_activity.len(), 1);
        assert!(matches!(
            snap.recent_activity[0],
            SidebarActivity::Event { .. }
        ));
    }

    #[test]
    fn merge_carryover_prefers_newer_observation() {
        let mut older = agent(
            "claude",
            "agent-1",
            AgentStatus::Idle,
            PermissionPosture::Default,
            1_000,
        );
        older.worktree_branch = Some("main".into());
        let mut newer = agent(
            "claude",
            "agent-1",
            AgentStatus::Running,
            PermissionPosture::Auto,
            2_000,
        );
        newer.worktree_branch = Some("feature".into());
        let merged =
            merge_agent_rollups(std::slice::from_ref(&older), std::slice::from_ref(&newer));
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].status, AgentStatus::Running);
        assert_eq!(merged[0].worktree_branch.as_deref(), Some("feature"));
    }

    #[test]
    fn merge_carryover_preserves_orphaned_entries() {
        let only_in_carryover = agent(
            "claude",
            "agent-1",
            AgentStatus::Idle,
            PermissionPosture::Default,
            1_000,
        );
        let only_live = agent(
            "codex",
            "agent-2",
            AgentStatus::Running,
            PermissionPosture::Default,
            2_000,
        );
        let merged = merge_agent_rollups(
            std::slice::from_ref(&only_in_carryover),
            std::slice::from_ref(&only_live),
        );
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn carryover_session_end_tombstones_older_agent_state() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let carried = agent(
            "claude",
            "agent-1",
            AgentStatus::Idle,
            PermissionPosture::Default,
            1_000,
        );
        let ended = EventEnvelope::new(
            workspace,
            "session",
            "claude",
            "agent-hook",
            "agent.lifecycle",
            serde_json::json!({
                "event_name": "SessionEnd",
                "agent_id": "agent-1",
                "status": "idle",
            }),
        );

        let merged = agent_rollup_with_carryover(&[ended], vec![carried]);

        assert!(
            merged.is_empty(),
            "active-log SessionEnd must tombstone older carryover state"
        );
    }

    #[test]
    fn carryover_round_trips_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agents.carryover.json");
        assert_eq!(
            read_carryover(&path).unwrap(),
            EventCarryover::default(),
            "missing file yields empty carryover"
        );

        let carryover = EventCarryover {
            agents: vec![{
                let mut agent = agent(
                    "claude",
                    "agent-1",
                    AgentStatus::Success,
                    PermissionPosture::Default,
                    3_000,
                );
                agent.worktree_branch = Some("main".into());
                agent
            }],
        };
        write_carryover(&path, &carryover).unwrap();
        let loaded = read_carryover(&path).unwrap();
        assert_eq!(loaded, carryover);
    }

    #[test]
    fn lifecycle_carries_capability_forward_when_event_omits_it() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let lifecycle = |params: serde_json::Value| {
            EventEnvelope::new(
                workspace.clone(),
                "session",
                "codex",
                "agent-hook",
                "agent.lifecycle",
                params,
            )
        };
        // SessionStart establishes the capability line and a yolo posture pill.
        let start = lifecycle(serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "sess-1",
            "status": "idle",
            "permission_posture": "yolo",
            "model": "GPT-5.5",
            "effort": "high",
            "worktree_branch": "main",
        }));
        // A prompt-submit moves the agent to running but reports no posture/model.
        let prompt = lifecycle(serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-1",
            "status": "running",
            "permission_posture": serde_json::Value::Null,
            "task": "fix auth flow",
            "worktree_path": "/tmp/hook-subprocess-cwd",
            "worktree_branch": "wrong-branch",
        }));

        let agents = reduce_agent_states(&[start, prompt]);
        assert_eq!(agents.len(), 1);
        let agent = &agents[0];
        assert_eq!(agent.status, AgentStatus::Running);
        assert_eq!(agent.task.as_deref(), Some("fix auth flow"));
        // Capability and the security-relevant yolo posture survive the prompt.
        assert_eq!(agent.permission_posture, PermissionPosture::Yolo);
        assert_eq!(agent.model.as_deref(), Some("GPT-5.5"));
        assert_eq!(agent.effort.as_deref(), Some("high"));
        assert_eq!(agent.worktree_branch.as_deref(), Some("main"));
    }

    #[test]
    fn canonical_model_strips_capability_tag() {
        assert_eq!(canonical_model("claude-opus-4-8[1m]"), "claude-opus-4-8");
        // Idempotent on a bare id.
        assert_eq!(canonical_model("claude-opus-4-8"), "claude-opus-4-8");
        assert_eq!(canonical_model("gpt-5.5"), "gpt-5.5");
    }

    #[test]
    fn model_label_holds_canonical_across_suffix_drop() {
        // The live flip: SessionStart reports the suffixed id, the prompt omits
        // model entirely, and the first Stop falls back to the transcript's
        // bare id. Canonicalizing at reduce time keeps the label stable so the
        // `[1m]` tag never appears and then vanishes.
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
        let start = lifecycle(serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "sess-1",
            "status": "idle",
            "model": "claude-opus-4-8[1m]",
        }));
        let prompt = lifecycle(serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-1",
            "status": "running",
        }));
        let stop = lifecycle(serde_json::json!({
            "event_name": "Stop",
            "agent_id": "sess-1",
            "status": "success",
            "model": "claude-opus-4-8",
        }));

        let agents = reduce_agent_states(&[start, prompt, stop]);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].model.as_deref(), Some("claude-opus-4-8"));
    }

    #[test]
    fn lifecycle_carries_enrichment_forward() {
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
        let start = lifecycle(serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "sess-1",
            "status": "idle",
            "permission_posture": "default",
            "context_pct": 38,
            "total_tokens": 12_400,
            "todo_done": 3,
            "todo_total": 5,
        }));
        let prompt = lifecycle(serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-1",
            "status": "running",
            "task": "fix auth flow",
        }));

        let agents = reduce_agent_states(&[start, prompt]);
        assert_eq!(agents.len(), 1);
        let agent = &agents[0];
        assert_eq!(agent.context_pct, Some(38));
        assert_eq!(agent.total_tokens, Some(12_400));
        assert_eq!(agent.todo_done, Some(3));
        assert_eq!(agent.todo_total, Some(5));
        assert_eq!(agent.task.as_deref(), Some("fix auth flow"));
    }

    #[test]
    fn lifecycle_reduces_pane_id_and_carries_it_forward() {
        // The hook stamps the mux pane id on every lifecycle event so the
        // reducer can bind each agent to its own pane. A later event that omits
        // pane_id must not unbind the agent.
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
        let start = lifecycle(serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "sess-1",
            "status": "idle",
            "pane_id": "tmux:%7",
        }));
        let prompt = lifecycle(serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-1",
            "status": "running",
        }));

        let agents = reduce_agent_states(&[start, prompt]);
        assert_eq!(agents.len(), 1);
        let bound = agents[0].pane.as_ref().expect("pane carries forward");
        assert_eq!(bound.pane_id.raw(), "%7");
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
        let mut codex = agent(
            "codex",
            "sess-1",
            AgentStatus::Running,
            PermissionPosture::Default,
            1_000,
        );
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
        let mut codex = agent(
            "codex",
            "sess-1",
            AgentStatus::Running,
            PermissionPosture::Default,
            1_000,
        );
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
        let mut session = agent(
            "claude",
            "live-claude",
            AgentStatus::Idle,
            PermissionPosture::Default,
            1_000,
        );
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
        let mut session = agent(
            "claude",
            "live-claude",
            AgentStatus::Running,
            PermissionPosture::Default,
            2_000,
        );
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
        let mut session = agent(
            "claude",
            "live-claude",
            AgentStatus::Running,
            PermissionPosture::Default,
            2_000,
        );
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
        let mut session = agent(
            "claude",
            "live-claude",
            AgentStatus::Running,
            PermissionPosture::Default,
            0,
        );
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
        let mut session = agent(
            "claude",
            "live-claude",
            AgentStatus::Running,
            PermissionPosture::Default,
            0,
        );
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

    #[test]
    fn two_same_kind_agents_bind_to_their_stamped_panes() {
        // Two claude sessions in one worktree are indistinguishable by name and
        // cwd alone; binding is by the hook-stamped pane id, so each session
        // lands on exactly its own pane instead of cross-wiring the rows.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut older = agent(
            "claude",
            "sess-a",
            AgentStatus::Idle,
            PermissionPosture::Default,
            1_000,
        );
        older.worktree_path = Some("/repo/main".to_owned());
        older.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
        let mut newer = agent(
            "claude",
            "sess-b",
            AgentStatus::Running,
            PermissionPosture::Default,
            2_000,
        );
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
        let mut claude = agent(
            "claude",
            "sess-1",
            AgentStatus::Running,
            PermissionPosture::Default,
            1_000,
        );
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
    fn each_live_pane_yields_exactly_one_row() {
        // One pane = one row, by construction: every live pane produces exactly
        // one row — agent or process — and no pane id is ever duplicated.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let stamped = |id, raw| {
            let mut a = agent(
                "claude",
                id,
                AgentStatus::Running,
                PermissionPosture::Default,
                1_000,
            );
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
        let mut codex = agent(
            "codex",
            "sess-codex",
            AgentStatus::Idle,
            PermissionPosture::Yolo,
            2_000,
        );
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

        let mut zombie = agent(
            "claude",
            "zombie-claude",
            AgentStatus::Idle,
            PermissionPosture::Default,
            1_000,
        );
        zombie.worktree_path = Some("/repo/main".to_owned());
        let mut fresh = agent(
            "claude",
            "fresh-claude",
            AgentStatus::Idle,
            PermissionPosture::Default,
            2_000,
        );
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

        let mut claude = agent(
            "claude",
            "stale-claude",
            AgentStatus::Idle,
            PermissionPosture::Default,
            1_000,
        );
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
            let mut a = agent(
                "claude",
                "stale-a",
                AgentStatus::Idle,
                PermissionPosture::Default,
                1_000,
            );
            a.worktree_path = Some("/repo/main".to_owned());
            a
        };
        let stale_b = {
            let mut a = agent(
                "claude",
                "stale-b",
                AgentStatus::Idle,
                PermissionPosture::Default,
                1_001,
            );
            a.worktree_path = Some("/repo/main".to_owned());
            a
        };
        let stale_c = {
            let mut a = agent(
                "claude",
                "stale-c",
                AgentStatus::Idle,
                PermissionPosture::Default,
                1_002,
            );
            a.worktree_path = Some("/repo/main".to_owned());
            a
        };
        let live = {
            let mut a = agent(
                "claude",
                "live",
                AgentStatus::Running,
                PermissionPosture::Auto,
                i64::from(u32::MAX),
            );
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
        assert_eq!(
            agent_rows[0].permission_posture,
            Some(PermissionPosture::Auto)
        );
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
                    PermissionPosture::Default,
                    1_000 + i,
                );
                agent.worktree_path = Some("/repo/main".to_owned());
                agent
            })
            .collect::<Vec<_>>();
        let mut failed = agent(
            "claude",
            "failed",
            AgentStatus::Failed,
            PermissionPosture::Default,
            2_000,
        );
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
                    PermissionPosture::Default,
                    1_000 + i,
                );
                agent.worktree_path = Some("/repo/main".to_owned());
                if i == 0 {
                    agent.pane = Some(PaneRef {
                        pane_id: PaneId::from_parts(MuxName::Tmux, "%99"),
                        session_name: "rimz-test".to_owned(),
                        view_id: Some("@0".to_owned()),
                        view_kind: Some(crate::ids::ViewKind::Window),
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
    fn liveness_drops_dead_agent_pid_and_rebuilds_groups() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut codex = agent(
            "codex",
            "sess-1",
            AgentStatus::Running,
            PermissionPosture::Default,
            1_000,
        );
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
            agent(
                "claude",
                "stale",
                AgentStatus::Idle,
                PermissionPosture::Default,
                0,
            ),
            now,
            GHOST_SESSION_TTL_SECS + 60,
        );
        stale.worktree_path = Some("/repo/stale".to_owned());
        let mut recent = aged(
            agent(
                "claude",
                "recent",
                AgentStatus::Idle,
                PermissionPosture::Default,
                0,
            ),
            now,
            60,
        );
        recent.worktree_path = Some("/repo/recent".to_owned());
        // Old but pid-bearing: TTL reaping is for pidless ghosts only.
        let mut pidful = aged(
            agent(
                "codex",
                "pidful",
                AgentStatus::Idle,
                PermissionPosture::Default,
                0,
            ),
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
        let mut older = aged(
            agent(
                "codex",
                "older",
                AgentStatus::Idle,
                PermissionPosture::Default,
                0,
            ),
            now,
            120,
        );
        older.worktree_path = Some("/repo/a".to_owned());
        older.worktree_branch = Some("main".to_owned());
        let mut newer = aged(
            agent(
                "codex",
                "newer",
                AgentStatus::Idle,
                PermissionPosture::Default,
                0,
            ),
            now,
            60,
        );
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
        let mut older = aged(
            agent(
                "claude",
                "older",
                AgentStatus::Running,
                PermissionPosture::Default,
                0,
            ),
            now,
            120,
        );
        older.worktree_path = Some("/repo/a".to_owned());
        older.worktree_branch = Some("main".to_owned());
        older.agent_pid = Some(111);
        older.pane = Some(pane_ref_from_id(PaneId::from_parts(MuxName::Tmux, "%1")));
        let mut newer = aged(
            agent(
                "claude",
                "newer",
                AgentStatus::Running,
                PermissionPosture::Default,
                0,
            ),
            now,
            60,
        );
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

    fn view_pane(raw: &str, view: &str, focused: bool) -> PaneRef {
        PaneRef {
            pane_id: PaneId::from_parts(MuxName::Zellij, raw),
            session_name: "rimz-test".to_owned(),
            view_id: Some(view.to_owned()),
            view_kind: Some(crate::ids::ViewKind::Tab),
            is_focused: focused,
            command: Some("zsh".to_owned()),
            cwd: Some("/repo/main".to_owned()),
            pane_pid: None,
            pane_process_start: None,
        }
    }

    #[test]
    fn own_view_counts_only_siblings_sharing_the_view() {
        let own = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let focused_here = PaneId::from_parts(MuxName::Zellij, "terminal_2");
        let panes = vec![
            view_pane("terminal_1", "tab_0", false),
            view_pane("terminal_2", "tab_0", true),
            view_pane("terminal_3", "tab_1", true), // another tab — not a sibling
        ];

        let view = SidebarOwnView::from_panes(&own, &panes).expect("own pane is present");

        assert_eq!(view.sibling_count, 1);
        assert!(!view.own_is_focused);
        assert_eq!(view.focused_pane_id, Some(focused_here));
    }

    #[test]
    fn own_view_marks_when_the_sidebar_itself_is_focused() {
        let own = PaneId::from_parts(MuxName::Zellij, "terminal_1");
        let panes = vec![
            view_pane("terminal_1", "tab_0", true),
            view_pane("terminal_2", "tab_0", false),
        ];

        let view = SidebarOwnView::from_panes(&own, &panes).expect("own pane is present");

        assert!(view.own_is_focused);
        assert_eq!(view.focused_pane_id, None);
    }

    #[test]
    fn own_view_is_none_when_own_pane_is_absent() {
        // A view the caller cannot find itself in is unknowable — never close.
        let own = PaneId::from_parts(MuxName::Zellij, "terminal_404");
        let panes = vec![view_pane("terminal_1", "tab_0", true)];

        assert!(SidebarOwnView::from_panes(&own, &panes).is_none());
    }
}
