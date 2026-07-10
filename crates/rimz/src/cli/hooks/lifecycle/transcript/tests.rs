use super::*;

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::TempDir::new().unwrap();
    let workspace_id =
        rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/hooks-test"));
    let paths = rimz::store::StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
    let runtime = rimz::store::RuntimePaths::under(workspace_id, dir.path()).unwrap();
    (dir, Store::open(paths, runtime).unwrap())
}

fn workspace() -> ResolvedWorkspace {
    ResolvedWorkspace {
        workspace_id: rimz::ids::WorkspaceId::from_project_root(std::path::Path::new(
            "/tmp/hooks-test",
        )),
        project_root: std::path::PathBuf::from("/tmp/hooks-test"),
        root_class: rimz::workspace::RootClass::Directory,
        worktree_root: std::path::PathBuf::from("/tmp/hooks-test/chat"),
        worktree_branch: None,
        session_name: "session".to_owned(),
        mux_hint: None,
    }
}

fn recorded(signal: LifecycleSignal) -> RecordedLifecycle {
    RecordedLifecycle {
        model_hint: None,
        observation: AgentLifecycleObservation::new(
            Some(rimz::ids::AgentSessionId::from("sess-1")),
            signal,
        ),
        appended_lifecycle: false,
        waiting_cleared: false,
    }
}

#[test]
fn conversation_entries_follow_confirmed_message_turn_causality() {
    let (_dir, store) = store();
    let workspace = workspace();
    let agent = rimz::testkit::agent_state("claude", "sess-1", jiff::Timestamp::UNIX_EPOCH);
    let parent = rimz::ids::MessageId::parse("msg_0123456789abcdef").unwrap();
    let first = rimz::message::MessageRecord::new(
        workspace.workspace_id.clone(),
        &agent,
        "first".to_owned(),
        true,
        rimz::message::DeliveryGate::Done,
    )
    .with_in_reply_to(vec![parent.clone()]);
    let second = rimz::message::MessageRecord::new(
        workspace.workspace_id.clone(),
        &agent,
        "second".to_owned(),
        true,
        rimz::message::DeliveryGate::Done,
    );
    let mut started = recorded(LifecycleSignal::TurnStarted);
    started.observation.prompt = Some("from @planner: first\n\nfrom @reviewer: second".to_owned());

    record_conversation(
        &workspace,
        &store,
        &rimz::agents::ClaudeAdapter,
        "UserPromptSubmit",
        &serde_json::json!({}),
        &started,
        &[first.clone(), second.clone()],
    )
    .unwrap();

    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].message_id.as_ref(), Some(&first.message_id));
    assert_eq!(entries[0].reply_to, vec![parent]);
    assert_eq!(entries[1].message_id.as_ref(), Some(&second.message_id));
    assert_eq!(
        rimz::store::agent_context::read_one(store.runtime_paths(), "claude", "sess-1")
            .unwrap()
            .context
            .turn_opened_by,
        vec![first.message_id.clone(), second.message_id.clone()]
    );

    record_conversation(
        &workspace,
        &store,
        &rimz::agents::ClaudeAdapter,
        "Stop",
        &serde_json::json!({ "last_assistant_message": "done" }),
        &recorded(LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        }),
        &[],
    )
    .unwrap();
    assert_eq!(
        rimz::transcript::read_all(store.paths())
            .unwrap()
            .last()
            .unwrap()
            .reply_to,
        vec![first.message_id.clone(), second.message_id.clone()]
    );

    record_conversation(
        &workspace,
        &store,
        &rimz::agents::ClaudeAdapter,
        "PreToolUse",
        &serde_json::json!({
            "tool_name": "AskUserQuestion",
            "tool_input": { "questions": [{ "question": "Ship?" }] }
        }),
        &recorded(LifecycleSignal::AwaitingInput {
            kind: rimz::agents::AskKind::Question,
            ask_id: Some(rimz::ids::AskId::parse("ask_0123456789abcdef").unwrap()),
            detail: None,
        }),
        &[],
    )
    .unwrap();
    assert_eq!(
        rimz::transcript::read_all(store.paths())
            .unwrap()
            .last()
            .unwrap()
            .reply_to,
        vec![first.message_id, second.message_id]
    );

    let mut hand_typed = recorded(LifecycleSignal::TurnStarted);
    hand_typed.observation.prompt = Some("typed directly".to_owned());
    record_conversation(
        &workspace,
        &store,
        &rimz::agents::ClaudeAdapter,
        "UserPromptSubmit",
        &serde_json::json!({}),
        &hand_typed,
        &[],
    )
    .unwrap();
    assert!(
        rimz::store::agent_context::read_one(store.runtime_paths(), "claude", "sess-1")
            .unwrap()
            .context
            .turn_opened_by
            .is_empty()
    );
    assert_eq!(
        rimz::transcript::read_all(store.paths())
            .unwrap()
            .last()
            .unwrap()
            .message_id,
        None
    );
}
