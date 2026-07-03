use jiff::Timestamp;
use tracing::{debug, warn};

use crate::agents::{AgentState, AgentStatus};
use crate::ledger::snapshot::row::{SidebarRow, SidebarSubAgent};

use super::super::layout::cmp_start_asc;
use super::super::reap::GHOST_SESSION_TTL_SECS;

/// Nest each subagent under its parent root row. A subagent is a reduced
/// `AgentState` carrying `parent_agent_id`; it is paneless, so it built no row
/// of its own (`rows_from_panes` binds only stamped panes).
pub(in crate::ledger::snapshot) fn attach_sub_agents(
    rows: &mut [SidebarRow],
    agents: &[AgentState],
    now: Timestamp,
) {
    let parent_turn_start = |kind: &str, id: &str| -> Option<Timestamp> {
        agents
            .iter()
            .find(|a| a.kind == kind && a.agent_id == id)
            .and_then(|a| a.turn_started_at)
    };
    let idle_secs = |child: &AgentState| now.duration_since(child.last_activity).as_secs();
    for child in agents.iter().filter(|a| a.parent_agent_id.is_some()) {
        let Some(parent_id) = child.parent_agent_id.as_deref() else {
            continue;
        };
        let parent_turn_started_at = parent_turn_start(&child.kind, parent_id);
        let parent_has_turn_boundary = parent_turn_started_at.is_some();
        let superseded =
            parent_turn_started_at.is_some_and(|started| started > child.last_activity);
        let keep = if child.status == AgentStatus::Running {
            if superseded {
                warn!(
                    target: "rimz::agent::lifecycle",
                    kind = %child.kind,
                    parent = parent_id,
                    child = %child.agent_id,
                    "running subagent superseded by a newer parent turn — reaped",
                );
                false
            } else if idle_secs(child) >= GHOST_SESSION_TTL_SECS {
                warn!(
                    target: "rimz::agent::lifecycle",
                    kind = %child.kind,
                    parent = parent_id,
                    child = %child.agent_id,
                    "subagent stuck running with no Stop past the ghost TTL — reaped",
                );
                false
            } else {
                true
            }
        } else {
            // Finished: turn-scoped — kept until the parent's next turn
            // supersedes it. The ghost TTL is the backstop for a parent that
            // never recorded a turn boundary.
            !superseded && (parent_has_turn_boundary || idle_secs(child) < GHOST_SESSION_TTL_SECS)
        };
        if !keep {
            continue;
        }
        let parent = rows
            .iter_mut()
            .filter(|row| row.name == child.kind && row.id == parent_id)
            .find_map(SidebarRow::as_agent_mut);
        if let Some(parent) = parent {
            parent.sub_agents.push(sub_agent_from_state(child, now));
        } else {
            // Transient projection state: the child was observed before its
            // parent's row materialized within this fold. Diagnostic-only — the
            // sidebar re-folds every frame, so a warn! here floods the off-box
            // channel with a per-frame repeat of an expected race. Keep it at
            // debug! for local sidebar diagnosis; it never reaches Sentry.
            debug!(
                target: "rimz::agent::lifecycle",
                kind = %child.kind,
                parent = parent_id,
                child = %child.agent_id,
                "subagent names a parent with no row — orphan, not rendered",
            );
        }
    }
    for agent in rows.iter_mut().filter_map(SidebarRow::as_agent_mut) {
        if agent.sub_agents.is_empty() {
            continue;
        }
        agent
            .sub_agents
            .sort_by(|a, b| a.id.cmp(&b.id).then(b.last_activity.cmp(&a.last_activity)));
        agent.sub_agents.dedup_by(|a, b| a.id == b.id);
        agent.sub_agents.sort_by(|a, b| {
            cmp_start_asc(a.registered_at, b.registered_at).then_with(|| a.id.cmp(&b.id))
        });
    }
}

/// Advance each parent row's *displayed* `last_activity` to its freshest
/// child's. Display-only — the rollup's own `last_activity` is untouched.
pub(super) fn fold_child_activity_onto_parents(rows: &mut [SidebarRow]) {
    for row in rows.iter_mut() {
        let Some(agent) = row.as_agent() else {
            continue;
        };
        if agent.sub_agents.is_empty() {
            continue;
        }
        let status = agent.status;
        if matches!(status, AgentStatus::Waiting | AgentStatus::Failed) {
            continue;
        }
        if crate::agents::is_turn_dead(status, agent.context.as_ref(), row.last_activity) {
            continue;
        }
        if let Some(freshest) = agent
            .sub_agents
            .iter()
            .map(|child| child.last_activity)
            .max()
        {
            row.last_activity = row.last_activity.max(freshest);
        }
    }
}

/// A child `AgentState` projected to the compact summary the parent's expanded
/// card paints.
pub(in crate::ledger::snapshot) fn sub_agent_from_state(
    child: &AgentState,
    now: Timestamp,
) -> SidebarSubAgent {
    let name = child
        .task
        .clone()
        .filter(|task| !task.is_empty())
        .unwrap_or_else(|| {
            warn!(
                target: "rimz::agent::lifecycle",
                kind = %child.kind,
                child = %child.agent_id,
                "subagent has no type label — rendering a degraded placeholder",
            );
            degraded_subagent_label(&child.agent_id)
        });
    let elapsed_secs = child.subagent_started_at.map(|started| {
        let until = if child.status == AgentStatus::Running {
            now
        } else {
            child.last_activity
        };
        until.duration_since(started).as_secs().max(0)
    });
    SidebarSubAgent {
        id: child.agent_id.to_string(),
        name,
        status: child.status,
        phase: child.phase,
        task: child.task.clone(),
        model: child.model.clone(),
        effort: child.effort.clone(),
        description: child.subagent_description.clone(),
        total_tokens: child.total_tokens,
        elapsed_secs,
        started_at: child.subagent_started_at,
        last_activity: child.last_activity,
        registered_at: child.registered_at,
    }
}

/// A placeholder label for a subagent that reported no type — a short id prefix
/// so it reads as a distinct, traceable child rather than the provider kind.
fn degraded_subagent_label(agent_id: &str) -> String {
    let short = agent_id.split('-').next().unwrap_or(agent_id);
    let short = short.get(..8).unwrap_or(short);
    if short.is_empty() {
        "subagent".to_owned()
    } else {
        format!("subagent {short}")
    }
}
