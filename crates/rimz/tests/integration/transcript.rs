use serde_json::json;

use rimz::ids::{AgentKind, AgentSessionId};
use rimz::transcript::{TranscriptEntry, TranscriptKind};

use crate::common::Env;

#[test]
fn transcript_cli_bounds_human_stdout_but_not_json_or_zero() {
    let env = Env::new();
    for n in 0..25 {
        append_transcript(
            &env,
            entry(
                "bounded",
                "bounded",
                TranscriptKind::Prompt,
                &format!("entry-{n:03}"),
                "2026-06-01T00:00:00Z",
            ),
        );
    }
    for flags in [vec![], vec!["--all"], vec!["--flat"]] {
        let output = env
            .rimz()
            .args(["transcript", "#bounded"])
            .args(flags)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let text = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            text.lines()
                .filter(|line| line.starts_with("entry-"))
                .count(),
            20,
            "{text}"
        );
        assert!(!text.contains("entry-000"), "{text}");
        assert_eq!(
            String::from_utf8(output.stderr).unwrap().trim(),
            "⋯ last 20 of 25 entries · -n 0 for all"
        );
    }
    let output = env
        .rimz()
        .args(["transcript", "#bounded", "-n", "0"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .filter(|line| line.starts_with("entry-"))
            .count(),
        25
    );
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    let output = env
        .rimz()
        .args(["transcript", "#bounded", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["entries"].as_array().unwrap().len(), 25);
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
}

#[test]
fn main_transcript_filter_stays_in_current_workspace_and_matches_root_records() {
    let env = Env::new();
    env.record(&env.project_root);
    let paths = env.store().paths().clone();
    for (id, channel, text) in [
        ("root", Some("project"), "root record"),
        ("legacy", None, "legacy record"),
        ("named", Some("main"), "named main record"),
        ("other", Some("feature"), "other record"),
    ] {
        let mut entry = TranscriptEntry::new(
            jiff::Timestamp::now(),
            AgentKind::new_unchecked("claude"),
            id.into(),
            TranscriptKind::Prompt,
            text.into(),
        );
        entry.channel = channel.map(str::to_owned);
        entry.from = Some("@sender#project".to_owned());
        rimz::transcript::append(&paths, &entry).unwrap();
    }
    for channel in ["main", "project", env.project_root.to_str().unwrap()] {
        let output = env
            .rimz()
            .args(["transcript", &format!("#{channel}"), "--all"])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            text.contains("root record") && text.contains("legacy record"),
            "{text}"
        );
        assert!(!text.contains("other record"), "{text}");
        assert!(text.contains("named main record"), "{text}");
        assert!(text.contains("#main"), "{text}");
        assert!(!text.contains("@sender#project"), "{text}");
    }
    let text = env.rimz().args(["transcript", "--all"]).output().unwrap();
    assert!(text.status.success());
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("@sender#main"), "{text}");
    assert!(!text.contains("@sender#project"), "{text}");
}

#[test]
fn agents_show_ambiguity_excludes_ended_matches() {
    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    for n in 0..12 {
        let mut observation = rimz::agents::AgentLifecycleObservation::new(
            Some(format!("peer-{n}").into()),
            rimz::agents::LifecycleSignal::Registered,
        );
        observation.launch.role = Some("peer".to_owned());
        observation.launch.channel = Some(format!("lane-{n}"));
        observation.agent_pid = Some(env.agent_owner_pid());
        observation.pane_id = Some(rimz::ids::PaneId::from_parts(
            rimz::ids::MuxName::Zellij,
            format!("terminal_{n}"),
        ));
        store
            .append_event(&rimz::EventEnvelope::agent_lifecycle(
                env.workspace_id.clone(),
                "session",
                "claude",
                "SessionStart",
                &observation,
            ))
            .unwrap();
        if n >= 2 {
            observation.signal = rimz::agents::LifecycleSignal::Ended;
            store
                .append_event(&rimz::EventEnvelope::agent_lifecycle(
                    env.workspace_id.clone(),
                    "session",
                    "claude",
                    "SessionEnd",
                    &observation,
                ))
                .unwrap();
        }
    }
    let live = store.snapshot_cached().unwrap();
    assert_eq!(live.agents.len(), 2, "live fixture: {:#?}", live.agents);
    for json in [false, true] {
        let mut command = env.rimz();
        command.args(["agents", "show", "@peer"]);
        if json {
            command.arg("--json");
        }
        let result = command.output().unwrap();
        assert!(
            !result.status.success(),
            "json={json}: stdout={} stderr={}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(
            stderr.contains("matches 2 agents: @peer#lane-0, @peer#lane-1"),
            "{stderr}"
        );
        assert!(!stderr.contains("lane-2"), "{stderr}");
    }
}

