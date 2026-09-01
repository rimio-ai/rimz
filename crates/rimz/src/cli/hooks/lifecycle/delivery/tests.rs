use super::*;

fn workspace_id() -> rimz::ids::WorkspaceId {
    rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/hooks-test"))
}

fn workspace() -> ResolvedWorkspace {
    ResolvedWorkspace {
        workspace_id: workspace_id(),
        project_root: std::path::PathBuf::from("/tmp/hooks-test"),
        cwd_project_root: None,
        root_class: rimz::workspace::RootClass::Directory,
        worktree_root: std::path::PathBuf::from("/tmp/hooks-test"),
        worktree_branch: None,
        session_name: "hooks-test".to_owned(),
        mux_hint: None,
    }
}

fn turn_started() -> RecordedLifecycle {
    let mut observation = AgentLifecycleObservation::new(
        Some(rimz::ids::AgentSessionId::from("sess-1")),
        LifecycleSignal::TurnStarted,
    );
    observation.worktree_path = Some("/tmp/hooks-test/worktree".to_owned());
    RecordedLifecycle {
        model_hint: None,
        observation,
        primary_event_id: None,
        events: Vec::new(),
        rotation_due: false,
        waiting_cleared: false,
    }
}

#[test]
fn turn_started_records_only_unsupervised_user_inputs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = workspace();
    let agent = rimz::agents::definition_by_kind("claude").unwrap();
    let agent_state = rimz::testkit::agent_state("claude", "sess-1", jiff::Timestamp::now());
    let human = rimz::message::MessageRecord::new(
        workspace_id(),
        &agent_state,
        "human prompt".to_owned(),
        true,
        rimz::message::DeliveryGate::Done,
    );
    let agent_message = human
        .clone()
        .with_sender(rimz::message::MessageSender::Agent {
            kind: rimz::ids::AgentKind::new_unchecked("codex"),
            name: None,
            profile: None,
            role: Some("coder".to_owned()),
            channel: None,
        });

    record_user_input_for_lifecycle(
        &workspace,
        agent,
        &turn_started(),
        &[],
        false,
        Some(dir.path()),
    );
    record_user_input_for_lifecycle(
        &workspace,
        agent,
        &turn_started(),
        std::slice::from_ref(&human),
        false,
        Some(dir.path()),
    );
    record_user_input_for_lifecycle(
        &workspace,
        agent,
        &turn_started(),
        std::slice::from_ref(&agent_message),
        false,
        Some(dir.path()),
    );
    let mut mixed = turn_started();
    mixed.observation.prompt =
        Some("Type: AGENT_MESSAGE\nFrom: @coder\nContent:\nhuman prompttyped directly".to_owned());
    record_user_input_for_lifecycle(
        &workspace,
        agent,
        &mixed,
        &[agent_message],
        false,
        Some(dir.path()),
    );
    record_user_input_for_lifecycle(
        &workspace,
        agent,
        &turn_started(),
        &[human],
        true,
        Some(dir.path()),
    );

    let records = rimz::agents::spending::user_input::load_in(dir.path());
    assert_eq!(records.len(), 3);
    assert!(
        records
            .iter()
            .all(|record| record.kind.as_str() == "claude")
    );
    assert!(records.iter().all(|record| {
        record.origin.as_deref() == Some(std::path::Path::new("/tmp/hooks-test/worktree"))
    }));
}
