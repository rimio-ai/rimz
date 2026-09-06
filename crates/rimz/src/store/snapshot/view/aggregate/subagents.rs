use std::collections::BTreeMap;

use jiff::Timestamp;
use tracing::debug;

use crate::agents::{AgentSessionUsage, AgentState, AgentStatus};
use crate::store::snapshot::row::{SidebarRow, SidebarSubAgent};

use super::super::layout::cmp_start_asc;
use crate::store::session_death::GHOST_SESSION_TTL_SECS;

use super::{AgentKey, AgentProjectionIndex};

/// Nest each subagent under its parent root row. A subagent is a reduced
/// `AgentState` carrying `parent_agent_id`; native children are paneless, while
/// launched children retain their pane binding and nest when their parent renders.
pub(super) fn attach_sub_agents_indexed(
    rows: &mut [SidebarRow],
    index: &AgentProjectionIndex<'_>,
    now: Timestamp,
) {
    let row_by_parent = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.as_agent().is_some())
        .map(|(row_index, row)| {
            (
                (
                    crate::ids::AgentKind::new_unchecked(row.name.clone()),
                    crate::ids::AgentSessionId::from(row.id.as_str()),
                ),
                row_index,
            )
        })
        .collect::<BTreeMap<AgentKey, usize>>();

    for (parent_key, children) in index.children() {
        let parent_turn_started_at = index
            .root(parent_key)
            .and_then(|parent| parent.turn_started_at);
        let visible = children
            .iter()
            .copied()
            .filter(|child| child_is_visible(child, parent_turn_started_at, now))
            .collect::<Vec<_>>();
        let Some(row_index) = row_by_parent.get(parent_key).copied() else {
            for child in visible {
                let child_key = (child.kind.clone(), child.agent_id.clone());
                if child.is_launched_child() && row_by_parent.contains_key(&child_key) {
                    continue;
                }
                let parent_id = child.parent_agent_id.as_deref().unwrap_or_default();
                // Transient projection state: the child was observed before its
                // parent's row materialized within this fold. Keep it locally
                // diagnosable without escalating an expected race.
                debug!(
                    target: "rimz::agent::lifecycle",
                    kind = %child.kind,
                    parent = parent_id,
                    child = %child.agent_id,
                    "subagent names a parent with no row — orphan, not rendered",
                );
            }
            continue;
        };
        let all_newest = newest_by_id(children.iter().copied());
        let visible_newest = newest_by_id(visible);
        // `row_by_parent` includes only rows whose card is an agent.
        let parent = rows[row_index]
            .as_agent_mut()
            .expect("row index contains only agent rows");
        let mut delegated_cost_usd = None;
        let mut sub_agent_cost_usd = None;
        for child in all_newest.values() {
            let cost_usd = child_cost_usd(child);
            sub_agent_cost_usd =
                crate::agents::spending::sum_optional_cost(sub_agent_cost_usd, cost_usd);
            if child.is_launched_child() {
                delegated_cost_usd =
                    crate::agents::spending::sum_optional_cost(delegated_cost_usd, cost_usd);
            }
        }
        parent.delegated_cost_usd = delegated_cost_usd;
        parent.sub_agent_count = u32::try_from(all_newest.len()).unwrap_or(u32::MAX);
        parent.sub_agent_cost_usd = sub_agent_cost_usd;
        parent.sub_agents.extend(
            visible_newest
                .into_values()
                .map(|child| sub_agent_from_state(child, now)),
        );
    }
    for agent in rows.iter_mut().filter_map(SidebarRow::as_agent_mut) {
        agent.sub_agents.sort_by(|a, b| {
            cmp_start_asc(a.registered_at, b.registered_at).then_with(|| a.id.cmp(&b.id))
        });
    }
}

fn newest_by_id<'a>(
    children: impl IntoIterator<Item = &'a AgentState>,
) -> BTreeMap<&'a str, &'a AgentState> {
    let mut newest = BTreeMap::<&str, &AgentState>::new();
    for child in children {
        newest
            .entry(child.agent_id.as_str())
            .and_modify(|current| {
                if child.last_activity > current.last_activity {
                    *current = child;
                }
            })
            .or_insert(child);
    }
    newest
}

