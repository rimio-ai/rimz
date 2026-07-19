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
        rotation_due: false,
        waiting_cleared: false,
    }
}

#[test]
fn one_terminal_extraction_is_shared_when_run_and_conversation_both_need_it() {
    let calls = std::cell::Cell::new(0);
    let terminal = recorded(LifecycleSignal::TurnEnded {
        errored: false,
        parked_on_background: false,
    });
    let message = assistant_message_for_lifecycle(&terminal, true, || {
        calls.set(calls.get() + 1);
        Some("  exact run output  ".to_owned())
    });
    assert_eq!(message.as_deref(), Some("  exact run output  "));
    assert_eq!(calls.get(), 1);
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
        rimz::agents::definition_by_kind("claude").unwrap(),
        &started,
        None,
        &[],
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
        rimz::agents::definition_by_kind("claude").unwrap(),
        &recorded(LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        }),
        Some("done"),
        &[],
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
        rimz::agents::definition_by_kind("claude").unwrap(),
        &recorded(LifecycleSignal::AwaitingInput {
            kind: rimz::agents::AskKind::Question,
            ask_id: Some(rimz::ids::AskId::parse("ask_0123456789abcdef").unwrap()),
            detail: None,
            native_key: None,
        }),
        None,
        &[rimz::transcript::AskQuestion {
            question: "Ship?".to_owned(),
            options: Vec::new(),
            multi_select: false,
            has_option_previews: false,
        }],
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
        rimz::agents::definition_by_kind("claude").unwrap(),
        &hand_typed,
        None,
        &[],
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

#[test]
fn cursor_response_hook_is_the_only_assistant_text_authority() {
    let (_dir, store) = store();
    let workspace = workspace();
    let opener = rimz::ids::MessageId::parse("msg_0123456789abcdef").unwrap();
    rimz::store::agent_context::merge_turn_opened_by(
        store.runtime_paths(),
        "cursor",
        "conv-1",
        vec![opener.clone()],
    )
    .unwrap();

    let payload = serde_json::json!({
        "conversation_id": "conv-1",
        "text": "  safe final  ",
        "thinking": "must not persist"
    });
    let decoded = rimz::agents::definition_by_kind("cursor")
        .unwrap()
        .decode_hook("afterAgentResponse", &payload)
        .unwrap();
    let recorded_response = record_assistant_response(
        &workspace,
        &store,
        rimz::agents::definition_by_kind("cursor").unwrap(),
        &decoded,
        None,
    )
    .expect("safe response");
    assert_eq!(recorded_response.1, "safe final");

    for signal in [
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        },
        LifecycleSignal::TurnEnded {
            errored: true,
            parked_on_background: false,
        },
        LifecycleSignal::TurnInterrupted,
    ] {
        let mut stopped = recorded(signal);
        stopped.observation.agent_id = Some(rimz::ids::AgentSessionId::from("conv-1"));
        record_conversation(
            &workspace,
            &store,
            rimz::agents::definition_by_kind("cursor").unwrap(),
            &stopped,
            None,
            &[],
            &[],
        )
        .unwrap();
    }

    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].entry,
        rimz::transcript::TranscriptKind::Assistant
    );
    assert_eq!(entries[0].text, "safe final");
    assert_eq!(entries[0].reply_to, vec![opener]);
}
