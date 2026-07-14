use jiff::Timestamp;

use crate::agents::{AgentState, AgentStatus};
use crate::store::snapshot::row::{AgentCard, RowCard, SidebarRow};

pub(in crate::store::snapshot) fn row_from_agent(agent: &AgentState, now: Timestamp) -> SidebarRow {
    // `SidebarRow.status` is the *displayed* status. It starts as the raw rollup
    // value and is projected in `project_display_status` once the row knows its
    // subagents and its account's rate-limit budget (stall → `failed`,
    // spent-budget → `paused`); native prompts already live on the rollup.
    // The rollup in `snapshot.agents` always keeps the true status.
    let (status, phase) = if agent.is_awaiting_input() {
        (AgentStatus::Waiting, crate::agents::TurnPhase::Idle)
    } else {
        (agent.status, agent.phase)
    };
    SidebarRow {
        id: agent.agent_id.to_string(),
        name: agent.kind.to_string(),
        pane: agent.pane.clone(),
        worktree_path: agent.worktree_path.clone(),
        worktree_branch: agent.worktree_branch.clone(),
        channel: agent.channel.clone(),
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        last_activity: agent.last_activity,
        card: RowCard::Agent(Box::new(AgentCard {
            status,
            phase,
            task: agent.task.clone(),
            prompt: agent.prompt.clone(),
            description: agent.description.clone(),
            model: agent.model.clone(),
            effort: agent.effort.clone(),
            handle: agent
                .role
                .clone()
                .or_else(|| agent.name_explicit.then(|| agent.name.clone()).flatten())
                .or_else(|| agent.profile.clone()),
            team: agent.team.clone(),
            launch_group: agent.launch_group.clone(),
            launch_ordinal: agent.launch_ordinal,
            context_pct: agent.context_pct,
            context_window: agent_context_window(agent),
            total_tokens: agent.total_tokens,
            cache_read_input_tokens: agent.cache_read_input_tokens,
            cache_write_input_tokens: agent.cache_write_input_tokens,
            fresh_input_tokens: agent.fresh_input_tokens,
            output_tokens: agent.output_tokens,
            context: agent.context.clone(),
            context_severity: None,
            registered_at: agent.registered_at,
            sub_agents: Vec::new(),
            compacting: agent.is_compacting(now),
            compaction_count: agent.compaction_count,
            turn_error_label: None,
        })),
    }
}

fn agent_context_window(agent: &AgentState) -> Option<u64> {
    agent.context_window.or_else(|| {
        crate::agents::descriptor_by_kind(agent.kind.as_str())
            .and_then(|descriptor| descriptor.default_context_window)
    })
}