#[test]
fn agents_show_from_main_checkout_uses_callers_channel() {
    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    for (n, role, channel) in [(0, "caller", "a"), (1, "peer", "a"), (2, "peer", "b")] {
        let mut observation = rimz::agents::AgentLifecycleObservation::new(
            Some(format!("session-{n}").into()),
            rimz::agents::LifecycleSignal::Registered,
        );
        observation.launch.role = Some(role.to_owned());
        observation.launch.channel = Some(channel.to_owned());
        observation.agent_pid = Some(env.agent_owner_pid());
        observation.pane_id = Some(rimz::ids::PaneId::from_parts(
            rimz::ids::MuxName::Zellij,
            format!("terminal_{n}"),
        ));
        store
            .append_event(&rimz::EventEnvelope::agent_lifecycle(
                env.workspace_id.clone(),
                "session",
                "claude",
                "SessionStart",
                &observation,
            ))
            .unwrap();
    }
    assert_eq!(store.snapshot_cached().unwrap().agents.len(), 3);
    let result = env
        .rimz()
        .args(["agents", "show", "@peer", "--json"])
        .env("RIMZ_AGENT_KIND", "claude")
        .env("RIMZ_AGENT_ID", "session-0")
        .env_remove("RIMZ_CHANNEL")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(json["agent"]["id"], "session-1");
}

#[test]
fn transcript_live_ambiguity_matches_agents_show() {
    let env = Env::new();
    env.record(&env.project_root);
    register_address_peer(&env, "claude", "live-a", "peer", "a", 1);
    register_address_peer(&env, "claude", "live-b", "peer", "b", 2);
    let show = env
        .rimz()
        .args(["agents", "show", "@peer"])
        .output()
        .unwrap();
    assert!(!show.status.success());
    let transcript = env.rimz().args(["transcript", "@peer"]).output().unwrap();
    assert!(
        !transcript.status.success(),
        "an empty log must not hide live ambiguity"
    );
    assert_eq!(transcript.stderr, show.stderr);
    assert!(String::from_utf8_lossy(&show.stderr).contains("matches 2 agents: @peer#a, @peer#b"));
}

#[test]
fn transcript_from_main_checkout_resolves_peer_and_me() {
    let env = Env::new();
    env.record(&env.project_root);
    for (n, role, channel) in [(0, "caller", "a"), (1, "peer", "a"), (2, "peer", "b")] {
        let id = format!("session-{n}");
        register_address_peer(&env, "claude", &id, role, channel, n);
        let mut line = entry(
            &id,
            channel,
            TranscriptKind::Prompt,
            &id,
            "2026-06-01T00:00:00Z",
        );
        line.role = Some(role.to_owned());
        append_transcript(&env, line);
    }
    for (target, wanted) in [
        ("@peer", "session-1"),
        ("@me", "session-0"),
        ("session-2", "session-2"),
        ("zellij:terminal_1", "session-1"),
    ] {
        let result = env
            .rimz()
            .args(["transcript", target, "--json"])
            .env("RIMZ_AGENT_KIND", "claude")
            .env("RIMZ_AGENT_ID", "session-0")
            .env_remove("RIMZ_CHANNEL")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "target={target}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let json: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        let lines = json["entries"].as_array().unwrap();
        assert_eq!(lines.len(), 1, "{json}");
        assert_eq!(lines[0]["text"], wanted);
    }
}

