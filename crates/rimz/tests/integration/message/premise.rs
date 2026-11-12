use super::*;

#[test]
fn a_prompt_applied_before_the_write_withdraws_its_batch_member_and_delivers_the_rest() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
    register_resting_agent(&env, "sess-raced", "raced", pane_env);
    run_success(
        env.rimz()
            .args(["config", "set", "harness.smart_compact", "off"]),
        "send without a seat line",
    );
    let store = env.store();
    let agent = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents
        .into_iter()
        .find(|agent| agent.agent_id.as_str() == "sess-raced")
        .unwrap();
    let session_name = env.resolve_workspace(&env.project_root).session_name;
    let queue = |text: &str, premises| {
        let record = rimz::store::message::MessageRecord::new(
            env.workspace_id.clone(),
            &agent,
            text.to_owned(),
            DeliveryGate::Done,
        )
        .with_premises(premises);
        store.queue_message(&record, &session_name).unwrap();
        record.message_id
    };
    let premised = queue(
        "only if unprompted",
        vec![rimz::store::message::Premise::NotPromptedSince {
            last_prompt_at: agent.last_prompt_at,
        }],
    );
    let plain = queue("still true", rimz::store::message::Premise::NONE);

    let trace = env.project_root.join("raced.log");
    let before = DeliveryRendezvous::new(&env, "raced-before-write");
    let child = traced_rimz(&env, &trace)
        .env("RIMZ_TEST_MESSAGE_BEFORE_WRITE", &before.path)
        .args(["message", "sweep"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut release = before.arrive();
    for payload in [
        json!({"hook_event_name": "UserPromptSubmit", "session_id": "sess-raced", "prompt": "real"}),
        json!({"hook_event_name": "Stop", "session_id": "sess-raced"}),
    ] {
        run_hook(&env, payload, pane_env);
    }
    release.write_all(&[1]).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");

    wait_for_text_then_enter(&trace, &user_message("still true"));
    let lines = trace_lines(&trace);
    assert!(
        !lines
            .iter()
            .any(|line| is_paste_to_any_pane(line, &user_message("only if unprompted"))),
        "a withdrawn member must never be typed: {lines:?}"
    );
    let history = store.list_message_history().unwrap();
    let withdrawn = history
        .iter()
        .find(|record| record.message_id == premised)
        .expect("withdrawn record in history");
    assert_eq!(withdrawn.status, MessageStatus::Canceled);
    assert_eq!(
        withdrawn.last_error.as_deref(),
        Some("agent was prompted after this message was queued")
    );
    let live = store.list_messages().unwrap();
    assert_eq!(
        live.iter()
            .map(|record| (&record.message_id, record.status))
            .collect::<Vec<_>>(),
        [(&plain, MessageStatus::Sent)]
    );
}

/// A resting receiver with `messages` queued on its card, in order, for a `message sweep` that
/// meets the write check.
fn queue_for_write_check(
    env: &Env,
    session: &str,
    messages: impl FnOnce(
        &rimz::agents::AgentState,
    ) -> Vec<(&'static str, Vec<rimz::store::message::Premise>)>,
) -> Vec<MessageId> {
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_resting_agent(env, session, "raced", &[("ZELLIJ_PANE_ID", "3")]);
    run_success(
        env.rimz()
            .args(["config", "set", "harness.smart_compact", "off"]),
        "send without a seat line",
    );
    let store = env.store();
    let agent = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents
        .into_iter()
        .find(|agent| agent.agent_id.as_str() == session)
        .unwrap();
    let session_name = env.resolve_workspace(&env.project_root).session_name;
    messages(&agent)
        .into_iter()
        .map(|(text, premises)| {
            let record = MessageRecord::new(
                env.workspace_id.clone(),
                &agent,
                text.to_owned(),
                DeliveryGate::Done,
            )
            .with_premises(premises);
            store.queue_message(&record, &session_name).unwrap();
            record.message_id
        })
        .collect()
}

fn prompt_and_rest(env: &Env, session: &str) {
    for payload in [
        json!({"hook_event_name": "UserPromptSubmit", "session_id": session, "prompt": "real"}),
        json!({"hook_event_name": "Stop", "session_id": session}),
    ] {
        run_hook(env, payload, &[("ZELLIJ_PANE_ID", "3")]);
    }
}

fn assert_withdrawn_untyped(env: &Env, trace: &Path, members: &[(&MessageId, &str)]) {
    let lines = trace_lines(trace);
    let history = env.store().list_message_history().unwrap();
    for (id, text) in members {
        assert!(
            !lines
                .iter()
                .any(|line| is_paste_to_any_pane(line, &user_message(text))),
            "a withdrawn member must never be typed: {lines:?}"
        );
        let record = history
            .iter()
            .find(|record| &record.message_id == *id)
            .unwrap_or_else(|| panic!("{text} withdrawn into history: {history:?}"));
        assert_eq!(record.status, MessageStatus::Canceled, "{record:?}");
    }
    assert!(
        env.store().list_messages().unwrap().is_empty(),
        "{:?}",
        env.store().list_messages().unwrap()
    );
}

#[test]
fn a_prompt_applied_while_the_write_check_withdraws_withdraws_the_survivor_too() {
    let env = Env::new();
    let lifespan_end = jiff::Timestamp::now() + Duration::from_secs(2);
    let ids = queue_for_write_check(&env, "sess-raced", |agent| {
        vec![
            (
                "only if unprompted",
                vec![rimz::store::message::Premise::NotPromptedSince {
                    last_prompt_at: agent.last_prompt_at,
                }],
            ),
            (
                "only for two seconds",
                vec![rimz::store::message::Premise::Lifespan {
                    until: lifespan_end,
                }],
            ),
        ]
    });

    let trace = env.project_root.join("raced.log");
    let before_write = DeliveryRendezvous::new(&env, "raced-before-write");
    let before_withdraw = DeliveryRendezvous::new(&env, "raced-before-withdraw");
    let mut child = traced_rimz(&env, &trace)
        .env("RIMZ_TEST_MESSAGE_BEFORE_WRITE", &before_write.path)
        .env("RIMZ_TEST_MESSAGE_BEFORE_WITHDRAW", &before_withdraw.path)
        .args(["message", "sweep"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut release = before_write.arrive();
    while jiff::Timestamp::now() <= lifespan_end {
        std::thread::sleep(Duration::from_millis(50));
    }
    release.write_all(&[1]).unwrap();
    // The receiver's prompt lands after the write check read its row and before its first
    // withdrawal commits; any later withdrawal pass is released at once.
    let mut release = before_withdraw.arrive();
    prompt_and_rest(&env, "sess-raced");
    release.write_all(&[1]).unwrap();
    while child.try_wait().unwrap().is_none() {
        if let Some(mut again) = before_withdraw.arrive_timeout(Duration::from_millis(50)) {
            again.write_all(&[1]).unwrap();
        }
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");

    assert_withdrawn_untyped(
        &env,
        &trace,
        &[
            (&ids[0], "only if unprompted"),
            (&ids[1], "only for two seconds"),
        ],
    );
}
