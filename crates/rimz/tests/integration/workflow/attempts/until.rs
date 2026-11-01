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

fn wait_for_advances_to_exit(env: &Env, id: &str) {
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
fn a_turn_end_hook_keeps_the_layout_without_an_explicit_advance() {
    let room = room();
    let (id, run) = room.launch();
    opened_turn(&room, &id, &run);
    room.hook(&id, &run, json!({"hook_event_name":"Stop"}));
    let value = until(&room.env, &id, decided);
    assert_eq!(value["record"]["version"], 2);
    assert_eq!(value["record"]["node"], "build");
    assert_eq!(
        value["record"]["attempt"]["run"]["run_id"],
        run.run_id.as_str()
    );
    assert_eq!(events(&value, "turn_end")[0]["action"], "kept");
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
    assert!(text.contains("turn_end") && text.contains("kept"), "{text}");
    let text = success(room.command(&["workflow", "show", &id]).output().unwrap());
    assert!(
        text.contains("● after"),
        "the held session's check was visited:\n{text}"
    );
    success(room.command(&["workflow", "cancel", &id]).output().unwrap());
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
    waiting.write_all(&[1]).unwrap();
    let value = until(&room.env, &id, decided);
    assert_eq!(events(&value, "turn_end")[0]["action"], "discarded");
    assert!(events(&value, "attempt").is_empty(), "{value:#}");
    assert_eq!(value["record"]["node"], "build");
    let trace = fs::read_to_string(room.env.project_root.join("spawn.log")).unwrap();
    assert!(
        !trace.contains("close-pane"),
        "a stale verdict must not close panes: {trace}"
    );
    let text = success(room.command(&["workflow", "show", &id]).output().unwrap());
    assert!(
        text.contains("● after") && text.contains("done ·"),
        "the discarded verdict's check was evaluated:\n{text}"
    );
    success(room.command(&["workflow", "cancel", &id]).output().unwrap());
}

#[test]
fn a_held_check_publishes_the_live_session_and_renders_checking() {
    let room = room();
    let spec = room.env.rimz_home().join("workflows/review/workflow.toml");
    fs::write(
        &spec,
        fs::read_to_string(&spec)
            .unwrap()
            .replace("on.pending = 'build'", "on.pending = '@wait'"),
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
    assert!(list.contains("with you; checking after"), "{list}");
    let text = success(room.command(&["workflow", "show", &id]).output().unwrap());
    assert!(
        text.contains("build is with you") && text.contains("checking after"),
        "{text}"
    );
    assert_eq!(text.matches("checking after").count(), 1, "{text}");
    waiting.write_all(&[1]).unwrap();
    let value = until(&room.env, &id, decided);
    assert_eq!(events(&value, "turn_end")[0]["action"], "kept");
    let text = success(room.command(&["workflow", "show", &id]).output().unwrap());
    assert!(text.contains("build is with you"), "{text}");
    assert_eq!(text.matches("said pending").count(), 1, "{text}");
    success(room.command(&["workflow", "cancel", &id]).output().unwrap());
}