#[test]
fn transcript_live_empty_focus_does_not_use_other_channel_lines() {
    let env = Env::new();
    env.record(&env.project_root);
    register_address_peer(&env, "claude", "live-a", "peer", "a", 1);
    let mut other = entry(
        "old-b",
        "b",
        TranscriptKind::Prompt,
        "other lane",
        "2026-06-01T00:00:00Z",
    );
    other.role = Some("peer".to_owned());
    append_transcript(&env, other);
    let result = env.rimz().args(["transcript", "@peer#a"]).output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&result.stderr).trim(),
        "No conversation for @peer#a yet."
    );
    assert!(result.stdout.is_empty());
    let result = env
        .rimz()
        .args(["transcript", "@peer#a", "--json"])
        .output()
        .unwrap();
    assert!(result.status.success());
    let json: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(json["entries"], json!([]));
}

#[test]
fn transcript_unknown_target_with_empty_log_fails() {
    assert_unknown_transcript_target(false);
}

#[test]
fn transcript_unknown_target_with_populated_log_uses_resolver_error() {
    assert_unknown_transcript_target(true);
}

fn assert_unknown_transcript_target(populated: bool) {
    let env = Env::new();
    env.record(&env.project_root);
    if populated {
        append_transcript(
            &env,
            entry(
                "old-a",
                "a",
                TranscriptKind::Prompt,
                "hello",
                "2026-06-01T00:00:00Z",
            ),
        );
    }
    let result = env.rimz().args(["transcript", "@ghost"]).output().unwrap();
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("no agent matches target `@ghost`"),
        "{stderr}"
    );
    assert!(stderr.contains("rimz agents list"), "{stderr}");
}

#[test]
fn transcript_historical_ambiguity_round_trips() {
    assert_historical_ambiguity_round_trips(false);
}

#[test]
fn transcript_shadowed_historical_candidates_round_trip() {
    assert_historical_ambiguity_round_trips(true);
}

fn assert_historical_ambiguity_round_trips(shadowed: bool) {
    let env = Env::new();
    env.record(&env.project_root);
    for channel in ["a", "b"] {
        let mut line = entry(
            &format!("old-{channel}"),
            channel,
            TranscriptKind::Prompt,
            channel,
            "2026-06-01T00:00:00Z",
        );
        line.role = Some("coder".to_owned());
        append_transcript(&env, line);
    }
    if shadowed {
        register_address_peer(&env, "codex", "live-a", "coder", "a", 1);
    }
    let result = env.rimz().args(["transcript", "@claude"]).output().unwrap();
    assert!(
        !result.status.success(),
        "historical channels must not silently pick one"
    );
    let first = if shadowed { "old-a" } else { "@coder#a" };
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains(&format!("matches 2 agents: {first}, @coder#b")),
        "{stderr}"
    );
    for (address, wanted) in [(first, "a"), ("@coder#b", "b")] {
        let result = env
            .rimz()
            .args(["transcript", address, "--json"])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let json: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(json["entries"][0]["text"], wanted, "{json}");
    }
}

fn register_address_peer(env: &Env, kind: &str, id: &str, role: &str, channel: &str, pane: u32) {
    let mut observation = rimz::agents::AgentLifecycleObservation::new(
        Some(id.into()),
        rimz::agents::LifecycleSignal::Registered,
    );
    observation.launch.role = Some(role.to_owned());
    observation.launch.channel = Some(channel.to_owned());
    observation.agent_pid = Some(env.agent_owner_pid());
    observation.pane_id = Some(rimz::ids::PaneId::from_parts(
        rimz::ids::MuxName::Zellij,
        format!("terminal_{pane}"),
    ));
    env.store()
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            "session",
            kind,
            "SessionStart",
            &observation,
        ))
        .unwrap();
}

