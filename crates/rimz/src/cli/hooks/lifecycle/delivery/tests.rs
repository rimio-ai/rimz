use super::*;

fn record_user_input_for_lifecycle(
    workspace: &ResolvedWorkspace,
    agent: &AgentDefinition,
    recorded: &RecordedLifecycle,
    delivered: &[rimz::store::message::MessageRecord],
    supervised: bool,
    state_root: Option<&std::path::Path>,
) {
    let sections = rimz::store::message::classify_submitted_prompt(
        recorded
            .observation
            .prompt
            .as_deref()
            .unwrap_or("human prompt"),
        &delivered.iter().collect::<Vec<_>>(),
        &[],
    );
    super::record_user_input_for_lifecycle(
        workspace, agent, recorded, &sections, supervised, state_root,
    );
}

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
        LifecycleSignal::TurnStarted { turn_id: None },
    );
    observation.worktree_path = Some("/tmp/hooks-test/worktree".to_owned());
    RecordedLifecycle {
        model_hint: None,
        observation,
        primary_event_id: None,
        events: Vec::new(),
        rotation_due: false,
        side_conversation: None,
        waiting_cleared: false,
    }
}

#[test]
fn claude_wrapped_harness_delivery_does_not_record_user_input() {
    use rimz::store::message::{DeliveryGate, HarnessNotice, MessageRecord, MessageSender};

    let dir = tempfile::tempdir().unwrap();
    let workspace = workspace();
    let agent = rimz::agents::definition_by_kind("claude").unwrap();
    let state = rimz::testkit::agent_state("claude", "sess-1", jiff::Timestamp::UNIX_EPOCH);
    let message = MessageRecord::new(
        workspace_id(),
        &state,
        "Implement is yours.".to_owned(),
        DeliveryGate::Done,
    )
    .with_sender(MessageSender::Harness {
        notice: HarnessNotice::Stage,
    });
    let payload = serde_json::json!({
        "session_id": "sess-1",
        "prompt": format!("<pasted_content id=\"e676\">\nType: STAGE\nFrom: @rimz\nContent:\n{}\n</pasted_content id=\"e676\">", message.text),
    });
    let decoded = agent.decode_hook("UserPromptSubmit", &payload).unwrap();
    let mut started = turn_started();
    started.observation.prompt = decoded.lifecycle().unwrap().prompt.clone();

    record_user_input_for_lifecycle(
        &workspace,
        agent,
        &started,
        &[message],
        false,
        Some(dir.path()),
    );

    assert!(rimz::agents::spending::user_input::load_in(dir.path()).is_empty());
}

#[test]
fn turn_started_records_only_unsupervised_user_inputs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = workspace();
    let agent = rimz::agents::definition_by_kind("claude").unwrap();
    let agent_state = rimz::testkit::agent_state("claude", "sess-1", jiff::Timestamp::now());
    let human = rimz::store::message::MessageRecord::new(
        workspace_id(),
        &agent_state,
        "human prompt".to_owned(),
        rimz::store::message::DeliveryGate::Done,
    );
    let agent_message = human
        .clone()
        .with_sender(rimz::store::message::MessageSender::Agent {
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
    mixed.observation.prompt = rimz::agents::SanitizedPrompt::new(Some(
        "Type: AGENT_MESSAGE\nFrom: @coder\nContent:\nhuman prompttyped directly",
    ));
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
        std::slice::from_ref(&human),
        true,
        Some(dir.path()),
    );
    for excluded in [
        human.clone().with_automated(true),
        rimz::store::message::MessageRecord {
            gate: rimz::store::message::DeliveryGate::Resume,
            ..human
        },
    ] {
        record_user_input_for_lifecycle(
            &workspace,
            agent,
            &turn_started(),
            &[excluded],
            false,
            Some(dir.path()),
        );
    }
    // A turn start whose prompt the adapter could not read classifies into no
    // sections at all, and still opens the spend window: antigravity with an
    // unreadable transcript and a plugin that omits the optional prompt both
    // reach here on a genuine human turn.
    super::record_user_input_for_lifecycle(
        &workspace,
        agent,
        &turn_started(),
        &[],
        false,
        Some(dir.path()),
    );

    let records = rimz::agents::spending::user_input::load_in(dir.path());
    assert_eq!(records.len(), 4);
    assert!(
        records
            .iter()
            .all(|record| record.kind.as_str() == "claude")
    );
    assert!(records.iter().all(|record| {
        record.origin.as_deref() == Some(std::path::Path::new("/tmp/hooks-test/worktree"))
    }));
}
