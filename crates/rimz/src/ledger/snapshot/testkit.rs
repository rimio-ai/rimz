//! Shared fixtures for the snapshot unit tests: canned agents, panes, and
//! lifecycle events the per-module test mods build scenarios from.
//!
//! Time is data here: every fixture anchors to the fixed [`epoch`], and a
//! scenario passes the same instant as the projection's `now` (via [`room`]),
//! so window verdicts — stall, compaction expiry, ghost TTLs, rate-limit
//! resets — are exact and the suite never reads the wall clock.

use std::path::Path;

use jiff::Timestamp;

use super::row::SidebarRow;
use super::view::SidebarSnapshot;
use crate::agents::lifecycle::{self, TurnPhase};
use crate::agents::{
    AgentContext, AgentLifecycleObservation, AgentRateLimits, AgentTurnError, RateLimitWindow,
    TurnErrorClass,
};
use crate::feed::{AgentState, AgentStatus, FeedItem, PaneRef};
use crate::ids::{AgentKind, MuxName, PaneId, WorkspaceId};
use crate::schema::event::EventEnvelope;

/// The suite's fixed "now": an arbitrary instant every fixture offsets from
/// and every scenario projects at, so the tests are deterministic on any
/// wall-clock day.
pub(super) fn epoch() -> Timestamp {
    Timestamp::from_second(1_750_000_000).expect("fixed test epoch is valid")
}

/// The instant `secs` seconds before the [`epoch`] — for stamping markers,
/// turn boundaries, and pane starts relative to the projection's now.
pub(super) fn ago(secs: i64) -> Timestamp {
    Timestamp::from_second(epoch().as_second() - secs).expect("offset from the test epoch is valid")
}

/// The canonical test workspace every scenario shares.
pub(super) fn workspace() -> WorkspaceId {
    WorkspaceId::from_project_root(Path::new("/tmp/x"))
}

/// Build a snapshot at the [`epoch`] — the one construction path the
/// scenarios share. Enrichment chains (`with_live_panes`, `with_agent_context`,
/// …) hang off the returned snapshot as in production.
pub(super) fn room(items: Vec<FeedItem>, agents: Vec<AgentState>) -> SidebarSnapshot {
    SidebarSnapshot::build_with_agents(workspace(), items, agents, epoch())
}

/// Build a snapshot at the [`epoch`] and admit each root agent through a
/// synthetic live pane. Tests that assert row projection, ranking, caps, or
/// display status use this instead of the frameless [`room`] constructor.
pub(super) fn room_with_agent_panes(
    items: Vec<FeedItem>,
    mut agents: Vec<AgentState>,
) -> SidebarSnapshot {
    let mut panes = Vec::new();
    for (idx, agent) in agents.iter_mut().enumerate() {
        if agent.parent_agent_id.is_some() {
            continue;
        }
        let pane = agent.pane.clone().unwrap_or_else(|| {
            let raw = format!("%agent-{idx}");
            pane(
                &raw,
                agent.kind.as_str(),
                agent.worktree_path.as_deref().unwrap_or("/repo/main"),
            )
        });
        agent.pane = Some(pane.clone());
        panes.push(pane);
    }
    SidebarSnapshot::build_with_agents(workspace(), items, agents, epoch())
        .with_live_panes(panes, None)
}

/// Every projected row across every worktree group, in render order.
pub(super) fn rows(snapshot: &SidebarSnapshot) -> Vec<&SidebarRow> {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .collect()
}

/// The projected row with this id, or a panic naming what was on screen.
pub(super) fn row<'a>(snapshot: &'a SidebarSnapshot, id: &str) -> &'a SidebarRow {
    rows(snapshot)
        .into_iter()
        .find(|row| row.id == id)
        .unwrap_or_else(|| panic!("row {id} present in {:?}", rows(snapshot)))
}

