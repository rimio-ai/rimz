use std::collections::{BTreeMap, BTreeSet};

use jiff::Timestamp;

use crate::agents::lifecycle::TurnPhase;
use crate::agents::{
    AgentContext, AgentState, AgentStatus, AgentTurnError, ProviderCapacity, TurnErrorClass,
    display_turn_error, effective_turn_error_class,
};
use crate::ids::{AgentKind, AgentSessionId};
use crate::store::snapshot::row::SidebarRow;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitingResolution {
    NotWaiting,
    Interrupted,
    AwaitingInput,
    Stale,
}

struct SettleFacts<'a> {
    status: AgentStatus,
    waiting_interrupted: bool,
    budget_park_label: Option<String>,
    turn_error: Option<(&'a AgentTurnError, TurnErrorClass)>,
    resume_exhausted: bool,
    has_live_child: bool,
    window_spent: bool,
    window_reset: bool,
    phase: TurnPhase,
    context: Option<&'a AgentContext>,
    last_activity: Timestamp,
    effective_status: AgentStatus,
    now: Timestamp,
    stalled_after_secs: u32,
}

struct Settled {
    status: AgentStatus,
    /// `None` leaves the label untouched; `Some(None)` clears it.
    turn_error_label: Option<Option<String>>,
}

impl Settled {
    fn status(status: AgentStatus) -> Self {
        Self {
            status,
            turn_error_label: None,
        }
    }

    fn with_label(status: AgentStatus, label: Option<String>) -> Self {
        Self {
            status,
            turn_error_label: Some(label),
        }
    }
}

/// Project each agent row's *displayed* status from its raw lifecycle status,
/// liveness, parked phase, live subagents, turn-error marker, and provider
/// budget windows.
pub(super) fn project_display_status(
    rows: &mut [SidebarRow],
    agents: &[AgentState],
    provider_capacities: &BTreeMap<AgentKind, ProviderCapacity>,
    exhausted_resumes: &BTreeSet<(AgentKind, AgentSessionId)>,
    now: Timestamp,
    stalled_after_secs: u32,
) {
    let rate_limit_kinds = rate_limit_window_kinds(provider_capacities, now);
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
        let (status, waiting_interrupted) = match resolve_waiting(
            agent.status,
            agent.context.as_ref(),
            last_activity,
            source_agent,
        ) {
            WaitingResolution::NotWaiting => (agent.status, false),
            WaitingResolution::Interrupted => (AgentStatus::Idle, true),
            WaitingResolution::AwaitingInput => continue,
            WaitingResolution::Stale => {
                agent.phase = TurnPhase::Reasoning;
                (AgentStatus::Running, false)
            }
        };
        if crate::agents::is_native_permission_wait(status, agent.context.as_ref(), last_activity) {
            agent.status = AgentStatus::Waiting;
            agent.phase = TurnPhase::Idle;
            continue;
        }
        // Keep the source agent's own activity clock for this fallback; the
        // row clock above is child-folded and drives the other ladder rungs.
        let effective_status = source_agent
            .map(AgentState::effective_status)
            .unwrap_or(status);
        let budget_park_label = source_agent
            .and_then(|state| state.budget_park.as_ref())
            .map(|park| park.label());
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
        )
        .map(|error| (error, effective_turn_error_class(error)));
        let window_spent = rate_limit_kinds.spent.contains(row_name.as_str());
        let window_reset = rate_limit_kinds.reset.contains(row_name.as_str());
        let Settled {
            status: projected,
            turn_error_label,
        } = settle(SettleFacts {
            status,
            waiting_interrupted,
            budget_park_label,
            turn_error,
            resume_exhausted,
            has_live_child,
            window_spent,
            window_reset,
            phase: agent.phase,
            context: agent.context.as_ref(),
            last_activity,
            effective_status,
            now,
            stalled_after_secs,
        });
        if let Some(label) = turn_error_label {
            agent.turn_error_label = label;
        }
        agent.status = projected;
        if projected != AgentStatus::Running {
            // Phase is a head on Running — the reduced state's invariant — so a
            // projection to a resting or attention status drops it.
            agent.phase = TurnPhase::Idle;
        }
    }
}

