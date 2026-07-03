use super::*;

#[test]
fn terminal_resume_prompt_events_fold_into_rollup() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let enqueued = recent(60);
    let updated = recent(30);

    event_log::append(
        &paths.events_log,
        &resume_event(
            &workspace,
            1,
            "sess-a",
            MessageStatus::Delivered,
            enqueued,
            updated,
        ),
    )
    .unwrap();
    event_log::append(
        &paths.events_log,
        &resume_event(
            &workspace,
            2,
            "sess-b",
            MessageStatus::Queued,
            enqueued,
            updated,
        ),
    )
    .unwrap();
    event_log::append(
        &paths.events_log,
        &message_event(
            &workspace,
            MessageEventFixture {
                id: 3,
                agent_id: "sess-c",
                gate: DeliveryGate::Done,
                body: MessageBody::Prompt,
                status: MessageStatus::Delivered,
                enqueued_at: Some(enqueued),
                updated_at: updated,
            },
        ),
    )
    .unwrap();
    event_log::append(
        &paths.events_log,
        &message_event(
            &workspace,
            MessageEventFixture {
                id: 4,
                agent_id: "sess-d",
                gate: DeliveryGate::Resume,
                body: MessageBody::Command,
                status: MessageStatus::Delivered,
                enqueued_at: Some(enqueued),
                updated_at: updated,
            },
        ),
    )
    .unwrap();

    let (cache, _, outcomes) = catch_up_rollup(&paths).unwrap();
    assert_eq!(cache.resume_outcomes.len(), 1);
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].message_id, message_id(1));
    assert_eq!(outcomes[0].enqueued_at, enqueued);
    assert_eq!(outcomes[0].updated_at, updated);
}

#[test]
fn resume_outcomes_default_missing_enqueued_at_to_event_time() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let updated = recent(30);

    event_log::append(
        &paths.events_log,
        &message_event(
            &workspace,
            MessageEventFixture {
                id: 1,
                agent_id: "sess-a",
                gate: DeliveryGate::Resume,
                body: MessageBody::Prompt,
                status: MessageStatus::Delivered,
                enqueued_at: None,
                updated_at: updated,
            },
        ),
    )
    .unwrap();

    let (_, _, outcomes) = catch_up_rollup(&paths).unwrap();
    assert_eq!(outcomes[0].enqueued_at, updated);
}

#[test]
fn resume_outcomes_keep_latest_per_agent_card() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let older_enqueued = recent(120);
    let newer_enqueued = recent(60);
    let older_updated = recent(30);
    let newer_updated = recent(20);

    for event in [
        resume_event(
            &workspace,
            1,
            "sess-a",
            MessageStatus::Delivered,
            older_enqueued,
            newer_updated,
        ),
        resume_event(
            &workspace,
            2,
            "sess-a",
            MessageStatus::Delivered,
            newer_enqueued,
            older_updated,
        ),
        resume_event(
            &workspace,
            3,
            "sess-a",
            MessageStatus::Delivered,
            newer_enqueued,
            newer_updated,
        ),
        resume_event(
            &workspace,
            4,
            "sess-a",
            MessageStatus::Delivered,
            newer_enqueued,
            newer_updated,
        ),
    ] {
        event_log::append(&paths.events_log, &event).unwrap();
    }

    let (_, _, outcomes) = catch_up_rollup(&paths).unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(
        outcomes[0].message_id,
        message_id(4),
        "max order is enqueued_at, then updated_at, then message_id"
    );
}

#[test]
fn resume_outcomes_prune_after_seven_days() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let old = recent(8 * 24 * 60 * 60);
    let fresh = recent(60);

    event_log::append(
        &paths.events_log,
        &resume_event(
            &workspace,
            1,
            "sess-old",
            MessageStatus::Delivered,
            old,
            old,
        ),
    )
    .unwrap();
    event_log::append(
        &paths.events_log,
        &resume_event(
            &workspace,
            2,
            "sess-fresh",
            MessageStatus::Delivered,
            fresh,
            fresh,
        ),
    )
    .unwrap();

    let (cache, _, outcomes) = catch_up_rollup(&paths).unwrap();
    assert_eq!(cache.resume_outcomes.len(), 1);
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].agent_id, AgentSessionId::from("sess-fresh"));
}

#[test]
fn cursor_warm_fold_picks_up_and_holds_resume_outcomes() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let mut cursor = RollupCursor::new();
    let (_, _, first) = cursor.fold(&paths).unwrap();
    assert!(first.is_empty());

    event_log::append(
        &paths.events_log,
        &resume_event(
            &workspace,
            1,
            "sess-a",
            MessageStatus::Delivered,
            recent(60),
            recent(30),
        ),
    )
    .unwrap();

    let (_, _, second) = cursor.fold(&paths).unwrap();
    assert_eq!(second.len(), 1);
    let (_, _, held) = cursor.fold(&paths).unwrap();
    assert_eq!(held, second);
}

#[test]
fn carryover_resume_outcomes_merge_after_rotation_reseed() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace, dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    write_carryover(
        &paths.agents_carryover,
        &EventCarryover {
            agents: Vec::new(),
            agent_identity: AgentIdentityState::default(),
            resume_outcomes: vec![ResumeOutcome {
                message_id: message_id(1),
                kind: AgentKind::new_unchecked("claude"),
                agent_id: AgentSessionId::from("sess-a"),
                agent_name: Some("lucid-atlas".to_owned()),
                status: MessageStatus::Delivered,
                enqueued_at: recent(60),
                updated_at: recent(30),
            }],
            lost: Vec::new(),
        },
    )
    .unwrap();
    reseed_rollup_cache_for_rotation(&paths).unwrap();

    let (cache, _, outcomes) = catch_up_rollup(&paths).unwrap();
    assert!(cache.resume_outcomes.is_empty());
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].agent_name.as_deref(), Some("lucid-atlas"));
}
