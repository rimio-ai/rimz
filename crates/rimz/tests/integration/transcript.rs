use serde_json::json;

use rimz::feed::{FeedItem, FeedKind, Surface};

use crate::common::Env;

#[test]
fn transcript_renders_agent_turns_channel_timeline_and_pending_asks() {
    let env = Env::new();
    let first = env.home_root.join("first-chat.jsonl");
    let codex_sessions = env.home_root.join("codex-sessions");
    let codex_day = codex_sessions.join("2026").join("06").join("01");
    std::fs::create_dir_all(&codex_day).expect("mkdir codex day");
    let second = codex_day.join("rollout-2026-06-01T00-00-00-sess-transcript-b.jsonl");
    std::fs::write(
        &first,
        r#"{"type":"user","timestamp":"2026-06-01T00:00:00Z","message":{"content":[{"type":"text","text":"first prompt"}]}}"#
            .to_owned()
            + "\n"
            + r#"{"type":"assistant","timestamp":"2026-06-01T00:00:01Z","message":{"content":[{"type":"text","text":"draft answer"}]}}"#
            + "\n"
            + r#"{"type":"assistant","timestamp":"2026-06-01T00:00:02Z","message":{"content":[{"type":"text","text":"final answer"}]}}"#
            + "\n",
    )
    .expect("write first transcript");
    std::fs::write(
        &second,
        r#"{"timestamp":"2026-06-01T00:00:01Z","type":"event_msg","payload":{"type":"user_message","message":"second prompt"}}"#
            .to_owned()
            + "\n"
            + r#"{"timestamp":"2026-06-01T00:00:03Z","type":"event_msg","payload":{"type":"agent_message","message":"second answer"}}"#
            + "\n",
    )
    .expect("write second transcript");

    register_claude_agent(&env, "sess-transcript-a", "feature-transcript", &first);
    register_codex_agent(
        &env,
        "sess-transcript-b",
        "feature-transcript",
        &codex_sessions,
    );
    push_pending_agent_ask(&env, "sess-transcript-a");

    let single = run_ok(env.rimz().args([
        "transcript",
        "sess-transcript-a",
        "--worktree",
        "feature-transcript",
    ]));
    assert!(single.contains("#feature-transcript"), "{single}");
    assert!(
        single.contains("user  00:00:00\n  first prompt"),
        "{single}"
    );
    assert!(
        single.contains("assistant  00:00:02\n  @claude  final answer"),
        "{single}"
    );
    assert!(
        !single.contains("draft answer"),
        "default view keeps only the final assistant message:\n{single}"
    );
    assert!(single.contains("\nask\n  approve patch"), "{single}");
    assert!(
        single.contains("approve patch: choose one [allow, deny]"),
        "{single}"
    );

    let details = run_ok(env.rimz().args([
        "transcript",
        "sess-transcript-a",
        "--worktree",
        "feature-transcript",
        "--details",
    ]));
    assert!(details.contains("draft answer"), "{details}");

    let channel = run_ok(env.rimz().args(["transcript", "#feature-transcript"]));
    let first_prompt = channel
        .find("first prompt")
        .expect("first prompt in channel");
    let second_prompt = channel
        .find("second prompt")
        .expect("second prompt in channel");
    let final_answer = channel
        .find("final answer")
        .expect("first final in channel");
    let second_answer = channel
        .find("second answer")
        .expect("second answer in channel");
    assert!(
        first_prompt < second_prompt
            && second_prompt < final_answer
            && final_answer < second_answer,
        "channel timeline should sort by transcript timestamps:\n{channel}"
    );
    assert!(channel.contains("#feature-transcript"), "{channel}");
    assert!(channel.contains("@claude"), "{channel}");
    assert!(channel.contains("@codex"), "{channel}");
    assert!(
        !channel.contains("you→@") && !channel.contains("@claude#feature-transcript"),
        "{channel}"
    );
    assert!(channel.contains("second answer"), "{channel}");

    let json = run_ok(env.rimz().args([
        "transcript",
        "sess-transcript-a",
        "--worktree",
        "feature-transcript",
        "--json",
    ]));
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("transcript json");
    assert_eq!(parsed["turns"][0]["messages"][0]["text"], "first prompt");
    assert_eq!(parsed["turns"][0]["messages"][1]["text"], "final answer");
    assert_eq!(parsed["ask"]["title"], "approve patch");

    let show = run_ok(env.rimz().args(["agents", "show", "sess-transcript-a"]));
    assert!(show.contains("ask:"), "{show}");
    assert!(show.contains("approve patch"), "{show}");

    let show_json = run_ok(
        env.rimz()
            .args(["agents", "show", "sess-transcript-a", "--json"]),
    );
    let parsed: serde_json::Value = serde_json::from_str(&show_json).expect("show json");
    assert_eq!(parsed["ask"]["title"], "approve patch");
    assert_eq!(parsed["ask"]["options"][0], "allow");
}

fn register_claude_agent(env: &Env, session_id: &str, branch: &str, transcript: &std::path::Path) {
    let transcript = transcript.to_string_lossy().into_owned();
    run_hook(
        env,
        "claude",
        json!({
            "hook_event_name": "SessionStart",
            "session_id": session_id,
            "worktree_branch": branch,
            "transcript_path": transcript,
        }),
    );
    run_hook(
        env,
        "claude",
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": session_id,
            "prompt": "work",
            "worktree_branch": branch,
            "transcript_path": transcript,
        }),
    );
}

fn register_codex_agent(env: &Env, session_id: &str, branch: &str, sessions: &std::path::Path) {
    let sessions = sessions.to_string_lossy().into_owned();
    run_hook_with_env(
        env,
        "codex",
        json!({
            "hook_event_name": "SessionStart",
            "session_id": session_id,
            "worktree_branch": branch,
        }),
        &[("RIMZ_CODEX_SESSIONS", sessions.as_str())],
    );
    run_hook_with_env(
        env,
        "codex",
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": session_id,
            "prompt": "work",
            "worktree_branch": branch,
        }),
        &[("RIMZ_CODEX_SESSIONS", sessions.as_str())],
    );
}

fn run_hook(env: &Env, source: &str, payload: serde_json::Value) {
    run_hook_with_env(env, source, payload, &[]);
}

fn run_hook_with_env(env: &Env, source: &str, payload: serde_json::Value, vars: &[(&str, &str)]) {
    let payload = serde_json::to_string(&payload).expect("payload");
    let mut cmd = env.hook_command(source);
    for (key, value) in vars {
        cmd.env(key, value);
    }
    let output = env
        .spawn_payload(cmd, &payload)
        .wait_with_output()
        .expect("wait hook");
    assert!(
        output.status.success(),
        "hook failed\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn push_pending_agent_ask(env: &Env, session_id: &str) {
    let mut item = FeedItem::new(
        env.workspace_id.clone(),
        Surface::NativeUi,
        FeedKind::Permission,
        "approve patch",
        "claude",
        "agent-hook",
    );
    item.body = Some("choose one".to_owned());
    item.options = vec!["allow".to_owned(), "deny".to_owned()];
    item.payload = json!({ "session_id": session_id });
    env.ledger()
        .push_feed_item(&item, "rimz-test")
        .expect("push pending ask");
}

fn run_ok(cmd: &mut std::process::Command) -> String {
    let out = cmd.output().expect("spawn rimz");
    assert!(
        out.status.success(),
        "command failed\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}
