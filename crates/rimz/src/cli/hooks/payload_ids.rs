use super::*;

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

/// The agent session id from a hook payload (`agent_id`, `session_id`, then
/// Cursor's `conversation_id`). Empty ids are filtered out.
pub(super) fn payload_agent_id(payload: &Value) -> Option<&str> {
    ["agent_id", "session_id", "conversation_id"]
        .into_iter()
        .find_map(|key| {
            payload
                .get(key)
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
        })
}

/// The sidecar key for local context enrichment. Root sessions file context
/// under `session_id` (or Cursor's `conversation_id`); child-specific
/// `agent_id`s are lifecycle identities, not Codex rollout files.
pub(super) fn payload_context_agent_id(payload: &Value) -> Option<&str> {
    ["session_id", "agent_id", "conversation_id"]
        .into_iter()
        .find_map(|key| {
            payload
                .get(key)
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
        })
}
