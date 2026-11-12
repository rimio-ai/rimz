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
/// `addressable` is the cached snapshot's resolver candidate set, with rest certificates.
/// `pending` is the complete pending queue, including delayed and resume-gated messages.
/// `sleeping` contains sessions with a pending one-shot wait in this workspace.
pub(crate) fn team_lifecycle_signals(
    event: &LifecycleEvent,
    member: &AgentState,
    live_cohort: &[&AgentState],
    addressable: &[&AgentState],
    pending: &[MessageRecord],
    sleeping: &BTreeSet<(AgentKind, AgentSessionId)>,
    audit: &crate::store::runtime::AuditRows,
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
    let at_rest = |status| matches!(status, AgentStatus::Idle | AgentStatus::Success);
    let has_pending = |members: &[&AgentState]| {
        pending.iter().any(|message| {
            members
                .iter()
                .any(|agent| message.same_card(agent.card_ref()))
        })
    };
    let others_at_rest = others.iter().all(|agent| at_rest(agent.status));
    let has_wait = |members: &[&AgentState]| {
        members
            .iter()
            .any(|agent| sleeping.contains(&(agent.kind.clone(), agent.agent_id.clone())))
    };
    let mut names = Vec::new();
    if !terminal
        && event.status == AgentStatus::Waiting
        && event.prior_status != Some(AgentStatus::Waiting)
    {
        names.push("team.waiting");
    }
    if matches!(
        event.signal,
        LifecycleSignal::TurnEnded { errored: true, .. }
    ) {
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
            json!(payload_handle(member, &peers, addressable)),
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
                        let handle = payload_handle(agent, &peers, addressable);
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
