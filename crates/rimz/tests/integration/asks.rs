use serde_json::json;

#[cfg(unix)]
use crate::common::path_with_front;
use crate::common::{CommandTimeoutExt, Env, permission_payload, tmux_pane};
use rimz::agents::{AgentState, AgentStatus, AskKind, OpenAsk};
use rimz::ids::AskId;
use rimz::transcript::{AskOption, AskQuestion, TranscriptEntry, TranscriptKind};

const QUESTION_CONTEXT: &str = concat!(
    "A staged rollout limits the blast radius.\n\n",
    "It also leaves time to observe each stage."
);

fn question_payload(env: &Env) -> String {
    let transcript_path = env.home_root.join("question-transcript.jsonl");
    let transcript = serde_json::to_string(&json!({
        "type": "assistant",
        "timestamp": "2026-07-13T10:00:01Z",
        "message": {
            "role": "assistant",
            "content": [{ "type": "text", "text": QUESTION_CONTEXT }]
        }
    }))
    .expect("Claude question transcript");
    std::fs::write(&transcript_path, format!("{transcript}\n"))
        .expect("write Claude question transcript");

    serde_json::to_string(&json!({
        "hook_event_name": "PreToolUse",
        "session_id": "sess-question",
        "transcript_path": transcript_path,
        "tool_name": "AskUserQuestion",
        "tool_input": {
            "questions": [{
                "question": "Choose deployment path?",
                "options": [
                    { "label": "safe", "description": "Use staged rollout" },
                    { "label": "fast" }
                ],
                "multiSelect": false
            }]
        }
    }))
    .expect("payload")
}

fn codex_plan_payload(transcript_path: &std::path::Path) -> String {
    serde_json::to_string(&json!({
        "hook_event_name": "Stop",
        "session_id": "sess-codex-plan",
        "turn_id": "turn-plan",
        "transcript_path": transcript_path,
        "permission_mode": "plan",
        "last_assistant_message": "Codex says:"
    }))
    .expect("Codex plan payload")
}

fn codex_question_payload() -> String {
    serde_json::to_string(&json!({
        "hook_event_name": "PreToolUse",
        "session_id": "sess-codex-question",
        "tool_name": "request_user_input",
        "tool_input": {
            "questions": [
                {
                    "id": "first",
                    "question": "First?",
                    "options": [{ "label": "A" }, { "label": "B" }]
                },
                {
                    "id": "second",
                    "question": "Second?",
                    "options": [{ "label": "X" }, { "label": "Y" }, { "label": "Z" }]
                }
            ]
        }
    }))
    .expect("Codex question payload")
}

fn write_codex_plan_rollout(env: &Env) -> std::path::PathBuf {
    let path = env.home_root.join("rollout-plan.jsonl");
    std::fs::write(
        &path,
        concat!(
            r##"{"timestamp":"2026-07-13T10:00:01Z","type":"event_msg","payload":{"type":"item_completed","turn_id":"turn-plan","item":{"type":"Plan","text":"# Plan\n\nShip safely."}}}"##,
            "\n",
            r#"{"timestamp":"2026-07-13T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-plan","last_agent_message":"Codex says:"}}"#,
            "\n",
        ),
    )
    .expect("write Codex plan rollout");
    path
}

