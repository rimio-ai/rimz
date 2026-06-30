use jiff::Timestamp;

use crate::agents::TurnErrorClass;
use crate::agents::lifecycle::TurnPhase;
use crate::agents::{
    AgentState, AgentStatus, display_turn_error, effective_turn_error_class,
    rate_limit_window_kinds,
};
use crate::ledger::snapshot::row::SidebarRow;

/// Project each agent row's *displayed* status from its raw lifecycle status,
/// liveness, live subagents, turn-error marker, and provider budget windows.
pub(super) fn project_display_status(
    rows: &mut [SidebarRow],
    agents: &[AgentState],
    now: Timestamp,
    stalled_after_secs: u32,
) {
    let rate_limit_kinds = rate_limit_window_kinds(agents, now);
    for row in rows.iter_mut() {
        let row_id = row.id.clone();
        let row_name = row.name.clone();
        let last_activity = row.last_activity;
        let turn_started_at = agents
            .iter()
            .find(|state| {
                state.parent_agent_id.is_none()
                    && state.kind == row_name
                    && state.agent_id == row_id
            })
            .and_then(|state| state.turn_started_at);
        let Some(agent) = row.as_agent_mut() else {
            continue;
        };
        let Some(status) = agent.status else {
            continue;
        };
        // A human-blocked `waiting` ask outranks every derived state.
        if status == AgentStatus::Waiting {
            continue;
        }
        let has_live_child = agent
            .sub_agents
            .iter()
            .any(|child| child.status == AgentStatus::Running);
        let turn_error = display_turn_error(
            status,
            agent.context.as_ref(),
            last_activity,
            turn_started_at,
        );
        let projected = if let Some((error, class)) = turn_error
            .map(|error| (error, effective_turn_error_class(error)))
            .filter(|(_, class)| {
                matches!(
                    class,
                    TurnErrorClass::PausedRateLimit
                        | TurnErrorClass::PausedSpendLimit
                        | TurnErrorClass::PausedOverloaded
                )
            }) {
            if matches!(
                class,
                TurnErrorClass::PausedRateLimit | TurnErrorClass::PausedSpendLimit
            ) && rate_limit_kinds.reset.contains(row_name.as_str())
                && !rate_limit_kinds.spent.contains(row_name.as_str())
            {
                agent.turn_error_label = error.label.clone();
                AgentStatus::Failed
            } else {
                AgentStatus::Paused
            }
        } else if status == AgentStatus::Running && has_live_child {
            AgentStatus::Running
        } else if let Some(error) = turn_error.filter(|error| error.class == TurnErrorClass::Failed)
        {
            agent.turn_error_label = error.label.clone();
            AgentStatus::Failed
        } else if crate::agents::is_turn_complete(status, agent.context.as_ref(), last_activity) {
            // A turn that finished without a `Stop` hook (Codex `/review` review
            // mode) settles to success instead of spinning until the stall
            // window misreads it as failed.
            AgentStatus::Success
        } else {
            let stalled = crate::agents::is_stalled(status, last_activity, now, stalled_after_secs);
            if stalled && rate_limit_kinds.spent.contains(row_name.as_str()) {
                AgentStatus::Paused
            } else if stalled {
                AgentStatus::Failed
            } else {
                status
            }
        };
        agent.status = Some(projected);
        if projected != AgentStatus::Running {
            // Phase is a head on Running — the reduced state's invariant — so a
            // Failed/Paused override drops it.
            agent.phase = TurnPhase::Idle;
        }
    }
}
