use super::*;

fn compact_event(
    workspace: &WorkspaceId,
    id: u64,
    agent_id: &str,
    agent_name: Option<&str>,
    status: MessageStatus,
    tokens: Option<u64>,
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
    message.compacted_context_tokens = tokens;
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
        &compact_event(
            &workspace,
            1,
            "sess-a",
            None,
            MessageStatus::Sent,
            Some(150_000),
        ),
    )
    .unwrap();

    let (_, agents, _) = catch_up_rollup(&paths).unwrap();
    let agent = agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "sess-a")
        .expect("agent");
    assert_eq!(agent.last_compact_command_tokens, Some(150_000));
    assert!(agent.compacted_awaiting_prompt.is_some());
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
            MessageStatus::Sent,
            Some(175_000),
        ),
    )
    .unwrap();

    let (_, agents, _) = catch_up_rollup(&paths).unwrap();
    let agent = agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "sess-a")
        .expect("agent");
    assert_eq!(agent.last_compact_command_tokens, Some(175_000));
    assert!(agent.compacted_awaiting_prompt.is_some());
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
            MessageStatus::Sent,
            Some(210_000),
        ),
    )
    .unwrap();

    let (_, agents, _) = catch_up_rollup(&paths).unwrap();
    let agent = agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "sess-a")
        .expect("agent");
    assert_eq!(agent.last_compact_command_tokens, Some(210_000));
    assert!(agent.compacted_awaiting_prompt.is_some());
}

#[test]
fn compact_marker_needs_no_tokens_and_delayed_delivery_cannot_rearm_it() {
    use lifecycle::LifecycleSignal;

    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let registered = lifecycle_at(
        &workspace,
        "claude",
        "SessionStart",
        "sess-a",
        LifecycleSignal::Registered,
    );
    event_log::append(&paths.events_log, &registered).unwrap();
    let sent = compact_event(&workspace, 1, "sess-a", None, MessageStatus::Sent, None);
    event_log::append(&paths.events_log, &sent).unwrap();
    let (_, agents, _) = catch_up_rollup(&paths).unwrap();
    assert_eq!(agents[0].compacted_awaiting_prompt, Some(sent.timestamp));
    assert_eq!(agents[0].last_compact_command_tokens, None);

    for signal in [LifecycleSignal::TurnStarted, LifecycleSignal::Registered] {
        event_log::append(&paths.events_log, &sent).unwrap();
        let (cache, _, _) = catch_up_rollup(&paths).unwrap();
        write_rollup_cache(&paths.rollup_cache, &cache).unwrap();
        event_log::append(
            &paths.events_log,
            &lifecycle_at(&workspace, "claude", "boundary", "sess-a", signal),
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
                Some(80_000),
            ),
        )
        .unwrap();
        let (_, agents, _) = catch_up_rollup(&paths).unwrap();
        assert_eq!(agents[0].compacted_awaiting_prompt, None);
        assert_eq!(agents[0].last_compact_command_tokens, Some(80_000));
        std::fs::remove_file(&paths.rollup_cache).unwrap();
        let (_, rebuilt, _) = catch_up_rollup(&paths).unwrap();
        assert_eq!(rebuilt[0].compacted_awaiting_prompt, None);
    }
}

#[test]
fn only_successful_manual_compaction_sets_the_prompt_marker() {
    use lifecycle::LifecycleSignal;

    for auto in [Some(false), Some(true), None] {
        for failed in [false, true] {
            for marked in [false, true] {
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
                        LifecycleSignal::Registered,
                    ),
                )
                .unwrap();
                let sent = compact_event(&workspace, 1, "sess-a", None, MessageStatus::Sent, None);
                if marked {
                    event_log::append(&paths.events_log, &sent).unwrap();
                }
                event_log::append(
                    &paths.events_log,
                    &lifecycle_at(
                        &workspace,
                        "claude",
                        "PreCompact",
                        "sess-a",
                        LifecycleSignal::Compacting,
                    ),
                )
                .unwrap();
                let completed = lifecycle_at(
                    &workspace,
                    "claude",
                    "SessionStart",
                    "sess-a",
                    LifecycleSignal::CompactionEnded { auto, failed },
                );
                event_log::append(&paths.events_log, &completed).unwrap();
                let (_, agents, _) = catch_up_rollup(&paths).unwrap();
                let expected = if auto == Some(false) && !failed {
                    Some(completed.timestamp)
                } else {
                    marked.then_some(sent.timestamp)
                };
                assert_eq!(
                    agents[0].compacted_awaiting_prompt, expected,
                    "auto={auto:?}, failed={failed}, marked={marked}"
                );
            }
        }
    }
}

