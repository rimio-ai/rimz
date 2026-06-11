use std::collections::BTreeSet;

use jiff::Timestamp;

use crate::agents::lifecycle::TurnPhase;
use crate::agents::{AgentContext, AgentTurnError, RateLimitWindow, TurnErrorClass};
use crate::feed::{AgentState, AgentStatus};
use crate::ids::AgentKind;
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
        let projected = if let Some(error) = turn_error.filter(|error| {
            matches!(
                error.class,
                TurnErrorClass::PausedRateLimit | TurnErrorClass::PausedOverloaded
            )
        }) {
            if error.class == TurnErrorClass::PausedRateLimit
                && rate_limit_kinds.reset.contains(row_name.as_str())
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
        } else {
            let stalled = crate::feed::is_stalled(status, last_activity, now, stalled_after_secs);
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

fn display_turn_error(
    status: AgentStatus,
    context: Option<&AgentContext>,
    last_activity: Timestamp,
    turn_started_at: Option<Timestamp>,
) -> Option<&AgentTurnError> {
    active_turn_error(status, context, last_activity)
        .or_else(|| terminal_turn_error(status, context, turn_started_at))
}

fn active_turn_error(
    status: AgentStatus,
    context: Option<&AgentContext>,
    last_activity: Timestamp,
) -> Option<&AgentTurnError> {
    if status != AgentStatus::Running {
        return None;
    }
    context
        .and_then(|context| context.turn_error.as_ref())
        .filter(|error| error.at > last_activity)
}

fn terminal_turn_error(
    status: AgentStatus,
    context: Option<&AgentContext>,
    turn_started_at: Option<Timestamp>,
) -> Option<&AgentTurnError> {
    if status != AgentStatus::Failed {
        return None;
    }
    let started = turn_started_at?;
    context
        .and_then(|context| context.turn_error.as_ref())
        .filter(|error| error.at >= started)
}

#[derive(Default)]
struct RateLimitKindSummary {
    spent: BTreeSet<AgentKind>,
    reset: BTreeSet<AgentKind>,
}

fn rate_limit_window_kinds(agents: &[AgentState], now: Timestamp) -> RateLimitKindSummary {
    let mut summary = RateLimitKindSummary::default();
    for agent in agents {
        if agent.parent_agent_id.is_some() {
            continue;
        }
        let Some(limits) = agent
            .context
            .as_ref()
            .and_then(|ctx| ctx.rate_limits.as_ref())
        else {
            continue;
        };
        let mut has_spent = false;
        let mut has_reset = false;
        for window in &limits.windows {
            if !window.is_spent() {
                continue;
            }
            if window_spent_unreset(window, now) {
                has_spent = true;
            } else {
                has_reset = true;
            }
        }
        if has_spent {
            summary.spent.insert(agent.kind.clone());
        }
        if has_reset {
            summary.reset.insert(agent.kind.clone());
        }
    }
    summary
}

/// Whether a window is spent and has not yet reset — the budget is gone *now*.
fn window_spent_unreset(window: &RateLimitWindow, now: Timestamp) -> bool {
    window.is_spent() && window.resets_at.is_none_or(|reset| reset > now)
}
