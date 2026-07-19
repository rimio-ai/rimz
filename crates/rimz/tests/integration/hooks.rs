//! Out-of-process integration tests for `rimz hooks feed`. Each test spawns a
//! real `rimz` binary; XDG roots are scoped under a tempdir so state and
//! runtime files don't escape.

use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use md5::{Digest as _, Md5};
use prost::Message;
use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::Sha256;

use crate::common::{
    Env, claude_pre_tool_use_payload, codex_permission_payload, codex_pre_tool_use_payload,
    permission_payload, pi_tool_call_payload, tmux_pane,
};

fn permission_cases() -> [(&'static str, String); 2] {
    [
        ("claude", permission_payload("Bash")),
        ("codex", codex_permission_payload()),
    ]
}

fn assert_hook_succeeded_neutral(source: &str, output: Output) {
    assert!(
        output.status.success(),
        "{source} hook stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "{source} neutral stdout must stay empty, got: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
}

fn lifecycle_event_count(env: &Env) -> usize {
    env.read_events()
        .iter()
        .filter(|event| event.method == "agent.lifecycle")
        .count()
}

fn run_claude_lifecycle(env: &Env, payload: Value) {
    let payload = serde_json::to_string(&payload).expect("payload");
    let output = env.run_hook("claude", &payload);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty(), "lifecycle hook is silent");
}

#[test]
fn kiro_hook_install_refuses_and_legacy_uninstall_still_cleans_up() {
    let env = Env::new();
    let path = env.home_root.join(".kiro/hooks/rimz.json");

    for args in [
        vec!["hooks", "install", "kiro"],
        vec!["hooks", "install", "--dry-run", "kiro"],
    ] {
        let out = env.rimz().args(args).output().expect("spawn Kiro install");
        assert!(!out.status.success(), "unsupported install must fail");
        assert!(
            String::from_utf8_lossy(&out.stderr)
                .contains("does not execute standalone hook configs"),
            "stderr should explain the verified limitation: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!path.exists(), "refused install must not write config");
    }

    std::fs::create_dir_all(path.parent().expect("hook parent")).expect("mkdir hook parent");
    std::fs::write(
        &path,
        r#"{"version":"v1","hooks":[{"action":{"command":"rimz hooks feed --source kiro --event Stop"}}]}"#,
    )
    .expect("write legacy hook");
    let out = env
        .rimz()
        .args(["hooks", "uninstall", "kiro"])
        .output()
        .expect("spawn Kiro uninstall");
    assert!(
        out.status.success(),
        "legacy uninstall failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!path.exists(), "legacy owned file should be removed");
}

#[test]
fn grok_global_hooks_preserve_user_config_and_route_camelcase_events_neutrally() {
    let env = Env::new();
    let path = env.agent_config_path("grok");
    std::fs::create_dir_all(path.parent().expect("Grok hooks parent"))
        .expect("mkdir Grok hooks parent");
    std::fs::write(
        &path,
        r#"{"theme":"user-theme","hooks":{"Custom":[{"command":"user-hook"}],"SessionStart":[{"_rimz_managed":true,"hooks":[{"type":"command","command":"RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source grok","timeout":4}]}]}}"#,
    )
    .expect("write user Grok hooks");

    env.install_agent_hooks("grok");
    assert!(env.agent_hooks_installed("grok"));
    let installed_bytes = std::fs::read(&path).expect("read Grok hooks");
    let installed: Value = serde_json::from_slice(&installed_bytes).expect("JSON");
    assert_eq!(installed["theme"], "user-theme");
    assert_eq!(installed["hooks"]["Custom"][0]["command"], "user-hook");
    assert!(installed["hooks"].get("PreToolUse").is_none());
    assert_eq!(
        installed["hooks"]["SessionStart"][0]["hooks"][0]["timeout"],
        4
    );
    let managed_commands = installed["hooks"]
        .as_object()
        .unwrap()
        .values()
        .filter_map(Value::as_array)
        .flatten()
        .filter(|entry| entry["_rimz_managed"] == true)
        .map(|entry| entry["hooks"][0]["command"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(managed_commands.len(), 12);
    assert!(
        managed_commands
            .iter()
            .all(|command| *command == "rimz hooks feed --source grok" && !command.contains('$'))
    );
    env.install_agent_hooks("grok");
    assert_eq!(std::fs::read(&path).unwrap(), installed_bytes);

    let run = |payload: Value| {
        let output = env.run_installed_hook("grok", &payload.to_string());
        assert_hook_succeeded_neutral("grok", output);
    };
    run(json!({
        "hookEventName": "session_start",
        "sessionId": "session-grok-1",
        "cwd": env.project_root,
        "source": "new"
    }));
    run(json!({
        "hookEventName": "user_prompt_submit",
        "sessionId": "session-grok-1",
        "prompt": "inspect the durable branch"
    }));
    run(json!({
        "hookEventName": "notification",
        "sessionId": "session-grok-1",
        "notificationType": "permission_prompt",
        "message": "Plan approval requested"
    }));
    let waiting = env.snapshot_json();
    assert_eq!(waiting["agents"][0]["kind"], "grok");
    assert_eq!(waiting["agents"][0]["status"], "waiting");
    assert_eq!(waiting["agents"][0]["open_ask"]["kind"], "plan_approval");

    run(json!({
        "hookEventName": "post_tool_use",
        "sessionId": "session-grok-1",
        "toolName": "apply_patch",
        "toolUseId": "tool-1"
    }));
    run(json!({
        "hookEventName": "stop",
        "sessionId": "session-grok-1",
        "reason": "end_turn"
    }));
    assert_eq!(env.snapshot_json()["agents"][0]["status"], "success");
    run(json!({
        "hookEventName": "session_end",
        "sessionId": "session-grok-1"
    }));
    assert_eq!(env.snapshot_json()["agents"].as_array().unwrap().len(), 0);

    let out = env
        .rimz()
        .args(["hooks", "uninstall", "grok"])
        .output()
        .expect("uninstall Grok hooks");
    assert!(
        out.status.success(),
        "Grok uninstall stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let restored: Value =
        serde_json::from_slice(&std::fs::read(path).expect("read restored hooks")).expect("JSON");
    assert_eq!(restored["theme"], "user-theme");
    assert_eq!(restored["hooks"]["Custom"][0]["command"], "user-hook");
    assert!(restored["hooks"].get("SessionStart").is_none());
}

#[test]
fn session_start_hooks_write_lifecycle_rows() {
    for (source, payload, expected_id, expected_fields) in [
        (
            "codex",
            json!({
                "hook_event_name": "SessionStart",
                "session_id": "sess-codex-01",
                "approval_policy": "ask",
                "worktree_branch": "feature-x",
            }),
            "sess-codex-01",
            vec![("worktree_branch", json!("feature-x"))],
        ),
        (
            "pi",
            json!({
                "hook_event_name": "session_start",
                "session_id": "019e9161-a5d0-791d-879e-39679acd4ded",
                "reason": "startup",
                "model": "gpt-5.5",
                "context_pct": 3,
                "context_window": 272000,
                "total_tokens": 8160,
            }),
            "019e9161-a5d0-791d-879e-39679acd4ded",
            vec![
                ("model", json!("gpt-5.5")),
                ("context_window", json!(272000)),
            ],
        ),
        (
            "claude",
            json!({
                "hook_event_name": "SessionStart",
                "session_id": "sess-claude-01",
                "permission_mode": "default",
                "worktree_branch": "feature-x",
            }),
            "sess-claude-01",
            vec![("worktree_branch", json!("feature-x"))],
        ),
    ] {
        let env = Env::new();
        let payload = serde_json::to_string(&payload).expect("payload");
        let output = env.run_hook(source, &payload);
        assert!(
            output.status.success(),
            "{source} stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stdout.is_empty(),
            "{source} lifecycle hook is silent"
        );

        let parsed = env.snapshot_json();
        let agents = parsed["agents"].as_array().expect("agents array");
        assert_eq!(agents.len(), 1, "{source} rolled up one agent: {agents:?}");
        assert_eq!(agents[0]["kind"], source);
        assert_eq!(agents[0]["agent_id"], expected_id);
        assert_eq!(agents[0]["status"], "idle");
        for (field, value) in expected_fields {
            assert_eq!(agents[0][field], value, "{source} {field}");
        }
    }
}

#[test]
fn kimi_subagent_join_surfaces_children_and_suppresses_child_stops() {
    let env = Env::new();
    let session_id = "session-kimi-parent";
    let kimi_home = env.home_root.join(".kimi-code");
    let session = kimi_home.join("sessions/wd_project").join(session_id);
    let main_home = session.join("agents/main");
    let first_home = session.join("agents/agent-0");
    let second_home = session.join("agents/agent-1");
    std::fs::create_dir_all(&main_home).expect("mkdir Kimi main agent");
    let main_wire = main_home.join("wire.jsonl");
    std::fs::write(
        &main_wire,
        concat!(
            "{\"type\":\"turn.prompt\",\"input\":[{\"type\":\"text\",\"text\":\"delegate two checks\"}],\"origin\":{\"kind\":\"user\"}}\n",
            "{\"type\":\"llm.request\",\"time\":1,\"kind\":\"loop\"}\n"
        ),
    )
    .expect("write Kimi main wire");
    let write_state = |children: usize| {
        let mut agents = serde_json::Map::new();
        agents.insert(
            "main".to_owned(),
            json!({"homedir": main_home, "type": "main", "parentAgentId": null}),
        );
        if children >= 1 {
            agents.insert(
                "agent-0".to_owned(),
                json!({"homedir": first_home, "type": "sub", "parentAgentId": "main"}),
            );
        }
        if children >= 2 {
            agents.insert(
                "agent-1".to_owned(),
                json!({"homedir": second_home, "type": "sub", "parentAgentId": "main"}),
            );
        }
        std::fs::write(
            session.join("state.json"),
            serde_json::to_vec(&json!({
                "workDir": env.project_root,
                "agents": agents
            }))
            .expect("serialize Kimi state"),
        )
        .expect("write Kimi state");
    };
    write_state(0);
    std::fs::write(
        kimi_home.join("session_index.jsonl"),
        format!(
            "{{\"sessionId\":{session_id},\"sessionDir\":{session_dir},\"workDir\":{work_dir}}}\n",
            session_id = serde_json::to_string(session_id).unwrap(),
            session_dir = serde_json::to_string(&session).unwrap(),
            work_dir = serde_json::to_string(&env.project_root).unwrap(),
        ),
    )
    .expect("write Kimi session index");
    let run = |payload: Value| {
        let output = env.run_hook("kimi", &payload.to_string());
        assert_hook_succeeded_neutral("kimi", output);
    };
    run(json!({
        "hook_event_name": "SessionStart",
        "session_id": session_id,
        "cwd": env.project_root,
        "source": "startup"
    }));
    run(json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": session_id,
        "cwd": env.project_root,
        "prompt": [{"type": "text", "text": "delegate two checks"}]
    }));

    std::fs::create_dir_all(&first_home).expect("mkdir first Kimi child");
    write_state(1);
    run(json!({
        "hook_event_name": "SubagentStart",
        "session_id": session_id,
        "cwd": env.project_root,
        "agent_name": "explore",
        "prompt": "inspect the parser"
    }));
    std::fs::write(
        first_home.join("wire.jsonl"),
        concat!(
            "{\"type\":\"config.update\",\"profileName\":\"explore\"}\n",
            "{\"type\":\"turn.prompt\",\"input\":[{\"type\":\"text\",\"text\":\"inspect the parser\"}],\"origin\":{\"kind\":\"system_trigger\"}}\n",
            "{\"type\":\"context.append_loop_event\",\"event\":{\"type\":\"content.part\",\"stepUuid\":\"c0\",\"part\":{\"type\":\"text\",\"text\":\"Parser complete\"}}}\n",
            "{\"type\":\"context.append_loop_event\",\"event\":{\"type\":\"step.end\",\"uuid\":\"c0\"}}\n"
        ),
    )
    .expect("write first Kimi child wire");

    std::fs::create_dir_all(&second_home).expect("mkdir second Kimi child");
    write_state(2);
    run(json!({
        "hook_event_name": "SubagentStart",
        "session_id": session_id,
        "cwd": env.project_root,
        "agent_name": "coder",
        "prompt": "inspect the renderer"
    }));
    std::fs::write(
        second_home.join("wire.jsonl"),
        concat!(
            "{\"type\":\"config.update\",\"profileName\":\"coder\"}\n",
            "{\"type\":\"turn.prompt\",\"input\":[{\"type\":\"text\",\"text\":\"inspect the renderer\"}],\"origin\":{\"kind\":\"system_trigger\"}}\n",
            "{\"type\":\"context.append_loop_event\",\"event\":{\"type\":\"content.part\",\"stepUuid\":\"c1\",\"part\":{\"type\":\"text\",\"text\":\"Renderer complete\"}}}\n",
            "{\"type\":\"context.append_loop_event\",\"event\":{\"type\":\"step.end\",\"uuid\":\"c1\"}}\n"
        ),
    )
    .expect("write second Kimi child wire");

    let snapshot = env.snapshot_json();
    let agents = snapshot["agents"].as_array().expect("agents");
    assert_eq!(agents.len(), 3, "one parent plus two Kimi children");
    assert_eq!(
        agents
            .iter()
            .filter(|agent| agent["parent_agent_id"] == session_id)
            .filter(|agent| agent["status"] == "running")
            .count(),
        2
    );

    let before_child_stop = lifecycle_event_count(&env);
    run(json!({
        "hook_event_name": "Stop",
        "session_id": session_id,
        "cwd": env.project_root
    }));
    assert_eq!(
        lifecycle_event_count(&env),
        before_child_stop,
        "a child-fired Stop appends no parent lifecycle event"
    );
    let snapshot = env.snapshot_json();
    let parent = snapshot["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["agent_id"] == session_id)
        .expect("Kimi parent");
    assert_eq!(parent["status"], "running");

    for (profile, response) in [
        ("explore", "Parser complete"),
        ("coder", "Renderer complete"),
    ] {
        run(json!({
            "hook_event_name": "SubagentStop",
            "session_id": session_id,
            "cwd": env.project_root,
            "agent_name": profile,
            "response": response
        }));
    }
    let snapshot = env.snapshot_json();
    let children = snapshot["agents"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|agent| agent["parent_agent_id"] == session_id)
        .collect::<Vec<_>>();
    assert_eq!(children.len(), 2);
    assert!(children.iter().all(|child| child["status"] == "success"));

    std::fs::write(
        &main_wire,
        concat!(
            "{\"type\":\"turn.prompt\",\"input\":[{\"type\":\"text\",\"text\":\"delegate two checks\"}],\"origin\":{\"kind\":\"user\"}}\n",
            "{\"type\":\"llm.request\",\"time\":1,\"kind\":\"loop\"}\n",
            "{\"type\":\"context.append_loop_event\",\"time\":2,\"event\":{\"type\":\"step.end\",\"uuid\":\"main-1\"}}\n"
        ),
    )
    .expect("close Kimi main step");
    run(json!({
        "hook_event_name": "Stop",
        "session_id": session_id,
        "cwd": env.project_root
    }));
    let snapshot = env.snapshot_json();
    let parent = snapshot["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["agent_id"] == session_id)
        .expect("Kimi parent");
    assert_eq!(parent["status"], "success");
}

#[test]
fn copilot_native_order_routes_camel_case_identity_context_and_cleanup() {
    let env = Env::new();
    env.install_agent_hooks("copilot");
    assert!(env.agent_hooks_installed("copilot"));

    let session_id = "copilot-session";
    let session_dir = env
        .home_root
        .join(".copilot/session-state")
        .join(session_id);
    std::fs::create_dir_all(&session_dir).unwrap();
    let transcript = session_dir.join("events.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            "{\"type\":\"user.message\",\"timestamp\":\"2026-07-13T15:13:21Z\",\"data\":{\"content\":\"integration prompt\"}}\n",
            "{\"type\":\"assistant.message\",\"timestamp\":\"2026-07-13T15:13:23Z\",\"data\":{\"content\":\"integration answer\"}}\n",
        ),
    )
    .unwrap();
    let otel = env.home_root.join("copilot-otel.jsonl");
    std::fs::write(
        &otel,
        format!(
            "{{\"type\":\"span\",\"name\":\"chat auto\",\"endTime\":[1783955603,1],\"attributes\":{{\"gen_ai.operation.name\":\"chat\",\"gen_ai.conversation.id\":\"{session_id}\",\"gen_ai.response.model\":\"gpt-5-mini\",\"gen_ai.usage.input_tokens\":100,\"gen_ai.usage.cache_read.input_tokens\":25,\"gen_ai.usage.output_tokens\":10}}}}\n"
        ),
    )
    .unwrap();

    let prompt = json!({"sessionId":session_id,"prompt":"integration prompt"}).to_string();
    let mut prompt_cmd = env.copilot_hook_command("userPromptSubmitted");
    prompt_cmd.env("COPILOT_OTEL_FILE_EXPORTER_PATH", &otel);
    let out = env
        .spawn_payload(prompt_cmd, &prompt)
        .wait_with_output()
        .expect("wait Copilot prompt");
    assert_hook_succeeded_neutral("copilot", out);
    assert_eq!(env.snapshot_json()["agents"][0]["status"], "running");
    assert!(
        env.agent_contexts().is_empty(),
        "the healthy statusline suppresses sparse asynchronous OTel refresh"
    );
    let statusline = json!({
        "session_id": session_id,
        "session_name": "Integration session",
        "version": "1.0.71",
        "model": {
            "id": "auto",
            "display_name": "Auto → gpt-5-mini (1x) (medium)"
        },
        "context_window": {
            "displayed_context_limit": 128000,
            "current_context_used_percentage": 37.5,
            "current_usage": {
                "input_tokens": 6000,
                "output_tokens": 900,
                "cache_creation_input_tokens": 2000,
                "cache_read_input_tokens": 40000
            },
            "total_input_tokens": 82000,
            "total_output_tokens": 6100,
            "total_cache_write_tokens": 7000,
            "total_cache_read_tokens": 69000,
            "total_reasoning_tokens": 1200
        },
        "cost": {
            "total_duration_ms": 312000,
            "total_api_duration_ms": 47000,
            "total_lines_added": 42,
            "total_lines_removed": 3,
            "total_premium_requests": 4
        },
        "ai_used": {"formatted":"1.42"}
    })
    .to_string();
    let statusline_out = env.run_statusline_feed("copilot", &statusline);
    assert!(
        statusline_out.status.success(),
        "Copilot statusline stderr: {}",
        String::from_utf8_lossy(&statusline_out.stderr)
    );
    assert!(statusline_out.stdout.is_empty());
    let contexts = env.agent_contexts();
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].agent_id.as_str(), session_id);
    assert_eq!(contexts[0].context.model_id.as_deref(), Some("gpt-5-mini"));
    assert_eq!(
        contexts[0].context.model_display_name.as_deref(),
        Some("gpt-5-mini")
    );
    assert_eq!(contexts[0].context.effort.as_deref(), Some("medium"));
    assert_eq!(
        contexts[0]
            .context
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.current_usage.as_ref())
            .and_then(|usage| usage.input_tokens),
        Some(6000)
    );
    assert_eq!(
        contexts[0]
            .context
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.context_window_size),
        Some(128_000)
    );
    assert_eq!(
        contexts[0]
            .context
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.used_percentage),
        Some(38)
    );
    assert!(
        contexts[0]
            .context
            .cost
            .as_ref()
            .and_then(|cost| cost.total_cost_usd)
            .is_some_and(|cost| cost > 0.0)
    );

    assert_hook_succeeded_neutral(
        "copilot",
        env.run_copilot_hook(
            "sessionStart",
            &json!({"sessionId":session_id,"source":"startup","initialPrompt":"integration prompt"}).to_string(),
        ),
    );
    assert_eq!(
        env.snapshot_json()["agents"][0]["status"],
        "running",
        "prompt-seeded session start normalizes to the duplicate turn edge"
    );

    assert_hook_succeeded_neutral(
        "copilot",
        env.run_copilot_hook(
            "preToolUse",
            &json!({"sessionId":session_id,"toolName":"ask_user","toolArgs":{"question":"Proceed?"}}).to_string(),
        ),
    );
    assert_eq!(env.snapshot_json()["agents"][0]["status"], "waiting");
    assert_hook_succeeded_neutral(
        "copilot",
        env.run_copilot_hook(
            "preToolUse",
            &json!({"sessionId":session_id,"toolName":"bash"}).to_string(),
        ),
    );
    assert_eq!(env.snapshot_json()["agents"][0]["status"], "running");

    assert_hook_succeeded_neutral(
        "copilot",
        env.run_copilot_hook(
            "agentStop",
            &json!({"sessionId":session_id,"stopReason":"end_turn","transcriptPath":transcript})
                .to_string(),
        ),
    );
    let snapshot = env.snapshot_json();
    assert_eq!(snapshot["agents"][0]["status"], "success");
    assert_eq!(
        snapshot["agents"][0]["transcript_path"],
        transcript.to_string_lossy().as_ref()
    );
    let history = env
        .rimz()
        .args(["agents", "history", session_id, "--json"])
        .output()
        .expect("run Copilot history");
    assert!(
        history.status.success(),
        "{}",
        String::from_utf8_lossy(&history.stderr)
    );
    let history: Value = serde_json::from_slice(&history.stdout).unwrap();
    assert_eq!(history[0]["prompt"], "integration prompt");
    assert_eq!(history[0]["outcome"], "done");
    let transcript_output = env
        .rimz()
        .args(["transcript", session_id])
        .output()
        .expect("run durable Copilot transcript");
    assert!(transcript_output.status.success());
    assert!(String::from_utf8_lossy(&transcript_output.stdout).contains("integration answer"));

    assert_hook_succeeded_neutral(
        "copilot",
        env.run_copilot_hook(
            "sessionEnd",
            &json!({"sessionId":session_id,"reason":"user_exit"}).to_string(),
        ),
    );
    assert!(env.snapshot_json()["agents"].as_array().unwrap().is_empty());
    assert!(env.agent_contexts().is_empty());
}

#[test]
fn cursor_lifecycle_hook_writes_state_and_returns_json_neutral() {
    let env = Env::new();
    let payload = serde_json::to_string(&json!({
        "hook_event_name": "sessionStart",
        "conversation_id": "conv-cursor-01",
        "session_id": "conv-cursor-01",
        "cursor_version": "1.7.0",
        "model_id": "cursor/model"
    }))
    .expect("payload");
    let output = env.run_hook("cursor", &payload);
    assert!(
        output.status.success(),
        "cursor stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "{}\n");

    let snapshot = env.snapshot_json();
    let agents = snapshot["agents"].as_array().expect("agents array");
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["kind"], "cursor");
    assert_eq!(agents[0]["agent_id"], "conv-cursor-01");
    assert_eq!(agents[0]["status"], "idle");
    assert_eq!(agents[0]["model"], "cursor/model");
}

#[test]
fn cursor_parent_hook_derives_chats_store_children_once() {
    let env = Env::new();
    let parent_id = "cursor-derived-parent";
    let child_id = "cursor-derived-child";
    let registered = env.run_hook(
        "cursor",
        &json!({
            "hook_event_name": "sessionStart",
            "conversation_id": parent_id,
        })
        .to_string(),
    );
    assert!(registered.status.success());
    assert_eq!(String::from_utf8_lossy(&registered.stdout), "{}\n");

    let cursor_home = env.home_root.join(".cursor");
    let bucket = cursor_home.join("chats").join(hex::encode(Md5::digest(
        env.project_root.to_str().unwrap().as_bytes(),
    )));
    let child = bucket.join(child_id);
    std::fs::create_dir_all(&child).unwrap();
    let connection = Connection::open(child.join("store.db")).unwrap();
    connection
        .execute_batch(
            "PRAGMA user_version = 1;\
             CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO meta(key, value) VALUES ('0', ?1)",
            [hex::encode(
                serde_json::to_vec(&json!({
                    "agentId": child_id,
                    "latestRootBlobId": "a".repeat(64),
                    "createdAt": Timestamp::now().as_millisecond(),
                    "subagentInfo": {
                        "parentAgentId": parent_id,
                        "rootParentAgentId": parent_id,
                        "toolCallId": "call-derived-child",
                        "typeName": "generalPurpose",
                    },
                }))
                .unwrap(),
            )],
        )
        .unwrap();
    drop(connection);
    let transcript = cursor_home
        .join("projects/project/agent-transcripts")
        .join(child_id)
        .join(format!("{child_id}.jsonl"));
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(
        &transcript,
        concat!(
            "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"<user_query>\\ninspect the hook store\\n</user_query>\"}]}}\n",
            "{\"type\":\"turn_ended\",\"status\":\"success\"}\n"
        ),
    )
    .unwrap();

    let stop_payload = json!({
        "hook_event_name": "stop",
        "conversation_id": parent_id,
        "status": "completed",
    })
    .to_string();
    let stopped = env.run_hook("cursor", &stop_payload);
    assert!(
        stopped.status.success(),
        "cursor stderr: {}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&stopped.stdout), "{}\n");

    let snapshot = env.snapshot_json();
    let agents = snapshot["agents"].as_array().expect("agents");
    let child = agents
        .iter()
        .find(|agent| agent["agent_id"] == child_id)
        .unwrap_or_else(|| panic!("derived child missing: {agents:?}"));
    assert_eq!(child["parent_agent_id"], parent_id);
    assert_eq!(child["name"], "generalPurpose");
    assert_eq!(child["role"], "generalPurpose");
    assert_eq!(child["task"], "inspect the hook store");
    assert_eq!(child["status"], "success");
    assert_eq!(
        child["transcript_path"],
        transcript.to_string_lossy().as_ref()
    );

    let derived_event_count = |env: &Env| {
        env.read_events()
            .iter()
            .filter(|event| {
                matches!(
                    event.kind(),
                    rimz::store::event::EventKind::AgentLifecycle(payload)
                        if payload.event_name.as_deref().is_some_and(|event_name| event_name.starts_with("chatsStoreSubagent"))
                )
            })
            .count()
    };
    assert_eq!(derived_event_count(&env), 2);
    let repeated = env.run_hook("cursor", &stop_payload);
    assert!(repeated.status.success());
    assert_eq!(derived_event_count(&env), 2, "derived facts dedupe");
}

#[test]
fn cursor_user_hook_uses_project_dir_for_pinned_worktree_attribution() {
    let env = Env::new();
    let cursor_cwd = env.home_root.join(".cursor");
    std::fs::create_dir_all(&cursor_cwd).unwrap();
    let payload = json!({
        "hook_event_name": "sessionStart",
        "conversation_id": "cursor-worktree-session",
        "session_id": "cursor-worktree-session",
        "cursor_version": "2026.07.09-a3815c0",
        "workspace_roots": [cursor_cwd],
    })
    .to_string();
    let mut command = env.hook_command("cursor");
    command
        .current_dir(&cursor_cwd)
        .env("CURSOR_PROJECT_DIR", &env.project_root);
    for (key, value) in rimz::workspace::pin_env(&env.workspace_id, &env.project_root) {
        command.env(key, value);
    }
    let output = env
        .spawn_payload(command, &payload)
        .wait_with_output()
        .unwrap();
    assert!(
        output.status.success(),
        "cursor stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "{}\n");
    assert_eq!(
        lifecycle_event_count(&env),
        1,
        "the verified pin routes the hook into the room store"
    );

    let snapshot = env.snapshot_json_with_panes(&[tmux_pane("%0", "agent", &env.project_root)]);
    let agent = snapshot["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["agent_id"] == "cursor-worktree-session")
        .expect("Cursor agent");
    assert_eq!(
        agent["worktree_path"],
        env.project_root.to_string_lossy().as_ref()
    );
    let groups = snapshot["worktree_groups"].as_array().unwrap();
    assert!(
        groups.iter().all(|group| group["key"] != "external"),
        "Cursor belongs to its project/worktree group: {groups:?}"
    );
    assert!(groups.iter().any(|group| {
        group["rows"].as_array().is_some_and(|rows| {
            rows.iter()
                .any(|row| row["id"] == "cursor-worktree-session")
        })
    }));
}

#[test]
fn cursor_ask_local_store_waits_in_pane_without_creating_a_structured_ask() {
    #[derive(Clone, PartialEq, Message)]
    struct CursorRoot {
        #[prost(string, repeated, tag = "4")]
        pending_tool_calls: Vec<String>,
    }

    let env = Env::new();
    let session_id = "22222222-2222-4222-8222-222222222222";
    let created_at_ms = Timestamp::now().as_millisecond() - 60_000;
    let cursor_home = env.home_root.join(".cursor");
    let bucket = cursor_home.join("chats").join(hex::encode(Md5::digest(
        env.project_root.to_str().unwrap().as_bytes(),
    )));
    let session = bucket.join(session_id);
    std::fs::create_dir_all(&session).unwrap();
    std::fs::write(
        session.join("meta.json"),
        serde_json::to_vec(&json!({
            "schemaVersion": 1,
            "createdAtMs": created_at_ms,
            "updatedAtMs": created_at_ms + 70_000,
            "hasConversation": true,
            "cwd": env.project_root,
        }))
        .unwrap(),
    )
    .unwrap();
    let public_transcript = cursor_home
        .join("projects/project/agent-transcripts")
        .join(session_id)
        .join(format!("{session_id}.jsonl"));
    std::fs::create_dir_all(public_transcript.parent().unwrap()).unwrap();
    std::fs::write(&public_transcript, "{\"type\":\"turn_started\"}\n").unwrap();

    let connection = Connection::open(session.join("store.db")).unwrap();
    connection
        .execute_batch(
            "PRAGMA journal_mode = WAL;\
             PRAGMA wal_autocheckpoint = 0;\
             PRAGMA user_version = 1;\
             CREATE TABLE blobs(id TEXT PRIMARY KEY, data BLOB);\
             CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);",
        )
        .unwrap();
    let publish_root = |connection: &Connection, pending: Vec<String>| {
        let root = CursorRoot {
            pending_tool_calls: pending,
        }
        .encode_to_vec();
        let blob_id = hex::encode(Sha256::digest(&root));
        let store_metadata = hex::encode(
            serde_json::to_vec(&json!({
                "agentId": session_id,
                "createdAt": created_at_ms,
                "latestRootBlobId": blob_id,
            }))
            .unwrap(),
        );
        connection.execute("DELETE FROM blobs", []).unwrap();
        connection.execute("DELETE FROM meta", []).unwrap();
        connection
            .execute(
                "INSERT INTO blobs(id, data) VALUES (?1, ?2)",
                (&blob_id, &root),
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO meta(key, value) VALUES ('0', ?1)",
                [store_metadata],
            )
            .unwrap();
    };
    publish_root(
        &connection,
        vec![
            json!({
                "role": "assistant",
                "content": [{
                    "type": "tool-call",
                    "toolCallId": "ask-call",
                    "toolName": "AskQuestion",
                    "args": {
                        "questions": [{
                            "prompt": "  What color do you like most?  ",
                            "options": [{"label": "PRIVATE_OPTION_SENTINEL"}]
                        }]
                    }
                }],
                "providerOptions": {
                "cursor": {"pendingToolCallStartedAtMs": created_at_ms + 60_000}
                }
            })
            .to_string(),
        ],
    );

    for payload in [
        json!({
            "hook_event_name": "sessionStart",
            "conversation_id": session_id,
        }),
        json!({
            "hook_event_name": "beforeSubmitPrompt",
            "conversation_id": session_id,
            "prompt": "ask me",
        }),
    ] {
        let mut command = env.hook_command("cursor");
        command.env("TMUX_PANE", "%0");
        let output = env
            .spawn_payload(command, &payload.to_string())
            .wait_with_output()
            .unwrap();
        assert!(
            output.status.success(),
            "cursor stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), "{}\n");
    }
    let pane = tmux_pane("%0", "agent", &env.project_root);
    let discovered = rimz::agents::session::discover_local_sessions_under(
        "cursor",
        &cursor_home,
        &[env.project_root.as_path()],
    );
    assert_eq!(discovered.len(), 1, "Cursor fixture must be discoverable");
    let waiting = env.snapshot_json_with_panes(std::slice::from_ref(&pane));
    let published_local_sessions =
        std::fs::read_to_string(env.runtime_paths().agent_projection_path())
            .unwrap_or_else(|error| format!("<unreadable: {error}>"));
    let agent = waiting["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["agent_id"] == session_id)
        .expect("Cursor session");
    assert_eq!(
        agent["status"], "waiting",
        "published local sessions: {published_local_sessions}"
    );
    assert_eq!(agent["task"], "What color do you like most?");
    assert!(agent.get("open_ask").is_none_or(Value::is_null));
    assert_eq!(
        agent["transcript_path"],
        public_transcript.to_string_lossy().as_ref()
    );
    assert!(!agent.to_string().contains("PRIVATE_OPTION_SENTINEL"));
    let asks = env
        .rimz()
        .args(["asks", "--json"])
        .output()
        .expect("list asks");
    assert!(asks.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&asks.stdout).unwrap(),
        json!([])
    );

    publish_root(&connection, Vec::new());
    drop(connection);
    let cleared = env.snapshot_json_with_panes(&[pane]);
    let agent = cleared["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["agent_id"] == session_id)
        .expect("Cursor session after Ask closes");
    assert_eq!(agent["status"], "running");
    assert_eq!(agent["task"], "ask me");
}

#[test]
fn cursor_progress_hook_touches_activity_by_conversation_id() {
    let env = Env::new();
    let payload = serde_json::to_string(&json!({
        "hook_event_name": "postToolUse",
        "conversation_id": "conv-cursor-progress",
        "cursor_version": "1.7.0",
        "tool_name": "Read",
        "duration": 12,
    }))
    .expect("payload");
    let output = env.run_hook("cursor", &payload);
    assert!(
        output.status.success(),
        "cursor stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "{}\n");

    let touches = rimz::agent_activity::read_all(&env.runtime_paths());
    assert_eq!(touches.len(), 1, "progress hook writes one heartbeat");
    assert_eq!(touches[0].kind.as_str(), "cursor");
    assert_eq!(touches[0].agent_id.as_str(), "conv-cursor-progress");
}

#[test]
fn cursor_concurrent_subagents_fold_independently_without_context_sidecars() {
    let env = Env::new();
    let run = |payload: Value| {
        let event = payload["hook_event_name"]
            .as_str()
            .expect("hook event name")
            .to_owned();
        let output = env.run_hook("cursor", &serde_json::to_string(&payload).unwrap());
        assert!(
            output.status.success(),
            "{event} stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), "{}\n");
    };

    run(json!({
        "hook_event_name": "sessionStart",
        "conversation_id": "cursor-parent",
        "model_id": "parent-model",
    }));
    let mut context = rimz::agents::AgentContext::new("cursor", Timestamp::now());
    context.model_id = Some("parent-sidecar-model".to_owned());
    let parent_context = rimz::store::agent_context::new_record("cursor", "cursor-parent", context);
    rimz::store::agent_context::write_record(&env.runtime_paths(), &parent_context)
        .expect("seed parent context");
    let parent_context_path = env
        .runtime_paths()
        .agent_context_path("cursor", "cursor-parent");
    let parent_context_before = std::fs::read(&parent_context_path).expect("parent context bytes");

    for (id, task, model, branch) in [
        ("cursor-child-a", "inspect hooks", "default", "feature/a"),
        (
            "cursor-child-b",
            "review identity",
            "cursor/small",
            "feature/b",
        ),
    ] {
        run(json!({
            "hook_event_name": "subagentStart",
            "conversation_id": "cursor-parent",
            "subagent_id": id,
            "parent_conversation_id": "cursor-parent",
            "subagent_type": "generalPurpose",
            "task": task,
            "subagent_model": model,
            "git_branch": branch,
            "model_id": "parent-model",
            "transcript_path": "/tmp/parent.jsonl",
        }));
    }

    run(json!({
        "hook_event_name": "subagentStop",
        "conversation_id": "cursor-parent",
        "subagent_id": "cursor-child-b",
        "parent_conversation_id": "cursor-parent",
        "status": "error",
        "transcript_path": "/tmp/parent.jsonl",
        "agent_transcript_path": "/tmp/cursor-child-b.jsonl",
        "model_id": "parent-model",
    }));
    run(json!({
        "hook_event_name": "subagentStop",
        "conversation_id": "cursor-parent",
        "subagent_id": "cursor-child-a",
        "parent_conversation_id": "cursor-parent",
        "status": "completed",
        "transcript_path": "/tmp/parent.jsonl",
        "agent_transcript_path": "/tmp/cursor-child-a.jsonl",
        "model_id": "parent-model",
    }));

    let snapshot = env.snapshot_json_with_panes(&[tmux_pane("%0", "agent", &env.project_root)]);
    let agents = snapshot["agents"].as_array().expect("agents");
    assert_eq!(agents.len(), 3, "one parent and two children: {agents:?}");
    for (id, task, model, branch, status, transcript) in [
        (
            "cursor-child-a",
            "inspect hooks",
            "auto",
            "feature/a",
            "success",
            "/tmp/cursor-child-a.jsonl",
        ),
        (
            "cursor-child-b",
            "review identity",
            "cursor/small",
            "feature/b",
            "failed",
            "/tmp/cursor-child-b.jsonl",
        ),
    ] {
        let child = agents
            .iter()
            .find(|agent| agent["agent_id"] == id)
            .unwrap_or_else(|| panic!("child {id} missing: {agents:?}"));
        assert_eq!(child["parent_agent_id"], "cursor-parent");
        assert_eq!(child["name"], "generalPurpose");
        assert_eq!(child["role"], "generalPurpose");
        assert_eq!(child["task"], task);
        assert_eq!(child["model"], model);
        assert_eq!(child["worktree_branch"], branch);
        assert_eq!(child["status"], status);
        assert_eq!(child["transcript_path"], transcript);
    }

    let rows: Vec<&Value> = snapshot["worktree_groups"]
        .as_array()
        .expect("groups")
        .iter()
        .flat_map(|group| group["rows"].as_array().expect("rows"))
        .collect();
    assert_eq!(
        rows.len(),
        1,
        "only the parent is a top-level row: {rows:?}"
    );
    assert_eq!(rows[0]["id"], "cursor-parent");
    assert_eq!(
        rows[0]["sub_agents"]
            .as_array()
            .expect("nested children")
            .len(),
        2
    );

    let activity = rimz::agent_activity::read_all(&env.runtime_paths());
    for id in ["cursor-child-a", "cursor-child-b"] {
        assert!(
            activity
                .iter()
                .any(|touch| touch.kind == "cursor" && touch.agent_id == id),
            "missing child-keyed activity for {id}: {activity:?}",
        );
        assert!(
            !env.runtime_paths()
                .agent_context_path("cursor", id)
                .exists(),
            "child lifecycle must not create an agent-context sidecar",
        );
    }
    assert!(env.subagent_contexts().is_empty());
    assert_eq!(
        std::fs::read(&parent_context_path).expect("parent context after child hooks"),
        parent_context_before,
        "child transcript/model fields must not mutate the parent sidecar",
    );
}

#[test]
fn cursor_response_tokens_and_interruption_flow_end_to_end() {
    let env = Env::new();
    let transcript_path = env.project_root.join("conv-cursor-flow.jsonl");
    std::fs::write(
        &transcript_path,
        concat!(
            "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"THINKING_SENTINEL_DO_NOT_INGEST\"}]}}\n",
            "{\"type\":\"turn_ended\",\"status\":\"success\"}\n"
        ),
    )
    .unwrap();
    let transcript_path = transcript_path.to_string_lossy().into_owned();
    let run = |event: &str, extra: Value| {
        let mut payload = json!({
            "hook_event_name": event,
            "conversation_id": "conv-cursor-flow",
            "cursor_version": "2026.07.09-a3815c0",
            "transcript_path": transcript_path,
        });
        payload
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().expect("hook extra is an object").clone());
        let output = env.run_hook("cursor", &serde_json::to_string(&payload).unwrap());
        assert!(
            output.status.success(),
            "{event} stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), "{}\n");
    };

    run("sessionStart", json!({"model_id": "cursor/model"}));
    run("beforeSubmitPrompt", json!({"prompt": "fix it"}));
    run(
        "afterAgentResponse",
        json!({"text": "  safe final response  "}),
    );
    run(
        "stop",
        json!({
            "status": "completed",
            "input_tokens": 0,
            "output_tokens": 12,
            "cache_read_tokens": 3,
            "cache_write_tokens": 4,
        }),
    );

    let snapshot = env.snapshot_json();
    let agent = &snapshot["agents"][0];
    assert_eq!(agent["status"], "success");
    assert_eq!(agent["fresh_input_tokens"], 0);
    assert_eq!(agent["output_tokens"], 12);
    assert_eq!(agent["cache_read_input_tokens"], 3);
    assert_eq!(agent["cache_write_input_tokens"], 4);
    let entries = rimz::transcript::read_all(env.store().paths()).unwrap();
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.entry == rimz::transcript::TranscriptKind::Assistant)
            .map(|entry| entry.text.as_str())
            .collect::<Vec<_>>(),
        ["safe final response"]
    );
    assert!(
        entries
            .iter()
            .all(|entry| !entry.text.contains("THINKING_SENTINEL_DO_NOT_INGEST"))
    );
    let context = rimz::store::agent_context::read_one(
        env.store().runtime_paths(),
        "cursor",
        "conv-cursor-flow",
    )
    .expect("cursor sidecar");
    assert_eq!(
        context.transcript_path.as_deref(),
        Some(transcript_path.as_str())
    );
    assert!(context.transcript_stat.is_some());

    let aborted = Env::new();
    let payload = serde_json::to_string(&json!({
        "hook_event_name": "stop",
        "conversation_id": "conv-cursor-aborted",
        "status": "aborted",
    }))
    .unwrap();
    let output = aborted.run_hook("cursor", &payload);
    assert!(output.status.success());
    assert_eq!(aborted.snapshot_json()["agents"][0]["status"], "idle");
    assert!(
        rimz::transcript::read_all(aborted.store().paths())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn cursor_transcript_recovery_does_not_settle_a_new_active_turn() {
    let env = Env::new();
    let transcript_path = env.project_root.join("conv-cursor-recovery.jsonl");
    let terminal = "{\"type\":\"turn_ended\",\"status\":\"success\"}\n";
    let first_turn = concat!(
        "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"turn one\"}]}}\n",
        "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"first answer\"}]}}\n",
        "{\"type\":\"turn_ended\",\"status\":\"success\"}\n",
    );
    std::fs::write(&transcript_path, first_turn).unwrap();
    let transcript_path_string = transcript_path.to_string_lossy().into_owned();
    let run = |event: &str, extra: Value| {
        let mut payload = json!({
            "hook_event_name": event,
            "conversation_id": "conv-cursor-recovery",
            "transcript_path": transcript_path_string,
        });
        payload
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().expect("hook extra is an object").clone());
        let output = env.run_hook("cursor", &serde_json::to_string(&payload).unwrap());
        assert!(
            output.status.success(),
            "{event} stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };

    run("sessionStart", json!({}));
    run("beforeSubmitPrompt", json!({"prompt": "turn two"}));
    let active_rewrite = concat!(
        "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"turn one\"}]}}\n",
        "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"first answer\"}]}}\n",
        "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"turn two\"}]}}\n",
        "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"THINKING_SENTINEL_DO_NOT_INGEST\"}]}}\n",
    );
    std::fs::write(&transcript_path, active_rewrite).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&transcript_path)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::SystemTime::now() + Duration::from_secs(60)),
        )
        .unwrap();
    rimz::sidebar::refresh::refresh_session_transcript_context(
        &env.runtime_paths(),
        "cursor",
        "conv-cursor-recovery",
        None,
    );

    let snapshot = env
        .store()
        .snapshot()
        .unwrap()
        .with_agent_context(env.agent_contexts());
    let active = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "conv-cursor-recovery")
        .unwrap();
    assert_eq!(
        active.effective_status(),
        rimz::agents::AgentStatus::Running
    );
    assert!(!rimz::message::gate_open_for_agent(
        rimz::message::DeliveryGate::Done,
        active,
        false,
        Timestamp::now(),
    ));

    std::fs::write(&transcript_path, format!("{active_rewrite}{terminal}")).unwrap();
    let file = std::fs::File::options()
        .write(true)
        .open(&transcript_path)
        .unwrap();
    file.set_times(
        std::fs::FileTimes::new()
            .set_modified(std::time::SystemTime::now() + Duration::from_secs(61)),
    )
    .unwrap();
    drop(file);
    rimz::sidebar::refresh::refresh_session_transcript_context(
        &env.runtime_paths(),
        "cursor",
        "conv-cursor-recovery",
        None,
    );

    let snapshot = env
        .store()
        .snapshot()
        .unwrap()
        .with_agent_context(env.agent_contexts());
    let settled = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "conv-cursor-recovery")
        .unwrap();
    assert_eq!(
        settled.effective_status(),
        rimz::agents::AgentStatus::Success
    );
    assert!(rimz::message::gate_open_for_agent(
        rimz::message::DeliveryGate::Done,
        settled,
        false,
        Timestamp::now(),
    ));
    assert!(
        serde_json::to_string(&env.agent_contexts())
            .unwrap()
            .find("THINKING_SENTINEL_DO_NOT_INGEST")
            .is_none()
    );
}

#[test]
fn duplicate_cursor_session_end_is_idempotent_beyond_audit_end_stamps() {
    let env = Env::new();
    let transcript_path = env.project_root.join("conv-cursor-end.jsonl");
    std::fs::write(
        &transcript_path,
        concat!(
            "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"safe\"}]}}\n",
            "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"THINKING_SENTINEL_DO_NOT_INGEST\"}]}}\n",
            "{\"type\":\"turn_ended\",\"status\":\"success\"}\n",
        ),
    )
    .unwrap();
    let transcript_path = transcript_path.to_string_lossy().into_owned();
    let session_id = "conv-cursor-end";
    let session_start = json!({
        "hook_event_name": "sessionStart",
        "conversation_id": session_id,
        "session_id": session_id,
        "cursor_version": "2026.07.09-a3815c0",
        "model_id": "cursor/model",
        "transcript_path": transcript_path,
    })
    .to_string();
    let output = env.run_hook("cursor", &session_start);
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "{}\n");
    assert_eq!(env.snapshot_json()["agents"].as_array().unwrap().len(), 1);
    assert_eq!(env.agent_contexts().len(), 1);

    let snapshot = env.store().snapshot_cached().unwrap();
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == session_id)
        .unwrap();
    let message = rimz::message::MessageRecord::new(
        env.workspace_id.clone(),
        agent,
        "queued work".to_owned(),
        true,
        rimz::message::DeliveryGate::Done,
    );
    let message_id = message.message_id.clone();
    env.store().queue_message(&message, "rimz-test").unwrap();

    let run = rimz::harness::run::RunRecord::new(
        env.workspace_id.clone(),
        rimz::ids::AgentKind::new_unchecked("cursor"),
        rimz::harness::run::PermissionMode::Auto,
        "go".to_owned(),
        env.project_root.clone(),
    );
    rimz::harness::run::create(env.store().paths(), &run).unwrap();
    let payload = json!({
        "hook_event_name": "sessionEnd",
        "conversation_id": session_id,
        "cursor_version": "2026.07.09-a3815c0",
        "duration_ms": 1,
        "final_status": "completed",
        "generation_id": "generation-sanitized",
        "is_background_agent": false,
        "model": "cursor/model",
        "reason": "completed",
        "session_id": session_id,
        "transcript_path": transcript_path,
        "workspace_roots": [env.project_root],
    })
    .to_string();
    let feed_end = || {
        let mut command = env.hook_command("cursor");
        command.env(rimz::harness::run::ENV_RUN_ID, run.run_id.as_str());
        let output = env
            .spawn_payload(command, &payload)
            .wait_with_output()
            .unwrap();
        assert!(
            output.status.success(),
            "sessionEnd stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), "{}\n");
    };
    let assert_ended_once = || {
        assert!(env.snapshot_json()["agents"].as_array().unwrap().is_empty());
        assert!(env.agent_contexts().is_empty());
        assert!(
            rimz::transcript::read_all(env.store().paths())
                .unwrap()
                .is_empty()
        );
        let archived = env.store().list_message_history().unwrap();
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0].message_id, message_id);
        assert_eq!(archived[0].status, rimz::message::MessageStatus::Archived);
        assert_eq!(
            env.read_events()
                .iter()
                .filter(|event| event.method == "message.archived")
                .count(),
            1,
        );
    };

    feed_end();
    assert_ended_once();
    let terminal_run = rimz::harness::run::load(env.store().paths(), &run.run_id).unwrap();
    assert_eq!(terminal_run.status, rimz::harness::run::RunStatus::Failed);

    feed_end();
    assert_ended_once();
    assert_eq!(
        rimz::harness::run::load(env.store().paths(), &run.run_id).unwrap(),
        terminal_run,
        "the duplicate terminal hook must not rewrite the run record",
    );
    assert_eq!(
        lifecycle_event_count(&env),
        3,
        "sessionStart plus two audit end observations remain durable",
    );
}

#[test]
fn internal_app_server_hook_is_suppressed_and_records_nothing() {
    // A `codex app-server` that RimZ cold-spawns for read-only enrichment fires
    // its own `SessionStart` hook on startup. The internal-app-server marker
    // rides that server's env into the hook child, so `rimz hooks feed` must
    // no-op — no rollup, no lifecycle row, no `refresh-context` spawn — which is
    // what keeps the refresh→spawn→hook→refresh recursion from forming. Without
    // the marker the identical payload rolls up an idle agent (see
    // `session_start_hooks_write_lifecycle_rows`).
    let env = Env::new();
    let payload = serde_json::to_string(&json!({
        "hook_event_name": "SessionStart",
        "session_id": "sess-internal-app-server",
        "approval_policy": "ask",
    }))
    .expect("payload");

    let mut cmd = env.hook_command("codex");
    cmd.env("RIMZ_CODEX_INTERNAL_APP_SERVER", "1");
    let output = env
        .spawn_payload(cmd, &payload)
        .wait_with_output()
        .expect("wait child");

    assert!(
        output.status.success(),
        "suppressed hook stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "suppressed hook stdout must stay empty, got: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );

    let parsed = env.snapshot_json();
    let agents = parsed["agents"].as_array().expect("agents array");
    assert!(
        agents.is_empty(),
        "internal app-server hook must not roll up an agent: {agents:?}"
    );
    assert_eq!(
        lifecycle_event_count(&env),
        0,
        "internal app-server hook must append no lifecycle event"
    );
}

#[test]
fn permission_hook_sets_waiting_status() {
    for (source, payload) in permission_cases() {
        let env = Env::new();
        let output = env.run_hook(source, &payload);
        assert_hook_succeeded_neutral(source, output);

        let parsed = env.snapshot_json();
        let agents = parsed["agents"].as_array().expect("agents array");
        assert_eq!(agents.len(), 1, "{source} should roll up one agent");
        assert_eq!(agents[0]["kind"], source);
        assert_eq!(agents[0]["status"], "waiting");
        assert!(agents[0]["waiting_since"].as_str().is_some());
        assert_eq!(agents[0]["open_ask"]["kind"], "permission");
        assert!(
            agents[0]["open_ask"]["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("ask_"))
        );
        if source == "claude" {
            assert!(agents[0]["open_ask"]["detail"].as_str().is_some());
        }
    }
}

#[test]
fn permission_waiting_clears_on_tool_use() {
    let env = Env::new();
    let output = env.run_hook("claude", &permission_payload("Bash"));
    assert_hook_succeeded_neutral("claude", output);

    let output = env.run_hook(
        "claude",
        &serde_json::to_string(&json!({
            "hook_event_name": "PostToolUse",
            "session_id": "sess-claude-permission",
            "tool_name": "Bash",
        }))
        .expect("payload"),
    );
    assert!(
        output.status.success(),
        "post-tool stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let parsed = env.snapshot_json();
    assert_eq!(parsed["agents"][0]["status"], "running");
    assert!(parsed["agents"][0]["waiting_since"].is_null());
    assert!(parsed["agents"][0]["open_ask"].is_null());
}

#[test]
fn pi_tool_call_emits_neutral_and_no_waiting_row() {
    // Pi has no native permission prompt (`native_ask_ui` = false): with no
    // native UI the hook must answer neutral (empty stdout = the tool runs).
    let env = Env::new();
    let output = env.run_hook("pi", &pi_tool_call_payload("bash"));
    assert_hook_succeeded_neutral("pi", output);

    let parsed = env.snapshot_json();
    assert_eq!(
        parsed["agents"].as_array().expect("agents array").len(),
        0,
        "pi must not orphan an unanswerable waiting row: {parsed}"
    );
}

#[test]
fn codex_subagent_lifecycle_uses_child_agent_identity() {
    let env = Env::new();
    let run = |payload: Value| {
        let payload = serde_json::to_string(&payload).expect("payload");
        let mut cmd = env.hook_command("codex");
        cmd.env("RIMZ_CODEX_BIN", "/nonexistent/codex-binary-xyz");
        let output = env
            .spawn_payload(cmd, &payload)
            .wait_with_output()
            .expect("wait codex hook");
        assert_hook_succeeded_neutral("codex", output);
    };
    run(json!({
        "hook_event_name": "SessionStart",
        "session_id": "sess-codex-parent",
        "source": "startup",
    }));

    let mut context = rimz::agents::AgentContext::new("codex", Timestamp::now());
    context.model_id = Some("parent-model".to_owned());
    let parent_context =
        rimz::store::agent_context::new_record("codex", "sess-codex-parent", context);
    rimz::store::agent_context::write_record(&env.runtime_paths(), &parent_context)
        .expect("seed parent context");
    let parent_context_path = env
        .runtime_paths()
        .agent_context_path("codex", "sess-codex-parent");
    let parent_context_before = std::fs::read(&parent_context_path).expect("parent context bytes");

    let sessions = env.home_root.join(".codex/sessions/2026/06/26");
    std::fs::create_dir_all(&sessions).expect("mkdir codex sessions");
    let child_rollout = sessions.join("rollout-child-thread-1.jsonl");
    std::fs::write(
        &child_rollout,
        concat!(
            r#"{"timestamp":"2026-06-26T00:00:00Z","type":"session_meta","payload":{"id":"child-thread-1","thread_source":"subagent","parent_thread_id":"nested-parent","agent_nickname":"Atlas","agent_path":"/root/research/explore_hooks","agent_role":"explorer","multi_agent_version":"v2"}}"#,
            "\n",
            r#"{"type":"turn_context","payload":{"model":"gpt-5.5-codex","effort":"xhigh"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":300,"cached_input_tokens":200,"output_tokens":21,"total_tokens":321},"model_context_window":1000}}}"#,
            "\n",
            r#"{"timestamp":"2026-06-26T00:00:05Z","type":"event_msg","payload":{"type":"stream_error","message":"child failed"}}"#,
            "\n",
        ),
    )
    .expect("write child rollout");

    let start_payload = serde_json::to_string(&json!({
        "hook_event_name": "SubagentStart",
        "session_id": "sess-codex-parent",
        "agent_id": "child-thread-1",
        "agent_type": "default",
        "permission_mode": "acceptEdits",
        "worktree_branch": "feature-x",
        "transcript_path": child_rollout.to_string_lossy(),
    }))
    .expect("payload");
    let mut cmd = env.hook_command("codex");
    cmd.env("RIMZ_CODEX_BIN", "/nonexistent/codex-binary-xyz");
    assert_hook_succeeded_neutral(
        "codex",
        env.spawn_payload(cmd, &start_payload)
            .wait_with_output()
            .expect("wait start"),
    );

    run(json!({
        "hook_event_name": "PostToolUse",
        "session_id": "sess-codex-parent",
        "agent_id": "child-thread-1",
        "agent_type": "default",
        "tool_name": "Bash",
        "transcript_path": child_rollout.to_string_lossy(),
    }));
    run(json!({
        "hook_event_name": "PermissionRequest",
        "session_id": "sess-codex-parent",
        "agent_id": "child-thread-1",
        "agent_type": "default",
        "tool_name": "Bash",
        "transcript_path": child_rollout.to_string_lossy(),
    }));

    let parsed = env.snapshot_json();
    let agents = parsed["agents"].as_array().expect("agents array");
    assert_eq!(agents.len(), 2, "one root plus one child: {agents:?}");
    let child = agents
        .iter()
        .find(|agent| agent["agent_id"] == "child-thread-1")
        .expect("child row");
    assert_eq!(child["status"], "waiting");
    assert_eq!(child["name"], "Atlas");
    assert_eq!(child["task"], "research/explore_hooks");
    assert_eq!(child["role"], "explorer");
    assert_eq!(child["model"], "gpt-5.5-codex");
    assert_eq!(child["effort"], "xhigh");
    assert_eq!(child["total_tokens"], 321);
    // The child keys off `agent_id`; the payload's `session_id` is captured as
    // the parent root so the sidebar can nest the child under it.
    assert_eq!(child["parent_agent_id"], "sess-codex-parent");

    run(json!({
        "hook_event_name": "SubagentStop",
        "session_id": "sess-codex-parent",
        "agent_id": "child-thread-1",
        "agent_type": "default",
        "agent_transcript_path": child_rollout.to_string_lossy(),
    }));
    assert_eq!(
        std::fs::read(&parent_context_path).expect("parent context after child hooks"),
        parent_context_before,
        "child hooks never merge transcript data into the parent sidecar"
    );

    let parsed = env.snapshot_json();
    let child = parsed["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["agent_id"] == "child-thread-1")
        .unwrap();
    assert_eq!(child["status"], "failed");
    assert_eq!(child["name"], "Atlas");
    assert_eq!(child["task"], "research/explore_hooks");
    assert_eq!(child["parent_agent_id"], "sess-codex-parent");
    let activity = rimz::agent_activity::read_all(&env.runtime_paths());
    assert!(
        activity
            .iter()
            .any(|touch| { touch.kind == "codex" && touch.agent_id == "child-thread-1" })
    );
}

#[test]
fn codex_subagent_permission_without_parent_frame_stays_metadata_only() {
    let env = Env::new();
    let start_payload = serde_json::to_string(&json!({
        "hook_event_name": "SubagentStart",
        "session_id": "sess-codex-parent",
        "agent_id": "child-thread-1",
        "agent_type": "review",
        "permission_mode": "default",
    }))
    .expect("payload");
    let output = env.run_hook("codex", &start_payload);
    assert!(output.status.success());

    let permission_payload = serde_json::to_string(&json!({
        "hook_event_name": "PermissionRequest",
        "session_id": "sess-codex-parent",
        "agent_id": "child-thread-1",
        "agent_type": "review",
        "tool_name": "Bash",
        "tool_input": { "command": "cargo test" },
    }))
    .expect("payload");
    let output = env.run_hook("codex", &permission_payload);
    assert!(output.status.success());
    assert!(output.stdout.is_empty());

    let parsed = env.snapshot_json();
    let groups = parsed["worktree_groups"].as_array().expect("groups");
    assert_eq!(
        groups.len(),
        0,
        "a child-only ask has no frame-backed parent card: {groups:?}"
    );
    let agents = parsed["agents"].as_array().expect("agents");
    assert_eq!(
        agents.len(),
        1,
        "the child waiting state remains store metadata"
    );
    assert_eq!(agents[0]["agent_id"], "child-thread-1");
    assert_eq!(agents[0]["status"], "waiting");
}

#[test]
fn claude_in_subagent_tool_event_does_not_disturb_parent() {
    // Claude stamps `agent_id` on every payload fired inside a subagent, so a
    // backgrounded child's mutating tool arrives on the parent's session with a
    // foreign id. It must fold to nothing: no lifecycle event appended, no
    // phantom child row, and — the load-bearing part — the parent's
    // `last_activity` stays its own (the child-keyed heartbeat carries the
    // child's progress instead).
    let env = Env::new();
    let output = env.run_hook(
        "claude",
        &serde_json::to_string(&json!({
            "hook_event_name": "SessionStart",
            "session_id": "sess-claude-parent",
        }))
        .expect("payload"),
    );
    assert!(output.status.success());
    let lifecycle_events_before = env
        .read_events()
        .iter()
        .filter(|event| event.method == "agent.lifecycle")
        .count();

    let output = env.run_hook(
        "claude",
        &serde_json::to_string(&json!({
            "hook_event_name": "PostToolUse",
            "session_id": "sess-claude-parent",
            "agent_id": "child-1",
            "tool_name": "Edit",
        }))
        .expect("payload"),
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty(), "lifecycle hook is silent");

    let lifecycle_events_after = env
        .read_events()
        .iter()
        .filter(|event| event.method == "agent.lifecycle")
        .count();
    assert_eq!(
        lifecycle_events_after, lifecycle_events_before,
        "a foreign-child tool event appends no lifecycle event"
    );
    let parsed = env.snapshot_json();
    let agents = parsed["agents"].as_array().expect("agents array");
    assert_eq!(agents.len(), 1, "no phantom child row: {agents:?}");
    assert_eq!(agents[0]["agent_id"], "sess-claude-parent");
}

#[test]
fn waiting_agent_survives_backgrounded_child_tool() {
    // The asking-while-running regression lock: a parent blocked on a native
    // ask must stay `waiting` while a backgrounded subagent works. Before the
    // foreign-id drop, the child's mutating PostToolUse advanced the parent's
    // `last_activity` past the ask and the `waiting` fold dropped.
    let env = Env::new();
    let run = |payload: &Value| {
        let payload = serde_json::to_string(payload).expect("payload");
        let output = env.run_installed_hook_in_pane("claude", &payload, &[("TMUX_PANE", "%0")]);
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };

    run(&json!({
        "hook_event_name": "SessionStart",
        "session_id": "sess-claude-parent",
    }));
    run(&json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "sess-claude-parent",
        "prompt": "fix the sidebar reload bug",
    }));

    // Blocking asks set Waiting.
    run(&json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "AskUserQuestion",
        "tool_input": { "questions": [{ "question": "which fix shape?" }] },
        "session_id": "sess-claude-parent",
    }));

    // The backgrounded child keeps working while the parent blocks.
    run(&json!({
        "hook_event_name": "PostToolUse",
        "session_id": "sess-claude-parent",
        "agent_id": "child-1",
        "tool_name": "Bash",
    }));

    let parsed = env.snapshot_json_with_panes(&[tmux_pane("%0", "claude", &env.project_root)]);
    let groups = parsed["worktree_groups"].as_array().expect("groups");
    let rows: Vec<&Value> = groups
        .iter()
        .flat_map(|group| group["rows"].as_array().expect("rows"))
        .collect();
    assert_eq!(rows.len(), 1, "one waiting parent row expected: {rows:?}");
    assert_eq!(rows[0]["status"], "waiting");
}