#[test]
fn manual_compaction_successor_marker_survives_rotation_until_a_prompt() {
    use lifecycle::LifecycleSignal;

    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let mut observation = crate::agents::AgentLifecycleObservation::new(
        Some("successor".into()),
        LifecycleSignal::CompactionEnded {
            auto: Some(false),
            failed: false,
        },
    );
    observation.compacted_from = Some("predecessor".into());
    let completed = EventEnvelope::agent_lifecycle(
        workspace.clone(),
        "session",
        "codex",
        "SessionStart",
        &observation,
    );
    event_log::append(&paths.events_log, &completed).unwrap();
    let (_, agents, _) = catch_up_rollup(&paths).unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(
        agents[0].compacted_awaiting_prompt,
        Some(completed.timestamp)
    );
    write_carryover(
        &paths.agents_carryover,
        &EventCarryover {
            agents,
            agent_identity: Default::default(),
            resume_outcomes: Vec::new(),
        },
    )
    .unwrap();
    std::fs::remove_file(&paths.events_log).unwrap();
    event_log::append(
        &paths.events_log,
        &lifecycle_at(
            &workspace,
            "codex",
            "PreCompact",
            "successor",
            LifecycleSignal::Compacting,
        ),
    )
    .unwrap();
    let (_, agents, _) = catch_up_rollup(&paths).unwrap();
    assert_eq!(
        agents[0].compacted_awaiting_prompt,
        Some(completed.timestamp)
    );
    event_log::append(
        &paths.events_log,
        &lifecycle_at(
            &workspace,
            "codex",
            "UserPromptSubmit",
            "successor",
            LifecycleSignal::TurnStarted,
        ),
    )
    .unwrap();
    let (_, agents, _) = catch_up_rollup(&paths).unwrap();
    assert_eq!(agents[0].compacted_awaiting_prompt, None);
}

#[test]
fn linked_successor_carries_the_compact_marker_without_a_trigger_bit() {
    use lifecycle::LifecycleSignal;

    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let mut predecessor = agent("codex", "predecessor", AgentStatus::Idle, 0);
    predecessor.compacted_awaiting_prompt = Some(epoch());
    write_carryover(
        &paths.agents_carryover,
        &EventCarryover {
            agents: vec![predecessor],
            agent_identity: Default::default(),
            resume_outcomes: Vec::new(),
        },
    )
    .unwrap();
    let mut observation = crate::agents::AgentLifecycleObservation::new(
        Some("successor".into()),
        LifecycleSignal::CompactionEnded {
            auto: None,
            failed: false,
        },
    );
    observation.compacted_from = Some("predecessor".into());
    let completed = EventEnvelope::agent_lifecycle(
        workspace.clone(),
        "session",
        "codex",
        "SessionStart",
        &observation,
    );
    event_log::append(&paths.events_log, &completed).unwrap();
    let (_, agents, _) = catch_up_rollup(&paths).unwrap();
    let successor = agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "successor")
        .unwrap();
    assert_eq!(successor.compacted_awaiting_prompt, Some(epoch()));
    event_log::append(
        &paths.events_log,
        &lifecycle_at(
            &workspace,
            "codex",
            "UserPromptSubmit",
            "successor",
            LifecycleSignal::TurnStarted,
        ),
    )
    .unwrap();
    event_log::append(&paths.events_log, &completed).unwrap();
    let (_, agents, _) = catch_up_rollup(&paths).unwrap();
    let successor = agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "successor")
        .unwrap();
    assert_eq!(successor.compacted_awaiting_prompt, None);
}