#[test]
fn asks_lists_and_shows_structured_question_json() {
    let env = Env::new();
    let hook = env.run_hook("claude", &question_payload(&env));
    assert!(
        hook.status.success(),
        "{}",
        String::from_utf8_lossy(&hook.stderr)
    );

    let output = env
        .rimz()
        .args(["asks", "--json"])
        .bounded_output()
        .expect("run asks");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let asks: serde_json::Value = serde_json::from_slice(&output.stdout).expect("asks json");
    let ask = &asks[0];
    assert!(
        ask["ask_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("ask_"))
    );
    assert_eq!(ask["kind"], "question");
    assert_eq!(ask["context"], QUESTION_CONTEXT);
    assert_eq!(ask["questions"][0]["question"], "Choose deployment path?");
    assert_eq!(ask["questions"][0]["options"][0]["label"], "safe");
    assert_eq!(ask["questions"][0]["options"][0]["mutates_trust"], false);

    let ask_id = ask["ask_id"].as_str().expect("ask id");
    let output = env
        .rimz()
        .args(["asks", "show", ask_id, "--json"])
        .bounded_output()
        .expect("show ask");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let shown: serde_json::Value = serde_json::from_slice(&output.stdout).expect("show json");
    assert_eq!(shown["ask_id"], ask_id);
    assert_eq!(shown["context"], QUESTION_CONTEXT);
    assert_eq!(shown["questions"][0]["options"][1]["label"], "fast");

    let output = env
        .rimz()
        .args(["asks", "show", ask_id])
        .bounded_output()
        .expect("show human ask");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let shown = String::from_utf8(output.stdout).expect("human ask");
    assert!(
        shown.contains("▌ A staged rollout limits the blast radius."),
        "{shown}"
    );
    assert!(
        shown.contains("▌ It also leaves time to observe each stage."),
        "{shown}"
    );
    assert!(shown.contains("\n▌\n"), "{shown}");
    assert_eq!(
        shown.matches("Choose deployment path?").count(),
        1,
        "{shown}"
    );
    assert!(shown.contains("question · "), "{shown}");
}

#[test]
fn pi_parallel_sibling_completion_keeps_the_keyed_ask_open() {
    let env = Env::new();
    let ask = serde_json::to_string(&json!({
        "hook_event_name": "tool_call",
        "session_id": "sess-pi-question",
        "tool_call_id": "ask-call",
        "tool_name": "ask_user_question",
        "has_ui": true,
        "tool_input": {
            "questions": [{
                "question": "Which route?",
                "options": [
                    { "label": "Safe", "description": "Stage it" },
                    { "label": "Fast", "description": "Ship it" }
                ]
            }]
        }
    }))
    .expect("pi ask payload");
    let output = env.run_hook("pi", &ask);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let sibling = serde_json::to_string(&json!({
        "hook_event_name": "tool_execution_end",
        "session_id": "sess-pi-question",
        "tool_call_id": "sibling-call",
        "tool_name": "bash"
    }))
    .expect("pi sibling payload");
    let output = env.run_hook("pi", &sibling);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = env
        .rimz()
        .args(["asks", "--json"])
        .bounded_output()
        .expect("list pi ask after sibling completion");
    assert!(output.status.success());
    let asks: serde_json::Value = serde_json::from_slice(&output.stdout).expect("asks json");
    assert_eq!(asks.as_array().map(Vec::len), Some(1));
    assert_eq!(asks[0]["questions"][0]["question"], "Which route?");

    let matching = serde_json::to_string(&json!({
        "hook_event_name": "tool_execution_end",
        "session_id": "sess-pi-question",
        "tool_call_id": "ask-call",
        "tool_name": "ask_user_question",
        "tool_details": { "answers": [{ "question": "Which route?", "answer": "Safe" }] }
    }))
    .expect("pi matching payload");
    let output = env.run_hook("pi", &matching);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = env
        .rimz()
        .args(["asks", "--json"])
        .bounded_output()
        .expect("list pi asks after matching completion");
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        json!([])
    );
}

