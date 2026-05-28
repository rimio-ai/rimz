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

use crate::feed::{
    AgentMode, AgentState, AgentStatus, FeedItem, FeedKind, FeedStatus, PaneRef, ResolverStepState,
    Surface,
};
use crate::ids::{PaneId, RequestId, ResolverId, WorkspaceId};
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
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SidebarStatusCount {
    pub status: AgentStatus,
    pub count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidebarRowKind {
    Agent,
    Item,
    Process,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SidebarRow {
    pub row_kind: SidebarRowKind,
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<AgentStatus>,
    pub mode: Option<AgentMode>,
    pub pane: Option<PaneRef>,
    pub request_id: Option<RequestId>,
    pub surface: Option<Surface>,
    pub task: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
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
        mut items: Vec<FeedItem>,
        events: Vec<EventEnvelope>,
        carryover_agents: Vec<AgentState>,
    ) -> Self {
        items.sort_by_key(|item| std::cmp::Reverse(item.updated_at));

        let live = reduce_agent_states(&events);
        let agents = merge_agent_rollups(&carryover_agents, &live);

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
        let worktree_groups = build_worktree_groups(&agents, &needs_attention, &resolver_working);

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
        }
    }

    /// Apply best-effort process liveness to agent overlays that published a
    /// PID. Hook protocols do not all expose a session-exit event; when a hook
    /// command can record the agent process identity, the sidebar uses it to
    /// suppress stale ledger overlays without scraping pane contents.
    pub fn drop_dead_agents_with(&mut self, mut is_alive: impl FnMut(u32, Option<&str>) -> bool) {
        let previous_len = self.agents.len();
        self.agents.retain(|agent| {
            agent
                .agent_pid
                .is_none_or(|pid| is_alive(pid, agent.agent_process_start.as_deref()))
        });
        if self.agents.len() != previous_len {
            self.worktree_groups =
                build_worktree_groups(&self.agents, &self.needs_attention, &self.resolver_working);
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
        );
        self
    }
}

const WORKTREE_ROW_CAP: usize = 6;

fn is_agent_native_item(item: &FeedItem) -> bool {
    item.source_kind == "agent-hook"
}

/// True when an agent-hook ask names a session (`agent_id`/`session_id`) that
/// the live agent rollup no longer carries. The rollup is the liveness source
/// of truth — gated by `SessionEnd` and process-liveness — so an ask whose
/// session has left it is stale and must not render as attention. Asks with no
/// session id can't be proven stale and are kept.
fn agent_hook_session_stale(item: &FeedItem, agents: &[AgentState]) -> bool {
    if item.source_kind != "agent-hook" {
        return false;
    }
    match agent_id_from_item(item) {
        Some(agent_id) => !agents
            .iter()
            .any(|agent| agent.kind == item.source && agent.agent_id == agent_id),
        None => false,
    }
}

fn build_worktree_groups(
    agents: &[AgentState],
    needs_attention: &[FeedItem],
    resolver_working: &[FeedItem],
) -> Vec<SidebarWorktreeGroup> {
    let rows = rows_from_ledger(agents, needs_attention, resolver_working);
    build_worktree_groups_from_rows(rows)
}

