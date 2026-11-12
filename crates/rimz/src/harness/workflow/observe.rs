//! What an observed agent attempt means, and which agents its stop or detach reaches.
//!
//! The attempt host reads the facts (the leader's supervised run record, a room snapshot, an
//! agent's open ask, a provider window's reset) and these rules project them: the ask that
//! blocks the attempt, the provider, budget, or turn stop that holds it, and its cohort.

use std::collections::BTreeSet;
use std::path::Path;

use jiff::Timestamp;

use super::{Observation, Observed, StopKind, Stopped, leader_turn};
use crate::Store;
use crate::agents::{AgentState, AskDelivery, AskKind, OpenAskDetail, TurnErrorClass};
use crate::ids::EventId;
use crate::store::StoreErr;
use crate::store::run::{RunRecord, StoppedTurnDisposition};
use crate::store::snapshot::SidebarSnapshot;
use crate::store::workflow::{Asked, Asking, ChildRef, Children, Limited, TeamRef};

/// Project the attempt led by `run` over `snapshot`, the room as the host read it with budget
/// parks projected; review suppression and turn errors read `snapshot.now`.
///
/// The leader is scanned first, then its live descendants. `open_ask` is called per agent only
/// until a blocking ask is found, and never for an agent reviewing a permission; its error fails
/// the observation. `reset_at` is called for the first provider-limited agent only. A
/// leader-turn observation of a pinned run also reads the leader's closed turn, the advance's
/// waking frame `woken_by` excluded from the ingress that defers rest.
pub fn observe<E: From<StoreErr>>(
    store: &Store,
    run: &RunRecord,
    snapshot: &SidebarSnapshot,
    observation: Observation,
    woken_by: Option<&EventId>,
    mut open_ask: impl FnMut(&AgentState) -> Result<Option<OpenAskDetail>, E>,
    mut reset_at: impl FnMut(&AgentState) -> Option<Timestamp>,
) -> Result<Observed, E> {
    let mut observed = Observed {
        status: run.status,
        ask: None,
        limited: None,
        stopped: run.stopped_turn.as_ref().map(|stopped| Stopped {
            since: stopped.at,
            kind: match stopped.disposition {
                StoppedTurnDisposition::Failed => StopKind::Error,
                StoppedTurnDisposition::Canceled => StopKind::Interrupt,
            },
        }),
        children: Children::default(),
        cost_usd: run.cost_usd,
        turn: None,
    };
    let Some(root) = leader(snapshot, run) else {
        return Ok(observed);
    };
    let stopped = descendants(&snapshot.agents, root);
    observed.children = children(snapshot, root, &stopped);
    let peers = crate::address::addressable_agents(snapshot);
    for current in std::iter::once(root).chain(stopped) {
        if observed.ask.is_none()
            && !current.is_reviewing_permission(snapshot.now)
            && let Some(ask) = open_ask(current)?
            && ask.delivery == AskDelivery::Blocking
        {
            observed.ask = Some(asking(current, ask, &peers));
        }
        let error = current
            .displayed_turn_error(snapshot.now)
            .map(|(class, error)| {
                let limited = matches!(
                    class,
                    TurnErrorClass::PausedRateLimit | TurnErrorClass::PausedSpendLimit
                );
                if limited && observed.limited.is_none() {
                    observed.limited = Some(Limited {
                        reset_at: reset_at(current),
                    });
                }
                Stopped {
                    since: error.at,
                    kind: if limited {
                        StopKind::Limit
                    } else {
                        StopKind::Error
                    },
                }
            });
        let budget = current.budget_park.as_ref().map(|park| Stopped {
            since: park.at,
            kind: StopKind::Budget,
        });
        for stopped in error.into_iter().chain(budget) {
            if observed.stopped.as_ref().is_none_or(|previous| {
                previous.kind == StopKind::Interrupt || stopped.since < previous.since
            }) {
                observed.stopped = Some(stopped);
            }
        }
    }
    if matches!(observation, Observation::LeaderTurn)
        && let Some(workflow) = &run.workflow
    {
        observed.turn = leader_turn(store, root, &workflow.run, &run.run_id, woken_by)?;
    }
    Ok(observed)
}

