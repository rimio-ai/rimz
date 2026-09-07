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
    for target_id in ["sess-a", "provisional"] {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        let mut carried = agent("claude", "sess-a", AgentStatus::Idle, 0);
        carried.name = Some("lucid-atlas".to_owned());
        write_carryover(
            &paths.agents_carryover,
            &EventCarryover {
                agents: vec![carried, agent("claude", "untouched", AgentStatus::Idle, 0)],
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
                target_id,
                Some("lucid-atlas"),
                MessageStatus::Sent,
                Some(210_000),
            ),
        )
        .unwrap();

        let (cache, agents, _) = catch_up_rollup(&paths).unwrap();
        let agent = agents
            .iter()
            .find(|agent| agent.agent_id.as_str() == "sess-a")
            .expect("agent");
        assert_eq!(agent.last_compact_command_tokens, Some(210_000));
        assert!(agent.compacted_awaiting_prompt.is_some());
        let marker = agent.compacted_awaiting_prompt;
        write_rollup_cache(&paths.rollup_cache, &cache).unwrap();
        let (advanced, agents, _) = catch_up_rollup(&paths).unwrap();
        assert_eq!(advanced.extent, cache.extent);
        let agent = agents
            .iter()
            .find(|agent| agent.agent_id.as_str() == "sess-a")
            .expect("carried agent after cache advancement");
        assert_eq!(agent.compacted_awaiting_prompt, marker);
        assert_eq!(agent.last_compact_command_tokens, Some(210_000));
        assert_eq!(advanced.raw_agents.len(), 1);
        assert_eq!(advanced.raw_agents[0].agent_id.as_str(), "sess-a");
    }
}

#[test]
fn carryover_compact_stamp_does_not_precede_earlier_lifecycle_events() {
    use lifecycle::LifecycleSignal;

    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    write_carryover(
        &paths.agents_carryover,
        &EventCarryover {
            agents: vec![agent("claude", "predecessor", AgentStatus::Idle, 0)],
            ..EventCarryover::default()
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
    event_log::append(
        &paths.events_log,
        &EventEnvelope::agent_lifecycle(
            workspace.clone(),
            "session",
            "claude",
            "SessionStart",
            &observation,
        ),
    )
    .unwrap();
    event_log::append(
        &paths.events_log,
        &lifecycle_at(
            &workspace,
            "claude",
            "UserPromptSubmit",
            "predecessor",
            LifecycleSignal::TurnStarted,
        ),
    )
    .unwrap();
    let sent = compact_event(
        &workspace,
        1,
        "predecessor",
        None,
        MessageStatus::Sent,
        Some(210_000),
    );
    event_log::append(&paths.events_log, &sent).unwrap();

    let (cache, agents, _) = catch_up_rollup(&paths).unwrap();
    write_rollup_cache(&paths.rollup_cache, &cache).unwrap();
    let (_, advanced, _) = catch_up_rollup(&paths).unwrap();
    for agents in [agents, advanced] {
        let predecessor = agents
            .iter()
            .find(|agent| agent.agent_id.as_str() == "predecessor")
            .unwrap();
        assert_eq!(predecessor.compacted_awaiting_prompt, Some(sent.timestamp));
        assert_eq!(predecessor.last_compact_command_tokens, Some(210_000));
        let successor = agents
            .iter()
            .find(|agent| agent.agent_id.as_str() == "successor")
            .unwrap();
        assert_eq!(successor.compacted_awaiting_prompt, None);
        assert_eq!(successor.last_compact_command_tokens, None);
    }
}

#[test]
fn kiro_compact_marker_persists_without_lifecycle_events_but_does_not_latch() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let kiro = agent("kiro", "sess-kiro", AgentStatus::Idle, 0);
    write_carryover(
        &paths.agents_carryover,
        &EventCarryover {
            agents: vec![kiro.clone()],
            agent_identity: Default::default(),
            resume_outcomes: Vec::new(),
        },
    )
    .unwrap();
    let mut command = MessageRecord::new(
        workspace,
        &kiro,
        "/compact".to_owned(),
        true,
        DeliveryGate::Done,
    )
    .with_body(MessageBody::Command);
    for (status, method) in [
        (MessageStatus::Sent, MessageEventMethod::Sent),
        (MessageStatus::TimedOut, MessageEventMethod::TimedOut),
    ] {
        command.status = status;
        event_log::append(
            &paths.events_log,
            &EventEnvelope::message_event(&command, "session", method, None),
        )
        .unwrap();
        let (cache, agents, _) = catch_up_rollup(&paths).unwrap();
        write_rollup_cache(&paths.rollup_cache, &cache).unwrap();
        assert!(agents[0].compacted_awaiting_prompt.is_some());
        assert!(!agents[0].compaction_unprompted(Timestamp::now()));
    }
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

    for (signal, expected) in [
        (LifecycleSignal::TurnStarted, None),
        (LifecycleSignal::Registered, Some(sent.timestamp)),
    ] {
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
        assert_eq!(agents[0].compacted_awaiting_prompt, expected);
        assert_eq!(agents[0].last_compact_command_tokens, Some(80_000));
        std::fs::remove_file(&paths.rollup_cache).unwrap();
        let (_, rebuilt, _) = catch_up_rollup(&paths).unwrap();
        assert_eq!(rebuilt[0].compacted_awaiting_prompt, expected);
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
