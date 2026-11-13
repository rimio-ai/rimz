use super::*;

fn room() -> Env {
    let env = Env::new();
    env.record(&env.project_root);
    feed_claude_hook(
        &env,
        json!({"hook_event_name": "SessionStart", "session_id": "raised-session",
            "source": "startup"}),
    );
    feed_claude_hook(
        &env,
        json!({"hook_event_name": "UserPromptSubmit", "session_id": "raised-session",
            "prompt": "work"}),
    );
    env
}

#[test]
fn outside_raise_after_a_prompt_resumes_parked_work_uses_the_seen_prompt() {
    use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
    let env = room();
    let store = env.store();
    let before = store.snapshot_cached().unwrap().agents[0].turn_started_at;
    let parked = AgentLifecycleObservation::new(
        Some("raised-session".into()),
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: true,
            turn_id: None,
        },
    );
    store
        .append_event(&rimz::store::event::EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            "rimz-test",
            "claude",
            "Stop",
            &parked,
        ))
        .unwrap();
    feed_claude_hook(
        &env,
        json!({"hook_event_name": "UserPromptSubmit",
        "session_id": "raised-session", "prompt": "continue"}),
    );
    feed_claude_hook(
        &env,
        json!({"hook_event_name": "Stop",
        "session_id": "raised-session", "last_assistant_message": "Ready."}),
    );
    let snapshot = store.snapshot_cached().unwrap();
    assert_eq!(snapshot.agents[0].turn_started_at, before);
    assert!(snapshot.agents[0].last_prompt_at > before);
    let output = env
        .rimz()
        .args(["asks", "raise", "@claude", "Review?"])
        .bounded_output()
        .unwrap();
    assert!(
        output.status.success(),
        "a fresh raise must accept the prompt it saw: {output:?}"
    );
    assert_eq!(listed_asks(&env)[0]["context"], "Review?");
}

#[test]
fn outside_raise_without_text_refuses_a_reply_from_before_the_latest_prompt() {
    let env = room();
    let store = env.store();
    rimz::transcript::append(
        store.paths(),
        &TranscriptEntry::new(
            jiff::Timestamp::now() - std::time::Duration::from_secs(60),
            rimz::ids::AgentKind::new_unchecked("claude"),
            "raised-session".into(),
            TranscriptKind::Assistant,
            "An old reply?".into(),
        ),
    )
    .unwrap();
    feed_claude_hook(
        &env,
        json!({"hook_event_name": "Stop", "session_id": "raised-session"}),
    );
    let output = env
        .rimz()
        .args(["asks", "raise", "@claude"])
        .bounded_output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "a previous turn is not this turn's reply: {output:?}"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("no recorded reply; provide TEXT"));
    rimz::transcript::append(
        store.paths(),
        &TranscriptEntry::new(
            jiff::Timestamp::now(),
            rimz::ids::AgentKind::new_unchecked("claude"),
            "raised-session".into(),
            TranscriptKind::Assistant,
            "A current reply?".into(),
        ),
    )
    .unwrap();
    let prompt_at = store.snapshot_cached().unwrap().agents[0].last_prompt_at;
    feed_claude_hook(
        &env,
        json!({"hook_event_name": "PreCompact", "session_id": "raised-session"}),
    );
    feed_claude_hook(
        &env,
        json!({"hook_event_name": "PostCompact", "session_id": "raised-session"}),
    );
    assert_eq!(
        store.snapshot_cached().unwrap().agents[0].last_prompt_at,
        prompt_at
    );
    let output = env
        .rimz()
        .args(["asks", "raise", "@claude"])
        .bounded_output()
        .unwrap();
    assert!(
        output.status.success(),
        "compaction must not hide the current reply: {output:?}"
    );
    assert_eq!(listed_asks(&env)[0]["context"], "A current reply?");
}

#[test]
fn outside_raise_without_text_refuses_an_agent_sleeping_on_a_timer() {
    let env = room();
    let output = env
        .rimz()
        .env("RIMZ_AGENT_KIND", "claude")
        .env("RIMZ_AGENT_ID", "raised-session")
        .args(["wait", "--in", "1h"])
        .bounded_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    feed_claude_hook(
        &env,
        json!({"hook_event_name": "Stop", "session_id": "raised-session",
        "last_assistant_message": "Waiting for the timer."}),
    );
    let output = env
        .rimz()
        .args(["asks", "raise", "@claude"])
        .bounded_output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "a timer is not clean rest: {output:?}"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("requires an agent resting cleanly"));
}