/// A blocking ask as the attempt holds it: a reviewing permission counts from the end of its
/// review, every other ask from when it opened.
fn asking(agent: &AgentState, ask: OpenAskDetail, peers: &[&AgentState]) -> Asking {
    let text = ask
        .context
        .into_iter()
        .chain(ask.questions.into_iter().map(|question| question.question))
        .collect::<Vec<_>>()
        .join("\n\n");
    Asking {
        since: agent.permission_review_until().unwrap_or(ask.open.since),
        ask: Asked {
            text,
            kind: match ask.open.kind {
                AskKind::Question => "question",
                AskKind::Permission => "permission",
                AskKind::PlanApproval => "plan_approval",
            }
            .into(),
            handle: crate::address::agent_handle(agent, peers, true),
        },
    }
}

/// The leader row of `run`, by its recorded session.
pub fn leader<'a>(snapshot: &'a SidebarSnapshot, run: &RunRecord) -> Option<&'a AgentState> {
    run.agent_id
        .as_ref()
        .and_then(|id| crate::address::launch_row(snapshot.agents.iter(), &run.kind, id))
}

/// The agents a stop or detach acts on, with the children the attempt reports.
pub struct Cohort<'a> {
    pub agents: Vec<&'a AgentState>,
    pub children: Children,
}

/// What stopping the attempt led by `run` reaches, or `None` when no live row belongs to it.
///
/// Its roots are the live rows the run's workflow attempt pins, and, once a pin is cleared, the
/// detached leader and its launch group. Every live descendant of a root is stopped, as is each
/// root but the leader; `children.live` keeps the rest of their launched fleets and cohorts.
pub fn stop_cohort<'a>(snapshot: &'a SidebarSnapshot, run: &RunRecord) -> Option<Cohort<'a>> {
    let launch_group = snapshot
        .agents
        .iter()
        .find(|agent| run.matches_agent(agent))
        .and_then(|agent| agent.launch_group.as_ref());
    let roots = snapshot
        .agents
        .iter()
        .filter(|agent| {
            let detached_cohort = agent.workflow.is_none()
                && (run.matches_agent(agent)
                    || (launch_group.is_some() && agent.launch_group.as_ref() == launch_group));
            agent.ended_at.is_none()
                && run.workflow.is_some()
                && (agent.workflow == run.workflow || detached_cohort)
        })
        .collect::<Vec<_>>();
    if roots.is_empty() {
        return None;
    }
    let mut stopped = Vec::new();
    let mut result = Children::default();
    for root in &roots {
        let tree = descendants(&snapshot.agents, root);
        result.live.extend(children(snapshot, root, &tree).live);
        stopped.extend(tree);
        if !run.matches_agent(root) {
            stopped.push(*root);
        }
    }
    let mut seen = BTreeSet::new();
    stopped.retain(|agent| seen.insert((&agent.kind, &agent.agent_id)));
    let peers = crate::address::addressable_agents(snapshot);
    result.stopped = stopped
        .iter()
        .map(|agent| child_ref(agent, &peers))
        .collect();
    let mut live = BTreeSet::new();
    result.live.retain(|agent| {
        !result
            .stopped
            .iter()
            .any(|stopped| stopped.kind == agent.kind && stopped.handle == agent.handle)
            && live.insert((agent.kind.clone(), agent.handle.clone()))
    });
    Some(Cohort {
        agents: roots,
        children: result,
    })
}

/// What detaching the attempt led by `run` releases: `agents` are the audit `rows` its workflow
/// attempt pins, and `children.live` their live children with every live pinned row but the
/// leader, read over `snapshot`. A run without a pin releases nothing.
pub fn detach_cohort<'a>(
    snapshot: &SidebarSnapshot,
    rows: impl IntoIterator<Item = &'a AgentState>,
    run: &RunRecord,
) -> Cohort<'a> {
    let Some(pin) = &run.workflow else {
        return Cohort {
            agents: Vec::new(),
            children: Children::default(),
        };
    };
    let pinned = rows
        .into_iter()
        .filter(|agent| agent.workflow.as_ref() == Some(pin))
        .collect::<Vec<_>>();
    let mut result = Children::default();
    let peers = crate::address::addressable_agents(snapshot);
    for root in &pinned {
        result.live.extend(children(snapshot, root, &[]).live);
        if root.ended_at.is_none() && !run.matches_agent(root) {
            result.live.push(child_ref(root, &peers));
        }
    }
    let mut seen = BTreeSet::new();
    result
        .live
        .retain(|child| seen.insert((child.kind.clone(), child.handle.clone())));
    Cohort {
        agents: pinned,
        children: result,
    }
}

