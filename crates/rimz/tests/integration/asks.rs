use serde_json::json;

#[cfg(unix)]
use crate::common::path_with_front;
use crate::common::{CommandTimeoutExt, Env, permission_payload, tmux_pane};

fn question_payload() -> String {
    serde_json::to_string(&json!({
        "hook_event_name": "PreToolUse",
        "session_id": "sess-question",
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

#[test]
fn asks_lists_and_shows_structured_question_json() {
    let env = Env::new();
    let hook = env.run_hook("claude", &question_payload());
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
    assert_eq!(shown["questions"][0]["options"][1]["label"], "fast");
}

#[test]
fn permission_request_does_not_replace_its_native_question_ask() {
    let env = Env::new();
    let question = env.run_hook("claude", &question_payload());
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
    assert_eq!(asks[0]["questions"][0]["options"][0]["label"], "allow");
    assert_eq!(
        asks[0]["questions"][0]["options"].as_array().unwrap().len(),
        1
    );
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
fn answer_refuses_an_unwired_agent_before_pane_delivery() {
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
        .expect("answer unsupported ask");
    assert_eq!(output.status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("codex does not support structured answers")
    );
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
        env.run_installed_hook_in_pane("claude", &question_payload(), &[("TMUX_PANE", "%7")]);
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
