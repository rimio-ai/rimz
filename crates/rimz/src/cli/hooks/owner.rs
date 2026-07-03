use super::proctree::walk_to_agent_ancestor;
use super::*;

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

/// Stamp the normalized pane id of the multiplexer pane the hook ran inside.
/// The hook helper is a child of the agent process, which is itself a child of
/// the user's mux pane, so the per-pane env var (`TMUX_PANE` /
/// `ZELLIJ_PANE_ID`) names the right pane unambiguously — the only way to tell
/// two same-kind agents in one worktree apart.
pub(super) fn attach_agent_pane(observation: &mut AgentLifecycleObservation) {
    if observation.pane_id.is_some() {
        return;
    }
    observation.pane_id = rimz::mux::ambient_pane_id();
}

pub(super) fn attach_agent_owner(source: &str, observation: &mut AgentLifecycleObservation) {
    if observation.runtime_owner.is_some() {
        return;
    }
    let Some(agent_id) = observation.agent_id.as_deref().filter(|id| !id.is_empty()) else {
        return;
    };
    let Some(pid) = observation.agent_pid.or_else(|| hook_agent_pid(source)) else {
        return;
    };
    let kind = if rimz::remote_control::pid_is_codex_daemon(pid) {
        RuntimeOwnerKind::Daemon
    } else {
        RuntimeOwnerKind::Agent
    };
    let owner = process_owner(kind, agent_id, pid);
    observation.agent_pid = Some(pid);
    observation.agent_process_start = owner.process_start.clone();
    observation.runtime_owner = Some(owner);
}
