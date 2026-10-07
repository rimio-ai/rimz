//! Synthetic store round-trip checks that do not spawn `rimz`.

#[test]
fn root_lane_is_stamped_through_every_fold_reader() {
    use rimz::agents::AgentLifecycleObservation;
    use rimz::agents::lifecycle::LifecycleSignal;
    use rimz::store::snapshot::RollupCursor;
    use rimz::workspace::RootClass;
    use rimz::workspace::record::{self, WorkspaceRecord};

    let h = crate::common::Harness::new();
    let root = std::path::PathBuf::from("/repo/project");
    record::write(
        h.store.paths(),
        &WorkspaceRecord {
            layout: 2,
            workspace_id: h.workspace_id.clone(),
            project_root: root,
            worktree_root: None,
            session_name: "rimz-test".into(),
            root_class: RootClass::Repo,
            rimz_bin: None,
            rimz_build: None,
            pins: Default::default(),
            updated_at: jiff::Timestamp::UNIX_EPOCH,
        },
    )
    .unwrap();
    for (index, (id, path, channel)) in [
        ("root", "/repo/project", None),
        ("worktree", "/repo/feature", None),
        ("named", "/repo/project", Some("design")),
    ]
    .into_iter()
    .enumerate()
    {
        let mut observation =
            AgentLifecycleObservation::new(Some(id.into()), LifecycleSignal::Registered);
        observation.worktree_path = Some(path.into());
        observation.pane_id = Some(rimz::ids::PaneId::from_parts(
            rimz::ids::MuxName::Tmux,
            format!("%{index}"),
        ));
        observation.agent_pid = Some(std::process::id());
        observation.launch.channel = channel.map(str::to_owned);
        h.store
            .append_event(&rimz::EventEnvelope::agent_lifecycle(
                h.workspace_id.clone(),
                "rimz-test",
                "claude",
                "SessionStart",
                &observation,
            ))
            .unwrap();
    }
    let assert_rows = |rows: Vec<&rimz::agents::AgentState>| {
        assert_eq!(rows.len(), 3);
        for row in rows {
            assert_eq!(row.root_lane, row.agent_id == "root", "{}", row.agent_id);
        }
    };
    let projection = h
        .store
        .runtime_projection(rimz::RuntimeScope::Runtime)
        .unwrap();
    assert_rows(projection.agents.iter().collect());
    let snapshot = h.store.snapshot().unwrap();
    assert_rows(snapshot.agents.iter().collect());
    let mut cursor = RollupCursor::new();
    for _ in 0..2 {
        let (_, rows, _) = cursor.fold(h.store.paths()).unwrap();
        assert_rows(rows.iter().collect());
    }
}

#[test]
fn runtime_and_audit_projections_retain_snapshot_resume_outcomes() {
    use rimz::store::event::{EventEnvelope, MessageEventMethod};
    use rimz::store::message::{DeliveryGate, MessageRecord, MessageStatus};

    let h = crate::common::Harness::new();
    let agent = rimz::testkit::agent_state("claude", "child", jiff::Timestamp::now());
    let mut message = MessageRecord::new(
        h.store.paths().workspace_id.clone(),
        &agent,
        "continue".to_owned(),
        DeliveryGate::Resume,
    );
    message.status = MessageStatus::Delivered;
    h.store
        .append_event(&EventEnvelope::message_event(
            &message,
            "test",
            MessageEventMethod::Delivered,
            None,
        ))
        .unwrap();
    let snapshot = h.store.snapshot().unwrap();
    let outcomes = snapshot.resume_outcomes.unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].message_id, message.message_id);
    for scope in [rimz::RuntimeScope::Audit, rimz::RuntimeScope::Runtime] {
        assert_eq!(
            h.store.runtime_projection(scope).unwrap().resume_outcomes,
            outcomes
        );
    }
}

#[test]
fn runtime_projection_serves_lock_free_while_a_writer_holds_the_lock() {
    // Reads resume from the persisted rollup fold base, so they never take
    // the workspace lock: a projection completes — and still sees every
    // committed agent — while a writer holds the lock.
    use std::sync::mpsc;
    use std::time::Duration;

    let h = crate::common::Harness::new();

    h.store
        .append_event(&crate::common::lifecycle_event(
            &h,
            "rimz-test",
            "SessionStart",
            "agent-1",
        ))
        .expect("append agent");

    let _guard = rimz::disk::lock::WorkspaceLock::acquire(&h.store.paths().workspace_lock)
        .expect("hold workspace lock");

    let store = h.store.clone();
    let (result_tx, result_rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let projection = store.runtime_projection(rimz::RuntimeScope::Runtime);
        let _ = result_tx.send(projection.map(|p| p.agents.len()));
    });

    let agents = result_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("projection completes while the workspace lock is held")
        .expect("projection succeeds");
    assert_eq!(agents, 1, "the committed agent survives the lock-free read");
    reader.join().expect("reader thread");
}
