use jiff::Timestamp;

use crate::agents::lifecycle::TurnPhase;
use crate::agents::{AgentState, AgentStatus};
use crate::feed::{FeedItem, FeedStatus, ResolverStepState, Surface};
use crate::ledger::snapshot::row::{AgentCard, RowCard, SidebarResolverState, SidebarRow};
use crate::pane::PaneRef;

pub(in crate::ledger::snapshot) fn row_from_agent(
    agent: &AgentState,
    now: Timestamp,
) -> SidebarRow {
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
        unread: false,
        inactive: false,
        last_activity: agent.last_activity,
        card: RowCard::Agent(Box::new(AgentCard {
            status: Some(agent.status),
            phase: agent.phase,
            request_id: None,
            surface: None,
            task: agent.task.clone(),
            prompt: agent.prompt.clone(),
            description: agent.description.clone(),
            model: agent.model.clone(),
            effort: agent.effort.clone(),
            handle: agent.role.clone().or_else(|| agent.profile.clone()),
            context_pct: Some(agent.context_pct.unwrap_or(0)),
            context_window: agent_context_window(agent),
            total_tokens: agent.total_tokens,
            cache_read_input_tokens: agent.cache_read_input_tokens,
            cache_write_input_tokens: agent.cache_write_input_tokens,
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
/// marker is still fresh. The rollup's next lifecycle signal clears the stamp;
/// this window is the display backstop for a session that dies mid-compact and
/// never produces another signal.
fn is_compacting(agent: &AgentState, now: Timestamp) -> bool {
    agent.compacting_since.is_some_and(|since| {
        now.duration_since(since).as_secs() < crate::agents::COMPACTING_WINDOW_SECS
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
pub(super) fn row_from_standalone_item(item: &FeedItem, pane: &PaneRef) -> SidebarRow {
    debug_assert_eq!(item.status, FeedStatus::Pending);
    debug_assert_ne!(item.source_kind, "agent-hook");
    let id = agent_id_from_item(item).unwrap_or_else(|| item.request_id.to_string());
    SidebarRow {
        id,
        name: item.source.clone(),
        pane: Some(pane.clone()),
        worktree_path: item.worktree_path.clone().or_else(|| pane.cwd.clone()),
        worktree_branch: item.worktree_branch.clone(),
        unread: false,
        inactive: false,
        last_activity: item.updated_at,
        card: RowCard::Agent(Box::new(AgentCard {
            status: Some(AgentStatus::Waiting),
            // A waiting row is blocked on the human, not reasoning — no turn phase.
            phase: TurnPhase::Idle,
            request_id: Some(item.request_id.clone()),
            surface: Some(item.surface),
            task: Some(item.title.clone()),
            prompt: None,
            description: None,
            model: None,
            effort: None,
            handle: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
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

pub(super) fn agent_id_from_item(item: &FeedItem) -> Option<String> {
    item.agent_session_id().map(ToOwned::to_owned)
}

pub(super) fn active_resolver_state(item: &FeedItem) -> Option<SidebarResolverState> {
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
