use std::time::{Duration, Instant};

use jiff::Timestamp;
use serde_json::json;

use rimz::feed::{FeedItem, FeedKind, Surface};
use rimz::ids::{AgentKind, AgentSessionId};
use rimz::ledger::transcript_log::{TranscriptEntry, TranscriptKind};

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
    assert!(
        single.contains("user\n    @claude, first prompt"),
        "{single}"
    );
    assert!(single.contains("@claude\n    final answer"), "{single}");
    assert!(!single.contains("needs attention"), "{single}");
    assert!(single.contains("you\n    @claude, allow"), "{single}");
    assert!(
        !single.contains("draft answer"),
        "durable log stores the turn-final assistant message only:\n{single}"
    );

    let channel = run_ok(env.rimz().args(["transcript", "#feature-transcript"]));
    assert!(channel.contains("#feature-transcript"), "{channel}");
    assert!(
        channel.contains("user\n    @claude, first prompt"),
        "{channel}"
    );
    assert!(channel.contains("@claude\n    final answer"), "{channel}");
    assert!(
        channel.contains("user\n    @codex, second prompt"),
        "{channel}"
    );
    assert!(channel.contains("@codex\n    second answer"), "{channel}");
    assert!(channel.contains("you\n    @claude, allow"), "{channel}");
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
    assert!(entries.iter().all(|entry| {
        !entry["text"]
            .as_str()
            .is_some_and(|text| text.contains("needs attention"))
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
fn transcript_records_native_ask_question_context_and_answer() {
    let env = Env::new();
    let branch = "native-ask-transcript";
    let session_id = "sess-native-ask";
    let claude_path = env.home_root.join("native-ask-chat.jsonl");
    write_claude_ask_transcript(&claude_path, "here is my read");
    let transcript = claude_path.to_string_lossy().into_owned();

    run_hook(
        &env,
        "claude",
        json!({
            "hook_event_name": "SessionStart",
            "session_id": session_id,
            "worktree_branch": branch,
            "transcript_path": transcript.as_str(),
        }),
    );
    run_hook(
        &env,
        "claude",
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": session_id,
            "prompt": "review deployment options",
            "worktree_branch": branch,
            "transcript_path": transcript.as_str(),
        }),
    );
    run_hook(
        &env,
        "claude",
        json!({
            "hook_event_name": "PreToolUse",
            "session_id": session_id,
            "tool_name": "AskUserQuestion",
            "tool_input": {
                "questions": [{
                    "question": "Choose deployment path?",
                    "options": [{ "label": "safe" }, { "label": "fast" }]
                }]
            },
            "worktree_branch": branch,
            "transcript_path": transcript.as_str(),
        }),
    );
    run_hook(
        &env,
        "claude",
        json!({
            "hook_event_name": "PostToolUse",
            "session_id": session_id,
            "tool_name": "AskUserQuestion",
            "tool_response": { "answers": ["safe"] },
            "worktree_branch": branch,
            "transcript_path": transcript.as_str(),
        }),
    );

    let output = run_ok(env.rimz().args(["transcript", &format!("#{branch}")]));
    assert!(output.contains("here is my read"), "{output}");
    assert!(
        output.contains("Choose deployment path? [safe, fast]"),
        "{output}"
    );
    assert!(output.contains("you\n    @claude, safe"), "{output}");
    assert!(!output.contains("claude needs attention"), "{output}");

    let feed = env.feed_list_json();
    let items = feed.as_array().expect("feed items");
    assert!(
        items.iter().all(|item| item["status"] != "pending"),
        "{feed}"
    );
}

#[test]
fn transcript_groups_chronological_entries_across_append_order() {
    let env = Env::new();
    let branch = "chronological-transcript";
    append_transcript(
        &env,
        entry(
            "sess-order",
            branch,
            TranscriptKind::Prompt,
            "first prompt",
            "2026-06-01T00:00:00Z",
        ),
    );
    append_transcript(
        &env,
        entry(
            "sess-order",
            branch,
            TranscriptKind::Prompt,
            "second prompt",
            "2026-06-01T00:00:03Z",
        ),
    );
    append_transcript(
        &env,
        entry(
            "sess-order",
            branch,
            TranscriptKind::Assistant,
            "first answer",
            "2026-06-01T00:00:02Z",
        ),
    );
    append_transcript(
        &env,
        entry(
            "sess-order",
            branch,
            TranscriptKind::Assistant,
            "second answer",
            "2026-06-01T00:00:04Z",
        ),
    );

    let output = run_ok(
        env.rimz()
            .args(["transcript", "sess-order", "--worktree", branch]),
    );

    assert!(
        output.contains("user\n    @claude, first prompt"),
        "{output}"
    );
    assert!(output.contains("@claude\n    first answer"), "{output}");
    assert!(
        output.contains("user\n    @claude, second prompt"),
        "{output}"
    );
    assert!(output.contains("@claude\n    second answer"), "{output}");
    assert!(
        output.find("first answer").unwrap() < output.find("second prompt").unwrap(),
        "{output}"
    );
}

#[test]
fn transcript_exact_session_target_filters_same_handle_peers() {
    let env = Env::new();
    let branch = "same-handle-transcript";
    append_transcript(
        &env,
        entry(
            "sess-same-a",
            branch,
            TranscriptKind::Prompt,
            "prompt from a",
            "2026-06-01T00:00:00Z",
        ),
    );
    append_transcript(
        &env,
        entry(
            "sess-same-a",
            branch,
            TranscriptKind::Assistant,
            "answer from a",
            "2026-06-01T00:00:01Z",
        ),
    );
    append_transcript(
        &env,
        entry(
            "sess-same-b",
            branch,
            TranscriptKind::Prompt,
            "prompt from b",
            "2026-06-01T00:00:02Z",
        ),
    );
    append_transcript(
        &env,
        entry(
            "sess-same-b",
            branch,
            TranscriptKind::Assistant,
            "answer from b",
            "2026-06-01T00:00:03Z",
        ),
    );

    let one = run_ok(
        env.rimz()
            .args(["transcript", "sess-same-a", "--worktree", branch]),
    );
    assert!(one.contains("prompt from a"), "{one}");
    assert!(one.contains("answer from a"), "{one}");
    assert!(!one.contains("prompt from b"), "{one}");
    assert!(!one.contains("answer from b"), "{one}");

    let ambiguous = env
        .rimz()
        .args(["transcript", &format!("@claude#{branch}")])
        .output()
        .expect("spawn transcript");
    assert!(!ambiguous.status.success(), "ambiguous target should fail");
    let stderr = String::from_utf8_lossy(&ambiguous.stderr);
    assert!(stderr.contains("matched multiple agents"), "{stderr}");
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
        channel.contains("@claude\n    @codex, do the thing"),
        "{channel}"
    );
    assert!(channel.contains("@codex\n    @claude, ack"), "{channel}");
    assert!(!channel.contains("hidden codex reply"), "{channel}");
    assert!(!channel.contains("hidden claude reply"), "{channel}");

    let codex = run_ok(
        env.rimz()
            .args(["transcript", "@codex#attribution-transcript"]),
    );
    assert!(
        codex.contains("@claude\n    @codex, do the thing"),
        "{codex}"
    );
    assert!(codex.contains("@codex\n    @claude, ack"), "{codex}");
    assert!(!codex.contains("hidden codex reply"), "{codex}");
    assert!(!codex.contains("hidden claude reply"), "{codex}");
}

