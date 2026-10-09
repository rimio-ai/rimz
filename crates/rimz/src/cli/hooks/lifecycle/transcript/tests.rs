use super::*;

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::TempDir::new().unwrap();
    let workspace_id =
        rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/hooks-test"));
    let paths = rimz::disk::paths::StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
    let runtime = rimz::disk::paths::RuntimePaths::under(workspace_id, dir.path()).unwrap();
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

fn conversation_frame(workspace: &ResolvedWorkspace) -> rimz::store::ingress::HookIngress {
    rimz::store::ingress::HookIngress {
        schema_version: "1".into(),
        event_id: rimz::ids::EventId::new(),
        ts: jiff::Timestamp::now(),
        source: rimz::ids::AgentKind::new_unchecked("claude"),
        event: Some("UserPromptSubmit".into()),
        payload: String::new(),
        cwd: workspace.project_root.clone(),
        hook_pid: std::process::id(),
        env: Default::default(),
    }
}

fn recorded(signal: LifecycleSignal) -> RecordedLifecycle {
    RecordedLifecycle {
        model_hint: None,
        observation: AgentLifecycleObservation::new(
            Some(rimz::ids::AgentSessionId::from("sess-1")),
            signal,
        ),
        receipt: Default::default(),
    }
}

struct TestConversationInput<'a> {
    assistant_message: Option<&'a str>,
    questions: &'a [rimz::transcript::AskQuestion],
    delivered: &'a [rimz::store::message::MessageRecord],
    run_id: Option<&'a rimz::RunId>,
}

fn record_conversation(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &AgentDefinition,
    recorded: &RecordedLifecycle,
    input: TestConversationInput<'_>,
) -> rimz::transcript::Result<()> {
    let sections = rimz::store::message::classify_submitted_prompt(
        recorded.observation.prompt.as_deref().unwrap_or_default(),
        &input.delivered.iter().collect::<Vec<_>>(),
        &[],
    );
    super::record_conversation(
        workspace,
        store,
        agent,
        recorded,
        ConversationInput {
            assistant_message: input.assistant_message,
            questions: input.questions,
            sections: &sections,
            run_id: input.run_id,
        },
    )
}

fn conversation_input<'a>(
    assistant_message: Option<&'a str>,
    questions: &'a [rimz::transcript::AskQuestion],
    delivered: &'a [rimz::store::message::MessageRecord],
) -> TestConversationInput<'a> {
    TestConversationInput {
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
fn claude_wrapped_stage_records_only_the_delivered_notice() {
    use rimz::store::message::{DeliveryGate, HarnessNotice, MessageRecord, MessageSender};
    use rimz::transcript::TranscriptKind;

    let (_dir, store) = store();
    let workspace = workspace();
    let agent = rimz::agents::definition_by_kind("claude").unwrap();
    let state = rimz::testkit::agent_state("claude", "sess-1", jiff::Timestamp::UNIX_EPOCH);
    let message = MessageRecord::new(
        workspace.workspace_id.clone(),
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
    let mut started = recorded(LifecycleSignal::TurnStarted { turn_id: None });
    started.observation.prompt = decoded.lifecycle().unwrap().prompt.clone();

    record_conversation(
        &workspace,
        &store,
        agent,
        &started,
        conversation_input(None, &[], std::slice::from_ref(&message)),
    )
    .unwrap();

    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].entry, TranscriptKind::Wait);
    assert_eq!(entries[0].from.as_deref(), Some("@rimz"));
    assert_eq!(entries[0].message_id.as_ref(), Some(&message.message_id));
    assert!(
        !entries
            .iter()
            .any(|entry| entry.entry == TranscriptKind::Prompt)
    );
}

#[test]
fn claude_human_paste_keeps_typed_and_pasted_text_in_one_prompt() {
    let (_dir, store) = store();
    let workspace = workspace();
    let agent = rimz::agents::definition_by_kind("claude").unwrap();
    let payload = serde_json::json!({
        "session_id": "sess-1",
        "prompt": "look at this\n\n<pasted_content id=\"a\">\npanic at foo.rs:1\n</pasted_content id=\"a\">\n\nwhat now?",
    });
    let decoded = agent.decode_hook("UserPromptSubmit", &payload).unwrap();
    let mut started = recorded(LifecycleSignal::TurnStarted { turn_id: None });
    started.observation.prompt = decoded.lifecycle().unwrap().prompt.clone();

    record_conversation(
        &workspace,
        &store,
        agent,
        &started,
        conversation_input(None, &[], &[]),
    )
    .unwrap();

    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].entry, rimz::transcript::TranscriptKind::Prompt);
    assert_eq!(
        entries[0].text,
        "look at this\n\npanic at foo.rs:1\n\nwhat now?"
    );
    assert!(!entries[0].text.contains("pasted_content"));
    assert_eq!(entries[0].enqueued_at, None);
}