fn build_worktree_groups_with_panes(
    agents: &[AgentState],
    needs_attention: &[FeedItem],
    resolver_working: &[FeedItem],
    panes: &[PaneRef],
) -> Vec<SidebarWorktreeGroup> {
    let mut rows = Vec::new();
    let mut used_panes = BTreeSet::new();
    let mut replaced_agents = BTreeSet::new();

    for item in needs_attention.iter().chain(resolver_working.iter()) {
        if item.source_kind == "agent-hook" {
            // An agent-hook ask is real only while its originating session is
            // live; it then binds to that session's own pane. A stale ask
            // (session ended or process reaped) never claims an unrelated pane,
            // so a dead claude prompt can't latch onto a fresh codex.
            let Some(agent) = matching_agent(item, agents) else {
                continue;
            };
            let Some(pane) = matching_pane_for_agent(agent, agents, panes, &used_panes) else {
                continue;
            };
            let Some(mut row) = row_from_item(item, agents) else {
                continue;
            };
            row.pane = Some(pane.clone());
            used_panes.insert(pane.pane_id.to_string());
            replaced_agents.insert((agent.kind.clone(), agent.agent_id.clone()));
            rows.push(row);
        } else if let Some(mut row) = row_from_item(item, agents) {
            if row.pane.is_none()
                && let Some(pane) = matching_pane_for_item(item, agents, panes, &used_panes)
            {
                row.pane = Some(pane.clone());
                used_panes.insert(pane.pane_id.to_string());
            }
            rows.push(row);
        }
    }

    // Assign panes to agents in two passes so an exact identity/command match
    // always wins over a loose "looks like an agent" candidate. Without this an
    // extra claude with no pane of its own would grab a codex pane in the same
    // worktree before the codex agent's exact match ran, mislabelling the tab.
    let mut overlaid = BTreeSet::new();
    for agent in agents {
        let key = (agent.kind.clone(), agent.agent_id.clone());
        if replaced_agents.contains(&key) {
            continue;
        }
        if let Some(pane) = exact_pane_for_agent(agent, agents, panes, &used_panes) {
            push_agent_row(&mut rows, agent, pane, &mut used_panes);
            overlaid.insert(key);
        }
    }
    for agent in agents {
        let key = (agent.kind.clone(), agent.agent_id.clone());
        if replaced_agents.contains(&key) || overlaid.contains(&key) {
            continue;
        }
        if let Some(pane) = candidate_pane_for_agent(agent, agents, panes, &used_panes) {
            push_agent_row(&mut rows, agent, pane, &mut used_panes);
        }
    }

    for pane in panes {
        if used_panes.contains(pane.pane_id.as_str()) {
            continue;
        }
        rows.push(row_from_process(pane));
    }

    build_worktree_groups_from_rows(rows)
}

