use super::proctree::walk_to_agent_ancestor;
use super::*;
use rimz::agents::HookIngressOwner;
use rimz::pane::RuntimeOwnerKind;
use rimz::store::runtime::process_owner;

pub(super) fn hook_agent_pid(source: &str) -> Option<u32> {
    if let Some(pid) = std::env::var("RIMZ_AGENT_PID")
        .ok()
        .and_then(|raw| raw.parse::<u32>().ok())
        .filter(|pid| *pid > 1)
    {
        return Some(pid);
    }
    walk_to_agent_ancestor(source)
}

/// Stamp the normalized pane id of the multiplexer pane an agent-owned hook ran
/// inside. The per-pane env var (`TMUX_PANE` / `ZELLIJ_PANE_ID`) distinguishes
/// same-kind agents in one worktree. A daemon-owned hook skips this ambient
/// stamp because its environment belongs to the shared daemon.
pub(super) fn attach_agent_pane(observation: &mut AgentLifecycleObservation) {
    attach_agent_pane_with(observation, rimz::mux::ambient_pane_id);
}

fn attach_agent_pane_with(
    observation: &mut AgentLifecycleObservation,
    ambient_pane_id: impl FnOnce() -> Option<PaneId>,
) {
    if observation.pane_id.is_some() {
        return;
    }
    // A daemon-owned hook inherits the daemon's launch pane, not the agent
    // session's pane. Leave it unstamped for focused-pane recovery below.
    if observation
        .runtime_owner
        .as_ref()
        .is_some_and(|owner| owner.kind == RuntimeOwnerKind::Daemon)
    {
        return;
    }
    observation.pane_id = ambient_pane_id();
}

pub(super) fn attach_agent_owner(
    ingress_owner: HookIngressOwner,
    observation: &mut AgentLifecycleObservation,
) {
    if observation.runtime_owner.is_some() {
        return;
    }
    let Some(agent_id) = observation.agent_id.as_deref().filter(|id| !id.is_empty()) else {
        return;
    };
    let Some(pid) = observation.agent_pid.or(ingress_owner.pid) else {
        return;
    };
    let owner = process_owner(ingress_owner.kind, agent_id, pid);
    observation.agent_pid = Some(pid);
    observation.agent_process_start = owner.process_start.clone();
    observation.runtime_owner = Some(owner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::ids::AgentSessionId;

    fn observation(owner_kind: RuntimeOwnerKind) -> AgentLifecycleObservation {
        let mut observation = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("sess-1")),
            LifecycleSignal::Registered,
        );
        observation.runtime_owner = Some(process_owner(owner_kind, "sess-1", std::process::id()));
        observation
    }

    #[test]
    fn pane_stamp_ignores_daemon_ambient_environment() {
        let ambient = PaneId::from_parts(MuxName::Zellij, "terminal_58");
        let mut daemon = observation(RuntimeOwnerKind::Daemon);
        attach_agent_pane_with(&mut daemon, || Some(ambient.clone()));
        assert_eq!(daemon.pane_id, None);

        let mut agent = observation(RuntimeOwnerKind::Agent);
        attach_agent_pane_with(&mut agent, || Some(ambient.clone()));
        assert_eq!(agent.pane_id, Some(ambient.clone()));

        let preset = PaneId::from_parts(MuxName::Tmux, "%57");
        daemon.pane_id = Some(preset.clone());
        attach_agent_pane_with(&mut daemon, || Some(ambient));
        assert_eq!(daemon.pane_id, Some(preset));
    }

    #[test]
    fn explicit_normalized_owner_pid_is_stamped_without_reprobing() {
        let pid = std::process::id();
        let mut observation = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("sess-1")),
            LifecycleSignal::Registered,
        );

        attach_agent_owner(HookIngressOwner::agent(Some(pid)), &mut observation);

        assert_eq!(observation.agent_pid, Some(pid));
        assert_eq!(
            observation.runtime_owner.as_ref().map(|owner| owner.pid),
            Some(pid)
        );
    }
}