#[test]
fn one_terminal_extraction_is_shared_when_run_and_conversation_both_need_it() {
    let calls = std::cell::Cell::new(0);
    let terminal = recorded(LifecycleSignal::TurnEnded {
        errored: false,
        parked_on_background: false,
        turn_id: None,
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
    let mut first = rimz::store::message::MessageRecord::new(
        workspace.workspace_id.clone(),
        &agent,
        "first".to_owned(),
        rimz::store::message::DeliveryGate::Done,
    )
    .with_in_reply_to(vec![parent.clone()]);
    first.enqueued_at = jiff::Timestamp::now()
        .checked_sub(jiff::SignedDuration::from_secs(120))
        .unwrap();
    let second = rimz::store::message::MessageRecord::new(
        workspace.workspace_id.clone(),
        &agent,
        "second".to_owned(),
        rimz::store::message::DeliveryGate::Done,
    );
    let mut started = recorded(LifecycleSignal::TurnStarted { turn_id: None });
    started.observation.prompt = rimz::agents::SanitizedPrompt::new(Some(
        "Type: AGENT_MESSAGE\nFrom: @calm-fox (planner)\nContent:\nfirst\n\nType: AGENT_MESSAGE\nFrom: @reviewer\nContent:\nsecond",
    ));

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
    assert_eq!(entries[0].from.as_deref(), Some("@calm-fox"));
    assert_eq!(entries[0].message_id.as_ref(), Some(&first.message_id));
    assert_eq!(entries[0].enqueued_at, Some(first.enqueued_at));
    assert!(entries[0].at > entries[0].enqueued_at.unwrap());
    assert_eq!(entries[0].reply_to, vec![parent]);
    assert_eq!(entries[1].message_id.as_ref(), Some(&second.message_id));
    assert_eq!(entries[1].enqueued_at, Some(second.enqueued_at));
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
            turn_id: None,
        }),
        conversation_input(Some("  done \n"), &[], &[]),
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
            Some(" \n "),
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
        vec![first.message_id.clone(), second.message_id.clone()]
    );

    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    assert_eq!(entries[2].text, "done");
    assert_eq!(entries[3].text, "");
    assert_eq!(entries[3].questions[0].question, "Ship?");
    assert!(entries[3].questions[0].options.is_empty());
    assert!(!entries[3].questions[0].multi_select);
    assert!(!entries[3].questions[0].has_option_previews);
    assert_eq!(
        entries[3].id.as_ref().unwrap().as_str(),
        "ask_0123456789abcdef"
    );
    for (signal, text) in [
        (
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
                turn_id: None,
            },
            None,
        ),
        (
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
                turn_id: None,
            },
            Some(" \n "),
        ),
        (
            LifecycleSignal::AwaitingInput {
                kind: rimz::agents::AskKind::Question,
                ask_id: None,
                detail: None,
                native_key: None,
            },
            Some("ignored without questions"),
        ),
        (
            LifecycleSignal::TurnInterrupted { turn_id: None },
            Some("interrupted text"),
        ),
    ] {
        record_conversation(
            &workspace,
            &store,
            rimz::agents::definition_by_kind("claude").unwrap(),
            &recorded(signal),
            conversation_input(text, &[], &[]),
        )
        .unwrap();
        assert_eq!(
            rimz::transcript::read_all(store.paths()).unwrap().len(),
            entries.len()
        );
    }

    let mut hand_typed = recorded(LifecycleSignal::TurnStarted { turn_id: None });
    hand_typed.receipt.waiting_cleared = true;
    hand_typed.observation.prompt = rimz::agents::SanitizedPrompt::new(Some("typed directly"));
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
    assert_eq!(answer.enqueued_at, None);
}

#[test]
fn harness_notices_retain_delivery_attribution_in_transcripts() {
    use rimz::store::message::{HarnessNotice, MessageSender};
    use rimz::transcript::TranscriptKind;

    for (sender, header, kind) in [
        (
            MessageSender::Subagent {
                kind: rimz::ids::AgentKind::new_unchecked("codex"),
                name: "lucid-atlas".to_owned(),
            },
            "SUBAGENT_REPORT",
            TranscriptKind::SubagentReport,
        ),
        (
            MessageSender::Harness {
                notice: HarnessNotice::Stage,
            },
            "STAGE",
            TranscriptKind::Wait,
        ),
    ] {
        let (_dir, store) = store();
        let workspace = workspace();
        let agent = rimz::testkit::agent_state("claude", "sess-1", jiff::Timestamp::UNIX_EPOCH);
        let message = rimz::store::message::MessageRecord::new(
            workspace.workspace_id.clone(),
            &agent,
            "child result".to_owned(),
            rimz::store::message::DeliveryGate::Done,
        )
        .with_sender(sender);
        let mut started = recorded(LifecycleSignal::TurnStarted { turn_id: None });
        started.observation.prompt = rimz::agents::SanitizedPrompt::new(Some(&format!(
            "Type: {header}\nFrom: @rimz\nContent:\nchild result"
        )));

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
        assert_eq!(entries[0].entry, kind);
        assert_eq!(entries[0].from.as_deref(), Some("@rimz"));
        assert_eq!(entries[0].text, "child result");
        assert_eq!(entries[0].message_id.as_ref(), Some(&message.message_id));
    }
}

