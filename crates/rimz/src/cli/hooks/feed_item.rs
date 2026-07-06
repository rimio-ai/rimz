use super::owner::hook_agent_pid;
use super::*;

pub(super) fn build_item(
    workspace: &ResolvedWorkspace,
    surface: Surface,
    feed_kind: FeedKind,
    agent: &dyn AgentAdapter,
    payload: Value,
) -> FeedItem {
    let mut item = FeedItem::new(
        workspace.workspace_id.clone(),
        surface,
        feed_kind,
        format!("{} needs attention", agent.descriptor().kind),
        agent.descriptor().kind,
        "agent-hook",
    );
    item.payload = payload;
    item.runtime_owner = agent_runtime_owner(agent.descriptor().kind, &item.payload);
    item.worktree_path = item
        .payload
        .get("worktree_path")
        .or_else(|| item.payload.get("cwd"))
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| Some(workspace.worktree_root.display().to_string()));
    item.worktree_branch = item
        .payload
        .get("worktree_branch")
        .and_then(Value::as_str)
        .filter(|branch| !branch.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| workspace.worktree_branch.clone());
    item
}

/// Spawn a hook-triggered `rimz` helper detached, with all stdio nulled (the
/// fresh-stdio invariant for hook helper children). The hook drops the child
/// into the shared reaper, so it returns before the helper runs and never adds
/// latency to the agent's turn. Best-effort: a spawn failure is logged and
/// ignored; durable queue work remains pending for a later transition.
pub(super) fn spawn_refresh_detached(spawn: &rimz::agents::RefreshSpawn) {
    let exe = rimz::proc::rimz_exe();
    let mut cmd = Command::new(exe);
    cmd.args(&spawn.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Err(err) = rimz::child_process::spawn_detached_reaped(&mut cmd, "adapter-refresh") {
        warn!(error = %err, "lifecycle: failed to spawn the adapter refresh helper");
    }
}

/// The agent session id from a hook payload, read in the same order as
/// [`rimz::feed::FeedItem::agent_session_id`] (`agent_id`, then `session_id`)
/// so a session resolves to the same key whether read from a lifecycle event
/// or from a stored ask. Empty ids are filtered out.
pub(super) fn payload_agent_id(payload: &Value) -> Option<&str> {
    ["agent_id", "session_id"].into_iter().find_map(|key| {
        payload
            .get(key)
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
    })
}

/// The sidecar key for local context enrichment. Root sessions file context
/// under `session_id`; child-specific `agent_id`s are lifecycle identities, not
/// Codex rollout files.
pub(super) fn payload_context_agent_id(payload: &Value) -> Option<&str> {
    ["session_id", "agent_id"].into_iter().find_map(|key| {
        payload
            .get(key)
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
    })
}

fn agent_runtime_owner(source: &str, payload: &Value) -> Option<rimz::RuntimeOwner> {
    let subject_id = payload_agent_id(payload)?;
    let pid = hook_agent_pid(source)?;
    let kind = if rimz::agents::codex::pid_is_codex_daemon(pid) {
        RuntimeOwnerKind::Daemon
    } else {
        RuntimeOwnerKind::Agent
    };
    Some(process_owner(kind, subject_id, pid))
}
