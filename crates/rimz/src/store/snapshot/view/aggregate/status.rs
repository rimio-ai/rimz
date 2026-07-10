use std::collections::{BTreeMap, BTreeSet};

use jiff::Timestamp;

use crate::agents::lifecycle::TurnPhase;
use crate::agents::{AccountBudget, TurnErrorClass};
use crate::agents::{
    AgentState, AgentStatus, display_turn_error, effective_turn_error_class,
    rate_limit_window_kinds,
};
use crate::ids::{AgentKind, AgentSessionId};
use crate::store::snapshot::row::SidebarRow;

/// Project each agent row's *displayed* status from its raw lifecycle status,
/// liveness, live subagents, turn-error marker, and provider budget windows.
pub(super) fn project_display_status(
    rows: &mut [SidebarRow],
    agents: &[AgentState],
    account_budgets: &BTreeMap<AgentKind, AccountBudget>,
    exhausted_resumes: &BTreeSet<(AgentKind, AgentSessionId)>,
    now: Timestamp,
    stalled_after_secs: u32,
) {
    let rate_limit_kinds = rate_limit_window_kinds(account_budgets, now);
    for row in rows.iter_mut() {
        let row_id = row.id.clone();
        let row_name = row.name.clone();
        let last_activity = row.last_activity;
        let source_agent = agents.iter().find(|state| {
            state.parent_agent_id.is_none() && state.kind == row_name && state.agent_id == row_id
        });
        let turn_started_at = source_agent.and_then(|state| state.turn_started_at);
        let Some(agent) = row.as_agent_mut() else {
            continue;
        };
        let mut status = agent.status;
        // An interruption marker proves Esc cancelled the native prompt.
        // Otherwise a human-blocked prompt outranks every derived state, while
        // a later activity heartbeat means it was answered in the pane.
        if status == AgentStatus::Waiting {
            if crate::agents::is_turn_interrupted(status, agent.context.as_ref(), last_activity) {
                status = AgentStatus::Idle;
            } else if source_agent.is_some_and(AgentState::is_awaiting_input) {
                continue;
            } else {
                agent.status = AgentStatus::Running;
                agent.phase = TurnPhase::Reasoning;
                status = AgentStatus::Running;
            }
        }
        let effective_status = source_agent
            .map(AgentState::effective_status)
            .unwrap_or(status);
        let budget_park = source_agent.and_then(|state| state.budget_park.as_ref());
        if let Some(park) = budget_park {
            agent.turn_error_label = Some(park.label());
        }
        let resume_exhausted = source_agent.is_some_and(|state| {
            exhausted_resumes.contains(&(state.kind.clone(), state.agent_id.clone()))
        });
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
        let projected = if budget_park.is_some() {
            AgentStatus::Paused
        } else if let Some((error, class)) = turn_error
            .map(|error| (error, effective_turn_error_class(error)))
            .filter(|(_, class)| {
                matches!(
                    class,
                    TurnErrorClass::PausedRateLimit
                        | TurnErrorClass::PausedSpendLimit
                        | TurnErrorClass::PausedOverloaded
                )
            })
        {
            let reset_without_budget = matches!(
                class,
                TurnErrorClass::PausedRateLimit | TurnErrorClass::PausedSpendLimit
            ) && rate_limit_kinds.reset.contains(row_name.as_str())
                && !rate_limit_kinds.spent.contains(row_name.as_str());
            if resume_exhausted || reset_without_budget {
                agent.turn_error_label = error.label.clone();
                AgentStatus::Failed
            } else {
                AgentStatus::Paused
            }
        } else if status == AgentStatus::Running && has_live_child {
            AgentStatus::Running
        } else if let Some((error, _class)) = turn_error
            .map(|error| (error, effective_turn_error_class(error)))
            .filter(|(_, class)| matches!(class, TurnErrorClass::Unknown | TurnErrorClass::Failed))
        {
            agent.turn_error_label = error.label.clone();
            AgentStatus::Failed
        } else if crate::agents::is_turn_complete(status, agent.context.as_ref(), last_activity) {
            // A turn that finished without a `Stop` hook (Codex `/review` review
            // mode) settles to success instead of spinning until the stall
            // window misreads it as failed.
            AgentStatus::Success
        } else if crate::agents::is_turn_interrupted(status, agent.context.as_ref(), last_activity)
        {
            // A turn or native ask interrupted without a `Stop` hook is at rest
            // with no result, so settle to idle before the stall window can
            // misread it as failed.
            AgentStatus::Idle
        } else {
            let stalled = crate::agents::is_stalled(status, last_activity, now, stalled_after_secs);
            if stalled && rate_limit_kinds.spent.contains(row_name.as_str()) {
                AgentStatus::Paused
            } else if stalled {
                AgentStatus::Failed
            } else {
                effective_status
            }
        };
        agent.status = projected;
        if projected != AgentStatus::Running {
            // Phase is a head on Running — the reduced state's invariant — so a
            // Failed/Paused override drops it.
            agent.phase = TurnPhase::Idle;
        }
    }
}
