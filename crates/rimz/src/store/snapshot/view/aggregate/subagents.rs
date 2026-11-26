use std::collections::BTreeMap;

use jiff::Timestamp;
use tracing::debug;

use crate::agents::lifecycle::TurnPhase;
use crate::agents::{
    AgentSessionUsage, AgentState, AgentStatus, DelegatedWait, DelegatedWaitStart, ParkDemotion,
    TurnSettleOutcome,
};
use crate::ids::{Lane, PaneId};
use crate::store::snapshot::row::{SidebarRow, SidebarSubAgent, SubAgentTokens};
use crate::utils::time::format_duration_coarse;

use super::super::layout::cmp_start_asc;
use crate::store::session_death::GHOST_SESSION_TTL_SECS;

use super::{AgentKey, AgentProjectionIndex};

pub(super) struct DelegatedChildren {
    pub(super) parks: BTreeMap<AgentKey, (AgentStatus, String)>,
    pub(super) waits: BTreeMap<AgentKey, DelegatedWait>,
}

/// Nest each subagent under its parent root row. A subagent is a reduced
/// `AgentState` carrying `parent_agent_id`; native children are paneless, while
/// launched children retain their pane binding and nest when their parent renders.
pub(super) fn attach_sub_agents_indexed(
    rows: &mut [SidebarRow],
    index: &AgentProjectionIndex<'_>,
    live_pane: impl Fn(&AgentState) -> Option<PaneId>,
    now: Timestamp,
    demotion: &ParkDemotion,
    stalled_after_secs: u32,
) -> DelegatedChildren {
    let mut delegated_parks = BTreeMap::new();
    let mut delegated_waits = BTreeMap::new();
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
        let parent_user_turn_started_at = index
            .root(parent_key)
            .and_then(|parent| parent.user_turn_started_at);
        let all_newest = newest_by_id(children.iter().copied());
        let Some(row_index) = row_by_parent.get(parent_key).copied() else {
            for child in all_newest.values() {
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
        // `row_by_parent` includes only rows whose card is an agent.
        let parent = rows[row_index]
            .as_agent_mut()
            .expect("row index contains only agent rows");
        let mut delegated_cost_usd = None;
        let mut children = all_newest.values().copied().collect::<Vec<_>>();
        children.sort_by(|a, b| {
            cmp_start_asc(a.registered_at, b.registered_at)
                .then_with(|| a.agent_id.cmp(&b.agent_id))
        });
        for child in children {
            if child.is_launched_child() {
                delegated_cost_usd = crate::agents::spending::sum_optional_cost(
                    delegated_cost_usd,
                    child_cost_usd(child),
                );
            }
            let Some(entry) = project_sub_agent_for_parent(
                child,
                live_pane(child),
                parent_user_turn_started_at,
                now,
                demotion,
                stalled_after_secs,
            ) else {
                continue;
            };
            if !entry.prior_turn
                && entry.status == AgentStatus::Waiting
                && !delegated_waits.contains_key(parent_key)
            {
                let handle = entry.petname.clone().unwrap_or_else(|| entry.name.clone());
                let detail = child
                    .open_ask
                    .as_ref()
                    .and_then(|ask| ask.detail.as_deref())
                    .and_then(crate::agents::single_line_description);
                delegated_waits.insert(
                    parent_key.clone(),
                    DelegatedWait {
                        child_id: entry.id.clone(),
                        kind: entry.kind.clone(),
                        handle,
                        pane: entry.pane.clone(),
                        start: delegated_wait_start(child),
                        detail,
                        own_turn_running: false,
                    },
                );
            }
            let park = (!entry.prior_turn)
                .then(|| launched_child_park(child, demotion, now))
                .flatten();
            if let Some((status, label)) = park
                && delegated_parks.get(parent_key).is_none_or(|(current, _)| {
                    *current == AgentStatus::Paused && status == AgentStatus::Failed
                })
            {
                let handle = entry.petname.as_deref().unwrap_or(&entry.name);
                delegated_parks.insert(parent_key.clone(), (status, format!("@{handle}: {label}")));
            }
            parent.sub_agents.push(entry);
        }
        parent.delegated_cost_usd = delegated_cost_usd;
    }
    for agent in rows.iter_mut().filter_map(SidebarRow::as_agent_mut) {
        agent.sub_agents.sort_by(|a, b| {
            cmp_start_asc(a.registered_at, b.registered_at).then_with(|| a.id.cmp(&b.id))
        });
    }
    DelegatedChildren {
        parks: delegated_parks,
        waits: delegated_waits,
    }
}

fn delegated_wait_start(child: &AgentState) -> DelegatedWaitStart {
    if let Some(until) = child.permission_review_until() {
        return DelegatedWaitStart::Fixed(until);
    }
    if let Some(since) = child
        .open_ask
        .as_ref()
        .map(|ask| ask.since)
        .or(child.waiting_since)
    {
        return DelegatedWaitStart::Fixed(since);
    }
    if let Some(settle) = child.context.as_ref().and_then(|ctx| ctx.settle)
        && matches!(
            settle.outcome,
            TurnSettleOutcome::NativeWait | TurnSettleOutcome::PlanProposed
        )
        && settle.at > child.last_activity
    {
        return DelegatedWaitStart::Refreshed(settle.at);
    }
    DelegatedWaitStart::Unknown
}

fn launched_child_park(
    child: &AgentState,
    demotion: &ParkDemotion,
    now: Timestamp,
) -> Option<(AgentStatus, String)> {
    if !child.is_launched_child() || child.ended_at.is_some() {
        return None;
    }
    let (class, error) = child.displayed_turn_error(now)?;
    if !class.pauses_turn() {
        return None;
    }
    let status = child.rowless_status(demotion, now).0;
    let label = error
        .label
        .clone()
        .unwrap_or_else(|| class.words().to_owned());
    Some((status, label))
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

fn child_rest_instant(child: &AgentState) -> Option<Timestamp> {
    if let Some(finished) = child.finished_at {
        return Some(finished);
    }
    // A row that ended while the turn its last prompt opened was still open and
    // has not registered at rest since (the reaper's end that rests a working
    // child at Idle) rests at `ended_at`. A later registration at rest moves
    // `turn_started_at` past `last_prompt_at`, distinguishing a reopened
    // child's subsequent idle close from a reaped turn.
    let ended_mid_prompted_turn = child.last_prompt_at.is_some_and(|prompted| {
        child.turn_ended_at.is_none_or(|ended| ended < prompted)
            && child
                .turn_started_at
                .is_none_or(|started| started <= prompted)
    });
    if ended_mid_prompted_turn && let Some(ended) = child.ended_at {
        return Some(ended);
    }
    match (child.turn_ended_at, child.last_prompt_at) {
        (Some(ended), Some(prompted)) => Some(ended.max(prompted)),
        (Some(ended), None) => Some(ended),
        (None, Some(prompted)) => Some(prompted),
        (None, None) => child.registered_at,
    }
}

fn provider_child_is_current(
    child: &AgentState,
    parent_user_turn_started_at: Option<Timestamp>,
    now: Timestamp,
) -> bool {
    let parent_id = child.parent_agent_id.as_deref().unwrap_or_default();
    let superseded =
        parent_user_turn_started_at.is_some_and(|started| started > child.last_activity);
    if child.status == AgentStatus::Running {
        if superseded {
            // Projection diagnostics stay at debug because persisted state
            // deterministically re-folds once per frame.
            debug!(
                target: "rimz::agent::lifecycle",
                kind = %child.kind,
                parent = parent_id,
                child = %child.agent_id,
                "running subagent superseded by a newer user turn — reaped",
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
        && (parent_user_turn_started_at.is_some()
            || now.duration_since(child.last_activity).as_secs() < GHOST_SESSION_TTL_SECS)
}

fn child_reopenable(child: &AgentState) -> bool {
    child.is_launched_child() && child.resumable()
}

#[cfg(test)]
pub(in crate::store::snapshot) fn attach_sub_agents(
    rows: &mut [SidebarRow],
    agents: &[AgentState],
    now: Timestamp,
) {
    let index = AgentProjectionIndex::new(agents, rows);
    attach_sub_agents_indexed(
        rows,
        &index,
        |_| None,
        now,
        &ParkDemotion::default(),
        crate::config::AttentionConfig::default()
            .stalled_after_secs
            .get(),
    );
}

/// Advance each parent row's *displayed* `last_activity` to its freshest
/// child's, so the stall check, the inactive/archive sinks, ranking, and unread
/// all read a delegating parent as alive while its children work. Display-only
/// — the rollup's own `last_activity` is untouched. The pre-fold clock is
/// recorded on the raised row as `own_last_activity`, which the card's
/// cache-age pin reads: the parent's provider session is quiet for the whole
/// wait even though the delegated work is not.
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
        let Some(freshest) = agent
            .current_sub_agents()
            .map(|child| child.last_activity)
            .max()
        else {
            continue;
        };
        if freshest <= row.last_activity {
            continue;
        }
        let own = std::mem::replace(&mut row.last_activity, freshest);
        // The same card the guard above matched — the mutable borrow only
        // starts here because reading the child clocks held the shared one.
        if let Some(card) = row.as_agent_mut() {
            card.own_last_activity = Some(own);
        }
    }
}

/// Project a child under its parent, applying the launched-child round rule
/// or the provider-native retention rule, and returning `None` when a
/// provider-native prior child is dropped.
fn project_sub_agent_for_parent(
    child: &AgentState,
    pane: Option<PaneId>,
    parent_user_turn_started_at: Option<Timestamp>,
    now: Timestamp,
    demotion: &ParkDemotion,
    stalled_after_secs: u32,
) -> Option<SidebarSubAgent> {
    if !child.is_launched_child() {
        let prior_turn = !provider_child_is_current(child, parent_user_turn_started_at, now);
        if prior_turn
            && !matches!(
                child.display_rest_status(child.status),
                AgentStatus::Success | AgentStatus::Failed
            )
        {
            return None;
        }
        return Some(project_sub_agent(
            child,
            pane,
            now,
            prior_turn,
            demotion,
            stalled_after_secs,
        ));
    }
    let mut entry = project_sub_agent(child, pane, now, false, demotion, stalled_after_secs);
    let live_never_started = child.ended_at.is_none()
        && child.last_prompt_at.is_none()
        && child.turn_ended_at.is_none()
        && entry.finished_at.is_none();
    let at_rest = !matches!(
        entry.status,
        AgentStatus::Running | AgentStatus::Waiting | AgentStatus::Paused | AgentStatus::Sleeping
    ) && entry.phase != TurnPhase::Parked
        && !entry.compacting
        && !entry.stalled
        && !live_never_started;
    entry.prior_turn = at_rest
        && parent_user_turn_started_at
            .is_some_and(|started| entry.rested_at.is_some_and(|rest| started > rest));
    Some(entry)
}

/// A child `AgentState` projected to the compact summary the parent's expanded
/// card paints.
fn project_sub_agent(
    child: &AgentState,
    pane: Option<PaneId>,
    now: Timestamp,
    prior_turn: bool,
    demotion: &ParkDemotion,
    stalled_after_secs: u32,
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
    let silent_secs = child
        .silent_child_for(now, stalled_after_secs)
        .map(|(secs, _)| secs);
    let started_at = child.subagent_started_at.or(child.registered_at);
    let tokens = child_tokens(child);
    let context_window = if matches!(tokens, Some(SubAgentTokens::Window(_))) {
        child
            .context
            .as_ref()
            .and_then(|context| context.tokens.as_ref())
            .and_then(|tokens| tokens.context_window_size)
            .or(child.usage.context_window)
    } else {
        None
    };
    let (status, phase, turn_error_label) = if let Some(secs) = silent_secs {
        (
            AgentStatus::Failed,
            TurnPhase::Idle,
            Some(format!("silent {}", format_duration_coarse(secs as i64))),
        )
    } else if child.is_launched_child() && child.ended_at.is_none() {
        child.rowless_card(demotion, now)
    } else if child.is_launched_child() {
        // Ended launched children keep their recorded rest status and are never
        // lifted to Running or Sleeping by an attached wait.
        (child.status, child.phase, None)
    } else {
        (child.display_rest_status(child.status), child.phase, None)
    };
    let kept = child.kept_verdict.filter(|_| {
        child.is_launched_child()
            && child.status == AgentStatus::Idle
            && status == AgentStatus::Idle
    });
    let (status, finished_at) = kept.map_or((status, child.finished_at), |kept| {
        (kept.status, kept.finished_at)
    });
    let rested_at = if child.is_launched_child() {
        kept.and_then(|kept| kept.finished_at)
            .or_else(|| child_rest_instant(child))
    } else {
        child_rest_instant(child).or(Some(child.last_activity))
    };
    let elapsed_secs = started_at.map(|started| {
        let until = if child.status == AgentStatus::Running
            && !matches!(status, AgentStatus::Success | AgentStatus::Failed)
        {
            now
        } else if child.is_launched_child() && child.status == AgentStatus::Idle {
            rested_at.unwrap_or(child.last_activity)
        } else {
            child.finished_at.unwrap_or(child.last_activity)
        };
        until.duration_since(started).as_secs().max(0)
    });
    SidebarSubAgent {
        id: child.agent_id.to_string(),
        kind: child.kind.clone(),
        pane: child.is_launched_child().then_some(pane).flatten(),
        prior_turn,
        reopenable: child_reopenable(child),
        name,
        petname: child
            .is_launched_child()
            .then(|| child.name.clone())
            .flatten()
            .filter(|name| !name.is_empty()),
        provider_native: child.is_provider_subagent(),
        status,
        stalled: silent_secs.is_some(),
        compacting: child.is_compacting(now) || child.soft_compact_in_flight(),
        phase,
        turn_error_label,
        task: child.task.clone(),
        first_prompt: child.first_prompt.clone(),
        profile: child.profile.clone(),
        lane: child
            .lane
            .as_ref()
            .and_then(Lane::name)
            .map(ToString::to_string),
        model: child.model.clone(),
        fast: child.is_launched_child() && child.fast_mode(),
        effort: child.effort.clone(),
        description: child
            .subagent_description
            .clone()
            .or_else(|| child.description.clone()),
        tokens,
        context_window,
        compaction_count: child.compaction_count,
        cost_usd: child_cost_usd(child),
        elapsed_secs,
        started_at,
        finished_at,
        rested_at,
        last_activity: child.last_activity,
        registered_at: child.registered_at,
    }
}

#[cfg(test)]
pub(in crate::store::snapshot) fn sub_agent_from_state(
    child: &AgentState,
    now: Timestamp,
    prior_turn: bool,
    stalled_after_secs: u32,
) -> SidebarSubAgent {
    project_sub_agent(
        child,
        None,
        now,
        prior_turn,
        &ParkDemotion::default(),
        stalled_after_secs,
    )
}

/// Prefer window occupancy; otherwise use the session spend fold or an
/// explicitly whole-run provider total, never an untyped per-call reading.
fn child_tokens(child: &AgentState) -> Option<SubAgentTokens> {
    if let Some(tokens) = child.context_used_tokens().filter(|tokens| *tokens > 0) {
        return Some(SubAgentTokens::Window(tokens));
    }
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
        .or(child.usage.run_total_tokens.filter(|tokens| *tokens > 0))
        .map(SubAgentTokens::Total)
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
