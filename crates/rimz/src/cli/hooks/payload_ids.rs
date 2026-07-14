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

/// The agent session id from a hook payload (`agent_id`, snake/camel session
/// id, then Cursor's `conversation_id` and Antigravity's `conversationId`).
/// Empty ids are filtered out.
pub(super) fn payload_agent_id(payload: &Value) -> Option<&str> {
    [
        "agent_id",
        "session_id",
        "sessionId",
        "conversation_id",
        "conversationId",
    ]
    .into_iter()
    .find_map(|key| {
        payload
            .get(key)
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
    })
}

/// The sidecar key for local context enrichment. Root sessions file context
/// under the snake/camel session id (or Cursor's `conversation_id` and
/// Antigravity's `conversationId`); child-specific `agent_id`s are lifecycle
/// identities, not Codex rollout files.
pub(super) fn payload_context_agent_id(payload: &Value) -> Option<&str> {
    [
        "session_id",
        "sessionId",
        "agent_id",
        "conversation_id",
        "conversationId",
    ]
    .into_iter()
    .find_map(|key| {
        payload
            .get(key)
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn camel_case_session_id_reaches_both_follow_on_selectors() {
        let payload = json!({"sessionId": "copilot-session"});
        assert_eq!(payload_agent_id(&payload), Some("copilot-session"));
        assert_eq!(payload_context_agent_id(&payload), Some("copilot-session"));
    }

    #[test]
    fn antigravity_conversation_id_reaches_both_follow_on_selectors() {
        let payload = json!({"conversationId": "antigravity-session"});
        assert_eq!(payload_agent_id(&payload), Some("antigravity-session"));
        assert_eq!(
            payload_context_agent_id(&payload),
            Some("antigravity-session")
        );
    }

    #[test]
    fn existing_precedence_remains_stable() {
        let payload = json!({
            "agent_id": "child",
            "session_id": "snake",
            "sessionId": "camel",
            "conversation_id": "cursor"
        });
        assert_eq!(payload_agent_id(&payload), Some("child"));
        assert_eq!(payload_context_agent_id(&payload), Some("snake"));
    }
}