fn push_agent_row(
    rows: &mut Vec<SidebarRow>,
    agent: &AgentState,
    pane: &PaneRef,
    used_panes: &mut BTreeSet<String>,
) {
    let mut row = row_from_agent(agent);
    row.worktree_path = row.worktree_path.or_else(|| pane.cwd.clone());
    row.pane = Some(pane.clone());
    used_panes.insert(pane.pane_id.to_string());
    rows.push(row);
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
        if let Some(row) = row_from_item(item, agents) {
            if let Some(agent_id) = agent_id_from_item(item) {
                replaced_agents.insert((item.source.clone(), agent_id));
            }
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

fn build_worktree_groups_from_rows(rows: Vec<SidebarRow>) -> Vec<SidebarWorktreeGroup> {
    let mut by_group: BTreeMap<String, (String, SidebarWorktreeKind, Vec<SidebarRow>)> =
        BTreeMap::new();
    for row in rows {
        let (kind, key, label) =
            worktree_group_key(row.worktree_path.as_deref(), row.worktree_branch.as_deref());
        by_group
            .entry(key)
            .and_modify(|(label, _, rows)| {
                if let Some(branch) = row
                    .worktree_branch
                    .as_deref()
                    .filter(|branch| !branch.is_empty())
                {
                    *label = branch.to_owned();
                }
                rows.push(row.clone());
            })
            .or_insert_with(|| (label, kind, vec![row]));
    }

    let mut groups = by_group
        .into_iter()
        .map(|(key, (label, kind, mut rows))| {
            rows.sort_by(compare_rows);
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
            }
        })
        .collect::<Vec<_>>();
    groups.sort_by(compare_groups);
    groups
}

fn row_from_agent(agent: &AgentState) -> SidebarRow {
    SidebarRow {
        row_kind: SidebarRowKind::Agent,
        id: agent.agent_id.clone(),
        name: agent.kind.clone(),
        status: Some(agent.status),
        mode: Some(agent.mode),
        pane: agent.pane.clone(),
        request_id: None,
        surface: None,
        task: agent.task.clone(),
        model: agent.model.clone(),
        effort: agent.effort.clone(),
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
        mode: None,
        pane: Some(pane.clone()),
        request_id: None,
        surface: None,
        task: None,
        model: None,
        effort: None,
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

fn row_from_item(item: &FeedItem, agents: &[AgentState]) -> Option<SidebarRow> {
    if item.status != FeedStatus::Pending {
        return None;
    }
    let matched = matching_agent(item, agents);
    let row_kind = if item.source_kind == "agent-hook" {
        SidebarRowKind::Agent
    } else {
        SidebarRowKind::Item
    };
    let is_agent = row_kind == SidebarRowKind::Agent;
    let task = if is_agent {
        matched
            .and_then(|agent| agent.task.clone())
            .or_else(|| Some(feed_kind_task(item.kind).to_owned()))
    } else {
        Some(item.title.clone())
    };
    let id = agent_id_from_item(item).unwrap_or_else(|| item.request_id.to_string());
    Some(SidebarRow {
        row_kind,
        id,
        name: item.source.clone(),
        status: Some(AgentStatus::Waiting),
        mode: matched.map(|agent| agent.mode),
        pane: item
            .pane
            .clone()
            .or_else(|| matched.and_then(|agent| agent.pane.clone())),
        request_id: Some(item.request_id.clone()),
        surface: Some(item.surface),
        task,
        model: matched.and_then(|agent| agent.model.clone()),
        effort: matched.and_then(|agent| agent.effort.clone()),
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

fn matching_pane_for_item<'a>(
    item: &FeedItem,
    agents: &[AgentState],
    panes: &'a [PaneRef],
    used_panes: &BTreeSet<String>,
) -> Option<&'a PaneRef> {
    if let Some(item_pane) = &item.pane
        && let Some(pane) = panes.iter().find(|pane| {
            !used_panes.contains(pane.pane_id.as_str())
                && pane.pane_id == item_pane.pane_id
                && pane_start_matches(item_pane, pane)
        })
    {
        return Some(pane);
    }

    if let Some(agent) = matching_agent(item, agents)
        && let Some(pane) = matching_pane_for_agent(agent, agents, panes, used_panes)
    {
        return Some(pane);
    }

    panes
        .iter()
        .find(|pane| {
            !used_panes.contains(pane.pane_id.as_str())
                && pane_command_matches(pane, &item.source)
                && worktree_matches(item.worktree_path.as_deref(), pane.cwd.as_deref())
        })
        .or_else(|| {
            if item.source_kind != "agent-hook" {
                return None;
            }
            panes.iter().find(|pane| {
                !used_panes.contains(pane.pane_id.as_str())
                    && pane_is_loose_agent_candidate(pane)
                    && worktree_matches(item.worktree_path.as_deref(), pane.cwd.as_deref())
            })
        })
}

fn matching_pane_for_agent<'a>(
    agent: &AgentState,
    agents: &[AgentState],
    panes: &'a [PaneRef],
    used_panes: &BTreeSet<String>,
) -> Option<&'a PaneRef> {
    exact_pane_for_agent(agent, agents, panes, used_panes)
        .or_else(|| candidate_pane_for_agent(agent, agents, panes, used_panes))
}

/// Pane that is unambiguously this agent's: the same normalized id it
/// published, or a foreground command equal to the agent kind in its worktree.
fn exact_pane_for_agent<'a>(
    agent: &AgentState,
    agents: &[AgentState],
    panes: &'a [PaneRef],
    used_panes: &BTreeSet<String>,
) -> Option<&'a PaneRef> {
    if let Some(agent_pane) = &agent.pane
        && let Some(pane) = panes.iter().find(|pane| {
            !used_panes.contains(pane.pane_id.as_str())
                && pane.pane_id == agent_pane.pane_id
                && pane_start_matches(agent_pane, pane)
        })
    {
        return Some(pane);
    }

    let pane = panes.iter().find(|pane| {
        !used_panes.contains(pane.pane_id.as_str())
            && pane_command_matches(pane, &agent.kind)
            && worktree_matches(agent.worktree_path.as_deref(), pane.cwd.as_deref())
    })?;
    if agent_command_match_oversubscribed(agent, agents, panes) {
        return None;
    }
    Some(pane)
}

/// Loose fallback for agents that run under a wrapper binary (e.g. codex as
/// `node`): any non-shell pane in the worktree. Only used after every exact
/// match is settled, so it can't steal a pane another agent owns by name.
fn candidate_pane_for_agent<'a>(
    agent: &AgentState,
    agents: &[AgentState],
    panes: &'a [PaneRef],
    used_panes: &BTreeSet<String>,
) -> Option<&'a PaneRef> {
    if agent_candidate_match_oversubscribed(agent, agents, panes) {
        return None;
    }
    panes.iter().find(|pane| {
        !used_panes.contains(pane.pane_id.as_str())
            && pane_is_loose_agent_candidate(pane)
            && worktree_matches(agent.worktree_path.as_deref(), pane.cwd.as_deref())
    })
}

