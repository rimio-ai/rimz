use jiff::Timestamp;

use crate::agents::AgentState;
use crate::store::snapshot::row::{AgentCard, RowCard, SidebarRow};

pub(in crate::store::snapshot) fn row_from_agent(agent: &AgentState, now: Timestamp) -> SidebarRow {
    // `SidebarRow.status` is the *displayed* status. It starts as the raw rollup
    // value and is projected in `project_display_status` once the row knows its
    // subagents and its account's rate-limit budget (stall → `failed`,
    // spent-budget → `paused`); native prompts already live on the rollup.
    // The rollup in `snapshot.agents` always keeps the true status.
    SidebarRow {
        id: agent.agent_id.to_string(),
        name: agent.kind.to_string(),
        pane: agent.pane.clone(),
        worktree_path: agent.worktree_path.clone(),
        checkout: agent.checkout.clone(),
        worktree_branch: agent.worktree_branch.clone(),
        channel: agent.channel.clone(),
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        last_activity: agent.last_activity,
        card: RowCard::Agent(std::sync::Arc::new(AgentCard::from_agent(agent, now))),
    }
}