#[test]
fn mixed_submit_records_stray_text_as_direct_input() {
    let (_dir, store) = store();
    let workspace = workspace();
    let agent = rimz::testkit::agent_state("claude", "sess-1", jiff::Timestamp::UNIX_EPOCH);
    let message = rimz::store::message::MessageRecord::new(
        workspace.workspace_id.clone(),
        &agent,
        "child result".to_owned(),
        rimz::store::message::DeliveryGate::Done,
    )
    .with_sender(rimz::store::message::MessageSender::Subagent {
        kind: rimz::ids::AgentKind::new_unchecked("codex"),
        name: "lucid-atlas".to_owned(),
    });
    let mut started = recorded(LifecycleSignal::TurnStarted { turn_id: None });
    started.observation.prompt = rimz::agents::SanitizedPrompt::new(Some(
        "Type: SUBAGENT_REPORT\nFrom: @lucid-atlas\nContent:\nchild resultdo you still",
    ));

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
    assert_eq!(entries[1].enqueued_at, None);
}

#[test]
fn user_message_header_records_prompt_without_envelope() {
    let (_dir, store) = store();
    let workspace = workspace();
    let agent = rimz::testkit::agent_state("claude", "sess-1", jiff::Timestamp::UNIX_EPOCH);
    let message = rimz::store::message::MessageRecord::new(
        workspace.workspace_id.clone(),
        &agent,
        "from a human".to_owned(),
        rimz::store::message::DeliveryGate::Done,
    );
    let mut started = recorded(LifecycleSignal::TurnStarted { turn_id: None });
    started.observation.prompt = rimz::agents::SanitizedPrompt::new(Some(
        "Type: USER_MESSAGE\nFrom: @user\nContent:\nfrom a human",
    ));

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
            parent_agent_kind: Some(rimz::ids::AgentKind::new_unchecked("codex")),
            launch_depth: Some(1),
            channel: Some("chat".to_owned()),
            ..Default::default()
        },
    );

    let mut run = rimz::store::run::RunRecord::new(
        workspace.workspace_id.clone(),
        rimz::ids::AgentKind::new_unchecked("claude"),
        rimz::agents::PermissionMode::Auto,
        "inspect the infra".to_owned(),
        workspace.worktree_root.clone(),
    );
    run.subagent = true;
    rimz::harness::run::create(store.paths(), &run).unwrap();
    let mut started = recorded(LifecycleSignal::TurnStarted { turn_id: None });
    started.observation.agent_id = Some(rimz::ids::AgentSessionId::from("child-session"));
    let submitted = format!(
        "{}\n\n{}",
        run.prompt,
        rimz::harness::launch_reminders::subagent_reminder()
    );
    started.observation.prompt = rimz::agents::SanitizedPrompt::new(Some(&submitted));

    record_conversation(
        &workspace,
        &store,
        rimz::agents::definition_by_kind("claude").unwrap(),
        &started,
        TestConversationInput {
            run_id: Some(&run.run_id),
            ..conversation_input(None, &[], &[])
        },
    )
    .unwrap();

    let mut later = started;
    later.observation.prompt = rimz::agents::SanitizedPrompt::new(Some("follow-up from the human"));
    record_conversation(
        &workspace,
        &store,
        rimz::agents::definition_by_kind("claude").unwrap(),
        &later,
        TestConversationInput {
            run_id: Some(&run.run_id),
            ..conversation_input(None, &[], &[])
        },
    )
    .unwrap();

    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].entry, rimz::transcript::TranscriptKind::Message);
    assert_eq!(entries[0].from.as_deref(), Some("@planner"));
    assert_eq!(entries[0].text, "inspect the infra");
    assert_eq!(
        entries[0].parent_agent_id.as_deref(),
        Some("parent-launch-session")
    );
    assert_eq!(
        entries[0].parent_agent_kind,
        Some(rimz::ids::AgentKind::new_unchecked("codex"))
    );
    assert_eq!(entries[1].entry, rimz::transcript::TranscriptKind::Prompt);
    assert_eq!(entries[1].from, None);
    assert_eq!(
        entries[1].parent_agent_id.as_deref(),
        Some("parent-launch-session")
    );

    let original_run_id = run.run_id.clone();
    run.run_id = rimz::RunId::new();
    run.retry_of = Some(original_run_id);
    run.loop_task = Some("review".to_owned());
    run.prompt =
        "inspect the infra\n\n<previous-attempt-failure>\ntry again\n</previous-attempt-failure>"
            .to_owned();
    rimz::harness::run::create(store.paths(), &run).unwrap();
    later.observation.prompt = rimz::agents::SanitizedPrompt::new(Some(&format!(
        "{}\n\n{}",
        run.prompt,
        rimz::harness::launch_reminders::subagent_reminder()
    )));
    record_conversation(
        &workspace,
        &store,
        rimz::agents::definition_by_kind("claude").unwrap(),
        &later,
        TestConversationInput {
            run_id: Some(&run.run_id),
            ..conversation_input(None, &[], &[])
        },
    )
    .unwrap();
    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[2].entry, rimz::transcript::TranscriptKind::Message);
    assert_eq!(entries[2].from.as_deref(), Some("@planner"));
    assert_eq!(entries[2].text, "inspect the infra");
}