pub(super) fn agent(kind: &str, id: &str, status: AgentStatus, last_seen: i64) -> AgentState {
    // The `last_seen` arg is a recency rank, not an absolute epoch: anchor it
    // to the fixed test epoch (larger rank = more recent, all within ~100s of
    // it) so a `running` test agent projected at `epoch()` is never falsely
    // flagged stalled. Tests that exercise the stall/ghost windows override
    // `last_activity` explicitly (see `AgentStateFx::active_ago`).
    let offset_ms = (100_000 - last_seen).max(0) as u64;
    let timestamp = epoch() - std::time::Duration::from_millis(offset_ms);
    AgentState {
        agent_id: id.into(),
        kind: AgentKind::new_unchecked(kind),
        name: None,
        kind_ordinal: None,
        status,
        phase: TurnPhase::Idle,
        pane: None,
        agent_pid: None,
        agent_process_start: None,
        runtime_owner: None,
        parent_agent_id: None,
        worktree_path: None,
        worktree_branch: None,
        task: None,
        prompt: None,
        transcript_path: None,
        recent_prompts: Vec::new(),
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
        subagent_description: None,
        subagent_started_at: None,
        turn_started_at: None,
        compacting_since: None,
        compaction_count: 0,
        last_seen: timestamp,
        last_activity: timestamp,
        registered_at: Some(timestamp),
    }
}

/// Fluent enrichment over [`agent`] for the fields scenarios actually vary —
/// each method returns the state, so the common shape reads as one chain
/// (`agent(..).worktree("/repo/main").in_pane("%1")`) while a rare field stays
/// a plain mutation on the binding.
pub(super) trait AgentStateFx: Sized {
    /// Stamp the tmux pane this agent claims.
    fn in_pane(self, raw: &str) -> Self;
    fn worktree(self, path: &str) -> Self;
    fn branch(self, branch: &str) -> Self;
    /// Pin `last_activity`/`last_seen` to `secs` before the [`epoch`] — the
    /// stall-window and TTL scenarios' lever.
    fn active_ago(self, secs: i64) -> Self;
    /// Stamp the current turn boundary `secs` before the [`epoch`].
    fn turn_started_ago(self, secs: i64) -> Self;
    /// Attach rate-limit windows (merged into any context already attached).
    fn limits(self, windows: Vec<RateLimitWindow>) -> Self;
    /// Attach a transcript turn-death marker stamped `secs_ago` before the
    /// [`epoch`] (merged into any context already attached).
    fn turn_error(self, secs_ago: i64, label: &str) -> Self;
    fn turn_error_class(self, secs_ago: i64, label: &str, class: TurnErrorClass) -> Self;
    fn paused_turn_error(self, secs_ago: i64, label: &str) -> Self;
    fn overloaded_turn_error(self, secs_ago: i64, label: &str) -> Self;
    /// Stamp the compaction head `secs` before the [`epoch`].
    fn compacting_ago(self, secs: i64) -> Self;
}

impl AgentStateFx for AgentState {
    fn in_pane(mut self, raw: &str) -> Self {
        self.pane = Some(PaneRef::from_id(PaneId::from_parts(MuxName::Tmux, raw)));
        self
    }

    fn worktree(mut self, path: &str) -> Self {
        self.worktree_path = Some(path.to_owned());
        self
    }

    fn branch(mut self, branch: &str) -> Self {
        self.worktree_branch = Some(branch.to_owned());
        self
    }

    fn active_ago(mut self, secs: i64) -> Self {
        let at = Timestamp::from_second(epoch().as_second() - secs).unwrap();
        self.last_activity = at;
        self.last_seen = at;
        self
    }

    fn turn_started_ago(mut self, secs: i64) -> Self {
        self.turn_started_at = Some(ago(secs));
        self
    }

    fn limits(mut self, windows: Vec<RateLimitWindow>) -> Self {
        self.context.get_or_insert_with(bare_context).rate_limits =
            Some(AgentRateLimits { windows });
        self
    }

    fn turn_error(mut self, secs_ago: i64, label: &str) -> Self {
        self.context.get_or_insert_with(bare_context).turn_error = Some(AgentTurnError {
            class: TurnErrorClass::Failed,
            at: epoch() - std::time::Duration::from_secs(secs_ago as u64),
            label: Some(label.to_owned()),
        });
        self
    }