#[test]
fn transcript_renders_durable_turns_asks_answers_and_channels() {
    let env = Env::new();
    env.record(&env.project_root);
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
    env.record(&env.project_root);
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
fn transcript_records_pane_typed_prompt_as_open_ask_answer() {
    let env = Env::new();
    env.record(&env.project_root);
    let branch = "pane-answer-transcript";
    let session_id = "sess-pane-answer";
    let claude_path = env.home_root.join("pane-answer-chat.jsonl");
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
                        { "label": "safe" },
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
            "hook_event_name": "UserPromptSubmit",
            "session_id": session_id,
            "prompt": "Ship after staging finishes.",
            "worktree_branch": branch,
            "transcript_path": transcript.as_str(),
        }),
    );

    let output = run_ok(env.rimz().args(["transcript", &format!("#{branch}")]));
    assert!(output.contains("│ │ Choose deployment path?"), "{output}");
    assert!(
        output.contains("│ │ ● other: Ship after staging finishes. — you"),
        "{output}"
    );
    assert!(!output.contains("◌ unanswered"), "{output}");
    assert_eq!(output.matches(" user  → @claude").count(), 1, "{output}");

    let entries = rimz::transcript::read_all(env.store().paths()).expect("read transcript");
    let ask = entries
        .iter()
        .find(|entry| entry.entry == TranscriptKind::Ask)
        .expect("ask entry");
    let answer = entries
        .iter()
        .find(|entry| entry.entry == TranscriptKind::Answer)
        .expect("answer entry");
    assert_eq!(answer.id, ask.id);
    assert_eq!(answer.from.as_deref(), Some("you"));
    assert_eq!(answer.answers[0].chosen, ["Ship after staging finishes."]);
    assert!(
        !entries.iter().any(|entry| {
            entry.entry == TranscriptKind::Prompt && entry.text == "Ship after staging finishes."
        }),
        "{entries:#?}"
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
    env.record(&env.project_root);
    let branch = "hook-routed-transcript";
    register_codex_turn(
        &env,
        "sess-hook-routed",
        branch,
        "Type: AGENT_MESSAGE\nFrom: @claude\nContent:\nship it",
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
        entry.entry != TranscriptKind::Prompt || !entry.text.starts_with("Type: AGENT_MESSAGE")
    }));

    let output = run_ok(env.rimz().args(["transcript", "#hook-routed-transcript"]));
    assert!(output.contains("@claude → @codex"), "{output}");
    assert!(output.contains("\nship it"), "{output}");
    assert!(output.contains("@codex"), "{output}");
    assert!(output.contains("│ codex reply"), "{output}");
}

#[test]
fn signal_delivery_is_acknowledged_and_hidden_from_rendered_transcript() {
    use rimz::store::message::{
        DeliveryGate, HarnessNotice, MessageRecord, MessageSender, MessageStatus,
    };

    let env = Env::new();
    env.record(&env.project_root);
    let session_id = "sess-signal-transcript";
    let branch = "signal-transcript";
    register_codex_turn(&env, session_id, branch, "visible request", "initial reply");
    let store = env.store();
    let snapshot = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("snapshot");
    let recipient = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id == session_id)
        .expect("registered recipient");
    let mut record = MessageRecord::new(
        env.workspace_id.clone(),
        recipient,
        "CI failed; inspect the build log.".to_owned(),
        DeliveryGate::Done,
    )
    .with_sender(MessageSender::Harness {
        notice: HarnessNotice::Signal,
    });
    record.status = MessageStatus::Sent;
    record.last_sent_at = Some(jiff::Timestamp::from_second(1_000).expect("fixed timestamp"));
    store
        .queue_message(&record, "rimz-test")
        .expect("seed sent signal");
    let prompt = "Type: SIGNAL\nFrom: @rimz\nContent:\n".to_owned() + &record.text;
    assert!(prompt.starts_with("Type: SIGNAL\n"));
    run_hook(
        &env,
        "codex",
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": session_id,
            "prompt": prompt,
            "worktree_branch": branch,
            "worktree_path": env.home_root.join(branch).display().to_string(),
        }),
    );

    assert!(store.list_messages().expect("live queue").is_empty());
    let history = store.list_message_history().expect("history");
    let delivered = history
        .iter()
        .find(|row| row.message_id == record.message_id)
        .expect("acknowledged signal");
    assert_eq!(delivered.status, MessageStatus::Delivered);
    let entries = rimz::transcript::read_all(store.paths()).expect("transcript");
    let signal = entries
        .iter()
        .find(|entry| entry.message_id.as_ref() == Some(&record.message_id))
        .expect("signal transcript entry");
    assert_eq!(signal.entry, TranscriptKind::Wait);
    assert_eq!(signal.from.as_deref(), Some("@rimz"));
    assert_eq!(signal.text, record.text);
    let output = run_ok(env.rimz().args(["transcript", "#signal-transcript"]));
    assert!(output.contains("visible request"), "{output}");
    assert!(!output.contains(&record.text), "{output}");
    assert!(!output.contains("Type: SIGNAL"), "{output}");
    let output = run_ok(
        env.rimz()
            .args(["transcript", "#signal-transcript", "--json", "--flat"]),
    );
    let json: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert!(
        json["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["text"] == record.text),
        "{output}"
    );
}