#[test]
fn asks_ignores_newer_transcript_question_with_a_different_id() {
    let env = Env::new();
    let hook = env.run_hook("claude", &question_payload(&env));
    assert!(
        hook.status.success(),
        "{}",
        String::from_utf8_lossy(&hook.stderr)
    );
    let snapshot = env.store().snapshot_cached().expect("agent snapshot");
    let agent = &snapshot.agents[0];
    let open = agent.open_ask.as_ref().expect("current ask");

    let mut foreign = TranscriptEntry::new(
        jiff::Timestamp::now(),
        agent.kind.clone(),
        agent.agent_id.clone(),
        TranscriptKind::Ask,
        "Foreign transcript question?".to_owned(),
    );
    let foreign_id = AskId::parse("ask_0123456789abcdef").expect("foreign ask id");
    assert_ne!(foreign_id, open.id);
    foreign.id = Some(foreign_id);
    foreign.questions = vec![AskQuestion {
        question: "Foreign transcript question?".to_owned(),
        options: vec![AskOption::from("foreign-option".to_owned())],
        multi_select: false,
        has_option_previews: false,
    }];
    rimz::transcript::append(env.store().paths(), &foreign).expect("append foreign ask");

    let output = env
        .rimz()
        .args(["asks", "--json"])
        .bounded_output()
        .expect("run asks");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let asks: serde_json::Value = serde_json::from_slice(&output.stdout).expect("asks json");
    assert_eq!(asks[0]["ask_id"], open.id.as_str());
    assert_eq!(
        asks[0]["questions"][0]["question"],
        open.detail.as_deref().expect("current question detail")
    );
    assert_eq!(asks[0]["questions"][0]["options"], json!([]));
    assert!(!asks.to_string().contains("Foreign transcript question?"));
    assert!(!asks.to_string().contains("foreign-option"));
}

#[test]
fn permission_request_does_not_replace_its_native_question_ask() {
    let env = Env::new();
    let question = env.run_hook("claude", &question_payload(&env));
    assert!(question.status.success());
    let before = env
        .rimz()
        .args(["asks", "--json"])
        .bounded_output()
        .expect("list question ask");
    let before: serde_json::Value = serde_json::from_slice(&before.stdout).expect("asks json");
    let ask_id = before[0]["ask_id"].clone();

    let duplicate = serde_json::to_string(&json!({
        "hook_event_name": "PermissionRequest",
        "session_id": "sess-question",
        "tool_name": "AskUserQuestion",
        "tool_input": { "questions": [] }
    }))
    .expect("duplicate payload");
    let duplicate = env.run_hook("claude", &duplicate);
    assert!(duplicate.status.success());

    let after = env
        .rimz()
        .args(["asks", "--json"])
        .bounded_output()
        .expect("list question after duplicate");
    let after: serde_json::Value = serde_json::from_slice(&after.stdout).expect("asks json");
    assert_eq!(after[0]["kind"], "question");
    assert_eq!(after[0]["ask_id"], ask_id);
}

#[test]
fn asks_synthesizes_safe_permission_options() {
    let env = Env::new();
    let hook = env.run_hook("claude", &permission_payload("Bash"));
    assert!(
        hook.status.success(),
        "{}",
        String::from_utf8_lossy(&hook.stderr)
    );

    let snapshot = env.store().snapshot_cached().expect("agent snapshot");
    let agent = &snapshot.agents[0];
    let open = agent.open_ask.as_ref().expect("current permission ask");
    let mut transcript_ask = TranscriptEntry::new(
        jiff::Timestamp::now(),
        agent.kind.clone(),
        agent.agent_id.clone(),
        TranscriptKind::Ask,
        "Foreign permission question?".to_owned(),
    );
    transcript_ask.id = Some(open.id.clone());
    transcript_ask.questions = vec![AskQuestion {
        question: "Foreign permission question?".to_owned(),
        options: vec![AskOption::from("foreign-option".to_owned())],
        multi_select: false,
        has_option_previews: false,
    }];
    rimz::transcript::append(env.store().paths(), &transcript_ask)
        .expect("append permission transcript ask");

    let output = env
        .rimz()
        .args(["asks", "--json"])
        .bounded_output()
        .expect("run asks");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let asks: serde_json::Value = serde_json::from_slice(&output.stdout).expect("asks json");
    assert_eq!(asks[0]["kind"], "permission");
    assert!(asks[0].get("context").is_none());
    assert_eq!(
        asks[0]["questions"][0]["question"],
        open.detail.as_deref().expect("current permission detail")
    );
    assert_eq!(asks[0]["questions"][0]["options"][0]["label"], "allow");
    assert_eq!(
        asks[0]["questions"][0]["options"].as_array().unwrap().len(),
        1
    );
    assert!(!asks.to_string().contains("Foreign permission question?"));
    assert!(!asks.to_string().contains("foreign-option"));
}

