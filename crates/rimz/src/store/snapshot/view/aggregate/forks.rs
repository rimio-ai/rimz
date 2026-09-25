use crate::agents::{AgentState, AgentStatus};
use crate::store::snapshot::row::SidebarRow;

/// Fold every other same-pane root's activity onto the bound row. Display-only —
/// each conversation keeps its own durable projection and card fields.
pub(super) fn fold_same_pane_activity_onto_bound_row(
    rows: &mut [SidebarRow],
    agents: &[AgentState],
) {
    for row in rows {
        let Some(card) = row.as_agent() else {
            continue;
        };
        if matches!(card.status, AgentStatus::Waiting | AgentStatus::Failed)
            || crate::agents::is_turn_dead(card.status, card.context.as_ref(), row.last_activity)
        {
            continue;
        }
        let Some(pane) = row.pane.as_ref() else {
            continue;
        };

        for fork in agents.iter().filter(|agent| {
            agent.parent_agent_id.is_none()
                && agent.kind.as_str() == row.name
                && agent.agent_id.as_str() != row.id
                && agent
                    .pane
                    .as_ref()
                    .is_some_and(|fork_pane| fork_pane.pane_id == pane.pane_id)
        }) {
            row.last_activity = row.last_activity.max(fork.last_activity);
        }
    }
}