#[test]
fn manual_compact_then_pre_tool_use_resumes_running() {
    let env = Env::new();
    let run = |payload: &serde_json::Value| {
        let payload = serde_json::to_string(payload).expect("payload");
        let output = env.run_hook("codex", &payload);
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty(), "lifecycle hook is silent");
    };

    run(&json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "sess-codex-compact",
        "prompt": "continue after compact",
    }));
    run(&json!({
        "hook_event_name": "PostCompact",
        "session_id": "sess-codex-compact",
        "trigger": "manual",
    }));
    let after_manual = env.snapshot_json();
    assert_eq!(after_manual["agents"][0]["status"], "idle");
    let before = lifecycle_event_count(&env);

    run(&json!({
        "hook_event_name": "PreToolUse",
        "session_id": "sess-codex-compact",
        "tool_name": "shell",
    }));

    assert_eq!(
        lifecycle_event_count(&env),
        before + 1,
        "a resting-row PreToolUse reconciliation is persisted"
    );
    let resumed = env.snapshot_json();
    assert_eq!(resumed["agents"][0]["status"], "running");
}

// --- Claude PreToolUse blocking events ---
//
// `ExitPlanMode` and `AskUserQuestion` are PreToolUse blocking hooks. The
// agent expects the decision to carry `updatedInput`; neutral keeps stdout
// empty and the agent's own UI is the answer surface.