    fn turn_error_class(mut self, secs_ago: i64, label: &str, class: TurnErrorClass) -> Self {
        self.context.get_or_insert_with(bare_context).turn_error = Some(AgentTurnError {
            class,
            at: epoch() - std::time::Duration::from_secs(secs_ago as u64),
            label: Some(label.to_owned()),
        });
        self
    }

    fn paused_turn_error(self, secs_ago: i64, label: &str) -> Self {
        self.turn_error_class(secs_ago, label, TurnErrorClass::PausedRateLimit)
    }

    fn overloaded_turn_error(self, secs_ago: i64, label: &str) -> Self {
        self.turn_error_class(secs_ago, label, TurnErrorClass::PausedOverloaded)
    }

    fn compacting_ago(mut self, secs: i64) -> Self {
        self.compacting_since = Some(epoch() - std::time::Duration::from_secs(secs as u64));
        self
    }
}

/// An empty rich context observed at the [`epoch`] — the base the fluent
/// `limits`/`turn_error` enrichments build on.
pub(super) fn bare_context() -> AgentContext {
    AgentContext {
        source: "claude".to_owned(),
        session_name: None,
        session_preview: None,
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
        turn_error: None,
        observed_at: epoch(),
    }
}

/// A rate-limit window reading: `used`% drained, resetting `resets_in_secs`
/// after the [`epoch`] (negative = the reset already passed).
pub(super) fn window(used: u8, resets_in_secs: i64) -> RateLimitWindow {
    let resets_at = Timestamp::from_second(epoch().as_second() + resets_in_secs).unwrap();
    RateLimitWindow {
        used_percentage: Some(used),
        resets_at: Some(resets_at),
        duration_mins: Some(300),
        ..Default::default()
    }
}

pub(super) fn pane(raw: &str, command: &str, cwd: &str) -> PaneRef {
    PaneRef {
        pane_id: PaneId::from_parts(MuxName::Tmux, raw),
        session_name: "rimz-test".to_owned(),
        view_id: Some("@0".to_owned()),
        view_kind: Some(crate::ids::ViewKind::Window),
        view_name: None,
        is_focused: false,
        command: Some(command.to_owned()),
        spawn_command: None,
        cwd: Some(cwd.to_owned()),
        pane_pid: None,
        pane_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    }
}

pub(super) fn pane_started(raw: &str, cwd: &str, start: Timestamp) -> PaneRef {
    PaneRef {
        pane_process_start: Some(start),
        elevated_agent: None,
        first_seen_at_ms: None,
        ..pane(raw, "claude", cwd)
    }
}

pub(super) fn agent_in(id: &str, path: &str, status: AgentStatus, rank: i64) -> AgentState {
    agent("claude", id, status, rank).worktree(path)
}

pub(super) fn lifecycle_at(
    workspace: &WorkspaceId,
    source: &str,
    event_name: &str,
    agent_id: &str,
    signal: lifecycle::LifecycleSignal,
) -> EventEnvelope {
    let observation = AgentLifecycleObservation::new(Some(agent_id.into()), signal);
    EventEnvelope::agent_lifecycle(
        workspace.clone(),
        "session",
        source,
        event_name,
        &observation,
    )
}

pub(super) fn sorted_value(mut agents: Vec<AgentState>) -> serde_json::Value {
    agents.sort_by_key(|a| (a.kind.clone(), a.agent_id.clone()));
    serde_json::to_value(agents).unwrap()
}

/// A paneless child `AgentState` of `parent`, stamped `secs_ago` before the
/// [`epoch`].
pub(super) fn child_state(
    parent: &str,
    id: &str,
    status: AgentStatus,
    secs_ago: i64,
) -> AgentState {
    let mut child = agent("claude", id, status, 0);
    child.parent_agent_id = Some(parent.into());
    child.task = Some("Explore".to_owned());
    child.active_ago(secs_ago)
}
