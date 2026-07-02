use super::*;

fn compact_event(
    workspace: &WorkspaceId,
    id: u64,
    agent_id: &str,
    agent_name: Option<&str>,
    status: MessageStatus,
    tokens: u64,
) -> EventEnvelope {
    let mut message = MessageRecord::new(
        workspace.clone(),
        &agent("claude", agent_id, AgentStatus::Idle, 0),
        "/compact".to_owned(),
        true,
        DeliveryGate::Any,
    );
    message.message_id = message_id(id);
    message.body = MessageBody::Command;
    message.status = status;
    message.agent_name = agent_name.map(ToOwned::to_owned);
    message.compacted_context_tokens = Some(tokens);
    let method = if status == MessageStatus::Delivered {
        MessageEventMethod::Delivered
    } else {
        MessageEventMethod::Sent
    };
    EventEnvelope::message_event(&message, "session", method, None)
}

#[test]
fn compact_command_events_stamp_the_agent_rollup() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    event_log::append(
        &paths.events_log,
        &lifecycle_at(
            &workspace,
            "claude",
            "SessionStart",
            "sess-a",
            lifecycle::LifecycleSignal::Registered,
        ),
    )
    .unwrap();
    event_log::append(
        &paths.events_log,
        &compact_event(&workspace, 1, "sess-a", None, MessageStatus::Sent, 150_000),
    )
    .unwrap();

    let (_, agents, _) = catch_up_rollup(&paths).unwrap();
    let agent = agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "sess-a")
        .expect("agent");
    assert_eq!(agent.last_compact_command_tokens, Some(150_000));
}

#[test]
fn compact_command_stamp_can_match_by_agent_name_after_session_adoption() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let mut lifecycle = lifecycle_at(
        &workspace,
        "claude",
        "SessionStart",
        "sess-a",
        lifecycle::LifecycleSignal::Registered,
    );
    lifecycle.params = serde_json::value::to_raw_value(&serde_json::json!({
        "event_name": "SessionStart",
        "agent_id": "sess-a",
        "agent_name": "lucid-atlas",
        "signal": { "signal": "registered" },
    }))
    .unwrap();
    event_log::append(&paths.events_log, &lifecycle).unwrap();
    event_log::append(
        &paths.events_log,
        &compact_event(
            &workspace,
            1,
            "provisional",
            Some("lucid-atlas"),
            MessageStatus::Delivered,
            175_000,
        ),
    )
    .unwrap();

    let (_, agents, _) = catch_up_rollup(&paths).unwrap();
    let agent = agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "sess-a")
        .expect("agent");
    assert_eq!(agent.last_compact_command_tokens, Some(175_000));
}

#[test]
fn compact_command_events_stamp_carryover_agents_after_rotation() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    write_carryover(
        &paths.agents_carryover,
        &EventCarryover {
            agents: vec![agent("claude", "sess-a", AgentStatus::Idle, 0)],
            agent_identity: Default::default(),
            resume_outcomes: Vec::new(),
        },
    )
    .unwrap();
    event_log::append(
        &paths.events_log,
        &compact_event(
            &workspace,
            1,
            "sess-a",
            None,
            MessageStatus::Delivered,
            210_000,
        ),
    )
    .unwrap();

    let (_, agents, _) = catch_up_rollup(&paths).unwrap();
    let agent = agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "sess-a")
        .expect("agent");
    assert_eq!(agent.last_compact_command_tokens, Some(210_000));
}