#[test]
fn asks_marks_plan_approval_mode_changes() {
    let env = Env::new();
    let payload = serde_json::to_string(&json!({
        "hook_event_name": "PreToolUse",
        "session_id": "sess-plan",
        "tool_name": "ExitPlanMode",
        "tool_input": { "plan": "1. Make the change\n2. Verify it" }
    }))
    .expect("payload");
    let hook = env.run_hook("claude", &payload);
    assert!(
        hook.status.success(),
        "{}",
        String::from_utf8_lossy(&hook.stderr)
    );

    let output = env
        .rimz()
        .args(["asks", "--json"])
        .bounded_output()
        .expect("run asks");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let asks: serde_json::Value = serde_json::from_slice(&output.stdout).expect("asks json");
    let options = asks[0]["questions"][0]["options"]
        .as_array()
        .expect("options");
    assert_eq!(options[0]["label"], "approve");
    assert_eq!(options[0]["mutates_trust"], true);
    assert!(
        options[0]["caution"]
            .as_str()
            .is_some_and(|text| text.contains("auto-accept"))
    );
    assert_eq!(options.len(), 1);
}

#[test]
fn asks_synthesizes_safe_plan_approval_without_transcript() {
    let env = Env::new();
    let payload = serde_json::to_string(&json!({
        "hook_event_name": "PreToolUse",
        "session_id": "sess-plan-missing-transcript",
        "tool_name": "ExitPlanMode",
        "tool_input": { "plan": "1. Make the change\n2. Verify it" }
    }))
    .expect("payload");
    let hook = env.run_hook("claude", &payload);
    assert!(
        hook.status.success(),
        "{}",
        String::from_utf8_lossy(&hook.stderr)
    );
    let snapshot = env.store().snapshot_cached().expect("agent snapshot");
    let open = snapshot.agents[0]
        .open_ask
        .as_ref()
        .expect("current plan ask");
    let transcript_dir = env.store().paths().transcript_dir.clone();
    std::fs::remove_dir_all(transcript_dir).expect("remove transcript bucket");

    let output = env
        .rimz()
        .args(["asks", "--json"])
        .bounded_output()
        .expect("run asks");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let asks: serde_json::Value = serde_json::from_slice(&output.stdout).expect("asks json");
    assert_eq!(
        asks[0]["questions"][0]["question"],
        open.detail.as_deref().expect("current plan detail")
    );
    let options = asks[0]["questions"][0]["options"]
        .as_array()
        .expect("options");
    assert_eq!(options.len(), 1);
    assert_eq!(options[0]["label"], "approve");
    assert_eq!(
        options[0]["caution"],
        "enables auto-accept for subsequent edits"
    );
}

#[test]
fn read_open_ask_rejects_ineligible_state_before_external_reads() {
    let env = Env::new();
    let store = env.store();
    assert!(!store.paths().transcript_dir.exists());

    let mut not_waiting = AgentState::stub("unknown", "sess-not-waiting", AgentStatus::Idle);
    not_waiting.open_ask = Some(OpenAsk {
        id: AskId::parse("ask_0123456789abcdef").expect("ask id"),
        kind: AskKind::Question,
        detail: Some("Ignored question".to_owned()),
        native_key: None,
        since: jiff::Timestamp::now(),
    });
    assert_eq!(
        rimz::agents::read_open_ask(store.paths(), &not_waiting).expect("eligible read"),
        None
    );

    let mut missing_open = AgentState::stub("unknown", "sess-no-open", AgentStatus::Waiting);
    missing_open.waiting_since = Some(missing_open.last_activity);
    assert!(missing_open.is_awaiting_input());
    assert_eq!(
        rimz::agents::read_open_ask(store.paths(), &missing_open).expect("eligible read"),
        None
    );
    assert!(!store.paths().transcript_dir.exists());
}