#[test]
fn claude_pre_tool_blocking_events_set_waiting() {
    for (tool, expected_kind) in [
        ("ExitPlanMode", "plan_approval"),
        ("AskUserQuestion", "question"),
    ] {
        let env = Env::new();
        let output = env.run_hook("claude", &claude_pre_tool_use_payload(tool));
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stdout.is_empty(),
            "neutral Claude blocking hook must keep stdout empty"
        );

        let parsed = env.snapshot_json();
        let agents = parsed["agents"].as_array().expect("agents array");
        assert_eq!(agents.len(), 1, "{tool}");
        assert_eq!(agents[0]["status"], "waiting", "{tool}");
        let event = env
            .read_events()
            .into_iter()
            .rev()
            .find(|event| event.method == "agent.lifecycle")
            .expect("lifecycle event");
        assert_eq!(
            event.params_value()["signal"]["kind"],
            expected_kind,
            "{tool}"
        );
    }
}

// --- Codex PreToolUse blocking events ---

#[test]
fn codex_request_user_input_sets_waiting() {
    let env = Env::new();
    let output = env.run_hook("codex", &codex_pre_tool_use_payload());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "neutral Codex blocking hook must keep stdout empty"
    );

    let parsed = env.snapshot_json();
    let agents = parsed["agents"].as_array().expect("agents array");
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["status"], "waiting");
    let event = env
        .read_events()
        .into_iter()
        .rev()
        .find(|event| event.method == "agent.lifecycle")
        .expect("lifecycle event");
    assert_eq!(event.params_value()["signal"]["kind"], "question");
}

