//! Reply-wait dependency cycles between running agent turns.

use std::collections::HashSet;

use crate::agents::{AgentState, AgentStatus};
use crate::ids::{AgentKind, MessageId};
use crate::message::{MessageRecord, MessageSender, MessageStatus, card_matches};

/// One existing hop of a reply-wait cycle, for diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitCycleHop {
    pub handle: String,
    pub message_id: MessageId,
}

/// Render a multi-hop wait path back to the caller for cycle diagnostics.
pub fn render_chain(cycle: &[WaitCycleHop]) -> Option<String> {
    (cycle.len() > 1).then(|| {
        cycle
            .iter()
            .map(|hop| hop.handle.as_str())
            .chain(std::iter::once("you"))
            .collect::<Vec<_>>()
            .join(" → ")
    })
}

#[derive(Clone, Debug)]
struct WaitEdge {
    sender: usize,
    receiver: usize,
    message_id: MessageId,
}

/// Walk live reply-wait edges from `target` and return the path when the
/// caller's card is reachable. Adding a caller-to-target wait closes that cycle.
pub fn wait_cycle(
    live: &[MessageRecord],
    history: &[MessageRecord],
    agents: &[AgentState],
    self_kind: &AgentKind,
    self_name: &str,
    target: &AgentState,
) -> Option<Vec<WaitCycleHop>> {
    let self_index = agents
        .iter()
        .position(|agent| agent.kind == *self_kind && agent.name.as_deref() == Some(self_name))?;
    let target_index = agents.iter().position(|agent| same_card(agent, target))?;
    let mut edges = Vec::new();

    for record in live.iter().filter(|record| {
        record.reply_wait
            && matches!(
                record.status,
                MessageStatus::Queued | MessageStatus::Claimed | MessageStatus::Sent
            )
    }) {
        let Some(sender) = running_sender_index(record, agents) else {
            continue;
        };
        let Some(receiver) = agents
            .iter()
            .position(|agent| record.same_agent_card(agent))
        else {
            continue;
        };
        edges.push(WaitEdge {
            sender,
            receiver,
            message_id: record.message_id.clone(),
        });
    }

    for (receiver, agent) in agents.iter().enumerate() {
        if agent.status != AgentStatus::Running {
            continue;
        }
        let Some(context) = agent.context.as_ref() else {
            continue;
        };
        for message_id in &context.turn_opened_by {
            let Some(record) = history.iter().find(|record| {
                record.message_id == *message_id
                    && record.reply_wait
                    && record.status == MessageStatus::Delivered
            }) else {
                continue;
            };
            let Some(sender) = running_sender_index(record, agents) else {
                continue;
            };
            edges.push(WaitEdge {
                sender,
                receiver,
                message_id: record.message_id.clone(),
            });
        }
    }

    let peers = agents.iter().collect::<Vec<_>>();
    let mut visited = HashSet::new();
    let mut path = Vec::new();
    walk_edges(
        target_index,
        self_index,
        &edges,
        agents,
        &peers,
        &mut visited,
        &mut path,
    )
    .then_some(path)
}

/// Return the newest message id participating in a cycle, including the
/// caller's own wait record.
pub fn youngest_wait_message(cycle: &[WaitCycleHop], own: &MessageId) -> MessageId {
    cycle
        .iter()
        .map(|hop| &hop.message_id)
        .chain(std::iter::once(own))
        .max_by(|left, right| left.as_str().cmp(right.as_str()))
        .cloned()
        .unwrap_or_else(|| own.clone())
}

fn walk_edges(
    current: usize,
    destination: usize,
    edges: &[WaitEdge],
    agents: &[AgentState],
    peers: &[&AgentState],
    visited: &mut HashSet<usize>,
    path: &mut Vec<WaitCycleHop>,
) -> bool {
    if current == destination {
        return true;
    }
    if !visited.insert(current) {
        return false;
    }
    for edge in edges.iter().filter(|edge| edge.sender == current) {
        path.push(WaitCycleHop {
            handle: crate::harness::target::agent_handle(&agents[current], peers, true),
            message_id: edge.message_id.clone(),
        });
        if walk_edges(
            edge.receiver,
            destination,
            edges,
            agents,
            peers,
            visited,
            path,
        ) {
            return true;
        }
        path.pop();
    }
    false
}

fn running_sender_index(record: &MessageRecord, agents: &[AgentState]) -> Option<usize> {
    let MessageSender::Agent {
        kind,
        name: Some(name),
        ..
    } = &record.sender
    else {
        return None;
    };
    agents.iter().position(|agent| {
        agent.kind == *kind
            && agent.name.as_deref() == Some(name)
            && agent.status == AgentStatus::Running
    })
}