#[test]
fn transcript_hook_strips_user_message_header_from_prompt_entry() {
    let env = Env::new();
    env.record(&env.project_root);
    register_codex_turn(
        &env,
        "sess-hook-user",
        "hook-user-transcript",
        "Type: USER_MESSAGE\nFrom: @user\nContent:\ncheck this",
        "checked",
    );

    let entries = rimz::transcript::read_all(env.store().paths()).expect("read log");
    let prompt = entries
        .iter()
        .find(|entry| {
            entry.agent_id.as_str() == "sess-hook-user" && entry.entry == TranscriptKind::Prompt
        })
        .expect("prompt entry");
    assert_eq!(prompt.from, None);
    assert_eq!(prompt.text, "check this");
}

#[test]
fn transcript_scopes_launched_child_and_attributes_brief() {
    let env = Env::new();
    env.record(&env.project_root);
    let branch = "launched-child-transcript";
    let store = env.store();
    let parent_kind = AgentKind::new_unchecked("claude");
    let parent_id = AgentSessionId::from("parent-transcript-session");
    store
        .append_event(&rimz::store::event::EventEnvelope::agent_launched(
            env.workspace_id.clone(),
            "rimz-test",
            &parent_kind,
            rimz::store::event::AgentLaunchPayload {
                agent_id: parent_id.clone(),
                launch_id: None,
                agent_name: "steady-parent".to_owned(),
                agent_name_explicit: true,
                launch: rimz::agents::LaunchParams {
                    role: Some("planner".to_owned()),
                    channel: Some(branch.to_owned()),
                    ..Default::default()
                },
                state: rimz::store::event::AgentLaunchState::Bound,
                run_id: None,
                pane_id: None,
                runtime_owner: None,
                worktree_path: Some(env.home_root.join(branch).display().to_string()),
                worktree_branch: Some(branch.to_owned()),
                prompt: None,
                description: None,
            },
        ))
        .expect("seed parent");
    let mut parent_entry = agent_entry(
        "claude",
        parent_id.as_str(),
        branch,
        TranscriptKind::Prompt,
        "root conversation",
        "2026-06-01T00:00:00Z",
    );
    parent_entry.role = Some("planner".to_owned());
    append_transcript(&env, parent_entry);

    let child_kind = AgentKind::new_unchecked("codex");
    let child_id = AgentSessionId::from("child-transcript-session");
    let mut run = rimz::store::run::RunRecord::new(
        env.workspace_id.clone(),
        child_kind.clone(),
        rimz::agents::PermissionMode::Auto,
        "inspect the launch path".to_owned(),
        env.home_root.join(branch),
    );
    run.agent_id = Some(child_id.clone());
    run.agent_name = Some("swift-child".to_owned());
    run.subagent = true;
    rimz::harness::run::create(store.paths(), &run).expect("create child run");
    store
        .append_event(&rimz::store::event::EventEnvelope::agent_launched(
            env.workspace_id.clone(),
            "rimz-test",
            &child_kind,
            rimz::store::event::AgentLaunchPayload {
                agent_id: child_id.clone(),
                launch_id: None,
                agent_name: "swift-child".to_owned(),
                agent_name_explicit: true,
                launch: rimz::agents::LaunchParams {
                    parent_agent_id: Some(parent_id.clone()),
                    parent_agent_kind: Some(parent_kind),
                    launch_depth: Some(1),
                    channel: Some(branch.to_owned()),
                    ..Default::default()
                },
                state: rimz::store::event::AgentLaunchState::Bound,
                run_id: Some(run.run_id.clone()),
                pane_id: None,
                runtime_owner: None,
                worktree_path: Some(env.home_root.join(branch).display().to_string()),
                worktree_branch: Some(branch.to_owned()),
                prompt: Some(run.prompt.clone()),
                description: None,
            },
        ))
        .expect("seed child");

    run_hook_for_run(
        &env,
        "codex",
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": child_id.as_str(),
            "prompt": run.prompt,
            "worktree_branch": branch,
            "worktree_path": env.home_root.join(branch),
        }),
        &run.run_id,
        "swift-child",
    );
    run_hook_for_run(
        &env,
        "codex",
        json!({
            "hook_event_name": "Stop",
            "session_id": child_id.as_str(),
            "last_assistant_message": "child answer",
            "worktree_branch": branch,
            "worktree_path": env.home_root.join(branch),
        }),
        &run.run_id,
        "swift-child",
    );

    let channel = run_ok(env.rimz().args(["transcript", &format!("#{branch}")]));
    assert!(channel.contains("root conversation"), "{channel}");
    assert!(!channel.contains("inspect the launch path"), "{channel}");
    assert!(!channel.contains("child answer"), "{channel}");
    let channel_json = run_ok(
        env.rimz()
            .args(["transcript", &format!("#{branch}"), "--json"]),
    );
    assert!(
        !channel_json.contains("inspect the launch path"),
        "{channel_json}"
    );
    assert!(!channel_json.contains("child answer"), "{channel_json}");

    let child = run_ok(env.rimz().args(["transcript", "@swift-child", "--all"]));
    assert!(child.contains("@planner → @codex"), "{child}");
    assert!(child.contains("inspect the launch path"), "{child}");
    assert!(child.contains("child answer"), "{child}");

    let parent = run_ok(env.rimz().args(["transcript", parent_id.as_str(), "--all"]));
    assert!(parent.contains("root conversation"), "{parent}");
    assert!(!parent.contains("inspect the launch path"), "{parent}");
    assert!(!parent.contains("child answer"), "{parent}");
}

