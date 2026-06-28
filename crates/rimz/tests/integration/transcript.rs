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
        single.contains("00:00:00 user: @claude, first prompt"),
        "{single}"
    );
    assert!(
        single.contains("00:00:02 @claude: final answer"),
        "{single}"
    );
    assert!(
        !single.contains("draft answer"),
        "default view keeps only the final assistant message:\n{single}"
    );
    assert!(
        single.contains("@claude: approve patch: choose one [allow, deny]"),
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
        "channel chat log should sort by transcript timestamps:\n{channel}"
    );
    assert!(channel.contains("#feature-transcript"), "{channel}");
    assert!(channel.contains("user: @claude, first prompt"), "{channel}");
    assert!(channel.contains("@claude: final answer"), "{channel}");
    assert!(channel.contains("user: @codex, second prompt"), "{channel}");
    assert!(channel.contains("@codex: second answer"), "{channel}");
    assert!(
        !channel.contains("you→@")
            && !channel.contains("assistant")
            && !channel.contains("@claude#feature-transcript"),
        "{channel}"
    );

    let json = run_ok(env.rimz().args([
        "transcript",
        "sess-transcript-a",
        "--worktree",
        "feature-transcript",
        "--json",
    ]));
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("transcript json");
    assert_eq!(parsed["entries"][0]["from"], "user");
    assert_eq!(parsed["entries"][0]["to"], "@claude");
    assert_eq!(parsed["entries"][0]["text"], "first prompt");
    assert_eq!(parsed["entries"][1]["from"], "@claude");
    assert_eq!(parsed["entries"][1]["text"], "final answer");
    assert_eq!(parsed["asks"][0]["title"], "approve patch");

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

#[test]
fn transcript_attributes_agent_messages_and_filters_agent_view() {
    let env = Env::new();
    let branch = "attribution-transcript";
    let claude_path = env.home_root.join("claude-attribution.jsonl");
    let codex_sessions = env.home_root.join("codex-attribution-sessions");
    let codex_day = codex_sessions.join("2026").join("06").join("01");
    std::fs::create_dir_all(&codex_day).expect("mkdir codex day");
    let codex_path = codex_day.join("rollout-2026-06-01T00-00-00-sess-attribution-codex.jsonl");
    std::fs::write(
        &claude_path,
        r#"{"type":"user","timestamp":"2026-06-01T00:00:02Z","message":{"content":[{"type":"text","text":"from @codex: ack"}]}}"#
            .to_owned()
            + "\n"
            + r#"{"type":"assistant","timestamp":"2026-06-01T00:00:03Z","message":{"content":[{"type":"text","text":"hidden claude reply"}]}}"#
            + "\n",
    )
    .expect("write claude transcript");
    std::fs::write(
        &codex_path,
        r#"{"timestamp":"2026-06-01T00:00:01Z","type":"event_msg","payload":{"type":"user_message","message":"from @claude: do the thing"}}"#
            .to_owned()
            + "\n"
            + r#"{"timestamp":"2026-06-01T00:00:04Z","type":"event_msg","payload":{"type":"agent_message","message":"hidden codex reply"}}"#
            + "\n",
    )
    .expect("write codex transcript");

    register_claude_agent(&env, "sess-attribution-claude", branch, &claude_path);
    register_codex_agent(&env, "sess-attribution-codex", branch, &codex_sessions);

    let channel = run_ok(env.rimz().args(["transcript", "#attribution-transcript"]));
    assert!(
        channel.contains("@claude: @codex, do the thing"),
        "{channel}"
    );
    assert!(channel.contains("@codex: @claude, ack"), "{channel}");
    assert!(!channel.contains("hidden codex reply"), "{channel}");
    assert!(!channel.contains("hidden claude reply"), "{channel}");

    let codex = run_ok(
        env.rimz()
            .args(["transcript", "@codex#attribution-transcript"]),
    );
    assert!(codex.contains("@claude: @codex, do the thing"), "{codex}");
    assert!(codex.contains("@codex: @claude, ack"), "{codex}");
    assert!(!codex.contains("hidden codex reply"), "{codex}");
    assert!(!codex.contains("hidden claude reply"), "{codex}");
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