fn peer_hook_fixture(store: &Store) -> rimz::agents::AgentState {
    append_launched_agent(
        store,
        "codex",
        "launcher",
        Some("launcher-id"),
        "launcher",
        Default::default(),
    );
    append_launched_agent(
        store,
        "claude",
        "peer-session",
        Some("peer-id"),
        "peer",
        rimz::agents::LaunchParams {
            launched_by: Some(Box::new(rimz::agents::LaunchedBy {
                kind: rimz::ids::AgentKind::new_unchecked("codex"),
                agent_id: "launcher-id".into(),
            })),
            ..Default::default()
        },
    );
    agent_state(
        store,
        rimz::agents::definition_by_kind("claude").unwrap(),
        &"peer-session".into(),
    )
    .unwrap()
}

fn feed_peer_hook(store: &Store, event: &str, fields: serde_json::Value) {
    let adapter = rimz::agents::definition_by_kind("claude").unwrap();
    let mut payload =
        serde_json::json!({"session_id": "peer-session", "cwd": "/tmp/hooks-test/chat"});
    payload
        .as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    let mut decoded = adapter.decode_hook(event, &payload).unwrap();
    super::super::handle_lifecycle_hook(
        &workspace(),
        store,
        adapter,
        &mut decoded,
        &payload,
        rimz::agents::HookIngressOwner::agent(Some(std::process::id())),
        &crate::cli::GlobalFlags {
            root: None,
            mux: None,
            zellij: false,
            tmux: false,
            color: crate::cli::ColorWhen::Never,
        },
    )
    .unwrap();
}

#[test]
fn peer_launch_hook_claims_prompt_but_later_identical_human_turn_does_not() {
    let (_dir, store) = store();
    let peer = peer_hook_fixture(&store);
    let run = rimz::harness::run::create_peer_prompt(
        store.paths(),
        &peer,
        None,
        rimz::agents::definition_by_kind("claude").unwrap(),
        "inspect the infra",
        &workspace().worktree_root,
        rimz::store::run::ReportTo::Launcher,
    )
    .unwrap()
    .unwrap();
    feed_peer_hook(
        &store,
        "UserPromptSubmit",
        serde_json::json!({"prompt":"inspect the infra"}),
    );
    let running = rimz::harness::run::load(store.paths(), &run.run_id).unwrap();
    assert_eq!(running.status, rimz::store::run::RunStatus::Running);
    assert_eq!(running.agent_id, Some(peer.agent_id.clone()));
    let state_root = super::super::user_input_state_root(&store).unwrap();
    assert!(rimz::agents::spending::user_input::load_in(state_root).is_empty());
    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    assert_eq!(entries[0].entry, rimz::transcript::TranscriptKind::Message);
    assert_eq!(entries[0].from.as_deref(), Some("@launcher"));
    feed_peer_hook(
        &store,
        "Stop",
        serde_json::json!({"last_assistant_message":"finished"}),
    );
    let done = rimz::harness::run::load(store.paths(), &run.run_id).unwrap();
    assert_eq!(done.status, rimz::store::run::RunStatus::Completed);
    let response = rimz::harness::run::response_path(store.paths(), &done).unwrap();
    let bytes = std::fs::read(&response).unwrap();
    feed_peer_hook(
        &store,
        "UserPromptSubmit",
        serde_json::json!({"prompt":"inspect the infra"}),
    );
    feed_peer_hook(
        &store,
        "Stop",
        serde_json::json!({"last_assistant_message":"human answer"}),
    );
    assert_eq!(rimz::harness::run::list(store.paths()).unwrap(), vec![done]);
    assert_eq!(std::fs::read(response).unwrap(), bytes);
    assert_eq!(
        rimz::agents::spending::user_input::load_in(state_root).len(),
        1
    );
    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    let human = entries
        .iter()
        .rev()
        .find(|entry| entry.text == "inspect the infra")
        .unwrap();
    assert_eq!(human.entry, rimz::transcript::TranscriptKind::Prompt);
    assert_eq!(human.from, None);
}