fn child_is_visible(
    child: &AgentState,
    parent_turn_started_at: Option<Timestamp>,
    now: Timestamp,
) -> bool {
    if child.is_launched_child() && child.ended_at.is_none() {
        return true;
    }
    let parent_id = child.parent_agent_id.as_deref().unwrap_or_default();
    let superseded = parent_turn_started_at.is_some_and(|started| started > child.last_activity);
    if child.status == AgentStatus::Running {
        if superseded {
            // Projection diagnostics stay at debug because persisted state
            // deterministically re-folds once per frame.
            debug!(
                target: "rimz::agent::lifecycle",
                kind = %child.kind,
                parent = parent_id,
                child = %child.agent_id,
                "running subagent superseded by a newer parent turn — reaped",
            );
            return false;
        }
        if now.duration_since(child.last_activity).as_secs() >= GHOST_SESSION_TTL_SECS {
            debug!(
                target: "rimz::agent::lifecycle",
                kind = %child.kind,
                parent = parent_id,
                child = %child.agent_id,
                "subagent stuck running with no Stop past the ghost TTL — reaped",
            );
            return false;
        }
        return true;
    }
    !superseded
        && (parent_turn_started_at.is_some()
            || now.duration_since(child.last_activity).as_secs() < GHOST_SESSION_TTL_SECS)
}

#[cfg(test)]
pub(in crate::store::snapshot) fn attach_sub_agents(
    rows: &mut [SidebarRow],
    agents: &[AgentState],
    now: Timestamp,
) {
    let index = AgentProjectionIndex::new(agents, rows);
    attach_sub_agents_indexed(rows, &index, now);
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
pub(in crate::store::snapshot) fn sub_agent_from_state(
    child: &AgentState,
    now: Timestamp,
) -> SidebarSubAgent {
    let name = if child.is_launched_child() {
        child
            .profile
            .clone()
            .unwrap_or_else(|| child.kind.as_str().to_owned())
    } else {
        child
            .name_explicit
            .then(|| child.name.clone())
            .flatten()
            .filter(|name| !name.is_empty())
            .or_else(|| child.task.clone().filter(|task| !task.is_empty()))
            .unwrap_or_else(|| {
                debug!(
                    target: "rimz::agent::lifecycle",
                    kind = %child.kind,
                    child = %child.agent_id,
                    "subagent has no type label — rendering a degraded placeholder",
                );
                degraded_subagent_label(&child.agent_id)
            })
    };
    let started_at = child.subagent_started_at.or(child.registered_at);
    let elapsed_secs = started_at.map(|started| {
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
        petname: child
            .is_launched_child()
            .then(|| child.name.clone())
            .flatten()
            .filter(|name| !name.is_empty()),
        provider_native: child.is_provider_subagent(),
        status: child.status,
        phase: child.phase,
        task: child.task.clone(),
        profile: child.profile.clone(),
        model: child.model.clone(),
        effort: child.effort.clone(),
        description: child
            .subagent_description
            .clone()
            .or_else(|| child.description.clone()),
        total_tokens: child_total_tokens(child),
        cost_usd: child_cost_usd(child),
        elapsed_secs,
        started_at,
        last_activity: child.last_activity,
        registered_at: child.registered_at,
    }
}

/// A pane-backed child's spend fold carries cumulative session counters; the
/// hook-reported total fills in before that fold lands or for providers that do
/// not produce one.
fn child_total_tokens(child: &AgentState) -> Option<u64> {
    child
        .context
        .as_ref()
        .and_then(|context| context.tokens.as_ref())
        .and_then(|tokens| tokens.session_usage.as_ref())
        .map(AgentSessionUsage::displayed_total_tokens)
        // Keep this type explicit: rust-analyzer 1.97.1's SCIP inlay-hints
        // pass panics in hir-ty/src/infer/callee.rs when inferring this
        // `Option::filter` closure through the iterator-style chain.
        .filter(|total: &u64| *total > 0)
        .or(child.usage.total_tokens)
}

fn child_cost_usd(child: &AgentState) -> Option<f64> {
    child.subagent_cost_usd.or_else(|| {
        child
            .is_launched_child()
            .then(|| {
                child
                    .context
                    .as_ref()
                    .and_then(|context| context.cost.as_ref())
                    .and_then(|cost| cost.total_cost_usd)
            })
            .flatten()
    })
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
