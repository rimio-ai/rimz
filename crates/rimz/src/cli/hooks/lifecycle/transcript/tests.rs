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
        cwd_project_root: None,
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
        primary_event_id: None,
        events: Vec::new(),
        rotation_due: false,
        waiting_cleared: false,
    }
}

fn conversation_input<'a>(
    assistant_message: Option<&'a str>,
    questions: &'a [rimz::transcript::AskQuestion],
    delivered: &'a [rimz::message::MessageRecord],
) -> ConversationInput<'a> {
    ConversationInput {
        assistant_message,
        questions,
        delivered,
        run_id: None,
    }
}

fn append_launched_agent(
    store: &Store,
    kind: &str,
    agent_id: &str,
    launch_id: Option<&str>,
    name: &str,
    launch: rimz::agents::LaunchParams,
) {
    let kind = rimz::ids::AgentKind::new_unchecked(kind);
    store
        .append_event(&rimz::store::event::EventEnvelope::agent_launched(
            store.paths().workspace_id.clone(),
            "session",
            &kind,
            rimz::store::event::AgentLaunchPayload {
                agent_id: rimz::ids::AgentSessionId::from(agent_id),
                launch_id: launch_id.map(rimz::ids::AgentSessionId::from),
                agent_name: name.to_owned(),
                agent_name_explicit: true,
                launch,
                state: rimz::store::event::AgentLaunchState::Bound,
                run_id: None,
                pane_id: None,
                runtime_owner: None,
                worktree_path: Some("/tmp/hooks-test/chat".to_owned()),
                worktree_branch: Some("chat".to_owned()),
                prompt: None,
                description: None,
            },
        ))
        .unwrap();
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
    started.observation.prompt = Some(
        "Type: AGENT_MESSAGE\nFrom: @planner\nContent:\nfirst\n\nType: AGENT_MESSAGE\nFrom: @reviewer\nContent:\nsecond"
            .to_owned(),
    );

    record_conversation(
        &workspace,
        &store,
        rimz::agents::definition_by_kind("claude").unwrap(),
        &started,
        conversation_input(None, &[], &[first.clone(), second.clone()]),
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
        conversation_input(Some("done"), &[], &[]),
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
        conversation_input(
            None,
            &[rimz::transcript::AskQuestion {
                question: "Ship?".to_owned(),
                options: Vec::new(),
                multi_select: false,
                has_option_previews: false,
            }],
            &[],
        ),
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
    hand_typed.waiting_cleared = true;
    hand_typed.observation.prompt = Some("typed directly".to_owned());
    record_conversation(
        &workspace,
        &store,
        rimz::agents::definition_by_kind("claude").unwrap(),
        &hand_typed,
        conversation_input(None, &[], &[]),
    )
    .unwrap();
    assert!(
        rimz::store::agent_context::read_one(store.runtime_paths(), "claude", "sess-1")
            .unwrap()
            .context
            .turn_opened_by
            .is_empty()
    );
    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    let answer = entries.last().unwrap();
    assert_eq!(answer.entry, rimz::transcript::TranscriptKind::Answer);
    assert_eq!(
        answer.id.as_ref().map(rimz::ids::AskId::as_str),
        Some("ask_0123456789abcdef")
    );
    assert_eq!(answer.from.as_deref(), Some("you"));
    assert_eq!(answer.text, "typed directly");
    assert_eq!(
        answer.answers,
        vec![rimz::transcript::AskAnswer {
            question: None,
            chosen: vec!["typed directly".to_owned()],
            note: None,
        }]
    );
    assert_eq!(answer.message_id, None);
}

#[test]
fn subagent_report_records_as_a_hidden_harness_report() {
    let (_dir, store) = store();
    let workspace = workspace();
    let agent = rimz::testkit::agent_state("claude", "sess-1", jiff::Timestamp::UNIX_EPOCH);
    let message = rimz::message::MessageRecord::new(
        workspace.workspace_id.clone(),
        &agent,
        "child result".to_owned(),
        true,
        rimz::message::DeliveryGate::Done,
    )
    .with_sender(rimz::message::MessageSender::Subagent {
        kind: rimz::ids::AgentKind::new_unchecked("codex"),
        name: "lucid-atlas".to_owned(),
    });
    let mut started = recorded(LifecycleSignal::TurnStarted);
    started.observation.prompt =
        Some("Type: SUBAGENT_REPORT\nFrom: @rimz\nContent:\nchild result".to_owned());

    record_conversation(
        &workspace,
        &store,
        rimz::agents::definition_by_kind("claude").unwrap(),
        &started,
        conversation_input(None, &[], std::slice::from_ref(&message)),
    )
    .unwrap();

    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].entry,
        rimz::transcript::TranscriptKind::SubagentReport
    );
    assert_eq!(entries[0].from.as_deref(), Some("@rimz"));
    assert_eq!(entries[0].text, "child result");
    assert_eq!(entries[0].message_id.as_ref(), Some(&message.message_id));
}