fn entry(
    session_id: &str,
    branch: &str,
    kind: TranscriptKind,
    text: &str,
    at: &str,
) -> TranscriptEntry {
    TranscriptEntry {
        at: at.parse().expect("timestamp"),
        kind: AgentKind::new_unchecked("claude"),
        agent_id: AgentSessionId::from(session_id),
        channel: Some(branch.to_owned()),
        name: None,
        profile: None,
        role: None,
        entry: kind,
        request_id: None,
        from: None,
        text: text.to_owned(),
    }
}

fn append_transcript(env: &Env, entry: TranscriptEntry) {
    rimz::ledger::transcript_log::append(env.ledger().paths(), &entry).expect("append transcript");
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

fn write_claude_ask_transcript(path: &std::path::Path, message: &str) {
    std::fs::write(
        path,
        format!(
            r#"{{"type":"assistant","timestamp":"2026-06-01T00:00:01Z","message":{{"content":[{{"type":"text","text":"{message}"}}]}}}}"#
        ) + "\n"
            + r#"{"type":"assistant","timestamp":"2026-06-01T00:00:02Z","message":{"content":[{"type":"tool_use","name":"AskUserQuestion","input":{"questions":[]}}]}}"#
            + "\n",
    )
    .expect("write claude ask transcript");
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
