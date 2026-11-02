//! Locked recheck and enqueue, outside the sidebar import graph.

use super::StallNudgeRequest;
use crate::Store;
use crate::agents::{AgentState, AgentStatus};
use crate::config::DONE_STAGE;
use crate::harness::board::{BoardErr, LockedBoard};
use crate::harness::schedule::pending::SessionWaits;
use crate::message::synthetic::SyntheticMessage;
use crate::store::message::{DeliveryGate, HarnessNotice, MessageRecord, MessageSender};
use crate::store::snapshot::find_agent;
use crate::utils::time::format_duration_coarse;
use crate::workspace::ResolvedWorkspace;
use jiff::{Timestamp, tz::TimeZone};

#[derive(Debug, thiserror::Error)]
pub enum NudgeErr {
    #[error(transparent)]
    Board(#[from] crate::harness::board::BoardErr),
    #[error(transparent)]
    Store(#[from] crate::store::StoreErr),
}

pub fn queue(
    request: &StallNudgeRequest,
    workspace: &ResolvedWorkspace,
    store: &Store,
    zone: &TimeZone,
    read_agents: impl FnOnce() -> crate::store::Result<(Vec<AgentState>, Timestamp)>,
) -> Result<Option<MessageRecord>, NudgeErr> {
    let locked = match LockedBoard::open(store, &request.checkout, Some(&request.team)) {
        Ok(board) => board,
        Err(BoardErr::Unlinked(_) | BoardErr::WrongTeam { .. }) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let Some(run) = crate::harness::scratch::parse_board_run(&locked.text, zone) else {
        return Ok(None);
    };
    if run.stage.name == DONE_STAGE
        || run.stage.name != request.stage
        || run.stage_started_at != Some(request.visit_started_at)
    {
        return Ok(None);
    }
    // Read waits before the rows and queue: consuming a wait publishes its wake first.
    let waits = SessionWaits::load(store.paths());
    let (agents, now) = read_agents()?;
    let Some(members) = request
        .members
        .iter()
        .map(|(kind, id)| find_agent(&agents, kind.as_str(), id))
        .collect::<Option<Vec<_>>>()
    else {
        return Ok(None);
    };
    if members.iter().any(|member| {
        member.ended_at.is_some()
            || member.last_activity > request.stalled_since
            || waits.contains(&member.kind, &member.agent_id)
            || !matches!(
                member.display_rest_status(member.effective_status(now)),
                AgentStatus::Idle | AgentStatus::Success
            )
    }) {
        return Ok(None);
    }
    let Some(target) = members
        .iter()
        .copied()
        .find(|member| member.kind == request.target.0 && member.agent_id == request.target.1)
    else {
        return Ok(None);
    };
    let (live, history) = store.list_messages_and_history()?;
    let for_member =
        |message: &MessageRecord| members.iter().any(|member| message.same_agent_card(member));
    if live.iter().any(|message| {
        for_member(message)
            && !message.status.is_terminal()
            && !matches!(
                message.sender,
                MessageSender::Harness {
                    notice: HarnessNotice::CacheKeepalive
                }
            )
    }) || live.iter().chain(&history).any(|message| {
        for_member(message)
            && message.enqueued_at >= request.visit_started_at
            && matches!(
                message.sender,
                MessageSender::Harness {
                    notice: HarnessNotice::TeamStalled
                }
            )
    }) {
        return Ok(None);
    }
    let message = SyntheticMessage {
        agent: target,
        text: text(request, target, &members, now),
        sender: MessageSender::Harness {
            notice: HarnessNotice::TeamStalled,
        },
        gate: DeliveryGate::Done,
        pane_id: target.pane.as_ref().map(|pane| pane.pane_id.clone()),
    }
    .record(workspace);
    store.queue_message(&message, &workspace.session_name)?;
    Ok(Some(message))
}

fn text(
    request: &StallNudgeRequest,
    target: &AgentState,
    members: &[&AgentState],
    now: Timestamp,
) -> String {
    let quiet = format_duration_coarse(now.duration_since(request.stalled_since).as_secs());
    let route = match request.leader_role.as_deref() {
        Some(role) if target.role.as_deref() != Some(role) => {
            let leaders = crate::address::role_holders(members.iter().copied(), role);
            let leader = match leaders.as_slice() {
                [leader] => crate::address::agent_handle(leader, members, false),
                _ => format!("@{role}"),
            };
            format!("message {leader} so the leader can ask the user")
        }
        _ => "ask the user".to_owned(),
    };
    format!(
        "The board is at stage {}, and no teammate is working, waiting, or has a message pending. \
         The team has been quiet for {quiet}. Check the board and its stage file, and continue \
         if you can. If you need the user, {route}. RimZ sends this once per stage visit.",
        request.stage,
    )
}

#[cfg(test)]
mod tests;