#[test]
fn mixed_submit_records_stray_text_as_direct_input() {
    let (_dir, store) = store();
    let workspace = workspace();
    let agent = rimz::testkit::agent_state("claude", "sess-1", jiff::Timestamp::UNIX_EPOCH);
    let message = rimz::message::MessageRecord::new(
        workspace.workspace_id.clone(),
        &agent,
        "child result".to_owned(),
        true,
        rimz::message::DeliveryGate::Done,
    )
    .with_sender(rimz::message::MessageSender::Subagent {
        kind: rimz::ids::AgentKind::new_unchecked("codex"),
        name: "lucid-atlas".to_owned(),
    });
    let mut started = recorded(LifecycleSignal::TurnStarted);
    started.observation.prompt = Some(
        "Type: SUBAGENT_REPORT\nFrom: @lucid-atlas\nContent:\nchild resultdo you still".to_owned(),
    );

    record_conversation(
        &workspace,
        &store,
        rimz::agents::definition_by_kind("claude").unwrap(),
        &started,
        conversation_input(None, &[], std::slice::from_ref(&message)),
    )
    .unwrap();

    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(
        entries[0].entry,
        rimz::transcript::TranscriptKind::SubagentReport
    );
    assert_eq!(entries[0].text, "child result");
    assert_eq!(entries[0].message_id.as_ref(), Some(&message.message_id));
    assert_eq!(entries[1].entry, rimz::transcript::TranscriptKind::Prompt);
    assert_eq!(entries[1].text, "do you still");
    assert_eq!(entries[1].message_id, None);
}

#[test]
fn user_message_header_records_prompt_without_envelope() {
    let (_dir, store) = store();
    let workspace = workspace();
    let agent = rimz::testkit::agent_state("claude", "sess-1", jiff::Timestamp::UNIX_EPOCH);
    let message = rimz::message::MessageRecord::new(
        workspace.workspace_id.clone(),
        &agent,
        "from a human".to_owned(),
        true,
        rimz::message::DeliveryGate::Done,
    );
    let mut started = recorded(LifecycleSignal::TurnStarted);
    started.observation.prompt =
        Some("Type: USER_MESSAGE\nFrom: @user\nContent:\nfrom a human".to_owned());

    record_conversation(
        &workspace,
        &store,
        rimz::agents::definition_by_kind("claude").unwrap(),
        &started,
        conversation_input(None, &[], std::slice::from_ref(&message)),
    )
    .unwrap();

    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].entry, rimz::transcript::TranscriptKind::Prompt);
    assert_eq!(entries[0].from, None);
    assert_eq!(entries[0].text, "from a human");
    assert_eq!(entries[0].message_id.as_ref(), Some(&message.message_id));
}

#[test]
fn launched_child_brief_is_attributed_to_parent() {
    let (_dir, store) = store();
    let workspace = workspace();
    append_launched_agent(
        &store,
        "codex",
        "provider-parent-session",
        Some("parent-launch-session"),
        "steady-parent",
        rimz::agents::LaunchParams {
            role: Some("planner".to_owned()),
            channel: Some("chat".to_owned()),
            ..Default::default()
        },
    );
    append_launched_agent(
        &store,
        "claude",
        "child-session",
        None,
        "swift-child",
        rimz::agents::LaunchParams {
            parent_agent_id: Some(rimz::ids::AgentSessionId::from("parent-launch-session")),
            parent_agent_kind: None,
            launch_depth: Some(1),
            channel: Some("chat".to_owned()),
            ..Default::default()
        },
    );

    let mut run = rimz::harness::run::RunRecord::new(
        workspace.workspace_id.clone(),
        rimz::ids::AgentKind::new_unchecked("claude"),
        rimz::agents::PermissionMode::Auto,
        "inspect the infra".to_owned(),
        workspace.worktree_root.clone(),
    );
    run.subagent = true;
    rimz::harness::run::create(store.paths(), &run).unwrap();
    let mut started = recorded(LifecycleSignal::TurnStarted);
    started.observation.agent_id = Some(rimz::ids::AgentSessionId::from("child-session"));
    started.observation.prompt = Some("  inspect the infra  ".to_owned());

    record_conversation(
        &workspace,
        &store,
        rimz::agents::definition_by_kind("claude").unwrap(),
        &started,
        ConversationInput {
            run_id: Some(&run.run_id),
            ..conversation_input(None, &[], &[])
        },
    )
    .unwrap();

    let mut later = started;
    later.observation.prompt = Some("follow-up from the human".to_owned());
    record_conversation(
        &workspace,
        &store,
        rimz::agents::definition_by_kind("claude").unwrap(),
        &later,
        ConversationInput {
            run_id: Some(&run.run_id),
            ..conversation_input(None, &[], &[])
        },
    )
    .unwrap();

    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].entry, rimz::transcript::TranscriptKind::Message);
    assert_eq!(entries[0].from.as_deref(), Some("@planner"));
    assert_eq!(
        entries[0].parent_agent_id.as_deref(),
        Some("parent-launch-session")
    );
    assert_eq!(entries[0].parent_agent_kind, None);
    assert_eq!(entries[1].entry, rimz::transcript::TranscriptKind::Prompt);
    assert_eq!(entries[1].from, None);
    assert_eq!(
        entries[1].parent_agent_id.as_deref(),
        Some("parent-launch-session")
    );
}