// --- Claude lifecycle and install/uninstall ---

#[test]
fn claude_compaction_bracket_closers_clear_head() {
    for (session_id, closer, expect_running, expect_event_count) in [
        (
            "sess-claude-compact",
            json!({
                "hook_event_name": "SessionStart",
                "session_id": "sess-claude-compact",
                "source": "compact",
            }),
            true,
            None,
        ),
        (
            "sess-claude-pretool-close",
            json!({
                "hook_event_name": "PreToolUse",
                "session_id": "sess-claude-pretool-close",
                "tool_name": "Read",
            }),
            false,
            Some(3),
        ),
    ] {
        let env = Env::new();
        run_claude_lifecycle(
            &env,
            json!({
                "hook_event_name": "UserPromptSubmit",
                "session_id": session_id,
                "prompt": "continue the turn",
            }),
        );
        run_claude_lifecycle(
            &env,
            json!({
                "hook_event_name": "PreCompact",
                "session_id": session_id,
            }),
        );
        run_claude_lifecycle(&env, closer);

        if let Some(count) = expect_event_count {
            assert_eq!(
                lifecycle_event_count(&env),
                count,
                "the non-mutating PreToolUse must be durable when it closes a compaction bracket"
            );
        }
        let parsed = env.snapshot_json();
        let agent = &parsed["agents"][0];
        assert_eq!(agent["compaction_count"], 1);
        assert!(
            agent.get("compacting_since").is_none_or(Value::is_null),
            "compacting head should be cleared: {agent:?}"
        );
        if expect_running {
            assert_eq!(agent["status"], "running");
            assert_eq!(agent["phase"], "reasoning");
        }
    }
}