#[test]
fn codex_plan_stop_lists_rollout_plan_as_waiting_ask() {
    let env = Env::new();
    let transcript_path = write_codex_plan_rollout(&env);
    let hook = env.run_hook("codex", &codex_plan_payload(&transcript_path));
    assert!(
        hook.status.success(),
        "{}",
        String::from_utf8_lossy(&hook.stderr)
    );
    assert!(hook.stdout.is_empty());
    assert_eq!(env.snapshot_json()["agents"][0]["status"], "waiting");

    let output = env
        .rimz()
        .args(["asks", "--json"])
        .bounded_output()
        .expect("list Codex plan ask");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let asks: serde_json::Value = serde_json::from_slice(&output.stdout).expect("asks json");
    assert_eq!(asks[0]["kind"], "plan_approval");
    assert_eq!(asks[0]["context"], "Codex says:");
    assert_eq!(
        asks[0]["questions"][0]["question"],
        "Requesting plan approval:\n\n# Plan\n\nShip safely."
    );
    assert_eq!(asks[0]["questions"][0]["options"][0]["label"], "implement");
    assert_eq!(asks[0]["questions"][0]["options"][0]["mutates_trust"], true);
}

#[test]
fn asks_empty_and_stale_answer_are_machine_readable() {
    let env = Env::new();
    let output = env
        .rimz()
        .args(["asks", "--json"])
        .bounded_output()
        .expect("run empty asks");
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        json!([])
    );

    let output = env
        .rimz()
        .args(["answer", "ask_0123456789abcdef", "1"])
        .bounded_output()
        .expect("run stale answer");
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("no longer current"));
}

#[test]
fn answer_keeps_unverified_codex_permissions_in_the_pane() {
    let env = Env::new();
    let hook = env.run_hook("codex", &permission_payload("shell"));
    assert!(
        hook.status.success(),
        "{}",
        String::from_utf8_lossy(&hook.stderr)
    );

    let output = env
        .rimz()
        .args(["answer", "@codex", "allow"])
        .bounded_output()
        .expect("answer pane-only permission ask");
    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("use the agent pane"), "{stderr}");
}

#[test]
fn answer_refuses_unconfirmable_claude_menu_actions_before_pane_delivery() {
    for (payload, selector, valid) in [
        (permission_payload("Bash"), "deny", "valid options: 1=allow"),
        (
            serde_json::to_string(&json!({
                "hook_event_name": "PreToolUse",
                "session_id": "sess-plan-pane-only",
                "tool_name": "ExitPlanMode",
                "tool_input": { "plan": "1. Make the change" }
            }))
            .expect("plan payload"),
            "keep-planning",
            "valid options: 1=approve",
        ),
    ] {
        let env = Env::new();
        let hook = env.run_hook("claude", &payload);
        assert!(hook.status.success());

        let output = env
            .rimz()
            .args(["answer", "@claude", selector])
            .bounded_output()
            .expect("answer pane-only action");
        assert_eq!(output.status.code(), Some(3));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("agent pane"), "{stderr}");
        assert!(stderr.contains(valid), "{stderr}");
    }
}

#[cfg(unix)]
fn fake_tmux(env: &Env) -> (std::path::PathBuf, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;

    let bin = env.home_root.join("mux-bin");
    std::fs::create_dir_all(&bin).expect("mkdir mux bin");
    let shim = bin.join("tmux");
    std::fs::write(
        &shim,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$RIMZ_TEST_MUX_LOG\"\nexit 0\n",
    )
    .expect("write tmux shim");
    let mut permissions = std::fs::metadata(&shim).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&shim, permissions).unwrap();
    (bin, env.home_root.join("tmux.log"))
}

