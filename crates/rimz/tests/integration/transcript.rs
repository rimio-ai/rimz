use serde_json::json;

use rimz::ids::{AgentKind, AgentSessionId};
use rimz::transcript::{TranscriptEntry, TranscriptKind};

use crate::common::Env;

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
    let single = run_ok(
        env.rimz()
            .args(["transcript", "sess-transcript-a", "--worktree", branch]),
    );
    assert!(single.contains("#feature-transcript"), "{single}");
    assert!(single.contains(" user  → @claude"), "{single}");
    assert!(single.contains("\nfirst prompt"), "{single}");
    assert!(single.contains("@claude"), "{single}");
    assert!(single.contains("│ final answer"), "{single}");
    assert!(!single.contains("needs attention"), "{single}");
    assert!(
        !single.contains("draft answer"),
        "durable log stores the turn-final assistant message only:\n{single}"
    );

    let channel = run_ok(env.rimz().args(["transcript", "#feature-transcript"]));
    assert!(channel.contains("#feature-transcript"), "{channel}");
    assert!(channel.contains(" user  → @claude"), "{channel}");
    assert!(channel.contains("\nfirst prompt"), "{channel}");
    assert!(channel.contains("@claude"), "{channel}");
    assert!(channel.contains("│ final answer"), "{channel}");
    assert!(channel.contains(" user  → @codex"), "{channel}");
    assert!(channel.contains("\nsecond prompt"), "{channel}");
    assert!(channel.contains("@codex"), "{channel}");
    assert!(channel.contains("│ second answer"), "{channel}");
    assert!(!channel.contains("other prompt"), "{channel}");

    let all = run_ok(env.rimz().args(["transcript", "@all", "--all"]));
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
    push_pending_agent_ask(&env, "sess-transcript-a");
    let show = run_ok(env.rimz().args(["agents", "show", "sess-transcript-a"]));
    assert!(show.contains("ask:"), "{show}");
    assert!(show.contains("approve patch [allow, deny]"), "{show}");
    let transcript_after_pending =
        run_ok(
            env.rimz()
                .args(["transcript", "sess-transcript-a", "--worktree", branch]),
        );
    assert!(
        transcript_after_pending.contains("approve patch"),
        "{transcript_after_pending}"
    );
    assert!(
        transcript_after_pending.contains("◌ unanswered"),
        "{transcript_after_pending}"
    );
    assert!(
        !transcript_after_pending.contains("needs attention"),
        "{transcript_after_pending}"
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
                    "options": [
                        {
                            "label": "safe",
                            "description": "Use staged rollout with rollback ready."
                        },
                        { "label": "fast" }
                    ]
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
            "tool_response": {
                "annotations": {},
                "answers": { "Choose deployment path?": "safe" },
                "questions": [{
                    "question": "Choose deployment path?",
                    "header": "Path",
                    "options": [
                        {
                            "label": "safe",
                            "description": "Use staged rollout with rollback ready."
                        },
                        { "label": "fast" }
                    ]
                }]
            },
            "worktree_branch": branch,
            "transcript_path": transcript.as_str(),
        }),
    );

    let output = run_ok(env.rimz().args(["transcript", &format!("#{branch}")]));
    assert!(output.contains("here is my read"), "{output}");
    assert!(output.contains("│ │ Choose deployment path?"), "{output}");
    assert!(output.contains("│ │ ● safe — you"), "{output}");
    assert!(
        output.contains("│ │     Use staged rollout with rollback ready."),
        "{output}"
    );
    assert!(output.contains("│ │ ○ fast"), "{output}");
    assert!(!output.contains(" you  → @claude"), "{output}");
    assert!(!output.contains("\"answers\""), "{output}");
    assert!(!output.contains("claude needs attention"), "{output}");

    let json = run_ok(
        env.rimz()
            .args(["transcript", &format!("#{branch}"), "--json"]),
    );
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("transcript json");
    let entries = parsed["entries"].as_array().expect("entries");
    assert!(entries.iter().any(|entry| {
        entry["questions"].as_array().is_some_and(|questions| {
            questions.first().is_some_and(|question| {
                question["question"] == "Choose deployment path?"
                    && question["options"].as_array().is_some_and(|options| {
                        options.len() == 2
                            && options[0]
                                == json!({
                                    "label": "safe",
                                    "description": "Use staged rollout with rollback ready."
                                })
                            && options[1] == json!("fast")
                    })
            })
        })
    }));
    assert!(entries.iter().any(|entry| {
        entry["answers"].as_array().is_some_and(|answers| {
            answers.first().is_some_and(|answer| {
                answer["question"] == "Choose deployment path?"
                    && answer["chosen"]
                        .as_array()
                        .is_some_and(|chosen| chosen.first() == Some(&json!("safe")))
            })
        })
    }));
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

    assert!(output.contains(" user  → @claude"), "{output}");
    assert!(output.contains("\nfirst prompt"), "{output}");
    assert!(output.contains("@claude"), "{output}");
    assert!(output.contains("│ first answer"), "{output}");
    assert!(output.contains("\nsecond prompt"), "{output}");
    assert!(output.contains("│ second answer"), "{output}");
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

    let latest = run_ok(
        env.rimz()
            .args(["transcript", &format!("@claude#{branch}")]),
    );
    assert!(!latest.contains("prompt from a"), "{latest}");
    assert!(!latest.contains("answer from a"), "{latest}");
    assert!(latest.contains("prompt from b"), "{latest}");
    assert!(latest.contains("answer from b"), "{latest}");
}

