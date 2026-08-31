use crate::agents::{AgentState, AgentStatus};
use crate::store::snapshot::row::SidebarRow;

/// Fold every other same-pane root's clocks onto the bound row. Display-only —
/// each conversation keeps its own durable projection and card fields.
pub(super) fn fold_same_pane_clocks_onto_bound_row(rows: &mut [SidebarRow], agents: &[AgentState]) {
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

        let mut fork_active_secs = None;
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
            if let Some(secs) = fork.estimated_active_secs {
                fork_active_secs = Some(fork_active_secs.unwrap_or(0_u64).saturating_add(secs));
            }
        }

        if let Some(fork_active_secs) = fork_active_secs
            && let Some(card) = row.as_agent_mut()
        {
            card.estimated_active_secs = Some(
                card.estimated_active_secs
                    .unwrap_or(0)
                    .saturating_add(fork_active_secs),
            );
        }
    }
}