#[test]
fn transcript_defaults_to_live_session_and_archives_prior_life() {
    let env = Env::new();
    env.record(&env.project_root);
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
    register_live_codex_turn(
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
        stderr.contains("1 earlier entry from a prior session"),
        "{stderr}"
    );
    assert!(
        stderr.contains("rimz transcript '#living-transcript' --all"),
        "{stderr}"
    );

    append_transcript(
        &env,
        entry(
            "sess-current-life",
            branch,
            TranscriptKind::Prompt,
            "third live entry",
            "2099-01-01T00:00:00Z",
        ),
    );
    for (last, has_note) in [("1", false), ("3", true)] {
        let output = env
            .rimz()
            .args(["transcript", &format!("#{branch}"), "-n", last])
            .output()
            .unwrap();
        assert!(output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(stderr.contains("earlier entry"), has_note, "{stderr}");
    }

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
}

#[test]
fn transcript_archive_hint_echoes_worktree_root_and_plural_entries() {
    let env = Env::new();
    env.record(&env.project_root);
    for text in ["old one", "old two"] {
        append_transcript(
            &env,
            entry(
                "old",
                "archive",
                TranscriptKind::Prompt,
                text,
                "2020-01-01T00:00:00Z",
            ),
        );
    }
    register_live_codex_turn(
        &env,
        "current",
        "archive",
        "current prompt",
        "current answer",
    );
    let output = env
        .rimz()
        .args(["transcript", "-w", "archive", "--root"])
        .arg(&env.project_root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("2 earlier entries"), "{stderr}");
    assert!(
        stderr.contains("rimz transcript -w archive --root"),
        "{stderr}"
    );
    let output = env.rimz().args(["transcript"]).output().unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains("rimz transcript --all"));
}

#[test]
fn transcript_follow_prints_appended_channel_entries_once() {
    follow_prints_appended_entries(&["transcript", "#following", "--flat"]);
}

#[test]
fn agents_logs_follow_prints_appended_channel_entries_once() {
    follow_prints_appended_entries(&["agents", "logs", "#following"]);
}