#[test]
fn peer_hook_enrolls_launcher_delivery_and_fails_it_on_session_end() {
    use rimz::store::message::{DeliveryGate, MessageRecord, MessageSender};
    let (_dir, store) = store();
    let peer = peer_hook_fixture(&store);
    let message = MessageRecord::new(
        store.paths().workspace_id.clone(),
        &peer,
        "next task".into(),
        DeliveryGate::Done,
    )
    .with_sender(MessageSender::Agent {
        kind: rimz::ids::AgentKind::new_unchecked("codex"),
        agent_id: Some("launcher-id".into()),
        name: Some("launcher".into()),
        profile: None,
        role: None,
        channel: None,
    });
    store.queue_message(&message, "session").unwrap();
    store
        .record_sent_batch(std::slice::from_ref(&message), "session")
        .unwrap();
    feed_peer_hook(
        &store,
        "UserPromptSubmit",
        serde_json::json!({"prompt":"Type: AGENT_MESSAGE\nFrom: @launcher\nContent:\nnext task"}),
    );
    let run = rimz::harness::run::open_peer_run(store.paths(), &peer)
        .unwrap()
        .expect("launcher delivery enrolls a run");
    assert_eq!(
        run.peer.as_ref().unwrap().opened_by,
        vec![message.message_id]
    );
    assert_eq!(run.prompt, "next task");
    assert_eq!(run.status, rimz::store::run::RunStatus::Running);
    feed_peer_hook(&store, "SessionEnd", serde_json::json!({}));
    assert_eq!(
        rimz::harness::run::load(store.paths(), &run.run_id)
            .unwrap()
            .status,
        rimz::store::run::RunStatus::Failed
    );
}