#[test]
fn printed_raised_answer_targets_its_ask_even_with_a_native_prompt() {
    let env = room();
    env.install_agent_hooks("claude");
    let id = seed_open(&env, vec!["safe".into(), "fast".into()]);
    feed_claude_hook(
        &env,
        json!({"hook_event_name": "PermissionRequest",
        "session_id": "raised-session", "tool_name": "Bash", "tool_input": {"command": "pwd"}}),
    );
    assert_eq!(listed_asks(&env).as_array().unwrap().len(), 2);
    for args in [vec!["asks"], vec!["asks", "show", id.as_str()]] {
        let output = env.rimz().args(args).bounded_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        let command = stdout
            .lines()
            .find_map(|line| {
                let command = line.split_once("rimz answer ")?.1;
                command.contains("<1|2>").then_some(command)
            })
            .expect("raised answer command");
        let target = command.split_whitespace().next().unwrap();
        let output = env
            .rimz()
            .args(["answer", target])
            .bounded_output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("1=safe, 2=fast"),
            "the printed command must resolve the raised options, not the native prompt: {output:?}"
        );
        assert_eq!(target, id.as_str());
    }
}

#[test]
fn raised_answer_routes_an_opaque_session_id_through_its_resolved_card() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    feed_claude_hook(
        &env,
        json!({"hook_event_name": "UserPromptSubmit",
        "session_id": "tenant:42", "prompt": "work"}),
    );
    let output = env
        .rimz()
        .args(["asks", "raise", "@claude", "Which?", "--option", "safe"])
        .bounded_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let output = env
        .rimz()
        .args(["answer", "@claude", "1"])
        .bounded_output()
        .unwrap();
    assert!(
        output.status.success(),
        "opaque ids must not be parsed as addresses: {output:?}"
    );
    let messages = env.store().list_messages().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].agent_id, "tenant:42");
    assert_eq!(messages[0].text, "safe");
}

#[test]
fn show_prints_a_single_raised_question_once() {
    let env = room();
    let id = seed_open(&env, vec!["safe".into()]);
    let output = env
        .rimz()
        .args(["asks", "show", id.as_str()])
        .bounded_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.matches("Which option?").count(), 1, "{stdout}");
    assert!(stdout.contains("1. safe"), "{stdout}");
}