fn pane_start_matches(expected: &PaneRef, actual: &PaneRef) -> bool {
    match (expected.pane_process_start, actual.pane_process_start) {
        (Some(expected), Some(actual)) => expected == actual,
        _ => true,
    }
}

fn pane_is_loose_agent_candidate(pane: &PaneRef) -> bool {
    pane.command.as_deref().is_some_and(|command| {
        let label = command_label(command);
        if is_shell_command(&label) || label == "rimz-sidebar" {
            return false;
        }
        command_agent_kind(command).is_none()
    })
}

fn agent_command_match_oversubscribed(
    agent: &AgentState,
    agents: &[AgentState],
    panes: &[PaneRef],
) -> bool {
    let pane_count = panes
        .iter()
        .filter(|pane| {
            pane_command_matches(pane, &agent.kind)
                && worktree_matches(agent.worktree_path.as_deref(), pane.cwd.as_deref())
        })
        .count();
    agent_match_oversubscribed(agent, agents, pane_count)
}

fn agent_candidate_match_oversubscribed(
    agent: &AgentState,
    agents: &[AgentState],
    panes: &[PaneRef],
) -> bool {
    let pane_count = panes
        .iter()
        .filter(|pane| {
            pane_is_loose_agent_candidate(pane)
                && worktree_matches(agent.worktree_path.as_deref(), pane.cwd.as_deref())
        })
        .count();
    agent_match_oversubscribed(agent, agents, pane_count)
}

fn agent_match_oversubscribed(
    agent: &AgentState,
    agents: &[AgentState],
    pane_count: usize,
) -> bool {
    pane_count > 0
        && agents
            .iter()
            .filter(|candidate| {
                candidate.kind == agent.kind
                    && same_agent_worktree(
                        agent.worktree_path.as_deref(),
                        candidate.worktree_path.as_deref(),
                    )
            })
            .count()
            > pane_count
}

fn same_agent_worktree(left: Option<&str>, right: Option<&str>) -> bool {
    match (
        left.filter(|value| !value.is_empty()),
        right.filter(|value| !value.is_empty()),
    ) {
        (Some(left), Some(right)) => left == right,
        (None, None) => true,
        _ => false,
    }
}

fn is_shell_command(command: &str) -> bool {
    matches!(
        command,
        "sh" | "bash"
            | "zsh"
            | "fish"
            | "dash"
            | "ksh"
            | "mksh"
            | "elvish"
            | "nu"
            | "xonsh"
            | "pwsh"
            | "powershell"
    )
}

fn pane_command_matches(pane: &PaneRef, expected: &str) -> bool {
    pane.command.as_deref().is_some_and(|command| {
        command_agent_kind(command)
            .map(|kind| kind == expected)
            .unwrap_or_else(|| command_label(command) == expected)
    })
}

fn worktree_matches(expected: Option<&str>, actual: Option<&str>) -> bool {
    expected
        .filter(|expected| !expected.is_empty())
        .zip(actual.filter(|actual| !actual.is_empty()))
        .is_none_or(|(expected, actual)| expected == actual)
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
        FeedKind::ToolTelemetry => "tool",
        FeedKind::SubAgentStarted => "sub-agent started",
        FeedKind::SubAgentStopped => "sub-agent stopped",
        FeedKind::Generic => "activity",
    }
}