#[test]
fn run_briefs_keep_loop_human_and_unresolved_parent_origins() {
    for (subagent, loop_task, expected_from) in [
        (false, Some("review"), Some("rimz")),
        (false, None, None),
        (true, None, Some("rimz")),
    ] {
        let (_dir, store) = store();
        let workspace = workspace();
        let mut run = rimz::store::run::RunRecord::new(
            workspace.workspace_id.clone(),
            rimz::ids::AgentKind::new_unchecked("claude"),
            rimz::agents::PermissionMode::Auto,
            "inspect the infra".to_owned(),
            workspace.worktree_root.clone(),
        );
        run.subagent = subagent;
        run.loop_task = loop_task.map(ToOwned::to_owned);
        rimz::harness::run::create(store.paths(), &run).unwrap();
        let mut started = recorded(LifecycleSignal::TurnStarted { turn_id: None });
        started.observation.prompt = rimz::agents::SanitizedPrompt::new(Some(&run.prompt));
        record_conversation(
            &workspace,
            &store,
            rimz::agents::definition_by_kind("claude").unwrap(),
            &started,
            TestConversationInput {
                run_id: Some(&run.run_id),
                ..conversation_input(None, &[], &[])
            },
        )
        .unwrap();
        let entries = rimz::transcript::read_all(store.paths()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entry, rimz::transcript::TranscriptKind::Prompt);
        assert_eq!(entries[0].from.as_deref(), expected_from);
        assert_eq!(entries[0].text, run.prompt);
        if expected_from.is_some() {
            let mut ask = rimz::transcript::TranscriptEntry::new(
                jiff::Timestamp::now(),
                run.kind.clone(),
                rimz::ids::AgentSessionId::from("sess-1"),
                rimz::transcript::TranscriptKind::Ask,
                String::new(),
            );
            ask.id = Some(rimz::ids::AskId::parse("ask_0123456789abcdef").unwrap());
            rimz::transcript::append(store.paths(), &ask).unwrap();
            started.receipt.waiting_cleared = true;
            record_conversation(
                &workspace,
                &store,
                rimz::agents::definition_by_kind("claude").unwrap(),
                &started,
                TestConversationInput {
                    run_id: Some(&run.run_id),
                    ..conversation_input(None, &[], &[])
                },
            )
            .unwrap();
            let entries = rimz::transcript::read_all(store.paths()).unwrap();
            assert_eq!(entries.len(), 3);
            assert_eq!(entries[2].entry, rimz::transcript::TranscriptKind::Prompt);
            assert_eq!(entries[2].from.as_deref(), expected_from);
            assert!(has_open_native_ask(&store, "claude", "sess-1"));
        }
    }
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
    let message = rimz::store::message::MessageRecord::new(
        workspace.workspace_id.clone(),
        &agent,
        "new context".to_owned(),
        rimz::store::message::DeliveryGate::Done,
    );
    let mut started = recorded(LifecycleSignal::TurnStarted { turn_id: None });
    started.receipt.waiting_cleared = true;
    started.observation.prompt = rimz::agents::SanitizedPrompt::new(Some(
        "Type: AGENT_MESSAGE\nFrom: @planner\nContent:\nnew context",
    ));

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

    use rimz::store::message::PromptSection;
    use rimz::transcript::SectionOrigin;
    let sections = [
        PromptSection {
            text: "attributed".into(),
            origin: SectionOrigin::Agent("@planner".into()),
            record: None,
        },
        PromptSection {
            text: "harness".into(),
            origin: SectionOrigin::Harness,
            record: None,
        },
        PromptSection {
            text: message.text.clone(),
            origin: SectionOrigin::Human,
            record: Some(&message),
        },
        PromptSection {
            text: "first human".into(),
            origin: SectionOrigin::Human,
            record: None,
        },
        PromptSection {
            text: "second human".into(),
            origin: SectionOrigin::Human,
            record: None,
        },
    ];
    super::record_conversation(
        &workspace,
        &store,
        rimz::agents::definition_by_kind("claude").unwrap(),
        &started,
        ConversationInput {
            assistant_message: None,
            questions: &[],
            sections: &sections,
            run_id: None,
        },
    )
    .unwrap();
    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    use rimz::transcript::TranscriptKind;
    assert_eq!(
        entries[2..]
            .iter()
            .map(|entry| entry.entry)
            .collect::<Vec<_>>(),
        vec![
            TranscriptKind::Message,
            TranscriptKind::Prompt,
            TranscriptKind::Prompt,
            TranscriptKind::Answer,
            TranscriptKind::Prompt
        ]
    );
    assert_eq!(entries[4].message_id, Some(message.message_id));
    assert_eq!(entries[5].id, ask.id);
    assert_eq!(entries[5].text, "first human");
    assert!(!has_open_native_ask(&store, "claude", "sess-1"));
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
    let mut started = recorded(LifecycleSignal::TurnStarted { turn_id: None });
    started.observation.prompt = rimz::agents::SanitizedPrompt::new(Some("new task"));

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
    let mut started = recorded(LifecycleSignal::TurnStarted { turn_id: None });
    started.receipt.waiting_cleared = true;
    started.observation.prompt = rimz::agents::SanitizedPrompt::new(Some("new task"));

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
    let mut started = recorded(LifecycleSignal::TurnStarted { turn_id: None });
    started.receipt.waiting_cleared = true;
    started.observation.prompt = rimz::agents::SanitizedPrompt::new(Some("next task"));

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
fn replay_keeps_a_prompt_that_answered_a_native_ask() {
    let (_dir, store) = store();
    let workspace = workspace();
    let agent = rimz::agents::definition_by_kind("claude").unwrap();
    let mut ask = rimz::transcript::TranscriptEntry::new(
        jiff::Timestamp::UNIX_EPOCH,
        agent.spec().kind_id(),
        "sess-1".into(),
        rimz::transcript::TranscriptKind::Ask,
        String::new(),
    );
    ask.id = Some(rimz::ids::AskId::parse("ask_0123456789abcdef").unwrap());
    rimz::transcript::append(store.paths(), &ask).unwrap();
    let mut started = recorded(LifecycleSignal::TurnStarted { turn_id: None });
    started.receipt.waiting_cleared = true;
    started.observation.prompt = rimz::agents::SanitizedPrompt::new(Some("safe"));
    let frame = conversation_frame(&workspace);
    let scoped = store.for_hook_ingress(frame.event_id.clone(), frame.ts);
    for attempt in [
        rimz::harness::hook_drain::FrameAttempt::First,
        rimz::harness::hook_drain::FrameAttempt::Replay,
    ] {
        rimz::harness::hook_drain::with_frame_env(&frame, attempt, || {
            record_conversation(
                &workspace,
                &scoped,
                agent,
                &started,
                conversation_input(None, &[], &[]),
            )
        })
        .unwrap();
    }
    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    assert_eq!(
        entries.len(),
        2,
        "replay must not turn the answer into a prompt"
    );
    assert_eq!(entries[1].entry, rimz::transcript::TranscriptKind::Answer);
    assert_eq!(entries[1].ingress.as_ref(), Some(&frame.event_id));
}

#[test]
fn replay_finishes_a_partially_recorded_prompt_batch() {
    use rimz::store::message::PromptSection;
    use rimz::transcript::{SectionOrigin, TranscriptKind};

    let (_dir, store) = store();
    let workspace = workspace();
    let agent = rimz::agents::definition_by_kind("claude").unwrap();
    let frame = conversation_frame(&workspace);
    let scoped = store.for_hook_ingress(frame.event_id.clone(), frame.ts);
    let mut first = rimz::transcript::TranscriptEntry::new(
        frame.ts,
        agent.spec().kind_id(),
        "sess-1".into(),
        TranscriptKind::Prompt,
        "first".into(),
    );
    first.ingress = Some(frame.event_id.clone());
    rimz::transcript::append(store.paths(), &first).unwrap();
    let sections = ["first", "second"].map(|text| PromptSection {
        text: text.into(),
        origin: SectionOrigin::Human,
        record: None,
    });
    for _ in 0..2 {
        rimz::harness::hook_drain::with_frame_env(
            &frame,
            rimz::harness::hook_drain::FrameAttempt::Replay,
            || {
                super::record_conversation(
                    &workspace,
                    &scoped,
                    agent,
                    &recorded(LifecycleSignal::TurnStarted { turn_id: None }),
                    ConversationInput {
                        assistant_message: None,
                        questions: &[],
                        sections: &sections,
                        run_id: None,
                    },
                )
            },
        )
        .unwrap();
    }
    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    assert_eq!(
        entries.len(),
        2,
        "replay must append only the missing section"
    );
    assert_eq!(entries[0], first);
    assert_eq!(entries[1].text, "second");
    assert_eq!(entries[1].entry, TranscriptKind::Prompt);
    assert_eq!(entries[1].ingress.as_ref(), Some(&frame.event_id));
}

#[test]
fn replay_does_not_count_a_queued_answer_as_a_prompt_entry() {
    let (_dir, store) = store();
    let workspace = workspace();
    let agent = rimz::agents::definition_by_kind("claude").unwrap();
    let frame = conversation_frame(&workspace);
    let scoped = store.for_hook_ingress(frame.event_id.clone(), frame.ts);
    let mut started = recorded(LifecycleSignal::TurnStarted { turn_id: None });
    started.observation.prompt = rimz::agents::SanitizedPrompt::new(Some("new task"));
    started.observation.ask_queue = Some(rimz::agents::AskQueueEdit {
        answered: vec![rimz::agents::AnsweredQuestion {
            ask_id: Some(rimz::ids::AskId::parse("ask_0123456789abcdef").unwrap()),
            native_key: "queued-question".into(),
            answer: "safe".into(),
        }],
        ..Default::default()
    });
    for _ in 0..2 {
        rimz::harness::hook_drain::with_frame_env(
            &frame,
            rimz::harness::hook_drain::FrameAttempt::Replay,
            || {
                record_conversation(
                    &workspace,
                    &scoped,
                    agent,
                    &started,
                    conversation_input(None, &[], &[]),
                )
            },
        )
        .unwrap();
    }
    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    assert_eq!(
        entries.len(),
        2,
        "the queued answer must not consume the prompt"
    );
    assert_eq!(entries[0].entry, rimz::transcript::TranscriptKind::Answer);
    assert_eq!(entries[1].entry, rimz::transcript::TranscriptKind::Prompt);
    assert_eq!(entries[1].text, "new task");
    assert_eq!(entries[1].ingress.as_ref(), Some(&frame.event_id));
}

#[test]
fn replay_does_not_count_a_queued_ask_as_the_blocking_ask() {
    use rimz::transcript::{AskQuestion, TranscriptKind};

    let (_dir, store) = store();
    let workspace = workspace();
    let agent = rimz::agents::definition_by_kind("claude").unwrap();
    let frame = conversation_frame(&workspace);
    let scoped = store.for_hook_ingress(frame.event_id.clone(), frame.ts);
    let queued_id = rimz::ids::AskId::parse("ask_0123456789abcdef").unwrap();
    let blocking_id = rimz::ids::AskId::parse("ask_fedcba9876543210").unwrap();
    let question = AskQuestion {
        question: "Choose?".into(),
        options: vec![],
        multi_select: false,
        has_option_previews: false,
    };
    let mut queued = rimz::transcript::TranscriptEntry::new(
        frame.ts,
        agent.spec().kind_id(),
        "sess-1".into(),
        TranscriptKind::Ask,
        String::new(),
    );
    queued.id = Some(queued_id.clone());
    queued.ingress = Some(frame.event_id.clone());
    queued.questions = vec![question.clone()];
    rimz::transcript::append(store.paths(), &queued).unwrap();
    let mut waiting = recorded(LifecycleSignal::AwaitingInput {
        kind: rimz::agents::AskKind::Question,
        ask_id: Some(blocking_id.clone()),
        detail: None,
        native_key: None,
    });
    waiting.observation.ask_queue = Some(rimz::agents::AskQueueEdit {
        queued: vec![rimz::agents::QueuedQuestion {
            ask_id: Some(queued_id),
            native_key: "queued-question".into(),
            detail: String::new(),
            question: question.clone(),
        }],
        ..Default::default()
    });
    for _ in 0..2 {
        rimz::harness::hook_drain::with_frame_env(
            &frame,
            rimz::harness::hook_drain::FrameAttempt::Replay,
            || {
                record_conversation(
                    &workspace,
                    &scoped,
                    agent,
                    &waiting,
                    conversation_input(None, std::slice::from_ref(&question), &[]),
                )
            },
        )
        .unwrap();
    }
    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    assert_eq!(
        entries.len(),
        2,
        "the queued ask must not consume the blocking ask"
    );
    assert_eq!(entries[0], queued);
    assert_eq!(entries[1].id, Some(blocking_id));
    assert_eq!(entries[1].ingress.as_ref(), Some(&frame.event_id));
}

#[test]
fn unheadered_system_batch_keeps_each_confirmed_message_causal() {
    let (_dir, store) = store();
    let workspace = workspace();
    let agent = rimz::testkit::agent_state("claude", "sess-1", jiff::Timestamp::UNIX_EPOCH);
    let first = rimz::store::message::MessageRecord::new(
        workspace.workspace_id.clone(),
        &agent,
        "\nfirst\n".to_owned(),
        rimz::store::message::DeliveryGate::Done,
    )
    .with_sender(rimz::store::message::MessageSender::System);
    let second = rimz::store::message::MessageRecord::new(
        workspace.workspace_id.clone(),
        &agent,
        "\nsecond\n".to_owned(),
        rimz::store::message::DeliveryGate::Done,
    )
    .with_sender(rimz::store::message::MessageSender::System);
    let mut started = recorded(LifecycleSignal::TurnStarted { turn_id: None });
    started.observation.prompt = rimz::agents::SanitizedPrompt::new(Some("first\n\n\n\nsecond"));

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
fn in_flight_turn_openers_keep_origin_causality_and_spend() {
    use rimz::store::message::{
        DeliveryGate, MessageBody, MessageRecord, MessageSender, MessageStatus,
        classify_submitted_prompt,
    };
    use rimz::transcript::TranscriptKind;
    let agent_sender = MessageSender::Agent {
        agent_id: None,
        kind: rimz::ids::AgentKind::new_unchecked("codex"),
        name: None,
        profile: None,
        role: Some("coder".to_owned()),
        channel: None,
    };
    for (sender, body, kind, from, user_inputs) in [
        (
            MessageSender::System,
            MessageBody::Command,
            TranscriptKind::Prompt,
            Some("rimz"),
            0,
        ),
        (
            MessageSender::Human,
            MessageBody::Command,
            TranscriptKind::Prompt,
            None,
            1,
        ),
        (
            agent_sender,
            MessageBody::Command,
            TranscriptKind::Message,
            Some("@coder"),
            0,
        ),
        (
            MessageSender::System,
            MessageBody::Prompt,
            TranscriptKind::Prompt,
            Some("rimz"),
            0,
        ),
    ] {
        let (dir, store) = store();
        let workspace = workspace();
        let agent = rimz::agents::definition_by_kind("claude").unwrap();
        let state = rimz::testkit::agent_state("claude", "sess-1", jiff::Timestamp::UNIX_EPOCH);
        let mut message = MessageRecord::new(
            workspace.workspace_id.clone(),
            &state,
            "/compact".to_owned(),
            DeliveryGate::Done,
        )
        .with_sender(sender)
        .with_body(body);
        message.status = MessageStatus::Sent;
        store.queue_message(&message, "session").unwrap();
        let mut started = recorded(LifecycleSignal::TurnStarted { turn_id: None });
        started.observation.prompt = rimz::agents::SanitizedPrompt::new(Some("/compact"));
        let in_flight = in_flight_messages_for_lifecycle(&store, agent, &started);
        assert_eq!(in_flight.len(), 1);
        if body == MessageBody::Command {
            assert!(
                confirm_sent_message_for_lifecycle(&store, agent, &started, &workspace).is_empty()
            );
        }
        let sections =
            classify_submitted_prompt("/compact", &[], &in_flight.iter().collect::<Vec<_>>());
        super::record_conversation(
            &workspace,
            &store,
            agent,
            &started,
            ConversationInput {
                assistant_message: None,
                questions: &[],
                sections: &sections,
                run_id: None,
            },
        )
        .unwrap();
        record_user_input_for_lifecycle(
            &workspace,
            agent,
            &started,
            &sections,
            &[],
            false,
            Some(dir.path()),
        );
        let entries = rimz::transcript::read_all(store.paths()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entry, kind);
        assert_eq!(entries[0].from.as_deref(), from);
        assert_eq!(entries[0].message_id.as_ref(), Some(&message.message_id));
        assert_eq!(
            store.list_messages().unwrap()[0].status,
            MessageStatus::Sent
        );
        assert_eq!(
            rimz::store::agent_context::read_one(store.runtime_paths(), "claude", "sess-1")
                .unwrap()
                .context
                .turn_opened_by,
            vec![message.message_id]
        );
        assert_eq!(
            rimz::agents::spending::user_input::load_in(dir.path()).len(),
            user_inputs
        );
        let typed = classify_submitted_prompt("real typed prompt", &[], &[]);
        record_user_input_for_lifecycle(
            &workspace,
            agent,
            &started,
            &typed,
            &[],
            false,
            Some(dir.path()),
        );
        assert_eq!(
            rimz::agents::spending::user_input::load_in(dir.path()).len(),
            user_inputs + 1
        );
    }
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
            turn_id: None,
        },
        LifecycleSignal::TurnEnded {
            errored: true,
            parked_on_background: false,
            turn_id: None,
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