#[test]
fn agent_message_does_not_answer_open_ask() {
    let (_dir, store) = store();
    let workspace = workspace();
    let agent = rimz::testkit::agent_state("claude", "sess-1", jiff::Timestamp::UNIX_EPOCH);
    let mut ask = rimz::transcript::TranscriptEntry::new(
        jiff::Timestamp::UNIX_EPOCH,
        rimz::ids::AgentKind::new_unchecked("claude"),
        rimz::ids::AgentSessionId::from("sess-1"),
        rimz::transcript::TranscriptKind::Ask,
        String::new(),
    );
    ask.id = Some(rimz::ids::AskId::parse("ask_0123456789abcdef").unwrap());
    rimz::transcript::append(store.paths(), &ask).unwrap();
    let message = rimz::message::MessageRecord::new(
        workspace.workspace_id.clone(),
        &agent,
        "new context".to_owned(),
        true,
        rimz::message::DeliveryGate::Done,
    );
    let mut started = recorded(LifecycleSignal::TurnStarted);
    started.waiting_cleared = true;
    started.observation.prompt =
        Some("Type: AGENT_MESSAGE\nFrom: @planner\nContent:\nnew context".to_owned());

    record_conversation(
        &workspace,
        &store,
        rimz::agents::definition_by_kind("claude").unwrap(),
        &started,
        conversation_input(None, &[], std::slice::from_ref(&message)),
    )
    .unwrap();

    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1].entry, rimz::transcript::TranscriptKind::Message);
    assert!(has_open_native_ask(&store, "claude", "sess-1"));
}

#[test]
fn prompt_without_waiting_transition_does_not_answer_stale_ask() {
    let (_dir, store) = store();
    let workspace = workspace();
    let mut ask = rimz::transcript::TranscriptEntry::new(
        jiff::Timestamp::UNIX_EPOCH,
        rimz::ids::AgentKind::new_unchecked("claude"),
        rimz::ids::AgentSessionId::from("sess-1"),
        rimz::transcript::TranscriptKind::Ask,
        String::new(),
    );
    ask.id = Some(rimz::ids::AskId::parse("ask_0123456789abcdef").unwrap());
    rimz::transcript::append(store.paths(), &ask).unwrap();
    let mut started = recorded(LifecycleSignal::TurnStarted);
    started.observation.prompt = Some("new task".to_owned());

    record_conversation(
        &workspace,
        &store,
        rimz::agents::definition_by_kind("claude").unwrap(),
        &started,
        conversation_input(None, &[], &[]),
    )
    .unwrap();

    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    assert_eq!(
        entries.last().unwrap().entry,
        rimz::transcript::TranscriptKind::Prompt
    );
    assert!(has_open_native_ask(&store, "claude", "sess-1"));
}

#[test]
fn idless_ask_does_not_capture_prompt() {
    let (_dir, store) = store();
    let workspace = workspace();
    let ask = rimz::transcript::TranscriptEntry::new(
        jiff::Timestamp::UNIX_EPOCH,
        rimz::ids::AgentKind::new_unchecked("claude"),
        rimz::ids::AgentSessionId::from("sess-1"),
        rimz::transcript::TranscriptKind::Ask,
        String::new(),
    );
    rimz::transcript::append(store.paths(), &ask).unwrap();
    let mut started = recorded(LifecycleSignal::TurnStarted);
    started.waiting_cleared = true;
    started.observation.prompt = Some("new task".to_owned());

    record_conversation(
        &workspace,
        &store,
        rimz::agents::definition_by_kind("claude").unwrap(),
        &started,
        conversation_input(None, &[], &[]),
    )
    .unwrap();

    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    assert_eq!(
        entries.last().unwrap().entry,
        rimz::transcript::TranscriptKind::Prompt
    );
    assert!(has_open_native_ask(&store, "claude", "sess-1"));
}

