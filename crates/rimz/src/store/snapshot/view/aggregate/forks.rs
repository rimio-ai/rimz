use crate::agents::{AgentState, AgentStatus};
use crate::store::snapshot::row::SidebarRow;

/// Fold later-registered, same-pane root clocks onto the pinned primary row.
/// Display-only — every fork keeps its own durable projection and card fields.
pub(super) fn fold_fork_clocks_onto_primaries(rows: &mut [SidebarRow], agents: &[AgentState]) {
    for row in rows {
        let Some(card) = row.as_agent() else {
            continue;
        };
        if matches!(card.status, AgentStatus::Waiting | AgentStatus::Failed)
            || crate::agents::is_turn_dead(card.status, card.context.as_ref(), row.last_activity)
        {
            continue;
        }
        let (Some(pane), Some(registered_at)) = (row.pane.as_ref(), card.registered_at) else {
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
                && agent
                    .registered_at
                    .is_some_and(|fork_registered_at| fork_registered_at > registered_at)
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