#[test]
fn transcript_attributes_agent_messages_and_filters_agent_view() {
    let env = Env::new();
    let branch = "attribution-transcript";
    append_transcript(
        &env,
        message_entry(
            "claude",
            "sess-attribution-claude",
            branch,
            "@codex",
            "ack",
            "2026-06-01T00:00:00Z",
        ),
    );
    append_transcript(
        &env,
        agent_entry(
            "claude",
            "sess-attribution-claude",
            branch,
            TranscriptKind::Assistant,
            "visible claude reply",
            "2026-06-01T00:00:01Z",
        ),
    );
    append_transcript(
        &env,
        message_entry(
            "codex",
            "sess-attribution-codex",
            branch,
            "@claude",
            "do the thing",
            "2026-06-01T00:00:02Z",
        ),
    );
    append_transcript(
        &env,
        agent_entry(
            "codex",
            "sess-attribution-codex",
            branch,
            TranscriptKind::Assistant,
            "visible codex reply",
            "2026-06-01T00:00:03Z",
        ),
    );

    let channel = run_ok(env.rimz().args(["transcript", "#attribution-transcript"]));
    assert!(channel.contains("@claude → @codex"), "{channel}");
    assert!(channel.contains("\ndo the thing"), "{channel}");
    assert!(channel.contains("@codex → @claude"), "{channel}");
    assert!(channel.contains("\nack"), "{channel}");
    assert!(channel.contains("@claude"), "{channel}");
    assert!(channel.contains("│ visible claude reply"), "{channel}");
    assert!(channel.contains("@codex"), "{channel}");
    assert!(channel.contains("│ visible codex reply"), "{channel}");

    let codex = run_ok(
        env.rimz()
            .args(["transcript", "@codex#attribution-transcript"]),
    );
    assert!(codex.contains("@claude → @codex"), "{codex}");
    assert!(codex.contains("\ndo the thing"), "{codex}");
    assert!(codex.contains("@codex → @claude"), "{codex}");
    assert!(codex.contains("\nack"), "{codex}");
    assert!(codex.contains("│ visible codex reply"), "{codex}");
    assert!(!codex.contains("visible claude reply"), "{codex}");
}

#[test]
fn transcript_hook_records_routed_prompt_as_message_entry() {
    let env = Env::new();
    let branch = "hook-routed-transcript";
    register_codex_turn(
        &env,
        "sess-hook-routed",
        branch,
        "from @claude: ship it",
        "codex reply",
    );

    let entries = rimz::transcript::read_all(env.store().paths()).expect("read log");
    let message = entries
        .iter()
        .find(|entry| {
            entry.agent_id.as_str() == "sess-hook-routed" && entry.entry == TranscriptKind::Message
        })
        .expect("message entry");
    assert_eq!(message.from.as_deref(), Some("@claude"));
    assert_eq!(message.text, "ship it");
    assert!(entries.iter().all(|entry| {
        entry.entry != TranscriptKind::Prompt || !entry.text.starts_with("from @claude:")
    }));

    let output = run_ok(env.rimz().args(["transcript", "#hook-routed-transcript"]));
    assert!(output.contains("@claude → @codex"), "{output}");
    assert!(output.contains("\nship it"), "{output}");
    assert!(output.contains("@codex"), "{output}");
    assert!(output.contains("│ codex reply"), "{output}");
}

