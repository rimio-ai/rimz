//! Pure cohort edges derived from an appended member lifecycle event.

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use crate::address::{AddressContext, ChannelOrigin, TargetErr, agent_handle, resolve_agents};
use crate::agents::{
    AgentState, AgentStatus, LifecycleEvent, LifecycleSignal, LifecycleTransition,
};
use crate::harness::signal::Signal;
use crate::ids::{AgentKind, AgentSessionId};
use crate::store::event::SignalSource;
use crate::store::message::MessageRecord;

/// Derive team signals from the transitioning audit row and its live cohort members.
///
/// Whether the edge can name a signal at all is decided first, from the event and `member`
/// alone, so the room-wide inputs are read only past that: `cohort` yields the live cohort
/// and the targets of pending one-shot waits in this workspace, and `addressable` the cached
/// snapshot's resolver candidate set, with rest certificates, once a name was derived.
/// `pending` is the complete pending queue, including delayed and resume-gated messages.
pub(crate) fn team_lifecycle_signals<'c, 'v>(
    event: &LifecycleEvent,
    member: &AgentState,
    pending: &[MessageRecord],
    audit: &crate::store::runtime::AuditRows,
    cohort: impl FnOnce() -> (Vec<&'c AgentState>, BTreeSet<(AgentKind, AgentSessionId)>),
    addressable: impl FnOnce() -> Vec<&'v AgentState>,
) -> Vec<Signal> {
    let Some(team) = member.team.as_deref().filter(|team| !team.is_empty()) else {
        return Vec::new();
    };
    if member.is_provider_subagent() {
        return Vec::new();
    }
    let root_terminal = matches!(event.signal, LifecycleSignal::Ended | LifecycleSignal::Lost);
    if !root_terminal && matches!(event.transition, LifecycleTransition::Ignored { .. }) {
        return Vec::new();
    }
    let terminal = root_terminal || matches!(event.signal, LifecycleSignal::SubagentStopped { .. });
    let waiting = !terminal
        && event.status == AgentStatus::Waiting
        && event.prior_status != Some(AgentStatus::Waiting);
    let failed = matches!(
        event.signal,
        LifecycleSignal::TurnEnded { errored: true, .. }
    );
    // A live member's cohort is the same set before and after its edge, so
    // `team.idle` needs the member itself to come to rest. A tool use, a turn
    // start, or a rest kept names nothing and reads no room-wide input.
    let resting = at_rest(event.status) && !event.prior_status.is_some_and(at_rest);
    if !(terminal || waiting || failed || resting) {
        return Vec::new();
    }
    let (live_cohort, sleeping) = cohort();
    let is_member =
        |agent: &&AgentState| agent.kind == member.kind && agent.agent_id == member.agent_id;
    if !terminal && !live_cohort.iter().any(is_member) {
        return Vec::new();
    }
    let others: Vec<_> = live_cohort
        .iter()
        .copied()
        .filter(|agent| !is_member(agent) && agent.ended_at.is_none())
        .collect();
    let mut live = others.clone();
    if !terminal {
        live.push(member);
    }
    let has_pending = |members: &[&AgentState]| {
        pending.iter().any(|message| {
            members
                .iter()
                .any(|agent| message.same_card(agent.card_ref()))
        })
    };
    let others_at_rest = others.iter().all(|agent| at_rest(agent.status));
    let has_wait = |members: &[&AgentState]| {
        members.iter().any(|agent| {
            sleeping.iter().any(|(kind, session)| {
                crate::harness::schedule::pending::wait_for(kind, session, agent)
            })
        })
    };
    let mut names = Vec::new();
    if waiting {
        names.push("team.waiting");
    }
    if failed {
        names.push("team.failed");
    }
    let post_idle = !live.is_empty()
        && others_at_rest
        && (terminal || at_rest(event.status))
        && !has_pending(&live)
        && !has_wait(&live);
    let mut members = others;
    members.push(member);
    let prior_idle = others_at_rest
        && event.prior_status.is_some_and(at_rest)
        && !has_pending(&members)
        && !has_wait(&members);
    if post_idle && !prior_idle {
        names.push("team.idle");
    }
    if terminal && live.is_empty() {
        names.push("team.ended");
    }
    if names.is_empty() {
        return Vec::new();
    }
    let addressable = addressable();
    let marked = members
        .iter()
        .map(|agent| audit.marked_copy(agent))
        .collect::<Vec<_>>();
    let member = &marked[members.len() - 1];
    let members = marked.iter().collect::<Vec<_>>();
    let mut peers = addressable.to_vec();
    peers.extend(members.iter().copied().filter(|agent| {
        !addressable
            .iter()
            .any(|peer| peer.kind == agent.kind && peer.agent_id == agent.agent_id)
    }));
    let Some(instance) = member.team_instance() else {
        return Vec::new();
    };
    let payload = Map::from_iter([
        ("team".to_owned(), json!(team)),
        ("instance".to_owned(), json!(instance.to_string())),
        (
            "member".to_owned(),
            json!(payload_handle(member, &peers, &addressable)),
        ),
        (
            "members".to_owned(),
            Value::Array(
                members
                    .iter()
                    .map(|agent| {
                        let status = if is_member(agent) {
                            event.status
                        } else {
                            agent.status
                        };
                        let handle = payload_handle(agent, &peers, &addressable);
                        json!({"handle": handle, "status": status})
                    })
                    .collect(),
            ),
        ),
    ]);
    names
        .into_iter()
        .map(|name| Signal {
            // Only the four valid static signal names above reach this parser.
            name: name.parse().expect("static team signal name is valid"),
            payload: payload.clone(),
            source: SignalSource::Lifecycle,
            watch: None,
        })
        .collect()
}

fn at_rest(status: AgentStatus) -> bool {
    matches!(status, AgentStatus::Idle | AgentStatus::Success)
}

fn payload_handle(
    agent: &AgentState,
    peers: &[&AgentState],
    addressable: &[&AgentState],
) -> String {
    let agent = addressable
        .iter()
        .copied()
        .find(|peer| peer.kind == agent.kind && peer.agent_id == agent.agent_id)
        .unwrap_or(agent);
    let handle = agent_handle(agent, peers, true);
    let context = AddressContext {
        channel: None,
        origin: ChannelOrigin::Explicit,
    };
    let conflict = match resolve_agents(&handle, None, &context, addressable).as_deref() {
        Ok([one]) => one.kind != agent.kind || one.agent_id != agent.agent_id,
        Ok(_) | Err(TargetErr::Ambiguous { .. }) => true,
        Err(_) => false,
    };
    if !conflict {
        return handle;
    }
    match crate::address::lane_label(agent) {
        Some(channel) => format!("@{}#{channel}", agent.agent_id),
        None => format!("@{}", agent.agent_id),
    }
}

#[cfg(test)]
mod tests;