#[cfg(unix)]
fn fake_agent_bin_dir(names: &[&str]) -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::TempDir::new().expect("fake agent bin dir");
    for name in names {
        let path = dir.path().join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").expect("write fake agent");
        let mut perms = std::fs::metadata(&path)
            .expect("fake agent metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod fake agent");
    }
    dir
}

#[cfg(unix)]
#[test]
fn hooks_install_and_uninstall_no_arg_round_trips_detected_agents() {
    let env = Env::new();
    let bin_dir = fake_agent_bin_dir(&["claude", "codex"]);

    let install = env
        .rimz()
        .env("PATH", bin_dir.path())
        .args(["hooks", "install"])
        .output()
        .expect("spawn install");
    assert!(
        install.status.success(),
        "install stderr: {}",
        String::from_utf8_lossy(&install.stderr)
    );
    let install_stdout = String::from_utf8_lossy(&install.stdout);
    assert!(install_stdout.contains("✓ claude  installed"));
    assert!(install_stdout.contains("~/.claude/settings.json"));
    assert!(install_stdout.contains("✓ codex  installed"));
    assert!(install_stdout.contains("~/.codex/config.toml"));
    assert!(env.agent_hooks_installed("claude"));
    assert!(env.agent_hooks_installed("codex"));

    let rerun = env
        .rimz()
        .env("PATH", bin_dir.path())
        .args(["hooks", "install"])
        .output()
        .expect("spawn install rerun");
    assert!(
        rerun.status.success(),
        "install rerun stderr: {}",
        String::from_utf8_lossy(&rerun.stderr)
    );
    let rerun_stdout = String::from_utf8_lossy(&rerun.stdout);
    assert!(rerun_stdout.contains("✓ claude  hooks up to date"));
    assert!(rerun_stdout.contains("✓ codex  hooks up to date"));

    let empty_path = fake_agent_bin_dir(&[]);
    let uninstall = env
        .rimz()
        .env("PATH", empty_path.path())
        .args(["hooks", "uninstall"])
        .output()
        .expect("spawn uninstall");
    assert!(
        uninstall.status.success(),
        "uninstall stderr: {}",
        String::from_utf8_lossy(&uninstall.stderr)
    );
    let uninstall_stdout = String::from_utf8_lossy(&uninstall.stdout);
    assert!(uninstall_stdout.contains("✓ claude  removed"));
    assert!(uninstall_stdout.contains("~/.claude/settings.json"));
    assert!(uninstall_stdout.contains("✓ codex  removed"));
    assert!(uninstall_stdout.contains("~/.codex/config.toml"));
    assert!(!env.agent_hooks_installed("claude"));
    assert!(!env.agent_hooks_installed("codex"));

    let empty = env
        .rimz()
        .env("PATH", empty_path.path())
        .args(["hooks", "uninstall"])
        .output()
        .expect("spawn empty uninstall");
    assert!(
        empty.status.success(),
        "empty uninstall stderr: {}",
        String::from_utf8_lossy(&empty.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&empty.stdout),
        "No RimZ-managed hooks are installed; nothing to uninstall.\n"
    );
}