#[cfg(unix)]
#[test]
fn answer_question_sends_to_bound_pane_and_timeout_has_distinct_exit() {
    let env = Env::new();
    let hook =
        env.run_installed_hook_in_pane("claude", &question_payload(&env), &[("TMUX_PANE", "%7")]);
    assert!(
        hook.status.success(),
        "{}",
        String::from_utf8_lossy(&hook.stderr)
    );
    let pane_fixture = env.write_pane_fixture(&[tmux_pane("%7", "claude", &env.project_root)]);
    let (bin, log) = fake_tmux(&env);

    let output = env
        .rimz()
        .args(["answer", "@claude", "safe", "--no-wait", "--mux", "tmux"])
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .env("RIMZ_TEST_MUX_LOG", &log)
        .env("PATH", path_with_front(&bin))
        .bounded_output()
        .expect("answer question");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(std::fs::read_to_string(&log).unwrap().contains("%7"));

    let output = env
        .rimz()
        .args(["answer", "@claude", "fast", "--wait", "1s", "--mux", "tmux"])
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .env("RIMZ_TEST_MUX_LOG", &log)
        .env("PATH", path_with_front(&bin))
        .bounded_output()
        .expect("timeout question answer");
    assert_eq!(output.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&output.stderr).contains("did not confirm"));
}

#[cfg(unix)]
#[test]
fn subagent_ask_lists_with_parent_and_answers_through_parent_pane() {
    let env = Env::new();
    let root = serde_json::to_string(&json!({
        "hook_event_name": "SessionStart",
        "session_id": "sess-claude-parent",
    }))
    .expect("root payload");
    let root = env.run_installed_hook_in_pane("claude", &root, &[("TMUX_PANE", "%7")]);
    assert!(root.status.success());

    for payload in [
        json!({
            "hook_event_name": "SubagentStart",
            "session_id": "sess-claude-parent",
            "agent_id": "child-1",
            "subagent_type": "Explore",
        }),
        json!({
            "hook_event_name": "PermissionRequest",
            "session_id": "sess-claude-parent",
            "agent_id": "child-1",
            "tool_name": "Bash",
            "tool_input": { "command": "cargo test" },
        }),
    ] {
        let payload = serde_json::to_string(&payload).expect("child payload");
        assert!(env.run_hook("claude", &payload).status.success());
    }

    let output = env
        .rimz()
        .args(["asks", "--json"])
        .bounded_output()
        .expect("list child ask");
    assert!(output.status.success());
    let asks: serde_json::Value = serde_json::from_slice(&output.stdout).expect("asks json");
    assert_eq!(asks.as_array().unwrap().len(), 1);
    assert!(
        asks[0]["agent"]["handle"]
            .as_str()
            .is_some_and(|handle| handle.starts_with('@'))
    );
    assert_eq!(asks[0]["agent"]["name"], "Explore");
    let ask_id = asks[0]["ask_id"].as_str().expect("ask id");

    let pane_fixture = env.write_pane_fixture(&[tmux_pane("%7", "claude", &env.project_root)]);
    let (bin, log) = fake_tmux(&env);
    let output = env
        .rimz()
        .args(["answer", ask_id, "allow", "--no-wait", "--mux", "tmux"])
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .env("RIMZ_TEST_MUX_LOG", &log)
        .env("PATH", path_with_front(&bin))
        .bounded_output()
        .expect("answer child ask");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let sent = std::fs::read_to_string(&log).expect("tmux trace");
    assert!(sent.contains("send-keys -l -t %7 -- 1"), "{sent}");
}

#[test]
fn subagent_ask_without_a_live_parent_has_no_routable_handle() {
    let env = Env::new();
    let payload = json!({
        "hook_event_name": "PermissionRequest",
        "session_id": "missing-parent",
        "agent_id": "orphan-child",
        "tool_name": "Bash",
        "tool_input": { "command": "cargo test" },
    });
    assert!(
        env.run_hook("claude", &payload.to_string())
            .status
            .success()
    );
    let snapshot = env.snapshot_json();
    let ask_id = snapshot["agents"][0]["open_ask"]["id"]
        .as_str()
        .expect("orphan ask id");

    let listed = env
        .rimz()
        .args(["asks", "--json"])
        .bounded_output()
        .expect("list asks");
    assert!(listed.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&listed.stdout).unwrap(),
        json!([])
    );

    let shown = env
        .rimz()
        .args(["asks", "show", ask_id])
        .bounded_output()
        .expect("show orphan ask");
    assert!(!shown.status.success());
    assert!(
        String::from_utf8_lossy(&shown.stderr).contains("has no live root agent"),
        "{}",
        String::from_utf8_lossy(&shown.stderr)
    );
}

