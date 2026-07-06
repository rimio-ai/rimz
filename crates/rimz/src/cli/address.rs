use rimz::agents::AgentState;
use rimz::ids::{AgentKind, AgentSessionId};
use rimz::message::card_matches;

pub(in crate::cli) fn message_target(
    address: Option<&str>,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
    agent_name: Option<&str>,
    agents: &[&AgentState],
) -> String {
    if let Some(address) = address {
        return address.to_owned();
    }
    agents
        .iter()
        .copied()
        .find(|agent| {
            card_matches(
                kind,
                agent_id,
                agent_name,
                &agent.kind,
                &agent.agent_id,
                agent.name.as_deref(),
            )
        })
        .map(|agent| rimz::harness::target::agent_handle(agent, agents, true))
        .unwrap_or_else(|| format!("{kind}:{agent_id}"))
}