/// The statusline feed passes the JSON through to the wrapped command verbatim
/// and forwards its stdout, so the user's rendering is unaffected.
#[test]
fn statusline_feed_passes_json_through_to_wrapped_command() {
    let env = Env::new();
    let claude_settings = env.agent_config_path("claude");
    std::fs::create_dir_all(claude_settings.parent().unwrap()).unwrap();
    // Wrap `cat`, which echoes the JSON it receives on stdin straight back.
    std::fs::write(
        &claude_settings,
        r#"{ "statusLine": { "type": "command", "command": "cat" } }"#,
    )
    .unwrap();
    env.install_agent_hooks("claude");

    let payload = r#"{"session_id":"sess-1","model":{"id":"claude-opus-4-8"}}"#;
    let out = env.run_statusline_feed("claude", payload);
    assert!(
        out.status.success(),
        "feed stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        payload,
        "wrapped command's stdout must be forwarded verbatim"
    );
}

#[test]
fn cursor_statusline_and_stop_hook_merge_rich_context_and_idempotent_cost() {
    let env = Env::new();
    let config = env.cursor_cli_config_path();
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(
        &config,
        r#"{ "statusLine": { "type": "command", "command": "cat", "padding": 2 } }"#,
    )
    .unwrap();
    env.install_agent_hooks("cursor");

    let transcript = env
        .home_root
        .join(".cursor/projects/fixture/agent-transcripts/sess-cursor/sess-cursor.jsonl");
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(
        &transcript,
        "{\"type\":\"turn_ended\",\"status\":\"success\"}\n",
    )
    .unwrap();

    let payload = r#"{
        "session_id": "sess-cursor",
        "model": {
            "id": "default",
            "display_name": "GPT-5.6 Sol 272K Medium",
            "param_summary": "272K Medium",
            "max_mode": false
        },
        "context_window": {
            "context_window_size": 200000,
            "used_percentage": 8.9,
            "current_usage": {
                "input_tokens": 14021,
                "output_tokens": 26,
                "cache_read_input_tokens": 8704,
                "cache_creation_input_tokens": 0
            }
        }
    }"#;
    let out = env.run_statusline_feed("cursor", payload);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), payload);

    let out = env.run_statusline_feed("cursor", payload);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let settled = env
        .agent_contexts()
        .into_iter()
        .find(|record| record.agent_id.as_str() == "sess-cursor")
        .expect("Cursor statusline sidecar");
    assert!(
        settled.context.settle.is_some(),
        "continuous statusline refresh re-derives the transcript-tail settle marker"
    );

    let stop = r#"{
        "hook_event_name": "stop",
        "conversation_id": "sess-cursor",
        "generation_id": "gen-1",
        "status": "completed",
        "model_id": "default",
        "input_tokens": 22725,
        "output_tokens": 26,
        "cache_read_tokens": 8704,
        "cache_write_tokens": 0
    }"#;
    assert!(env.run_installed_hook("cursor", stop).status.success());
    assert!(env.run_installed_hook("cursor", stop).status.success());

    let record = env
        .agent_contexts()
        .into_iter()
        .find(|record| record.agent_id.as_str() == "sess-cursor")
        .expect("Cursor sidecar");
    assert_eq!(record.context.model_id.as_deref(), Some("auto"));
    assert_eq!(
        record.context.model_display_name.as_deref(),
        Some("GPT-5.6 Sol")
    );
    assert_eq!(record.context.effort.as_deref(), Some("medium"));
    let tokens = record.context.tokens.unwrap();
    assert_eq!(tokens.context_window_size, Some(200_000));
    assert_eq!(tokens.used_percentage, Some(9));
    assert_eq!(tokens.current_usage.unwrap().input_tokens, Some(14_021));
    let cost = record.context.cost.unwrap().total_cost_usd.unwrap();
    assert!((cost - 0.019_858_25).abs() < 1e-12, "{cost}");
}