/// `snapshot` without the agents an occupancy read excludes: each handle in `excluded` that
/// resolves, with its live launched descendants.
pub fn without_excluded(mut snapshot: SidebarSnapshot, excluded: &[String]) -> SidebarSnapshot {
    if excluded.is_empty() {
        return snapshot;
    }
    let peers = crate::address::addressable_agents(&snapshot);
    let context = crate::address::AddressContext {
        channel: None,
        origin: crate::address::ChannelOrigin::Explicit,
    };
    let excluded = excluded
        .iter()
        .filter_map(|handle| crate::address::resolve_agent(handle, None, &context, &peers).ok())
        .flat_map(|agent| std::iter::once(agent).chain(descendants(&snapshot.agents, agent)))
        .map(|agent| (agent.kind.clone(), agent.agent_id.clone()))
        .collect::<BTreeSet<_>>();
    std::sync::Arc::make_mut(&mut snapshot.agents)
        .retain(|agent| !excluded.contains(&(agent.kind.clone(), agent.agent_id.clone())));
    snapshot
}

/// `root`'s live launched descendants, depth first. An ended child is skipped with its whole
/// subtree, so its live children are never reached.
fn descendants<'a>(agents: &'a [AgentState], root: &AgentState) -> Vec<&'a AgentState> {
    let mut pending = crate::address::launched_children(agents, root);
    let mut found = Vec::new();
    let mut seen = BTreeSet::new();
    while let Some(child) = pending.pop() {
        if child.ended_at.is_some() || !seen.insert((&child.kind, &child.agent_id)) {
            continue;
        }
        pending.extend(crate::address::launched_children(agents, child));
        found.push(child);
    }
    found
}

fn child_ref(agent: &AgentState, peers: &[&AgentState]) -> ChildRef {
    ChildRef {
        handle: crate::address::agent_handle(agent, peers, true),
        kind: agent.kind.clone(),
        team: agent.team.as_ref().map(|name| TeamRef {
            name: name.clone(),
            channel: agent.channel().unwrap_or_else(|| "external".into()),
            stage: agent
                .lane_path()
                .and_then(|path| crate::harness::scratch::board_stage(Path::new(path)))
                .map(|stage| stage.name),
        }),
    }
}

/// `root`'s children: `stopped` as given, and live its launched fleet and the team cohorts it
/// launched, less ended rows, rows that are no launch, and the stopped, each once.
fn children(snapshot: &SidebarSnapshot, root: &AgentState, stopped: &[&AgentState]) -> Children {
    let peers = crate::address::addressable_agents(snapshot);
    let mut live = crate::address::launched_fleet(snapshot.agents.iter(), root);
    for cohort in crate::address::team_cohorts(snapshot.agents.iter()) {
        if cohort.members.iter().any(|member| member.launcher_is(root)) {
            live.extend(cohort.members);
        }
    }
    let mut seen = BTreeSet::new();
    live.retain(|agent| {
        agent.ended_at.is_none()
            && crate::address::is_launch_row(snapshot.agents.iter(), agent)
            && !stopped
                .iter()
                .any(|child| child.kind == agent.kind && child.agent_id == agent.agent_id)
            && seen.insert((&agent.kind, &agent.agent_id))
    });
    Children {
        stopped: stopped
            .iter()
            .map(|child| child_ref(child, &peers))
            .collect(),
        live: live
            .into_iter()
            .map(|child| child_ref(child, &peers))
            .collect(),
    }
}

#[cfg(test)]
mod tests;