#[test]
fn transcript_defaults_to_live_session_and_archives_prior_life() {
    let env = Env::new();
    let branch = "living-transcript";
    append_transcript(
        &env,
        entry(
            "sess-prior-life",
            branch,
            TranscriptKind::Prompt,
            "prior prompt",
            "2020-01-01T00:00:00Z",
        ),
    );
    let mut owner = register_live_codex_turn(
        &env,
        "sess-current-life",
        branch,
        "current prompt",
        "current answer",
    );

    let output = env
        .rimz()
        .args(["transcript", &format!("#{branch}")])
        .output()
        .expect("spawn transcript");
    assert!(
        output.status.success(),
        "command failed\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("current prompt"), "{stdout}");
    assert!(stdout.contains("current answer"), "{stdout}");
    assert!(!stdout.contains("prior prompt"), "{stdout}");
    assert!(
        stderr.contains("1 earlier line from a prior session"),
        "{stderr}"
    );
    assert!(stderr.contains("rimz transcript --all"), "{stderr}");

    let all = run_ok(
        env.rimz()
            .args(["transcript", &format!("#{branch}"), "--all"]),
    );
    assert!(!all.contains("History archive"), "{all}");
    assert_eq!(all.matches("Live ·").count(), 1, "{all}");
    assert!(all.contains("prior prompt"), "{all}");
    assert!(all.contains("current prompt"), "{all}");

    let json = run_ok(
        env.rimz()
            .args(["transcript", &format!("#{branch}"), "--json"]),
    );
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("transcript json");
    assert_eq!(parsed["archived_count"], json!(1));
    let entries = parsed["entries"].as_array().expect("entries");
    assert!(
        entries
            .iter()
            .any(|entry| entry["text"] == "current prompt")
    );
    assert!(entries.iter().all(|entry| entry["text"] != "prior prompt"));

    let _ = owner.kill();
    let _ = owner.wait();
}

#[test]
fn transcript_empty_scope_exits_zero_with_note_or_empty_json() {
    let env = Env::new();
    append_transcript(
        &env,
        entry(
            "sess-other-scope",
            "other-scope",
            TranscriptKind::Prompt,
            "other prompt",
            "2026-06-01T00:00:00Z",
        ),
    );

    let output = env
        .rimz()
        .args(["transcript", "#missing-scope"])
        .output()
        .expect("spawn transcript");
    assert!(
        output.status.success(),
        "command failed\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No conversation for #missing-scope yet."),
        "{stderr}"
    );

    let json = run_ok(env.rimz().args(["transcript", "#missing-scope", "--json"]));
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("empty transcript json");
    assert_eq!(parsed, json!({ "entries": [] }));
}

#[test]
fn transcript_details_flag_is_gone() {
    let env = Env::new();
    let output = env
        .rimz()
        .args(["transcript", "--details"])
        .output()
        .expect("spawn transcript");

    assert!(!output.status.success(), "--details should be rejected");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--details"), "{stderr}");
}

fn entry(
    session_id: &str,
    branch: &str,
    kind: TranscriptKind,
    text: &str,
    at: &str,
) -> TranscriptEntry {
    agent_entry("claude", session_id, branch, kind, text, at)
}

fn agent_entry(
    kind: &str,
    session_id: &str,
    branch: &str,
    entry: TranscriptKind,
    text: &str,
    at: &str,
) -> TranscriptEntry {
    let mut entry = TranscriptEntry::new(
        at.parse().expect("timestamp"),
        AgentKind::new_unchecked(kind),
        AgentSessionId::from(session_id),
        entry,
        text.to_owned(),
    );
    entry.channel = Some(branch.to_owned());
    entry
}

fn message_entry(
    kind: &str,
    session_id: &str,
    branch: &str,
    from: &str,
    text: &str,
    at: &str,
) -> TranscriptEntry {
    let mut entry = agent_entry(kind, session_id, branch, TranscriptKind::Message, text, at);
    entry.from = Some(from.to_owned());
    entry
}

fn append_transcript(env: &Env, entry: TranscriptEntry) {
    rimz::transcript::append(env.store().paths(), &entry).expect("append transcript");
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
            + r#"{"type":"assistant","timestamp":"2026-06-01T00:00:02Z","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"pwd"}}]}}"#
            + "\n"
            + r#"{"type":"user","timestamp":"2026-06-01T00:00:03Z","message":{"content":[{"type":"tool_result","content":"/tmp/project"}]}}"#
            + "\n"
            + r#"{"type":"assistant","timestamp":"2026-06-01T00:00:04Z","message":{"content":[{"type":"tool_use","name":"AskUserQuestion","input":{"questions":[]}}]}}"#
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
    let worktree_path = env.home_root.join(branch).display().to_string();
    run_hook(
        env,
        "claude",
        json!({
            "hook_event_name": "SessionStart",
            "session_id": session_id,
            "worktree_branch": branch,
            "worktree_path": worktree_path.as_str(),
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
            "worktree_path": worktree_path.as_str(),
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
            "worktree_path": worktree_path.as_str(),
            "transcript_path": transcript,
        }),
    );
}

