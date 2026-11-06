use super::*;
use std::io::Write as _;
use std::os::unix::net::{UnixListener, UnixStream};

fn room() -> Room {
    let room = Room::new("lifecycle='until'\n");
    let path = room.env.rimz_home().join("workflows/review/workflow.toml");
    fs::write(
        &path,
        fs::read_to_string(&path).unwrap().replace(
            "on.done = '@done'",
            "on.done = '@done'\non.pending = 'build'\ntimeout = '1h'",
        ),
    )
    .unwrap();
    fs::write(room.env.rimz_home().join("workflows/review/checks/pr.sh"),
        "if grep -q '^  \"node\": \"pr\"' \"$RIMZ_WORKFLOW_DIR/context.json\"; then printf '{\"outcome\":\"go\",\"key\":7}'; elif [ -f \"$RIMZ_WORKFLOW_DIR/close\" ]; then printf '{\"outcome\":\"done\"}'; else printf '{\"outcome\":\"pending\"}'; fi\n",
    ).unwrap();
    room
}

fn opened_turn(room: &Room, id: &str, run: &RunRecord) {
    room.hook(id, run, json!({"hook_event_name":"SessionStart"}));
    room.hook(
        id,
        run,
        json!({"hook_event_name":"UserPromptSubmit", "prompt":"Review the PR"}),
    );
}

fn paths(room: &Room, id: &str) -> rimz::disk::paths::WorkflowPaths {
    room.env
        .state_path_for(&room.env.project_root)
        .workflow(&id.parse().unwrap())
}