#[test]
fn prompt_after_answered_ask_starts_a_new_turn() {
    let (_dir, store) = store();
    let workspace = workspace();
    let ask_id = rimz::ids::AskId::parse("ask_0123456789abcdef").unwrap();
    let mut ask = rimz::transcript::TranscriptEntry::new(
        jiff::Timestamp::UNIX_EPOCH,
        rimz::ids::AgentKind::new_unchecked("claude"),
        rimz::ids::AgentSessionId::from("sess-1"),
        rimz::transcript::TranscriptKind::Ask,
        String::new(),
    );
    ask.id = Some(ask_id.clone());
    let mut answer = ask.clone();
    answer.at = "1970-01-01T00:00:01Z".parse().unwrap();
    answer.entry = rimz::transcript::TranscriptKind::Answer;
    rimz::transcript::append(store.paths(), &ask).unwrap();
    rimz::transcript::append(store.paths(), &answer).unwrap();
    let mut started = recorded(LifecycleSignal::TurnStarted);
    started.waiting_cleared = true;
    started.observation.prompt = Some("next task".to_owned());

    record_conversation(
        &workspace,
        &store,
        rimz::agents::definition_by_kind("claude").unwrap(),
        &started,
        conversation_input(None, &[], &[]),
    )
    .unwrap();

    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    assert_eq!(
        entries.last().unwrap().entry,
        rimz::transcript::TranscriptKind::Prompt
    );
    assert_eq!(entries.last().unwrap().text, "next task");
}

#[test]
fn duplicate_answer_race_preserves_prompt() {
    let (_dir, store) = store();
    let ask_id = rimz::ids::AskId::parse("ask_0123456789abcdef").unwrap();
    let prompt = rimz::transcript::TranscriptEntry::new(
        jiff::Timestamp::UNIX_EPOCH,
        rimz::ids::AgentKind::new_unchecked("claude"),
        rimz::ids::AgentSessionId::from("sess-1"),
        rimz::transcript::TranscriptKind::Prompt,
        "next task".to_owned(),
    );
    let mut answer = prompt.clone();
    answer.entry = rimz::transcript::TranscriptKind::Answer;
    answer.id = Some(ask_id);
    let mut existing_answer = answer.clone();
    existing_answer.at = "1970-01-01T00:00:01Z".parse().unwrap();
    rimz::transcript::append(store.paths(), &existing_answer).unwrap();

    append_turn_entry(store.paths(), &answer, Some(&prompt)).unwrap();

    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].entry, rimz::transcript::TranscriptKind::Prompt);
    assert_eq!(entries[1].entry, rimz::transcript::TranscriptKind::Answer);
}

#[test]
fn unheadered_system_batch_keeps_each_confirmed_message_causal() {
    let (_dir, store) = store();
    let workspace = workspace();
    let agent = rimz::testkit::agent_state("claude", "sess-1", jiff::Timestamp::UNIX_EPOCH);
    let first = rimz::message::MessageRecord::new(
        workspace.workspace_id.clone(),
        &agent,
        "\nfirst\n".to_owned(),
        true,
        rimz::message::DeliveryGate::Done,
    )
    .with_sender(rimz::message::MessageSender::System);
    let second = rimz::message::MessageRecord::new(
        workspace.workspace_id.clone(),
        &agent,
        "\nsecond\n".to_owned(),
        true,
        rimz::message::DeliveryGate::Done,
    )
    .with_sender(rimz::message::MessageSender::System);
    let mut started = recorded(LifecycleSignal::TurnStarted);
    started.observation.prompt = Some("first\n\n\n\nsecond".to_owned());

    record_conversation(
        &workspace,
        &store,
        rimz::agents::definition_by_kind("claude").unwrap(),
        &started,
        conversation_input(None, &[], &[first.clone(), second.clone()]),
    )
    .unwrap();

    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].message_id.as_ref(), Some(&first.message_id));
    assert_eq!(entries[1].message_id.as_ref(), Some(&second.message_id));
    assert_eq!(entries[0].from.as_deref(), Some("rimz"));
    assert_eq!(entries[1].from.as_deref(), Some("rimz"));
    assert_eq!(
        rimz::store::agent_context::read_one(store.runtime_paths(), "claude", "sess-1")
            .unwrap()
            .context
            .turn_opened_by,
        vec![first.message_id, second.message_id]
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
        LifecycleSignal::TurnInterrupted { turn_id: None },
    ] {
        let mut stopped = recorded(signal);
        stopped.observation.agent_id = Some(rimz::ids::AgentSessionId::from("conv-1"));
        record_conversation(
            &workspace,
            &store,
            rimz::agents::definition_by_kind("cursor").unwrap(),
            &stopped,
            conversation_input(None, &[], &[]),
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
