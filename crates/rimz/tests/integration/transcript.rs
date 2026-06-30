use std::time::{Duration, Instant};

use jiff::Timestamp;
use serde_json::json;

use rimz::feed::{FeedItem, FeedKind, Surface};

use crate::common::Env;

const BRIDGE_ITEM_WAIT: Duration = Duration::from_secs(5);

#[test]
fn transcript_renders_durable_turns_asks_answers_and_channels() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let branch = "feature-transcript";
    let other = "other-transcript";
    let claude_path = env.home_root.join("first-chat.jsonl");
    write_claude_transcript(&claude_path, "draft answer", "final answer");

    register_claude_turn(
        &env,
        "sess-transcript-a",
        branch,
        &claude_path,
        "first prompt",
    );
    register_codex_turn(
        &env,
        "sess-transcript-b",
        branch,
        "second prompt",
        "second answer",
    );
    register_codex_turn(
        &env,
        "sess-transcript-c",
        other,
        "other prompt",
        "other answer",
    );
    bridge_permission_to_allow(&env, "sess-transcript-a", branch, &claude_path);

    let single = run_ok(
        env.rimz()
            .args(["transcript", "sess-transcript-a", "--worktree", branch]),
    );
    assert!(single.contains("#feature-transcript"), "{single}");
    assert!(single.contains("user: @claude, first prompt"), "{single}");
    assert!(single.contains("@claude: final answer"), "{single}");
    assert!(single.contains("@claude: final answer\n"), "{single}");
    assert!(single.contains("claude needs attention"), "{single}");
    assert!(single.contains("you: @claude, allow"), "{single}");
    assert!(
        !single.contains("draft answer"),
        "durable log stores the turn-final assistant message only:\n{single}"
    );

    let channel = run_ok(env.rimz().args(["transcript", "#feature-transcript"]));
    assert!(channel.contains("#feature-transcript"), "{channel}");
    assert!(channel.contains("user: @claude, first prompt"), "{channel}");
    assert!(channel.contains("@claude: final answer"), "{channel}");
    assert!(channel.contains("user: @codex, second prompt"), "{channel}");
    assert!(channel.contains("@codex: second answer"), "{channel}");
    assert!(channel.contains("you: @claude, allow"), "{channel}");
    assert!(!channel.contains("other prompt"), "{channel}");

    let all = run_ok(env.rimz().args(["transcript", "@all"]));
    assert!(all.contains("@claude#feature-transcript"), "{all}");
    assert!(all.contains("@codex#feature-transcript"), "{all}");
    assert!(all.contains("@codex#other-transcript"), "{all}");

    let json = run_ok(env.rimz().args([
        "transcript",
        "sess-transcript-a",
        "--worktree",
        branch,
        "--json",
    ]));
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("transcript json");
    assert!(parsed.get("asks").is_none(), "{parsed}");
    let entries = parsed["entries"].as_array().expect("entries");
    assert!(entries.iter().any(|entry| {
        entry["from"] == "user" && entry["to"] == "@claude" && entry["text"] == "first prompt"
    }));
    assert!(
        entries
            .iter()
            .any(|entry| { entry["from"] == "@claude" && entry["text"] == "final answer" })
    );
    assert!(entries.iter().any(|entry| {
        entry["from"] == "@claude"
            && entry["text"].as_str().is_some_and(|text| {
                text.contains("final answer") && text.contains("claude needs attention")
            })
    }));
    assert!(entries.iter().any(|entry| {
        entry["from"] == "you" && entry["to"] == "@claude" && entry["text"] == "allow"
    }));

    push_pending_agent_ask(&env, "sess-transcript-a");
    let show = run_ok(env.rimz().args(["agents", "show", "sess-transcript-a"]));
    assert!(show.contains("ask:"), "{show}");
    assert!(
        show.contains("approve patch: choose one [allow, deny]"),
        "{show}"
    );
    let transcript_after_pending =
        run_ok(
            env.rimz()
                .args(["transcript", "sess-transcript-a", "--worktree", branch]),
        );
    assert!(
        !transcript_after_pending.contains("approve patch"),
        "live pending asks are no longer overlaid on transcript output:\n{transcript_after_pending}"
    );
}