#[derive(Default)]
struct RateLimitKindSummary {
    spent: BTreeSet<AgentKind>,
    reset: BTreeSet<AgentKind>,
}

fn rate_limit_window_kinds(
    provider_capacities: &BTreeMap<AgentKind, ProviderCapacity>,
    now: Timestamp,
) -> RateLimitKindSummary {
    let mut summary = RateLimitKindSummary::default();
    for (kind, capacity) in provider_capacities {
        let mut has_spent = false;
        let mut has_reset = false;
        for window in capacity.projected_windows(now) {
            if !window.is_spent() {
                continue;
            }
            if window.resets_at.is_none_or(|reset| reset > now) {
                has_spent = true;
            } else {
                has_reset = true;
            }
        }
        if has_spent {
            summary.spent.insert(kind.clone());
        }
        if has_reset {
            summary.reset.insert(kind.clone());
        }
    }
    summary
}

fn resolve_waiting(
    status: AgentStatus,
    context: Option<&AgentContext>,
    last_activity: Timestamp,
    source_agent: Option<&AgentState>,
) -> WaitingResolution {
    if status != AgentStatus::Waiting {
        return WaitingResolution::NotWaiting;
    }
    // An interruption marker proves Esc cancelled the native prompt.
    // Otherwise a human-blocked prompt outranks every derived state, while
    // a later activity heartbeat means it was answered in the pane.
    if crate::agents::is_turn_interrupted(status, context, last_activity) {
        WaitingResolution::Interrupted
    } else if source_agent.is_some_and(AgentState::is_awaiting_input) {
        WaitingResolution::AwaitingInput
    } else {
        WaitingResolution::Stale
    }
}

fn settle(facts: SettleFacts<'_>) -> Settled {
    let SettleFacts {
        status,
        waiting_interrupted,
        budget_park_label,
        turn_error,
        resume_exhausted,
        has_live_child,
        window_spent,
        window_reset,
        phase,
        context,
        last_activity,
        effective_status,
        now,
        stalled_after_secs,
    } = facts;

    if !waiting_interrupted && let Some(label) = budget_park_label {
        return Settled::with_label(AgentStatus::Paused, Some(label));
    }
    if let Some((error, class)) = turn_error.filter(|(_, class)| class.pauses_turn()) {
        let reset_without_budget = class.is_limit() && window_reset && !window_spent;
        if resume_exhausted || reset_without_budget {
            return Settled::with_label(AgentStatus::Failed, error.label.clone());
        }
        return Settled::status(AgentStatus::Paused);
    }
    if let Some((error, _class)) = turn_error
        .filter(|(_, class)| matches!(class, TurnErrorClass::Unknown | TurnErrorClass::Failed))
    {
        return Settled::with_label(AgentStatus::Failed, error.label.clone());
    }
    if has_live_child
        && matches!(
            status,
            AgentStatus::Idle | AgentStatus::Success | AgentStatus::Running
        )
    {
        return Settled::status(AgentStatus::Running);
    }
    if crate::agents::is_turn_complete(status, context, last_activity) {
        // A turn that finished without a `Stop` hook (Codex `/review` review
        // mode) settles to success instead of spinning until the stall
        // window misreads it as failed.
        return Settled::status(AgentStatus::Success);
    }
    if crate::agents::is_turn_interrupted(status, context, last_activity) {
        // A turn or native ask interrupted without a `Stop` hook is at rest
        // with no result, so settle to idle before the stall window can
        // misread it as failed.
        return Settled::status(AgentStatus::Idle);
    }

    let stalled = crate::agents::is_stalled(status, last_activity, now, stalled_after_secs);
    if stalled && phase == TurnPhase::Parked {
        // A clean end parked on background work that has gone quiet
        // past the stall window: the turn's success verdict was
        // earned, the chore is just still humming. Reawakened activity
        // re-runs the row.
        Settled::status(AgentStatus::Success)
    } else if stalled && window_spent {
        Settled::status(AgentStatus::Paused)
    } else if stalled {
        Settled::status(AgentStatus::Failed)
    } else {
        Settled::status(effective_status)
    }
}