#[test]
fn hook_driven_raised_closes_have_no_operator_actor_even_in_an_agent_process() {
    let env = room();
    seed_open(&env, vec![]);
    let store = env.store();
    let mut ask = rimz::store::raised::read(store.paths()).asks.remove(0);
    ask.value.state = rimz::agents::state::RaisedAskState::Armed;
    rimz::store::raised::raise(store.paths(), ask).unwrap();
    let frame = rimz::store::ingress::HookIngress {
        boot_id: None,
        schema_version: "1".into(),
        event_id: rimz::EventId::new(),
        ts: jiff::Timestamp::now(),
        source: rimz::ids::AgentKind::new_unchecked("claude"),
        event: Some("StopFailure".into()),
        payload: json!({"session_id": "raised-session", "error": "failed"}).to_string(),
        cwd: env.project_root.clone(),
        hook_pid: std::process::id(),
        env: std::collections::BTreeMap::from([
            ("RIMZ_WORKSPACE_ID".into(), env.workspace_id.to_string()),
            (
                "RIMZ_PROJECT_ROOT".into(),
                env.project_root.display().to_string(),
            ),
        ]),
    };
    let mut command = env.rimz();
    command
        .env("RIMZ_AGENT_KIND", "claude")
        .env("RIMZ_AGENT_ID", "raised-session")
        .env("RIMZ_AGENT_NAME", "worker")
        .args(["hooks", "apply"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let output = env
        .spawn_payload(command, &serde_json::to_string(&frame).unwrap())
        .wait_with_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let closes: Vec<_> = env
        .read_events()
        .into_iter()
        .filter(|event| event.method == "ask.closed")
        .collect();
    assert_eq!(closes.len(), 1);
    assert_eq!(closes[0].params_value()["cause"], "turn_failed");
    assert!(
        closes[0].params_value().get("actor").is_none(),
        "a hook is not an operator action"
    );
}

#[test]
fn a_user_shell_message_closes_a_raised_question_when_its_prompt_lands() {
    let env = room();
    env.install_agent_hooks("claude");
    let id = seed_open(&env, vec![]);
    let output = env
        .rimz()
        .args(["message", "@claude", "yes, ship it"])
        .bounded_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let store = env.store();
    let messages = store.list_messages().unwrap();
    assert_eq!(messages.len(), 1);
    assert!(messages[0].is_user_input());
    let sent = store.record_sent_batch(&messages, "rimz-test").unwrap();
    assert_eq!(sent.len(), 1);
    feed_claude_hook(
        &env,
        json!({"hook_event_name": "UserPromptSubmit",
        "session_id": "raised-session", "prompt": sent[0].text}),
    );
    assert_eq!(listed_asks(&env), json!([]));
    let closes: Vec<_> = env
        .read_events()
        .into_iter()
        .filter(|event| event.method == "ask.closed")
        .collect();
    assert_eq!(closes.len(), 1);
    assert_eq!(closes[0].params_value()["ask_id"], id.as_str());
    assert_eq!(closes[0].params_value()["cause"], "user_turn");
}

#[test]
fn outside_raise_lists_full_context_options_and_withdrawal_audit() {
    let env = room();
    let text = "The result is ready. Which option?\nPlease review the full context.";
    let output = env
        .rimz()
        .args([
            "asks", "raise", "@claude", text, "--option", "safe", "--option", "fast",
        ])
        .bounded_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let asks = listed_asks(&env);
    assert_eq!(asks.as_array().unwrap().len(), 1);
    assert_eq!(asks[0]["kind"], "question");
    assert_eq!(asks[0]["delivery"], "raised");
    assert_eq!(asks[0]["detail"], "Which option?");
    assert_eq!(asks[0]["context"], text);
    assert_eq!(asks[0]["questions"][0]["options"][1]["label"], "fast");
    assert!(asks[0]["raised_by"].is_null());
    let id = asks[0]["ask_id"].as_str().unwrap();
    let output = env
        .rimz()
        .args(["asks", "show", id])
        .bounded_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("full context") && stdout.contains("2. fast"),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("rimz answer {id} <1|2>")),
        "{stdout}"
    );
    let snapshot = env.store().snapshot_cached().unwrap();
    assert!(!snapshot.agents[0].is_awaiting_input());
    let output = env
        .rimz()
        .args(["asks", "withdraw", "@claude"])
        .bounded_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(listed_asks(&env), json!([]));
    for method in ["ask.raised", "ask.withdrawn"] {
        let events = env
            .read_events()
            .into_iter()
            .filter(|event| event.method == method)
            .collect::<Vec<_>>();
        assert_eq!(events.len(), 1, "{method}");
        assert_eq!(events[0].params_value()["ask_id"], id);
        assert!(events[0].params_value().get("actor").is_none());
    }
}

#[test]
fn self_raise_is_armed_until_stop_and_a_later_human_turn_closes_it() {
    let env = room();
    let output = env
        .rimz()
        .env("RIMZ_AGENT_KIND", "claude")
        .env("RIMZ_AGENT_ID", "raised-session")
        .env("RIMZ_AGENT_NAME", "worker")
        .args(["asks", "raise"])
        .bounded_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("final reply"));
    assert_eq!(listed_asks(&env), json!([]));
    feed_claude_hook(
        &env,
        json!({"hook_event_name": "Stop", "session_id": "raised-session",
            "last_assistant_message": "Ready. Should we ship?"}),
    );
    let asks = listed_asks(&env);
    assert_eq!(asks.as_array().unwrap().len(), 1);
    assert_eq!(asks[0]["detail"], "Should we ship?");
    assert_eq!(asks[0]["raised_by"], "@worker#main");
    feed_claude_hook(
        &env,
        json!({"hook_event_name": "UserPromptSubmit", "session_id": "raised-session",
            "prompt": "yes, ship it"}),
    );
    assert_eq!(listed_asks(&env), json!([]));
    let closes = env
        .read_events()
        .into_iter()
        .filter(|event| event.method == "ask.closed")
        .collect::<Vec<_>>();
    assert_eq!(closes.len(), 1);
    assert_eq!(closes[0].params_value()["cause"], "user_turn");
}

#[test]
fn raise_requires_a_target_in_a_user_shell_and_text_for_an_active_outside_target() {
    let env = room();
    for verb in ["raise", "withdraw"] {
        let output = env.rimz().args(["asks", verb]).bounded_output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(&format!("rimz asks {verb} @<agent>")),
            "{output:?}"
        );
    }
    let output = env
        .rimz()
        .args(["asks", "raise", "@claude"])
        .bounded_output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let output = env
        .rimz()
        .args(["asks", "raise", "@claude", "--", "--help"])
        .bounded_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(listed_asks(&env)[0]["context"], "--help");
}