#[test]
fn transcript_attributes_agent_messages_and_filters_agent_view() {
    let env = Env::new();
    let branch = "attribution-transcript";
    let claude_path = env.home_root.join("claude-attribution.jsonl");
    write_claude_transcript(&claude_path, "hidden claude draft", "hidden claude reply");
    register_claude_turn(
        &env,
        "sess-attribution-claude",
        branch,
        &claude_path,
        "from @codex: ack",
    );
    register_codex_turn(
        &env,
        "sess-attribution-codex",
        branch,
        "from @claude: do the thing",
        "hidden codex reply",
    );

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

fn write_claude_transcript(path: &std::path::Path, draft: &str, final_message: &str) {
    std::fs::write(
        path,
        format!(
            r#"{{"type":"assistant","timestamp":"2026-06-01T00:00:01Z","message":{{"content":[{{"type":"text","text":"{draft}"}}]}}}}"#
        ) + "\n"
            + &format!(
                r#"{{"type":"assistant","timestamp":"2026-06-01T00:00:02Z","message":{{"content":[{{"type":"text","text":"{final_message}"}}]}}}}"#
            )
            + "\n",
    )
    .expect("write claude transcript");
}

fn register_claude_turn(
    env: &Env,
    session_id: &str,
    branch: &str,
    transcript: &std::path::Path,
    prompt: &str,
) {
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
            "prompt": prompt,
            "worktree_branch": branch,
            "transcript_path": transcript,
        }),
    );
    run_hook(
        env,
        "claude",
        json!({
            "hook_event_name": "Stop",
            "session_id": session_id,
            "worktree_branch": branch,
            "transcript_path": transcript,
        }),
    );
}

fn register_codex_turn(env: &Env, session_id: &str, branch: &str, prompt: &str, answer: &str) {
    run_hook(
        env,
        "codex",
        json!({
            "hook_event_name": "SessionStart",
            "session_id": session_id,
            "worktree_branch": branch,
        }),
    );
    run_hook(
        env,
        "codex",
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": session_id,
            "prompt": prompt,
            "worktree_branch": branch,
        }),
    );
    run_hook(
        env,
        "codex",
        json!({
            "hook_event_name": "Stop",
            "session_id": session_id,
            "last_assistant_message": answer,
            "worktree_branch": branch,
        }),
    );
}

fn bridge_permission_to_allow(
    env: &Env,
    session_id: &str,
    branch: &str,
    transcript: &std::path::Path,
) {
    env.enrol("opus-policy", 10, "30s");
    env.write_heartbeat("opus-policy", Timestamp::now());
    let transcript = transcript.to_string_lossy().into_owned();
    let payload = serde_json::to_string(&json!({
        "hook_event_name": "PermissionRequest",
        "session_id": session_id,
        "tool_name": "Bash",
        "tool_input": { "command": "echo hi" },
        "worktree_branch": branch,
        "transcript_path": transcript,
    }))
    .expect("payload");
    let mut cmd = env.hook_command("claude");
    cmd.env_remove("RIMZ_AGENT_PID");
    cmd.env(rimz::run::ENV_AGENT_ROLE, "claude");
    let child = env.spawn_payload(cmd, &payload);
    let request_id = env
        .poll_pending_request_id(Instant::now() + BRIDGE_ITEM_WAIT)
        .expect("bridge item should appear in feed");

    let resolve = env.resolve(&request_id, r#"{"choice":"allow"}"#, "opus-policy", "cli");
    assert!(
        resolve.status.success(),
        "resolve failed: {}",
        String::from_utf8_lossy(&resolve.stderr)
    );
    let output = child.wait_with_output().expect("wait hook");
    assert!(
        output.status.success(),
        "hook failed\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_hook(env: &Env, source: &str, payload: serde_json::Value) {
    let payload = serde_json::to_string(&payload).expect("payload");
    let mut cmd = env.hook_command(source);
    cmd.env_remove("RIMZ_AGENT_PID");
    cmd.env(rimz::run::ENV_AGENT_ROLE, source);
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