/// With no wrapped command, the feed prints nothing (Claude falls back to its
/// built-in statusline), captures the per-session context sidecar, and folds it
/// onto the session row once lifecycle creates that row.
#[test]
fn statusline_feed_with_no_wrap_captures_context_and_folds_snapshot() {
    let env = Env::new();
    env.install_agent_hooks("claude");

    let payload = r#"{
        "session_id": "sess-ctx",
        "model": { "id": "claude-opus-4-8", "display_name": "Opus" },
        "context_window": { "used_percentage": 42 },
        "cost": { "total_cost_usd": 0.5 }
    }"#;
    let out = env.run_statusline_feed("claude", payload);
    assert!(out.status.success());
    assert!(
        out.stdout.is_empty(),
        "no wrap means empty stdout, got: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    let contexts = env.agent_contexts();
    assert_eq!(contexts.len(), 1, "the session's context was captured");
    let record = &contexts[0];
    assert_eq!(record.kind, "claude");
    assert_eq!(record.agent_id, "sess-ctx");
    assert_eq!(record.context.model_display_name.as_deref(), Some("Opus"));
    assert_eq!(
        record.context.tokens.as_ref().unwrap().used_percentage,
        Some(42)
    );

    let start = env.run_installed_hook(
        "claude",
        r#"{ "hook_event_name": "SessionStart", "session_id": "sess-ctx", "permission_mode": "default" }"#,
    );
    assert!(start.status.success());

    let snapshot = env.snapshot_json();
    let agents = snapshot["agents"].as_array().expect("agents array");
    let agent = agents
        .iter()
        .find(|a| a["agent_id"] == "sess-ctx")
        .expect("session agent present");
    assert_eq!(agent["context"]["model_display_name"], "Opus");
    assert_eq!(agent["context"]["tokens"]["used_percentage"], 42);
}

#[test]
fn qwen_statusline_and_hook_fold_into_snapshot() {
    let env = Env::new();
    env.install_agent_hooks("qwen");
    assert!(env.agent_hooks_installed("qwen"));

    let statusline = json!({
        "session_id": "sess-qwen-rewind",
        "version": "0.19.10",
        "model": {"display_name": "[DeepSeek] deepseek-v4-pro"},
        "context_window": {
            "context_window_size": 1000000,
            "used_percentage": 3.9,
            "current_usage": 38727
        },
        "metrics": {
            "models": {
                "qwen3-coder-plus": {
                    "tokens": {
                        "prompt": 10000,
                        "completion": 2000,
                        "total": 12000,
                        "cached": 7000,
                        "thoughts": 500
                    }
                }
            },
            "files": {"total_lines_added": 17, "total_lines_removed": 4}
        },
        "vim": {"mode": "NORMAL"}
    })
    .to_string();
    let out = env.run_statusline_feed("qwen", &statusline);
    assert!(
        out.status.success(),
        "Qwen statusline stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "unwrapped Qwen statusline is neutral"
    );

    let contexts = env.agent_contexts();
    assert_eq!(contexts.len(), 1);
    let context = &contexts[0];
    assert_eq!(context.agent_id, "sess-qwen-rewind");
    assert_eq!(context.context.agent_version.as_deref(), Some("0.19.10"));
    assert_eq!(
        context.context.model_display_name.as_deref(),
        Some("DeepSeek V4 Pro")
    );
    assert_eq!(context.context.vim_mode.as_deref(), Some("NORMAL"));
    let tokens = context.context.tokens.as_ref().unwrap();
    assert_eq!(tokens.context_window_size, Some(1_000_000));
    assert_eq!(tokens.used_percentage, Some(4));
    assert_eq!(tokens.current_context_tokens, Some(38_727));
    assert_eq!(tokens.current_usage, None);
    assert_eq!(tokens.session_usage, None);
    let cost = context.context.cost.as_ref().unwrap();
    assert!(cost.total_cost_usd.is_some_and(|usd| usd > 0.0));
    assert_eq!(cost.coverage, rimz::agents::CostCoverage::Session);
    assert_eq!(cost.total_lines_added, Some(17));
    assert_eq!(cost.total_lines_removed, Some(4));

    let transcript = env.project_root.join("qwen-rewound-session.jsonl");
    std::fs::write(
        &transcript,
        include_str!("../../src/agents/adapters/qwen/tests/fixtures/rewound-session.jsonl"),
    )
    .unwrap();
    let hook = json!({
        "hook_event_name": "SessionStart",
        "session_id": "sess-qwen-rewind",
        "source": "startup",
        "transcript_path": transcript
    })
    .to_string();
    let out = env.run_installed_hook("qwen", &hook);
    assert_hook_succeeded_neutral("qwen", out);

    let snapshot = env.snapshot_json();
    let agents = snapshot["agents"].as_array().expect("agents array");
    assert_eq!(
        agents.len(),
        1,
        "statusline and hook fold onto one Qwen row"
    );
    let agent = &agents[0];
    assert_eq!(agent["kind"], "qwen");
    assert_eq!(agent["agent_id"], "sess-qwen-rewind");
    assert_eq!(agent["model"], "qwen-active-final");
    assert_eq!(agent["total_tokens"], 555);
    assert_eq!(agent["context_window"], 333_333);
    assert_eq!(agent["cache_read_input_tokens"], 50);
    assert_eq!(agent["fresh_input_tokens"], 400);
    assert_eq!(agent["output_tokens"], 105);
    assert_eq!(agent["context"]["model_display_name"], "DeepSeek V4 Pro");
    assert_eq!(agent["context"]["tokens"]["context_window_size"], 1_000_000);
    assert_eq!(agent["context"]["tokens"]["used_percentage"], 4);
    assert_eq!(agent["context"]["tokens"]["current_context_tokens"], 38_727);
    assert!(agent["context"]["tokens"]["current_usage"].is_null());
    assert!(agent["context"]["tokens"]["session_usage"].is_null());
    assert!(
        agent["context"]["cost"]["total_cost_usd"]
            .as_f64()
            .is_some_and(|usd| usd > 0.0)
    );
    assert_eq!(agent["context"]["cost"]["total_lines_added"], 17);
    assert_eq!(agent["context"]["cost"]["total_lines_removed"], 4);
}