fn same_card(left: &AgentState, right: &AgentState) -> bool {
    card_matches(
        &left.kind,
        &left.agent_id,
        left.name.as_deref(),
        &right.kind,
        &right.agent_id,
        right.name.as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;

    use super::*;
    use crate::agents::AgentStatus;
    use crate::ids::WorkspaceId;
    use crate::message::DeliveryGate;

    #[test]
    fn detects_mutual_queued_cycle() {
        let agents = [
            agent("a", AgentStatus::Running),
            agent("b", AgentStatus::Running),
        ];
        let live = [
            wait_message(&agents[0], &agents[1], 1, MessageStatus::Queued),
            wait_message(&agents[1], &agents[0], 2, MessageStatus::Queued),
        ];

        let cycle = wait_cycle(&live, &[], &agents, &agents[0].kind, "a", &agents[1])
            .expect("mutual wait closes a cycle");

        assert_eq!(cycle, [hop("@b", 2)]);
    }

    #[test]
    fn detects_three_agent_chain() {
        let agents = [
            agent("a", AgentStatus::Running),
            agent("b", AgentStatus::Running),
            agent("c", AgentStatus::Running),
        ];
        let live = [
            wait_message(&agents[1], &agents[2], 2, MessageStatus::Sent),
            wait_message(&agents[2], &agents[0], 3, MessageStatus::Claimed),
        ];

        let cycle = wait_cycle(&live, &[], &agents, &agents[0].kind, "a", &agents[1])
            .expect("chain reaches caller");

        assert_eq!(cycle, [hop("@b", 2), hop("@c", 3)]);
        assert_eq!(render_chain(&cycle).as_deref(), Some("@b → @c → you"));
    }

    #[test]
    fn single_hop_needs_no_chain_rendering() {
        assert_eq!(render_chain(&[hop("@b", 2)]), None);
    }

    #[test]
    fn detects_delivered_wait_that_opened_the_reply_turn() {
        let mut agents = [
            agent("a", AgentStatus::Running),
            agent("b", AgentStatus::Running),
        ];
        let delivered = wait_message(&agents[1], &agents[0], 7, MessageStatus::Delivered);
        agents[0].context = Some(crate::store::agent_context::empty_context(
            "codex",
            Timestamp::now(),
        ));
        agents[0].context.as_mut().expect("context").turn_opened_by =
            vec![delivered.message_id.clone()];

        let cycle = wait_cycle(&[], &[delivered], &agents, &agents[0].kind, "a", &agents[1])
            .expect("delivered wait remains live through its reply turn");

        assert_eq!(cycle, [hop("@b", 7)]);
    }

    #[test]
    fn drops_edge_when_sender_is_not_running() {
        let agents = [
            agent("a", AgentStatus::Running),
            agent("b", AgentStatus::Idle),
        ];
        let live = [wait_message(
            &agents[1],
            &agents[0],
            2,
            MessageStatus::Queued,
        )];

        assert!(wait_cycle(&live, &[], &agents, &agents[0].kind, "a", &agents[1]).is_none());
    }

    #[test]
    fn drops_edge_from_unnamed_sender() {
        let agents = [
            agent("a", AgentStatus::Running),
            agent("b", AgentStatus::Running),
        ];
        let mut message = wait_message(&agents[1], &agents[0], 2, MessageStatus::Queued);
        if let MessageSender::Agent { name, .. } = &mut message.sender {
            *name = None;
        }

        assert!(wait_cycle(&[message], &[], &agents, &agents[0].kind, "a", &agents[1]).is_none());
    }

    #[test]
    fn picks_youngest_wait_message() {
        let cycle = [hop("@b", 2), hop("@c", 9)];
        assert_eq!(youngest_wait_message(&cycle, &message_id(7)), message_id(9));
        assert_eq!(
            youngest_wait_message(&cycle, &message_id(10)),
            message_id(10)
        );
    }

    fn agent(name: &str, status: AgentStatus) -> AgentState {
        let mut agent = AgentState::stub("codex", &format!("sess-{name}"), status);
        agent.name = Some(name.to_owned());
        agent.name_explicit = true;
        agent.kind_ordinal = None;
        agent
    }

    fn wait_message(
        sender: &AgentState,
        receiver: &AgentState,
        id: u64,
        status: MessageStatus,
    ) -> MessageRecord {
        let mut message = MessageRecord::new(
            WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-wait-guard")),
            receiver,
            "reply".to_owned(),
            true,
            DeliveryGate::Done,
        )
        .with_sender(MessageSender::Agent {
            kind: sender.kind.clone(),
            name: sender.name.clone(),
            profile: None,
            role: None,
            channel: None,
        })
        .with_reply_wait(true);
        message.message_id = message_id(id);
        message.status = status;
        message
    }

    fn hop(handle: &str, id: u64) -> WaitCycleHop {
        WaitCycleHop {
            handle: handle.to_owned(),
            message_id: message_id(id),
        }
    }

    fn message_id(id: u64) -> MessageId {
        MessageId::parse(&format!("msg_{id:016x}")).expect("message id")
    }
}