fn register_codex_turn(env: &Env, session_id: &str, branch: &str, prompt: &str, answer: &str) {
    let worktree_path = env.home_root.join(branch).display().to_string();
    run_hook(
        env,
        "codex",
        json!({
            "hook_event_name": "SessionStart",
            "session_id": session_id,
            "worktree_branch": branch,
            "worktree_path": worktree_path.as_str(),
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
            "worktree_path": worktree_path.as_str(),
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
            "worktree_path": worktree_path.as_str(),
        }),
    );
}

fn register_live_codex_turn(
    env: &Env,
    session_id: &str,
    branch: &str,
    prompt: &str,
    answer: &str,
) -> std::process::Child {
    let worktree_path = env.home_root.join(branch).display().to_string();
    let owner = dummy_agent_process();
    run_hook_for_owner(
        env,
        "codex",
        json!({
            "hook_event_name": "SessionStart",
            "session_id": session_id,
            "worktree_branch": branch,
            "worktree_path": worktree_path.as_str(),
        }),
        owner.id(),
    );
    run_hook_for_owner(
        env,
        "codex",
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": session_id,
            "prompt": prompt,
            "worktree_branch": branch,
            "worktree_path": worktree_path.as_str(),
        }),
        owner.id(),
    );
    run_hook_for_owner(
        env,
        "codex",
        json!({
            "hook_event_name": "Stop",
            "session_id": session_id,
            "last_assistant_message": answer,
            "worktree_branch": branch,
            "worktree_path": worktree_path.as_str(),
        }),
        owner.id(),
    );
    owner
}

fn run_hook(env: &Env, source: &str, payload: serde_json::Value) {
    let mut owner = dummy_agent_process();
    run_hook_for_owner(env, source, payload, owner.id());
    let _ = owner.kill();
    let _ = owner.wait();
}

fn run_hook_for_owner(env: &Env, source: &str, payload: serde_json::Value, owner_pid: u32) {
    let mut payload = payload;
    stamp_worktree_path(env, &mut payload);
    let payload = serde_json::to_string(&payload).expect("payload");
    let mut cmd = env.hook_command(source);
    scrub_launch_identity(&mut cmd);
    cmd.env("RIMZ_AGENT_PID", owner_pid.to_string());
    cmd.env(rimz::harness::run::ENV_AGENT_ROLE, source);
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

fn dummy_agent_process() -> std::process::Child {
    let mut cmd = std::process::Command::new("sleep");
    scrub_launch_identity(&mut cmd);
    cmd.arg("5").spawn().expect("spawn dummy agent process")
}

fn scrub_launch_identity(cmd: &mut std::process::Command) {
    for key in [
        rimz::harness::run::ENV_AGENT_NAME,
        rimz::harness::run::ENV_AGENT_PROFILE,
        rimz::harness::run::ENV_AGENT_ROLE,
        rimz::harness::run::ENV_TEAM,
        rimz::harness::run::ENV_LAUNCH_GROUP,
        rimz::harness::run::ENV_LAUNCH_ORDINAL,
        rimz::harness::run::ENV_CHANNEL,
        rimz::harness::run::ENV_AGENT_MODEL,
        rimz::harness::run::ENV_AGENT_EFFORT,
    ] {
        cmd.env(key, "");
    }
}

fn stamp_worktree_path(env: &Env, payload: &mut serde_json::Value) {
    if payload.get("worktree_path").is_some() {
        return;
    }
    let Some(branch) = payload
        .get("worktree_branch")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
    else {
        return;
    };
    let Some(object) = payload.as_object_mut() else {
        return;
    };
    object.insert(
        "worktree_path".to_owned(),
        json!(env.home_root.join(branch).display().to_string()),
    );
}

fn push_pending_agent_ask(env: &Env, session_id: &str) {
    run_hook(
        env,
        "claude",
        json!({
            "hook_event_name": "PreToolUse",
            "session_id": session_id,
            "tool_name": "AskUserQuestion",
            "tool_input": {
                "questions": [{
                    "question": "approve patch",
                    "options": [
                        { "label": "allow" },
                        { "label": "deny" }
                    ]
                }]
            },
        }),
    );
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