#[test]
fn statusline_feed_captures_claude_turn_interruption() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let transcript = env.project_root.join("session.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            "{\"type\":\"user\",\"timestamp\":\"2026-06-04T03:01:00.000Z\",",
            "\"message\":{\"content\":\"[Request interrupted by user]\"}}\n",
            "{\"type\":\"system\",\"subtype\":\"turn_duration\"}\n",
        ),
    )
    .unwrap();
    let payload = json!({
        "session_id": "sess-interrupted",
        "transcript_path": transcript,
    })
    .to_string();

    let out = env.run_statusline_feed("claude", &payload);
    assert!(
        out.status.success(),
        "feed stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let contexts = env.agent_contexts();
    let marker = contexts
        .iter()
        .find(|record| record.agent_id == "sess-interrupted")
        .and_then(|record| record.context.settle)
        .expect("statusline feed persists the transcript interruption marker");
    assert_eq!(marker.outcome, rimz::agents::TurnSettleOutcome::Interrupted);
    assert_eq!(
        marker.at,
        "2026-06-04T03:01:00Z".parse::<Timestamp>().unwrap()
    );
}

/// The `--subagent` feed harvests every task in a `subagentStatusLine` payload
/// into one per-child sidecar, keyed by the task id, and emits nothing when no
/// wrap is configured (Claude renders its own child rows).
#[test]
fn subagent_statusline_feed_writes_one_sidecar_per_task() {
    let env = Env::new();
    env.install_agent_hooks("claude");

    let payload = r#"{
        "columns": 80,
        "tasks": [
            {
                "id": "child-1",
                "type": "Explore",
                "status": "running",
                "description": "locate the render seam",
                "startTime": 1700000000,
                "tokenCount": 12400
            },
            {
                "id": "child-2",
                "type": "review",
                "description": "audit the trust hash",
                "startTime": 1700000055,
                "tokenCount": 3100
            }
        ]
    }"#;
    let out = env.run_subagent_statusline_feed("claude", payload);
    assert!(
        out.status.success(),
        "feed stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "no wrap means empty stdout, got: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    let mut records = env.subagent_contexts();
    records.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
    assert_eq!(records.len(), 2, "one sidecar per task");
    assert_eq!(records[0].agent_id, "child-1");
    assert_eq!(
        records[0].context.description.as_deref(),
        Some("locate the render seam")
    );
    assert_eq!(records[0].context.token_count, Some(12_400));
    assert!(records[0].context.started_at.is_some());
    assert_eq!(records[1].agent_id, "child-2");
    assert_eq!(records[1].context.token_count, Some(3_100));
}

/// Build the `rimz hooks feed --source codex` command with `RIMZ_CODEX_BIN`
/// pointed at `codex_bin`, mirroring an installed hook. The detached
/// `rimz agents refresh-context` child inherits this env, so it spawns
/// `codex_bin app-server` for its read-only enrichment.
fn codex_hook_with_app_server(env: &Env, codex_bin: &std::path::Path) -> Command {
    let mut cmd = env.rimz();
    cmd.args(["hooks", "feed", "--source", "codex"])
        .env("RIMZ_AGENT_PID", std::process::id().to_string())
        .env("RIMZ_CODEX_BIN", codex_bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// Absolute path to the built `codex app-server` stub fixture.
fn codex_appserver_stub() -> std::path::PathBuf {
    crate::common::cargo_bin(
        "codex-appserver-stub",
        env!("CARGO_BIN_EXE_codex-appserver-stub"),
    )
}

/// A Codex turn boundary spawns a detached refresh that reads the app-server
/// (here, a stub) and writes the session's context sidecar with the rich
/// details Claude gets from its statusline: rate-limit windows, model display
/// name, and version. The context gauge (`tokens`) stays `None` — the
/// app-server exposes no read-only token usage, so that stays rollout-sourced.
#[test]
fn codex_turn_boundary_refreshes_context_sidecar_from_app_server() {
    let env = Env::new();
    let payload = serde_json::to_string(&json!({
        "hook_event_name": "SessionStart",
        "session_id": "sess-codex-rt",
        "approval_policy": "ask",
        "model": "gpt-5.5-codex",
    }))
    .expect("payload");

    let cmd = codex_hook_with_app_server(&env, &codex_appserver_stub());
    let out = env
        .spawn_payload(cmd, &payload)
        .wait_with_output()
        .expect("wait hook");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty(), "lifecycle hook is silent");

    // The refresh is detached, so the sidecar lands after the hook returns.
    let deadline = Instant::now() + Duration::from_secs(10);
    let record = loop {
        if let Some(record) = env
            .agent_contexts()
            .into_iter()
            .find(|record| record.agent_id == "sess-codex-rt")
        {
            break record;
        }
        assert!(
            Instant::now() < deadline,
            "codex context sidecar was never written"
        );
        std::thread::sleep(Duration::from_millis(50));
    };

    assert_eq!(record.kind, "codex");
    assert_eq!(record.context.source, "codex");
    let limits = record.context.rate_limits.expect("rate limits present");
    // Wire order preserved: primary (300 min) then secondary (10080 min).
    assert_eq!(limits.windows[0].duration_mins, Some(300));
    assert_eq!(limits.windows[0].used_percentage, Some(42));
    assert_eq!(limits.windows[1].duration_mins, Some(10080));
    assert_eq!(limits.windows[1].used_percentage, Some(7));
    assert_eq!(
        record.context.model_display_name.as_deref(),
        Some("GPT-5.5 Codex")
    );
    assert_eq!(
        record.context.effort, None,
        "model/list defaultReasoningEffort is not the session's actual effort"
    );
    assert_eq!(record.context.agent_version.as_deref(), Some("9.9.9"));
    assert!(
        record.context.tokens.is_none(),
        "no read-only token source for Codex — the gauge stays rollout-sourced"
    );
}

#[test]
fn codex_stop_over_error_rollout_writes_turn_error_sidecar() {
    let env = Env::new();
    let session_id = "sess-codex-error";
    let sessions = env.home_root.join("codex-sessions");
    let day = sessions.join("2026").join("06").join("11");
    std::fs::create_dir_all(&day).expect("mkdir codex sessions");
    std::fs::write(
        day.join(format!("rollout-2026-06-11T07-18-00-{session_id}.jsonl")),
        json!({
            "timestamp": "2026-06-11T07:18:00.000Z",
            "type": "event_msg",
            "payload": {
                "type": "turn_error",
                "message": "You've hit your usage limit",
                "codexErrorInfo": "usageLimitExceeded"
            }
        })
        .to_string()
            + "\n",
    )
    .expect("write rollout");

    let payload = serde_json::to_string(&json!({
        "hook_event_name": "Stop",
        "session_id": session_id,
        "model": "gpt-5.5-codex",
    }))
    .expect("payload");
    let mut cmd = env.hook_command("codex");
    cmd.env("RIMZ_CODEX_SESSIONS", &sessions)
        .env("RIMZ_CODEX_BIN", "/nonexistent/codex-binary-xyz");
    let out = env
        .spawn_payload(cmd, &payload)
        .wait_with_output()
        .expect("wait hook");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty(), "lifecycle hook is silent");

    let record = env
        .agent_contexts()
        .into_iter()
        .find(|record| record.agent_id == session_id)
        .expect("turn-error sidecar");
    let marker = record.context.turn_error.expect("turn-error marker");
    assert_eq!(marker.class, rimz::agents::TurnErrorClass::PausedRateLimit);
    assert_eq!(
        marker.at,
        "2026-06-11T07:18:00.000Z".parse::<Timestamp>().unwrap()
    );
    assert_eq!(marker.label.as_deref(), Some("You've hit your usage limit"));

    let snapshot = env.snapshot_json();
    let agent = snapshot["agents"]
        .as_array()
        .expect("agents array")
        .iter()
        .find(|agent| agent["agent_id"].as_str() == Some(session_id))
        .expect("codex agent in snapshot");
    assert_eq!(
        agent["status"].as_str(),
        Some("failed"),
        "Stop over rollout turn_error must not reduce as success"
    );
}

/// The sidebar's idle/account refresh path calls the uniform hidden
/// `agents refresh-usage --kind codex` helper, which reads codex's app-server
/// (its realtime channel, pollable while idle) and merges the windows into the
/// shared provider cache.
#[test]
fn codex_rate_limit_refresh_merges_account_cache_from_app_server() {
    let env = Env::new();
    let claim_id = env.seed_usage_claim("codex");
    let out = env
        .rimz()
        .env("RIMZ_CODEX_BIN", codex_appserver_stub())
        .args([
            "agents",
            "refresh-usage",
            "--kind",
            "codex",
            "--workspace-id",
            env.workspace_id.as_str(),
            "--claim-id",
            &claim_id,
        ])
        .output()
        .expect("spawn agents refresh-usage codex");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let cache_path = env.runtime_paths().shared_rate_limits_path();
    let cache: Value = serde_json::from_slice(&std::fs::read(cache_path).expect("rate cache"))
        .expect("rate cache json");
    assert_eq!(
        cache["entries"]["codex"]["limits"]["windows"][0]["used_percentage"], 42,
        "the short window comes from the app-server primary window"
    );
    assert_eq!(
        cache["entries"]["codex"]["limits"]["windows"][1]["used_percentage"], 7,
        "the long window comes from the app-server secondary window"
    );
    let credits_path = env.runtime_paths().shared_credits_path();
    let credits: Value =
        serde_json::from_slice(&std::fs::read(credits_path).expect("credits cache"))
            .expect("credits cache json");
    assert_eq!(
        credits["entries"]["codex"]["extra_credits"]["known"]["remaining_usd"], 18.5,
        "the app-server credits balance lands in the shared credits cache"
    );
    assert_eq!(
        credits["entries"]["codex"]["plan"], "team",
        "the app-server plan remains available after the session goes idle"
    );
}