fn follow_prints_appended_entries(command: &[&str]) {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    use std::time::Duration;

    let env = Env::new();
    append_transcript(
        &env,
        entry(
            "follow-session",
            "following",
            TranscriptKind::Prompt,
            "excluded by tail",
            "2026-01-01T00:00:00Z",
        ),
    );
    append_transcript(
        &env,
        entry(
            "follow-session",
            "following",
            TranscriptKind::Prompt,
            "initial entry",
            "2026-01-01T00:01:00Z",
        ),
    );
    let mut child = env
        .rimz()
        .args(command)
        .args(["-f", "--json", "-n", "1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if send.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let initial = receive.recv_timeout(Duration::from_secs(10));
    if initial.is_ok() {
        append_transcript(
            &env,
            entry(
                "follow-session",
                "following",
                TranscriptKind::Prompt,
                "appended entry",
                "2026-01-01T00:02:00Z",
            ),
        );
    }
    let appended = receive.recv_timeout(Duration::from_secs(10));
    let duplicate = receive.recv_timeout(Duration::from_millis(1500));
    let _ = child.kill();
    let output = child.wait_with_output().unwrap();
    reader.join().unwrap();
    let initial = initial.expect("follow prints its first view");
    let appended = appended.expect("follow observes an entry appended after its first view");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&initial).unwrap()["text"],
        "initial entry"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&appended).unwrap()["text"],
        "appended entry"
    );
    assert!(duplicate.is_err(), "entry printed more than once");
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
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
    env.record(&env.project_root);
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

fn register_live_codex_turn(env: &Env, session_id: &str, branch: &str, prompt: &str, answer: &str) {
    let worktree_path = env.home_root.join(branch).display().to_string();
    let owner_pid = env.agent_owner_pid();
    run_hook_for_owner(
        env,
        "codex",
        json!({
            "hook_event_name": "SessionStart",
            "session_id": session_id,
            "worktree_branch": branch,
            "worktree_path": worktree_path.as_str(),
        }),
        owner_pid,
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
        owner_pid,
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
        owner_pid,
    );
}

fn run_hook(env: &Env, source: &str, payload: serde_json::Value) {
    let mut owner = dummy_agent_process(env);
    run_hook_for_owner(env, source, payload, owner.id());
    let _ = owner.kill();
    let _ = owner.wait();
}

fn run_hook_for_owner(env: &Env, source: &str, payload: serde_json::Value, owner_pid: u32) {
    run_hook_for_owner_and_run(env, source, payload, owner_pid, None, None);
}

fn run_hook_for_run(
    env: &Env,
    source: &str,
    payload: serde_json::Value,
    run_id: &rimz::RunId,
    agent_name: &str,
) {
    let mut owner = dummy_agent_process(env);
    run_hook_for_owner_and_run(
        env,
        source,
        payload,
        owner.id(),
        Some(run_id),
        Some(agent_name),
    );
    let _ = owner.kill();
    let _ = owner.wait();
}

fn run_hook_for_owner_and_run(
    env: &Env,
    source: &str,
    payload: serde_json::Value,
    owner_pid: u32,
    run_id: Option<&rimz::RunId>,
    agent_name: Option<&str>,
) {
    let mut payload = payload;
    stamp_worktree_path(env, &mut payload);
    let payload = serde_json::to_string(&payload).expect("payload");
    let mut cmd = env.hook_command(source);
    scrub_launch_identity(&mut cmd);
    cmd.env("RIMZ_AGENT_PID", owner_pid.to_string());
    cmd.env(rimz::harness::launch::ENV_AGENT_ROLE, source);
    if let Some(run_id) = run_id {
        cmd.env(rimz::harness::launch::ENV_RUN_ID, run_id.as_str());
    }
    if let Some(agent_name) = agent_name {
        cmd.env(rimz::harness::launch::ENV_AGENT_NAME, agent_name);
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

fn dummy_agent_process(env: &Env) -> std::process::Child {
    let mut cmd = std::process::Command::new("sleep");
    scrub_launch_identity(&mut cmd);
    cmd.env("HOME", &env.home_root)
        .env("XDG_RUNTIME_DIR", &env.runtime_root)
        .arg("600")
        .spawn()
        .expect("spawn dummy agent process")
}

fn scrub_launch_identity(cmd: &mut std::process::Command) {
    for key in [
        rimz::harness::launch::ENV_AGENT_NAME,
        rimz::harness::launch::ENV_AGENT_PROFILE,
        rimz::harness::launch::ENV_AGENT_ROLE,
        rimz::harness::launch::ENV_TEAM,
        rimz::harness::launch::ENV_LAUNCH_GROUP,
        rimz::harness::launch::ENV_LAUNCH_ORDINAL,
        rimz::workspace::ENV_CHANNEL,
        rimz::harness::launch::ENV_AGENT_MODEL,
        rimz::harness::launch::ENV_AGENT_EFFORT,
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