fn seed_open(env: &Env, options: Vec<String>) -> AskId {
    let store = env.store();
    let at = jiff::Timestamp::now();
    let id = AskId::new();
    rimz::store::raised::raise(
        store.paths(),
        rimz::store::raised::Entry {
            kind: rimz::ids::AgentKind::new_unchecked("claude"),
            agent_id: "raised-session".into(),
            value: rimz::agents::state::RaisedAsk {
                ask_id: id.clone(),
                state: rimz::agents::state::RaisedAskState::Open,
                raised_at: at,
                basis_at: at,
                opened_at: Some(at),
                text: Some("Which option?".into()),
                raised_by: None,
                options,
            },
        },
    )
    .unwrap();
    id
}

#[test]
fn raised_answer_enqueues_the_label_from_the_caller_then_closes_with_one_audit() {
    for agent_caller in [false, true] {
        let env = room();
        env.install_agent_hooks("claude");
        let id = seed_open(&env, vec!["safe".into(), "fast".into()]);
        let mut command = env.rimz();
        if agent_caller {
            command
                .env("RIMZ_AGENT_KIND", "claude")
                .env("RIMZ_AGENT_ID", "raised-session")
                .env("RIMZ_AGENT_NAME", "worker");
        }
        let output = command
            .args(["answer", id.as_str(), "2"])
            .bounded_output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let messages = env.store().list_messages().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].text, "fast");
        if agent_caller {
            assert_eq!(messages[0].sender.render(), "@worker#main");
        } else {
            assert!(matches!(
                messages[0].sender,
                rimz::store::message::MessageSender::Human
            ));
        }
        assert!(String::from_utf8_lossy(&output.stdout).contains(messages[0].message_id.as_str()));
        assert!(
            rimz::store::raised::read(env.store().paths())
                .asks
                .is_empty()
        );
        let closes = env
            .read_events()
            .into_iter()
            .filter(|event| event.method == "ask.closed")
            .collect::<Vec<_>>();
        assert_eq!(closes.len(), 1);
        assert_eq!(closes[0].params_value()["cause"], "answered");
        assert_eq!(
            closes[0].params_value()["actor"].as_str(),
            agent_caller.then_some("@worker#main")
        );
    }
}

#[test]
fn raised_answer_invalid_selectors_and_failed_dispatch_leave_the_ask_open() {
    let env = room();
    let id = seed_open(&env, vec![]);
    let output = env
        .rimz()
        .args(["answer", id.as_str(), "1"])
        .bounded_output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    let id = seed_open(&env, vec!["safe".into()]);
    let output = env
        .rimz()
        .args(["answer", id.as_str(), "2"])
        .bounded_output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    let output = env
        .rimz()
        .args(["answer", id.as_str(), "1"])
        .bounded_output()
        .unwrap();
    assert!(
        !output.status.success(),
        "missing installed hooks refuses dispatch"
    );
    assert_eq!(
        rimz::store::raised::read(env.store().paths()).asks[0]
            .value
            .ask_id,
        id
    );
    assert!(env.store().list_messages().unwrap().is_empty());
    assert!(
        !env.read_events()
            .iter()
            .any(|event| event.method == "ask.closed")
    );
}

#[test]
fn outside_raise_without_text_uses_the_last_clean_reply_and_closed_targets_refuse() {
    let env = room();
    feed_claude_hook(
        &env,
        json!({"hook_event_name": "Stop", "session_id": "raised-session",
        "last_assistant_message": "Ready. Review this?"}),
    );
    let output = env
        .rimz()
        .args(["asks", "raise", "@claude"])
        .bounded_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(listed_asks(&env)[0]["context"], "Ready. Review this?");
    feed_claude_hook(
        &env,
        json!({"hook_event_name": "SessionEnd", "session_id": "raised-session"}),
    );
    let output = env
        .rimz()
        .args(["asks", "raise", "@claude", "Review?"])
        .bounded_output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
}

#[test]
fn ended_session_retires_a_raised_ask_with_one_audit() {
    let env = room();
    let id = seed_open(&env, vec![]);
    feed_claude_hook(
        &env,
        json!({"hook_event_name": "SessionEnd", "session_id": "raised-session"}),
    );
    assert!(
        rimz::store::raised::read(env.store().paths())
            .asks
            .is_empty()
    );
    let closes = env
        .read_events()
        .into_iter()
        .filter(|event| event.method == "ask.closed")
        .collect::<Vec<_>>();
    assert_eq!(closes.len(), 1);
    assert_eq!(closes[0].params_value()["ask_id"], id.as_str());
    assert_eq!(closes[0].params_value()["cause"], "retired");
}