pub(super) fn wait_for_advances_to_exit(env: &Env, id: &str) {
    let spec = rimz::testkit::sandbox::SandboxSpec {
        home_root: env.home_root.clone(),
        runtime_root: env.runtime_root.clone(),
    };
    let command = ["workflow", "advance", id].map(std::ffi::OsString::from);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let running = rimz::testkit::sandbox::sandbox_processes(&spec)
            .into_iter()
            .filter_map(rimz::proc::argv)
            .any(|argv| {
                argv.windows(command.len())
                    .any(|args| args == command.as_slice())
            });
        if !running {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "workflow advance {id} did not exit"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn gate(listener: &UnixListener) -> UnixStream {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok((stream, _)) = listener.accept() {
            return stream;
        }
        assert!(
            Instant::now() < deadline,
            "until check did not reach its evaluation boundary"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A decision appends its `turn_end` row before it writes the record, so a read between the two
/// sees the row beside a record that still holds the session.
fn decided(value: &Value) -> bool {
    !events(value, "turn_end").is_empty() && value["record"]["held"].is_null()
}

#[test]
fn a_turn_end_hook_pushes_a_harness_prompt_without_relaunching() {
    let room = room();
    let (id, run) = room.launch();
    opened_turn(&room, &id, &run);
    room.hook(&id, &run, json!({"hook_event_name":"Stop"}));
    let value = until(&room.env, &id, decided);
    assert_eq!(value["record"]["version"], 3);
    assert_eq!(value["record"]["node"], "build");
    assert_eq!(
        value["record"]["attempt"]["run"]["run_id"],
        run.run_id.as_str()
    );
    assert_eq!(events(&value, "turn_end")[0]["action"], "pushed");
    let value = until(&room.env, &id, |value| {
        value["record"]["attempt"]["push"]["queued"] == true
    });
    let message_id = value["record"]["attempt"]["push"]["message"]
        .as_str()
        .unwrap();
    let (live, history) = room.env.store().list_messages_and_history().unwrap();
    let message = live
        .iter()
        .chain(&history)
        .find(|message| message.message_id.as_str() == message_id)
        .unwrap();
    assert_eq!(message.sender, rimz::store::message::MessageSender::System);
    assert_eq!(message.gate, rimz::store::message::DeliveryGate::Done);
    assert!(!message.is_user_input());
    assert_eq!(value["record"]["visits"]["build"]["total"], 2);
    let trace = fs::read_to_string(room.env.project_root.join("spawn.log")).unwrap();
    assert!(
        !trace.contains("close-pane"),
        "a keep must preserve the layout: {trace}"
    );
    let list = success(room.command(&["workflow", "list"]).output().unwrap());
    assert!(list.contains("with you"), "{list}");
    let text = success(
        room.command(&["workflow", "show", &id, "--history"])
            .output()
            .unwrap(),
    );
    assert!(
        text.contains("turn_end pushed") && text.contains("visit 1"),
        "{text}"
    );
    let text = success(room.command(&["workflow", "show", &id]).output().unwrap());
    assert!(
        text.contains("◆ after"),
        "the held session's check was visited:\n{text}"
    );
    room.hook(&id, &run, json!({"hook_event_name":"UserPromptSubmit"}));
    room.hook(&id, &run, json!({"hook_event_name":"Stop"}));
    let value = settled(&room.env, &id, "parked");
    assert_eq!(value["record"]["visits"]["build"]["total"], 2);
    assert_eq!(events(&value, "turn_end").len(), 2);
    let text = success(room.command(&["workflow", "show", &id]).output().unwrap());
    let block: Vec<_> = text
        .lines()
        .skip_while(|line| !line.starts_with("max-visits:"))
        .take_while(|line| *line != "you can")
        .collect();
    assert_eq!(block.len(), 4, "{text}");
    assert_eq!(block[0], "max-visits: 2 of 2 visits used");
    assert!(
        block[1].ends_with(&format!("after pending · build 1 · push-back {message_id}")),
        "{text}"
    );
    assert!(
        block[2].contains(&format!(
            "after pending · build 2 · @{} ended · ",
            run.agent_name.as_deref().unwrap()
        )),
        "{text}"
    );
    assert!(
        block[3].ends_with("build 3 refused: max-visits 2"),
        "{text}"
    );
}

#[test]
fn a_push_waits_for_receipt_then_judges_the_next_stop_without_the_tick() {
    let room = room();
    let definition = room.env.rimz_home().join("workflows/review/workflow.toml");
    fs::write(
        &definition,
        fs::read_to_string(&definition)
            .unwrap()
            .replace("lifecycle='until'\n", ""),
    )
    .unwrap();
    fs::write(
        room.env.project_root.join("panes.json"),
        r#"[{"id":61,"is_plugin":false,"tab_id":1,"title":"codex"}]"#,
    )
    .unwrap();
    let (id, run) = room.launch();
    let store = room.env.store();
    rimz::disk::atomic::write_temp_then_rename_cache(
        &store.runtime_paths().lane_path("workflow-tick.json"),
        &BTreeMap::from([(
            id.clone(),
            jiff::Timestamp::now() + Duration::from_secs(3600),
        )]),
    )
    .unwrap();
    opened_turn(&room, &id, &run);
    room.hook(&id, &run, json!({"hook_event_name":"Stop"}));
    let value = until(&room.env, &id, |value| {
        value["record"]["attempt"]["push"]["queued"] == true
    });
    let message = store
        .list_messages()
        .unwrap()
        .into_iter()
        .find(|message| {
            message.message_id.as_str() == value["record"]["attempt"]["push"]["message"]
        })
        .unwrap();
    assert_eq!(message.status, rimz::store::message::MessageStatus::Sent);
    wait_for_advances_to_exit(&room.env, &id);
    fs::write(paths(&room, &id).work.join("close"), "").unwrap();
    room.hook(
        &id,
        &run,
        json!({"hook_event_name":"UserPromptSubmit", "prompt":"Live workflow probe"}),
    );
    room.hook(&id, &run, json!({"hook_event_name":"Stop"}));
    wait_for_advances_to_exit(&room.env, &id);
    assert_eq!(
        rimz::store::message::read_status(store.paths(), &message.message_id).unwrap(),
        Some(rimz::store::message::MessageStatus::Sent),
    );
    assert_eq!(events(&show(&room.env, &id), "turn_end").len(), 1);
    room.hook(
        &id,
        &run,
        json!({"hook_event_name":"UserPromptSubmit", "prompt":message.text}),
    );
    assert_eq!(
        rimz::store::message::read_status(store.paths(), &message.message_id).unwrap(),
        Some(rimz::store::message::MessageStatus::Delivered),
    );
    room.hook(&id, &run, json!({"hook_event_name":"Stop"}));
    let value = settled(&room.env, &id, "done");
    assert_eq!(events(&value, "turn_end")[1]["action"], "closed");
    assert_eq!(value["record"]["visits"]["build"]["total"], 2);
}

#[test]
fn continued_restart_retires_a_pending_push_before_judging_the_next_stop() {
    restart_after_pending_push(true);
}

#[test]
fn fresh_restart_retires_a_pending_push_before_judging_the_next_stop() {
    restart_after_pending_push(false);
}

fn restart_after_pending_push(continued: bool) {
    let room = room();
    fs::write(
        room.env.project_root.join("panes.json"),
        r#"[{"id":61,"is_plugin":false,"tab_id":1,"title":"codex"}]"#,
    )
    .unwrap();
    let (id, run) = room.launch();
    let store = room.env.store();
    rimz::disk::atomic::write_temp_then_rename_cache(
        &store.runtime_paths().lane_path("workflow-tick.json"),
        &BTreeMap::from([(
            id.clone(),
            jiff::Timestamp::now() + Duration::from_secs(3600),
        )]),
    )
    .unwrap();
    let transcript = room.env.project_root.join("session.jsonl");
    fs::write(&transcript, "{}\n").unwrap();
    let mut owner = dummy_agent_process();
    for payload in [
        json!({"hook_event_name":"SessionStart", "transcript_path":transcript}),
        json!({"hook_event_name":"UserPromptSubmit", "prompt":"Review the PR"}),
        json!({"hook_event_name":"Stop"}),
    ] {
        room.hook_with_owner(&id, &run, owner.id(), payload);
    }
    let pushed = until(&room.env, &id, |value| {
        value["record"]["attempt"]["push"]["queued"] == true
    });
    let message_id = pushed["record"]["attempt"]["push"]["message"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        rimz::store::message::read_status(store.paths(), &message_id).unwrap(),
        Some(rimz::store::message::MessageStatus::Sent),
    );
    wait_for_advances_to_exit(&room.env, &id);
    let mut args = vec!["workflow", "restart", &id];
    if continued {
        args.push("--continue");
    }
    let mut child = room
        .command(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    super::restart::wait_for_pane_close(&room, &mut child);
    // The trace shim closes the pane but cannot terminate its owner.
    owner.kill().unwrap();
    owner.wait().unwrap();
    success(child.wait_with_output().unwrap());
    let replacement = room.attempt(&id);
    assert_ne!(replacement.run_id, run.run_id);
    fs::write(paths(&room, &id).work.join("close"), "").unwrap();
    let session = if continued {
        "workflow-session"
    } else {
        "fresh-workflow-session"
    };
    for payload in [
        json!({"hook_event_name":"SessionStart", "transcript_path":transcript}),
        json!({"hook_event_name":"UserPromptSubmit", "prompt":"Review the resumed work"}),
        json!({"hook_event_name":"Stop"}),
    ] {
        room.hook_in_session(
            &id,
            &replacement,
            room.env.agent_owner_pid(),
            session,
            payload,
            &[],
        );
    }
    let value = settled(&room.env, &id, "done");
    assert_eq!(events(&value, "turn_end").len(), 2, "{value:#}");
    assert_eq!(events(&value, "turn_end")[1]["action"], "closed");
    assert_eq!(value["record"]["visits"]["build"]["total"], 1);
    assert_eq!(
        rimz::store::message::read_status(store.paths(), &message_id).unwrap(),
        Some(rimz::store::message::MessageStatus::Canceled),
    );
}

#[test]
fn a_closing_turn_end_hook_stops_the_layout_and_uses_one_verdict() {
    let room = room();
    let (id, run) = room.launch();
    fs::write(paths(&room, &id).work.join("close"), "").unwrap();
    opened_turn(&room, &id, &run);
    room.hook(&id, &run, json!({"hook_event_name":"Stop"}));
    let value = settled(&room.env, &id, "done");
    assert_eq!(events(&value, "attempt").len(), 1, "{value:#}");
    assert_eq!(events(&value, "attempt")[0]["outcome"], "ended");
    let decisions = events(&value, "turn_end");
    assert_eq!(decisions.len(), 1, "{value:#}");
    assert_eq!(decisions[0]["action"], "closed");
    assert_eq!(decisions[0]["target"], "@done");
    assert_eq!(
        events(&value, "evaluation")
            .iter()
            .filter(|row| row["node"] == "after")
            .count(),
        1,
        "{value:#}"
    );
    let trace = fs::read_to_string(room.env.project_root.join("spawn.log")).unwrap();
    assert!(trace.contains("close-pane"), "{trace}");
}

#[test]
fn restart_abandons_an_inflight_until_answer_before_relaunching() {
    let room = room();
    let (id, run) = room.launch();
    let mut owner = dummy_agent_process();
    room.hook_with_owner(
        &id,
        &run,
        owner.id(),
        json!({"hook_event_name":"SessionStart"}),
    );
    room.hook_with_owner(
        &id,
        &run,
        owner.id(),
        json!({"hook_event_name":"UserPromptSubmit"}),
    );
    wait_for_advances_to_exit(&room.env, &id);
    let socket = room.env.runtime_root.join("restart-result.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    room.hook_with_environment(
        &id,
        &run,
        owner.id(),
        json!({"hook_event_name":"Stop"}),
        &[("RIMZ_TEST_WORKFLOW_RESULT_GATE", &socket)],
    );
    let mut waiting = gate(&listener);
    // The trace shim closes the pane but cannot terminate its owner.
    owner.kill().unwrap();
    owner.wait().unwrap();
    let output = room
        .command(&["workflow", "restart", &id])
        .output()
        .unwrap();
    waiting.write_all(&[1]).unwrap();
    let receipt = success(output);
    assert!(receipt.contains("visits restarted at 1"), "{receipt}");
    let value = until(&room.env, &id, |value| {
        value["record"]["attempt"]["run"]["run_id"]
            .as_str()
            .is_some_and(|current| current != run.run_id.as_str())
    });
    assert!(value["record"]["held"].is_null(), "{value:#}");
    assert_eq!(value["record"]["visits"]["build"]["total"], 1);
    assert!(
        !events(&value, "turn_end")
            .iter()
            .any(|event| event["action"] == "pushed"),
        "{value:#}"
    );
}

#[test]
fn a_park_after_a_turn_end_omits_the_printed_reason() {
    let room = room();
    let definition = room.env.rimz_home().join("workflows/review");
    let spec = definition.join("workflow.toml");
    fs::write(
        &spec,
        fs::read_to_string(&spec)
            .unwrap()
            .replace("on.done = '@done'", "on.done = '@user'"),
    )
    .unwrap();
    fs::write(
        definition.join("checks/pr.sh"),
        concat!(
            "if grep -q '^  \"node\": \"pr\"' \"$RIMZ_WORKFLOW_DIR/context.json\"; then\n",
            "  printf '%s\\n' '{\"outcome\":\"go\"}'\n",
            "else\n",
            "  printf '%s\\n' '{\"outcome\":\"done\",\"reason\":\"Review the finished work\"}'\n",
            "fi\n",
        ),
    )
    .unwrap();
    let (id, run) = room.launch();
    opened_turn(&room, &id, &run);
    room.hook(&id, &run, json!({"hook_event_name":"Stop"}));
    let value = settled(&room.env, &id, "parked");
    assert_eq!(events(&value, "turn_end")[0]["target"], "@user");
    let text = success(
        room.command(&["workflow", "show", &id, "--history"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        text.matches("Review the finished work").count(),
        1,
        "{text}"
    );
    assert!(text.ends_with("after park user\n"), "{text}");
}

#[test]
fn a_turn_started_while_the_verdict_is_waiting_discards_without_stopping() {
    let room = room();
    let (id, run) = room.launch();
    fs::write(paths(&room, &id).work.join("close"), "").unwrap();
    opened_turn(&room, &id, &run);
    wait_for_advances_to_exit(&room.env, &id);
    let socket = room.env.runtime_root.join("until-result.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    room.hook_with_environment(
        &id,
        &run,
        room.env.agent_owner_pid(),
        json!({"hook_event_name":"Stop"}),
        &[("RIMZ_TEST_WORKFLOW_RESULT_GATE", &socket)],
    );
    let mut waiting = gate(&listener);
    room.hook(
        &id,
        &run,
        json!({"hook_event_name":"UserPromptSubmit", "prompt":"One more question"}),
    );
    let value = until(&room.env, &id, decided);
    assert_eq!(events(&value, "turn_end")[0]["action"], "discarded");
    assert_eq!(
        events(&value, "turn_end")[0]["reason"],
        "leader turn moved on"
    );
    assert!(events(&value, "attempt").is_empty(), "{value:#}");
    assert_eq!(value["record"]["node"], "build");
    waiting.write_all(&[1]).unwrap();
    wait_for_advances_to_exit(&room.env, &id);
    assert_eq!(
        events(&show(&room.env, &id), "evaluation"),
        events(&value, "evaluation"),
        "the abandoned check's late result must write nothing"
    );
    let trace = fs::read_to_string(room.env.project_root.join("spawn.log")).unwrap();
    assert!(
        !trace.contains("close-pane"),
        "a stale verdict must not close panes: {trace}"
    );
    let text = success(room.command(&["workflow", "show", &id]).output().unwrap());
    assert!(
        text.contains("◆ after"),
        "the abandoned check was reached:\n{text}"
    );
    success(room.command(&["workflow", "cancel", &id]).output().unwrap());
}

#[test]
fn a_held_check_publishes_the_live_session_and_renders_checking() {
    let room = room();
    let spec = room.env.rimz_home().join("workflows/review/workflow.toml");
    fs::write(
        &spec,
        fs::read_to_string(&spec).unwrap().replace(
            "on.pending = 'build'",
            "on.pending = '@wait'\nwake=['review.ready']",
        ),
    )
    .unwrap();
    let (id, run) = room.launch();
    opened_turn(&room, &id, &run);
    wait_for_advances_to_exit(&room.env, &id);
    let socket = room.env.runtime_root.join("until-context.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    room.hook_with_environment(
        &id,
        &run,
        room.env.agent_owner_pid(),
        json!({"hook_event_name":"Stop"}),
        &[("RIMZ_TEST_WORKFLOW_CONTEXT_GATE", &socket)],
    );
    let mut waiting = gate(&listener);
    let value = show(&room.env, &id);
    let view = context(&value);
    assert_eq!(view["node"], "after");
    assert_eq!(view["live_attempt"]["step"], "build");
    assert_eq!(view["live_attempt"]["run_id"], run.run_id.as_str());
    assert_eq!(view["live_attempt"]["visit"], 1);
    assert!(view["prev_attempt"].is_null());
    assert!(view.get("visits").is_none());
    let list = success(room.command(&["workflow", "list"]).output().unwrap());
    assert!(
        list.contains("with you in @") && list.contains(" · checking after"),
        "{list}"
    );
    let text = success(room.command(&["workflow", "show", &id]).output().unwrap());
    assert!(
        text.contains("build is with you") && text.contains("checking after"),
        "{text}"
    );
    assert_eq!(text.matches("checking after").count(), 1, "{text}");
    waiting.write_all(&[1]).unwrap();
    let value = until(&room.env, &id, |value| {
        !events(value, "turn_end").is_empty()
            && value["record"]["held"].is_object()
            && value["record"]["held"]["verdict"].is_null()
            && value["record"]["wait"].is_object()
    });
    assert_eq!(events(&value, "turn_end")[0]["action"], "kept");
    let text = success(room.command(&["workflow", "show", &id]).output().unwrap());
    assert!(text.contains("build is with you"), "{text}");
    assert_eq!(text.matches("checking after: pending").count(), 1, "{text}");
    fs::write(paths(&room, &id).work.join("close"), "").unwrap();
    let signal = json!({"name":"review.ready","payload":{},"source":"cli"}).to_string();
    success(
        room.command(&["workflow", "advance", "--wake-signal", &signal])
            .output()
            .unwrap(),
    );
    let value = settled(&room.env, &id, "done");
    assert_eq!(value["record"]["visits"]["build"]["total"], 1);
    assert_eq!(events(&value, "turn_end")[1]["action"], "closed");
}

#[test]
fn a_signal_cannot_shorten_a_delayed_close_and_views_show_the_countdown() {
    delayed_close_views("@done");
    delayed_close_views("finish");
}

fn delayed_close_views(target: &str) {
    let room = room();
    let spec = room.env.rimz_home().join("workflows/review/workflow.toml");
    fs::write(
        &spec,
        format!(
            "{}\n[steps.finish]\nagent='claude'\nprompt-file='prompt.md'\nlifecycle='turn'\nthen='after'\nmax-visits=1\n",
            fs::read_to_string(&spec).unwrap().replace(
                "on.done = '@done'",
                &format!("on.done = {{to='{target}',delay='30m'}}\nwake=['review.ready']"),
            ),
        ),
    )
    .unwrap();
    let (id, run) = room.launch();
    opened_turn(&room, &id, &run);
    fs::write(paths(&room, &id).work.join("close"), "").unwrap();
    room.hook(&id, &run, json!({"hook_event_name":"Stop"}));
    let before = until(&room.env, &id, |value| {
        value["record"]["held"]["verdict"].is_object() || value["record"]["state"] == "done"
    });
    assert!(
        before["record"]["held"]["verdict"].is_object(),
        "a delayed verdict must keep the session open: {before}"
    );
    for args in [vec!["workflow", "list"], vec!["workflow", "show", &id]] {
        let text = success(room.command(&args).output().unwrap());
        assert_eq!(
            text.matches("checking after: closing in ").count(),
            1,
            "{text}"
        );
        assert!(text.contains(&format!(" → {target}")), "{text}");
    }
    for args in [
        vec!["workflow", "show", &id],
        vec!["workflow", "show", &id, "--history"],
    ] {
        let text = success(room.command(&args).output().unwrap());
        assert!(
            !text.contains(&format!("after said done → {target}")),
            "{text}"
        );
        assert!(!text.contains(&format!("after done → {target}")), "{text}");
    }
    let paths = paths(&room, &id);
    let spec = rimz::harness::workflow::load(&paths.home).unwrap();
    let journal = rimz::store::workflow::journal(&paths).unwrap();
    let record = rimz::store::workflow::load(&paths).unwrap();
    let hops = rimz::harness::workflow::hops(&spec, &journal, record.held.as_ref());
    assert!(!rimz::harness::workflow::reached(&hops, &journal).contains_key("finish"));
    assert!(
        rimz::harness::workflow::routing_hop(&spec, &journal, target, record.held.as_ref())
            .is_none()
    );
    success(
        room.command(&["workflow", "resume", &id, "--extend", "visits=+1"])
            .output()
            .unwrap(),
    );
    let signal = json!({"name":"review.ready","payload":{},"source":"cli"}).to_string();
    success(
        room.command(&["workflow", "advance", "--wake-signal", &signal])
            .output()
            .unwrap(),
    );
    until(&room.env, &id, |value| !events(value, "wake").is_empty());
    wait_for_advances_to_exit(&room.env, &id);
    success(
        room.command(&["workflow", "advance", &id])
            .output()
            .unwrap(),
    );
    let after = show(&room.env, &id);
    assert_eq!(
        after["record"]["held"]["verdict"],
        before["record"]["held"]["verdict"]
    );
    assert_eq!(
        events(&after, "evaluation").len(),
        events(&before, "evaluation").len()
    );
    assert!(events(&after, "turn_end").is_empty(), "{after}");
    success(room.command(&["workflow", "cancel", &id]).output().unwrap());
    let text = success(
        room.command(&["workflow", "show", &id, "--history"])
            .output()
            .unwrap(),
    );
    assert!(
        !text.contains(&format!("after said done → {target}")),
        "{text}"
    );
    assert!(!text.contains(&format!("after done → {target}")), "{text}");
}