fn worktree_group_key(
    path: Option<&str>,
    branch: Option<&str>,
) -> (SidebarWorktreeKind, String, String) {
    if let Some(path) = path.filter(|path| !path.is_empty()) {
        let label = branch
            .filter(|branch| !branch.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| {
                std::path::Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .filter(|name| !name.is_empty())
                    .unwrap_or(path)
                    .to_owned()
            });
        return (SidebarWorktreeKind::Worktree, path.to_owned(), label);
    }
    if let Some(branch) = branch.filter(|branch| !branch.is_empty()) {
        return (
            SidebarWorktreeKind::Worktree,
            format!("branch:{branch}"),
            branch.to_owned(),
        );
    }
    (
        SidebarWorktreeKind::Workspace,
        "workspace".to_owned(),
        "workspace".to_owned(),
    )
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

/// Merge two agent rollups, preferring the newer `last_seen` per
/// `(agent_kind, agent_id)` pair. `live` wins on ties so a same-second
/// observation in the active log overrides carryover.
pub fn merge_agent_rollups(base: &[AgentState], live: &[AgentState]) -> Vec<AgentState> {
    let mut map: BTreeMap<(String, String), AgentState> = BTreeMap::new();
    for entry in base.iter().chain(live.iter()) {
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

/// Fold `agent.lifecycle` events into the latest [`AgentState`] per
/// agent_id, keyed by `(agent_kind, agent_id)`. Anonymous lifecycle events
/// (no agent_id) collapse to a single rollup keyed by `agent_kind`. Events
/// are walked in log order, so the newest observation wins.
///
/// Each event is a *partial* update: `status` always comes from the event,
/// but the stable capability/identity fields (`mode`, `model`, `effort`,
/// worktree, pane) carry forward from the prior state when the event omits
/// them. A `UserPromptSubmit` therefore moves the agent to running without
/// erasing its model line or demoting a `bypass` mode pill.
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
        let mode: AgentMode = event
            .params
            .get("mode")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .or_else(|| prior.map(|p| p.mode))
            .unwrap_or(AgentMode::Unknown);
        let param_string = |key: &str| {
            event
                .params
                .get(key)
                .and_then(|v| v.as_str())
                .map(ToOwned::to_owned)
        };
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
        // Task is activity-bound, not identity: a fresh event replaces it
        // (idle clears it back to "—"); only capability fields persist.
        let task = param_string("task");
        let model = param_string("model").or_else(|| prior.and_then(|p| p.model.clone()));
        let effort = param_string("effort").or_else(|| prior.and_then(|p| p.effort.clone()));
        let pane = prior.and_then(|p| p.pane.clone());
        let state = AgentState {
            agent_id: agent_id.clone(),
            kind: kind.clone(),
            status,
            mode,
            pane,
            agent_pid,
            agent_process_start,
            worktree_path,
            worktree_branch,
            task,
            model,
            effort,
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
    snapshot.display_name = display_name_for(paths);
    snapshot.drop_dead_agents_with(agent_process_alive);
    Ok(snapshot)
}

#[cfg(target_os = "linux")]
fn agent_process_alive(pid: u32, expected_start: Option<&str>) -> bool {
    let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(_) => return false,
    };
    match expected_start {
        Some(expected) => linux_process_start_from_stat(&stat) == Some(expected),
        None => true,
    }
}

#[cfg(not(target_os = "linux"))]
fn agent_process_alive(_pid: u32, _expected_start: Option<&str>) -> bool {
    true
}

#[cfg(target_os = "linux")]
fn linux_process_start_from_stat(stat: &str) -> Option<&str> {
    let after_comm = stat.rsplit_once(") ")?.1;
    after_comm.split_whitespace().nth(19)
}

fn display_name_for(paths: &StatePaths) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::FeedKind;
    use crate::ids::{MuxName, PaneId, WorkspaceId};
    use std::path::Path;

    fn agent(
        kind: &str,
        id: &str,
        status: AgentStatus,
        mode: AgentMode,
        last_seen: i64,
    ) -> AgentState {
        let timestamp = Timestamp::from_second(last_seen).unwrap();
        AgentState {
            agent_id: id.into(),
            kind: kind.into(),
            status,
            mode,
            pane: None,
            agent_pid: None,
            agent_process_start: None,
            worktree_path: None,
            worktree_branch: None,
            task: None,
            model: None,
            effort: None,
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
        assert_eq!(snap.worktree_groups[0].label, "workspace");
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
            AgentMode::Interactive,
            1_000,
        );
        older.worktree_branch = Some("main".into());
        let mut newer = agent(
            "claude",
            "agent-1",
            AgentStatus::Running,
            AgentMode::Auto,
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
            AgentMode::Interactive,
            1_000,
        );
        let only_live = agent(
            "codex",
            "agent-2",
            AgentStatus::Running,
            AgentMode::Plan,
            2_000,
        );
        let merged = merge_agent_rollups(
            std::slice::from_ref(&only_in_carryover),
            std::slice::from_ref(&only_live),
        );
        assert_eq!(merged.len(), 2);
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
                    AgentMode::Interactive,
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
        // SessionStart establishes the capability line and a bypass pill.
        let start = lifecycle(serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "sess-1",
            "status": "idle",
            "mode": "bypass",
            "model": "GPT-5.5",
            "effort": "high",
            "worktree_branch": "main",
        }));
        // A prompt-submit moves the agent to running but reports no mode/model.
        let prompt = lifecycle(serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-1",
            "status": "running",
            "mode": serde_json::Value::Null,
            "task": "fix auth flow",
            "worktree_path": "/tmp/hook-subprocess-cwd",
            "worktree_branch": "wrong-branch",
        }));

        let agents = reduce_agent_states(&[start, prompt]);
        assert_eq!(agents.len(), 1);
        let agent = &agents[0];
        assert_eq!(agent.status, AgentStatus::Running);
        assert_eq!(agent.task.as_deref(), Some("fix auth flow"));
        // Capability and the security-relevant bypass pill survive the prompt.
        assert_eq!(agent.mode, AgentMode::Bypass);
        assert_eq!(agent.model.as_deref(), Some("GPT-5.5"));
        assert_eq!(agent.effort.as_deref(), Some("high"));
        assert_eq!(agent.worktree_branch.as_deref(), Some("main"));
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
    fn live_panes_overlay_matching_agent_rows() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut codex = agent(
            "codex",
            "sess-1",
            AgentStatus::Running,
            AgentMode::Interactive,
            1_000,
        );
        codex.worktree_path = Some("/repo/main".to_owned());
        codex.worktree_branch = Some("main".to_owned());
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
    fn live_panes_overlay_runtime_command_by_worktree() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut codex = agent(
            "codex",
            "sess-1",
            AgentStatus::Running,
            AgentMode::Interactive,
            1_000,
        );
        codex.worktree_path = Some("/repo/main".to_owned());

        let snapshot =
            SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), vec![codex])
                .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

        assert_eq!(snapshot.worktree_groups.len(), 1);
        assert_eq!(snapshot.worktree_groups[0].rows.len(), 1);
        let row = &snapshot.worktree_groups[0].rows[0];
        assert_eq!(row.row_kind, SidebarRowKind::Agent);
        assert_eq!(row.name, "codex");
        assert_eq!(row.pane.as_ref().unwrap().pane_id.raw(), "%1");
    }

    #[test]
    fn live_panes_do_not_render_unmatched_ledger_agents() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut codex = agent(
            "codex",
            "sess-1",
            AgentStatus::Running,
            AgentMode::Interactive,
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
            AgentMode::Interactive,
            1_000,
        );
        session.worktree_path = Some("/repo/main".to_owned());

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
            AgentMode::Bypass,
            2_000,
        );
        codex.worktree_path = Some("/repo/main".to_owned());

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
            AgentMode::Interactive,
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

    #[test]
    fn one_codex_pane_does_not_choose_among_multiple_old_codex_agents() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut first = agent(
            "codex",
            "old-1",
            AgentStatus::Idle,
            AgentMode::Bypass,
            1_000,
        );
        first.worktree_path = Some("/repo/main".to_owned());
        first.model = Some("gpt-5.5".to_owned());
        let mut second = agent(
            "codex",
            "old-2",
            AgentStatus::Idle,
            AgentMode::Bypass,
            1_100,
        );
        second.worktree_path = Some("/repo/main".to_owned());
        second.model = Some("gpt-5.5".to_owned());

        let snapshot = SidebarSnapshot::build_with_carryover(
            workspace,
            Vec::new(),
            Vec::new(),
            vec![first, second],
        )
        .with_live_panes(vec![pane("%1", "node /usr/bin/codex", "/repo/main")], None);

        assert_eq!(snapshot.worktree_groups.len(), 1);
        let rows = &snapshot.worktree_groups[0].rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].row_kind, SidebarRowKind::Process);
        assert_eq!(rows[0].name, "codex");
        assert_eq!(rows[0].status, None);
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
                    AgentMode::Interactive,
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
            AgentMode::Interactive,
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
                    AgentMode::Interactive,
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
            AgentMode::Interactive,
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
}