#[cfg(unix)]
#[test]
fn answer_confirmable_claude_menu_actions_reach_bound_pane() {
    for (payload, selector, expected_command) in [
        (
            permission_payload("Bash"),
            "allow",
            "send-keys -l -t %7 -- 1",
        ),
        (
            serde_json::to_string(&json!({
                "hook_event_name": "PreToolUse",
                "session_id": "sess-plan-approve",
                "tool_name": "ExitPlanMode",
                "tool_input": { "plan": "1. Make the change" }
            }))
            .expect("plan payload"),
            "approve",
            "send-keys -t %7 BTab",
        ),
    ] {
        let env = Env::new();
        let hook = env.run_installed_hook_in_pane("claude", &payload, &[("TMUX_PANE", "%7")]);
        assert!(
            hook.status.success(),
            "{}",
            String::from_utf8_lossy(&hook.stderr)
        );
        let pane_fixture = env.write_pane_fixture(&[tmux_pane("%7", "claude", &env.project_root)]);
        let (bin, log) = fake_tmux(&env);

        let output = env
            .rimz()
            .args(["answer", "@claude", selector, "--no-wait", "--mux", "tmux"])
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_TEST_MUX_LOG", &log)
            .env("PATH", path_with_front(&bin))
            .bounded_output()
            .expect("answer confirmed menu action");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let sent = std::fs::read_to_string(&log).unwrap();
        assert!(sent.contains("%7"), "{sent}");
        assert!(sent.contains(expected_command), "{sent}");
    }
}

#[cfg(unix)]
#[test]
fn answer_codex_plan_implement_reaches_bound_pane() {
    let env = Env::new();
    let transcript_path = write_codex_plan_rollout(&env);
    let hook = env.run_installed_hook_in_pane(
        "codex",
        &codex_plan_payload(&transcript_path),
        &[("TMUX_PANE", "%7")],
    );
    assert!(
        hook.status.success(),
        "{}",
        String::from_utf8_lossy(&hook.stderr)
    );
    let pane_fixture = env.write_pane_fixture(&[tmux_pane("%7", "codex", &env.project_root)]);
    let (bin, log) = fake_tmux(&env);

    let output = env
        .rimz()
        .args([
            "answer",
            "@codex",
            "implement",
            "--no-wait",
            "--mux",
            "tmux",
        ])
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .env("RIMZ_TEST_MUX_LOG", &log)
        .env("PATH", path_with_front(&bin))
        .bounded_output()
        .expect("answer Codex plan");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let sent = std::fs::read_to_string(&log).unwrap();
    assert!(sent.contains("send-keys -t %7 Enter"), "{sent}");
}

#[cfg(unix)]
#[test]
fn answer_codex_questions_sends_verified_option_choreography() {
    let env = Env::new();
    let hook =
        env.run_installed_hook_in_pane("codex", &codex_question_payload(), &[("TMUX_PANE", "%7")]);
    assert!(
        hook.status.success(),
        "{}",
        String::from_utf8_lossy(&hook.stderr)
    );
    let pane_fixture = env.write_pane_fixture(&[tmux_pane("%7", "codex", &env.project_root)]);
    let (bin, log) = fake_tmux(&env);

    let output = env
        .rimz()
        .args(["answer", "@codex", "B", "Z", "--no-wait", "--mux", "tmux"])
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .env("RIMZ_TEST_MUX_LOG", &log)
        .env("PATH", path_with_front(&bin))
        .bounded_output()
        .expect("answer Codex questions");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let sent = std::fs::read_to_string(&log).unwrap();
    assert_eq!(sent.matches("send-keys -t %7 Down").count(), 3, "{sent}");
    assert_eq!(sent.matches("send-keys -t %7 Enter").count(), 2, "{sent}");
}
