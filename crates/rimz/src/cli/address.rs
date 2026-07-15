use rimz::agents::{AgentCardRef, AgentState};
use rimz::ids::{AgentKind, AgentSessionId};

pub(in crate::cli) fn message_target(
    address: Option<&str>,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
    agent_name: Option<&str>,
    channel: Option<&str>,
    agents: &[&AgentState],
) -> String {
    if let Some(address) = address {
        return address.to_owned();
    }
    agents
        .iter()
        .copied()
        .find(|agent| AgentCardRef::new(kind, agent_id, agent_name).matches(agent.card_ref()))
        .map(|agent| rimz::harness::target::agent_handle(agent, agents, true))
        .or_else(|| {
            let agent_name = agent_name.filter(|value| !value.is_empty())?;
            let mut rendered = format!("@{agent_name}");
            if let Some(channel) = channel.filter(|value| !value.is_empty()) {
                rendered.push('#');
                rendered.push_str(channel);
            }
            Some(rendered)
        })
        .unwrap_or_else(|| format!("{kind}:{agent_id}"))
}
