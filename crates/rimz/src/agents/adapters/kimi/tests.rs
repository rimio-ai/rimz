use std::path::Path;

use serde_json::json;

use super::*;
use crate::agents::AgentHookClass;
use crate::agents::testkit::{hook_lifecycle, hook_observation, hook_output, hook_signal};

#[test]
fn native_questions_and_permissions_use_distinct_hooks() {
    let question = json!({
        "session_id": "s1",
        "tool_name": "AskUserQuestion",
        "tool_input": {"questions":[{"question":"Ship it?","options":[],"multi_select":false}]}
    });
    let classified = hook_output(&KimiAdapter, "PreToolUse", &question);
    assert_eq!(classified.class(), AgentHookClass::AwaitingUser);
    assert_eq!(classified.ask_kind(), Some(super::super::AskKind::Question));

    let plan_pre_tool = json!({"session_id":"s1","tool_name":"ExitPlanMode"});
    assert_eq!(
        hook_output(&KimiAdapter, "PreToolUse", &plan_pre_tool).class(),
        AgentHookClass::Lifecycle
    );
    assert!(hook_observation(&KimiAdapter, "PreToolUse", &plan_pre_tool).is_none());

    let plan_permission = json!({
        "session_id":"s1",
        "tool_call_id":"t1",
        "tool_name":"ExitPlanMode",
        "action":"Exit plan mode"
    });
    assert!(matches!(
        hook_signal(&KimiAdapter, "PermissionRequest", &plan_permission),
        LifecycleSignal::AwaitingInput {
            kind: super::super::AskKind::PlanApproval,
            ..
        }
    ));

    let permission = json!({"session_id":"s1","tool_name":"Bash","action":"Run tests"});
    assert!(matches!(
        hook_signal(&KimiAdapter, "PermissionRequest", &permission),
        LifecycleSignal::AwaitingInput {
            kind: super::super::AskKind::Permission,
            ..
        }
    ));
    insta::assert_snapshot!(format!("{:?}", hook_output(&KimiAdapter, "PermissionRequest", &Value::Null).json_reply().cloned()), @"None");
}

#[test]
fn permission_result_and_interrupt_clear_waiting_state() {
    let result = hook_lifecycle(
        &KimiAdapter,
        "PermissionResult",
        &json!({"session_id":"s1","tool_name":"Bash","decision":"approved"}),
    );
    assert_eq!(
        result.signal,
        LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
            name: None,
            native_key: None,
        }
    );
    assert!(matches!(
        hook_signal(
            &KimiAdapter,
            "Interrupt",
            &json!({"session_id":"s1","turn_id":"t1","reason":"cancelled"})
        ),
        LifecycleSignal::TurnEnded { errored: false, .. }
    ));
}

#[test]
fn failed_tools_clear_waits_and_background_questions_do_not_open_them() {
    let failed = hook_lifecycle(
        &KimiAdapter,
        "PostToolUseFailure",
        &json!({"session_id":"s1","tool_name":"AskUserQuestion"}),
    );
    assert_eq!(
        failed.signal,
        LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
            name: None,
            native_key: None,
        }
    );

    let background = json!({
        "session_id":"s1",
        "tool_name":"AskUserQuestion",
        "tool_input":{"background":true,"questions":[{"question":"Ship it?"}]}
    });
    assert_eq!(
        hook_output(&KimiAdapter, "PreToolUse", &background).class(),
        AgentHookClass::Lifecycle
    );
    assert!(hook_observation(&KimiAdapter, "PreToolUse", &background).is_none());
}

#[test]
fn prompt_parts_flags_tools_and_resume_match_kimi_code() {
    let prompt = json!({
        "session_id":"s1",
        "cwd":"/tmp/project",
        "prompt":[{"type":"text","text":"fix"},{"type":"image","url":"x"},{"type":"text","text":"parser"}]
    });
    let observed = hook_lifecycle(&KimiAdapter, "UserPromptSubmit", &prompt);
    assert_eq!(observed.prompt.as_deref(), Some("fix\nparser"));
    assert_eq!(
        KimiAdapter
            .spec()
            .launch
            .permission_args(PermissionMode::Auto),
        ["--auto"]
    );
    assert_eq!(
        KimiAdapter
            .spec()
            .launch
            .permission_args(PermissionMode::Yolo),
        ["--yolo"]
    );
    assert_eq!(
        KimiAdapter.resume_command("s1", Path::new("/tmp")).unwrap(),
        ["kimi", "--session", "s1"]
    );
    assert_eq!(
        KimiAdapter
            .launch_command(&["--yolo".to_owned()], Some("review"))
            .unwrap(),
        [
            "kimi",
            "--prompt",
            "review",
            "--output-format",
            "stream-json"
        ]
    );
    assert_eq!(
        KimiAdapter.launch_command(&["--yolo".to_owned()], Some("")),
        Some(vec!["kimi".to_owned()])
    );
    assert_eq!(
        KimiAdapter.spec().launch.compact_command(),
        Some("/compact")
    );

    let write = hook_lifecycle(
        &KimiAdapter,
        "PostToolUse",
        &json!({"session_id":"s1","tool_name":"Write"}),
    );
    assert_eq!(
        write.signal,
        LifecycleSignal::ToolUsed {
            mutates: true,
            edits: true,
            name: None,
            native_key: None,
        }
    );
    assert_eq!(KimiAdapter.spec().process_names, ["kimi", "kimi-code"]);
    assert_eq!(KimiAdapter.spec().extra_bin_dirs, [".kimi-code/bin"]);
}

#[test]
fn install_merge_includes_native_permission_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "default_model = \"custom\"\n\n[[hooks]]\nevent = \"SessionStart\"\ncommand = \"my-hook\"\n",
    )
    .unwrap();
    install::install(&path).unwrap();
    assert!(install::installed(&path));
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("PermissionRequest"));
    assert!(text.contains("PermissionResult"));
    assert!(text.contains("Interrupt"));
    assert!(text.contains("my-hook"));
}

#[test]
fn session_index_resolves_valid_main_wire_and_rejects_escape() {
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("sessions/wd_project/s1");
    std::fs::create_dir_all(session.join("agents/main")).unwrap();
    std::fs::write(
        session.join("state.json"),
        r#"{"workDir":"/tmp/project","agents":{}}"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("session_index.jsonl"),
        format!(
            "{{\"sessionId\":\"s1\",\"sessionDir\":{},\"workDir\":\"/tmp/project\"}}\n{{\"sessionId\":\"s1\",\"sessionDir\":\"/tmp\",\"workDir\":\"/tmp/project\"}}\n",
            serde_json::to_string(&session).unwrap()
        ),
    )
    .unwrap();
    assert_eq!(
        wire::session_dir_under(dir.path(), "s1", Some(Path::new("/tmp/project"))).as_deref(),
        Some(std::fs::canonicalize(&session).unwrap().as_path())
    );
    assert_eq!(
        wire::session_dir_under(dir.path(), "s1", Some(Path::new("/other"))),
        None
    );
}

#[test]
fn subagent_start_join_matches_unique_and_swarm_children() {
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("session-1");
    std::fs::create_dir_all(session.join("agents/main")).unwrap();
    let first = session.join("agents/agent-0");
    std::fs::write(
        session.join("state.json"),
        serde_json::to_vec(&json!({
            "agents": {
                "main": {"homedir": session.join("agents/main"), "type": "main", "parentAgentId": null},
                "agent-0": {"homedir": first, "type": "sub", "parentAgentId": "main", "swarmItem": "parser"}
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let matched = subagents::resolve_start(&session, Some("inspect the parser")).unwrap();
    assert_eq!(matched.id, "agent-0");
    assert_eq!(matched.task.as_deref(), Some("inspect the parser"));

    let second = session.join("agents/agent-1");
    std::fs::write(
        session.join("state.json"),
        serde_json::to_vec(&json!({
            "agents": {
                "agent-0": {"homedir": first, "type": "sub", "parentAgentId": "main", "swarmItem": "parser"},
                "agent-1": {"homedir": second, "type": "sub", "parentAgentId": "main", "swarmItem": "renderer"}
            }
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        subagents::resolve_start(&session, Some("inspect the renderer"))
            .unwrap()
            .id,
        "agent-1"
    );
    assert!(subagents::resolve_start(&session, Some("inspect code")).is_none());
}

#[test]
fn subagent_start_join_retries_the_queued_state_write() {
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("session-1");
    std::fs::create_dir_all(session.join("agents/main")).unwrap();
    std::fs::write(session.join("state.json"), r#"{"agents":{}}"#).unwrap();
    let write_session = session.clone();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(15));
        let child = write_session.join("agents/agent-0");
        std::fs::write(
            write_session.join("state.json"),
            serde_json::to_vec(&json!({
                "agents": {
                    "agent-0": {"homedir": child, "type": "sub", "parentAgentId": "main"}
                }
            }))
            .unwrap(),
        )
        .unwrap();
    });

    assert_eq!(
        subagents::resolve_start(&session, Some("inspect parser"))
            .unwrap()
            .id,
        "agent-0"
    );
    writer.join().unwrap();
}

#[test]
fn subagent_stop_join_requires_a_unique_response_match() {
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("session-1");
    let first = session.join("agents/agent-0");
    let second = session.join("agents/agent-1");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    std::fs::write(
        session.join("state.json"),
        serde_json::to_vec(&json!({
            "agents": {
                "agent-0": {"homedir": first, "type": "sub", "parentAgentId": "main"},
                "agent-1": {"homedir": second, "type": "sub", "parentAgentId": "main"}
            }
        }))
        .unwrap(),
    )
    .unwrap();
    write_child_wire(
        &first.join("wire.jsonl"),
        "inspect parser",
        "Done: parser",
        "explore",
    );
    write_child_wire(
        &second.join("wire.jsonl"),
        "inspect renderer",
        "Done: renderer",
        "coder",
    );

    let matched = subagents::resolve_stop(&session, Some("Done: renderer")).unwrap();
    assert_eq!(matched.id, "agent-1");
    assert_eq!(matched.task.as_deref(), Some("inspect renderer"));
    assert_eq!(matched.profile.as_deref(), Some("coder"));
    assert_eq!(matched.model.as_deref(), Some("deepseek-v4-pro"));
    assert_eq!(matched.effort.as_deref(), Some("high"));
    assert!(subagents::resolve_stop(&session, Some("Done:")).is_none());
    assert!(subagents::resolve_stop(&session, None).is_none());

    std::fs::write(
        session.join("state.json"),
        serde_json::to_vec(&json!({
            "agents": {
                "agent-0": {"homedir": first, "type": "sub", "parentAgentId": "main"}
            }
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        subagents::resolve_stop(&session, Some(" ")).unwrap().id,
        "agent-0"
    );
}

#[test]
fn main_turn_mid_step_is_fail_open_and_closes_at_step_end() {
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("session-1");
    let main = session.join("agents/main/wire.jsonl");
    std::fs::create_dir_all(main.parent().unwrap()).unwrap();
    assert!(!subagents::main_turn_mid_step(&session));
    std::fs::write(
        &main,
        "{\"type\":\"llm.request\",\"time\":1,\"kind\":\"loop\"}\n",
    )
    .unwrap();
    assert!(subagents::main_turn_mid_step(&session));
    std::fs::write(
        &main,
        concat!(
            "{\"type\":\"llm.request\",\"time\":1,\"kind\":\"loop\"}\n",
            "{\"type\":\"context.append_loop_event\",\"time\":2,\"event\":{\"type\":\"step.end\",\"uuid\":\"s1\"}}\n"
        ),
    )
    .unwrap();
    assert!(!subagents::main_turn_mid_step(&session));
}

#[test]
fn main_turn_mid_step_waits_for_a_queued_step_end_write() {
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("session-1");
    let main = session.join("agents/main/wire.jsonl");
    std::fs::create_dir_all(main.parent().unwrap()).unwrap();
    std::fs::write(
        &main,
        "{\"type\":\"llm.request\",\"time\":1,\"kind\":\"loop\"}\n",
    )
    .unwrap();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(15));
        use std::io::Write;
        writeln!(
            std::fs::OpenOptions::new()
                .append(true)
                .open(main)
                .unwrap(),
            "{{\"type\":\"context.append_loop_event\",\"time\":2,\"event\":{{\"type\":\"step.end\",\"uuid\":\"s1\"}}}}"
        )
        .unwrap();
    });

    assert!(!subagents::main_turn_mid_step(&session));
    writer.join().unwrap();
}

#[test]
fn subagent_observations_namespace_identity_and_keep_the_parent_link() {
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("session-1");
    let child = session.join("agents/agent-0");
    std::fs::create_dir_all(session.join("agents/main")).unwrap();
    std::fs::write(
        session.join("state.json"),
        serde_json::to_vec(&json!({
            "agents": {
                "agent-0": {"homedir": child, "type": "sub", "parentAgentId": "main"}
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let start_payload = json!({
        "session_id": "session-1",
        "cwd": "/workspace",
        "agent_name": "explore",
        "prompt": "trace the parser"
    });
    let mut start = KimiAdapter
        .observe_subagent_lifecycle(
            "SubagentStart",
            &start_payload,
            &payloads::parse(&start_payload),
            &session,
        )
        .unwrap();
    start.transcript_path = Some("[session]/agents/agent-0/wire.jsonl".to_owned());
    insta::assert_debug_snapshot!(start, @r###"
    AgentLifecycleObservation {
        agent_id: Some(
            AgentSessionId(
                "session-1:agent-0",
            ),
        ),
        agent_name: Some(
            "explore",
        ),
        launch: LaunchParams {
            profile: None,
            mode: None,
            role: None,
            model: None,
            effort: None,
            budget: None,
            team: None,
            launch_group: None,
            launch_ordinal: None,
            channel: None,
            kind_ordinal: None,
        },
        signal: SubagentStarted,
        agent_pid: None,
        agent_process_start: None,
        runtime_owner: None,
        worktree_path: Some(
            "/workspace",
        ),
        worktree_branch: None,
        task: Some(
            "trace the parser",
        ),
        prompt: Some(
            "trace the parser",
        ),
        description: None,
        transcript_path: Some(
            "[session]/agents/agent-0/wire.jsonl",
        ),
        origin: None,
        usage: AgentUsageSummary {
            context_pct: None,
            context_window: None,
            total_tokens: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
        },
        pane_id: None,
        pane_stamp: None,
        parent_agent_id: Some(
            AgentSessionId(
                "session-1",
            ),
        ),
    }
    "###);

    std::fs::create_dir_all(&child).unwrap();
    write_child_wire(
        &child.join("wire.jsonl"),
        "trace the parser",
        "Parser traced",
        "explore",
    );
    use std::io::Write as _;
    writeln!(
        std::fs::OpenOptions::new()
            .append(true)
            .open(child.join("wire.jsonl"))
            .unwrap(),
        "{{\"type\":\"llm.request\",\"modelAlias\":\"kimi-code/kimi-k2.5\",\"thinkingEffort\":\"xhigh\"}}"
    )
    .unwrap();
    let stop_payload = json!({
        "session_id": "session-1",
        "agent_name": "explore",
        "response": "Parser traced"
    });
    let stop = KimiAdapter
        .observe_subagent_lifecycle(
            "SubagentStop",
            &stop_payload,
            &payloads::parse(&stop_payload),
            &session,
        )
        .unwrap();
    assert_eq!(stop.agent_id.as_deref(), Some("session-1:agent-0"));
    assert_eq!(stop.parent_agent_id.as_deref(), Some("session-1"));
    assert_eq!(stop.task.as_deref(), Some("trace the parser"));
    assert_eq!(stop.launch.model.as_deref(), Some("kimi-k2.5"));
    assert_eq!(stop.launch.effort.as_deref(), Some("xhigh"));
    assert_eq!(
        stop.signal,
        LifecycleSignal::SubagentStopped { errored: false }
    );
}

#[test]
fn subagent_start_observation_carries_configured_attribution() {
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("session-1");
    let child = session.join("agents/agent-0");
    std::fs::create_dir_all(&child).unwrap();
    std::fs::create_dir_all(session.join("agents/main")).unwrap();
    std::fs::write(
        session.join("state.json"),
        serde_json::to_vec(&json!({
            "agents": {
                "agent-0": {"homedir": child, "type": "sub", "parentAgentId": "main"}
            }
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        child.join("wire.jsonl"),
        "{\"type\":\"config.update\",\"modelAlias\":\"kimi-code/deepseek-v4-pro\",\"thinkingEffort\":\"high\"}\n",
    )
    .unwrap();
    let payload = json!({
        "session_id": "session-1",
        "agent_name": "explore",
        "prompt": "trace attribution"
    });

    let observation = KimiAdapter
        .observe_subagent_lifecycle(
            "SubagentStart",
            &payload,
            &payloads::parse(&payload),
            &session,
        )
        .unwrap();
    assert_eq!(observation.launch.model.as_deref(), Some("deepseek-v4-pro"));
    assert_eq!(observation.launch.effort.as_deref(), Some("high"));
}

fn write_child_wire(path: &Path, prompt: &str, response: &str, profile: &str) {
    std::fs::write(
        path,
        format!(
            "{{\"type\":\"config.update\",\"profileName\":{profile},\"modelAlias\":\"kimi-code/deepseek-v4-pro\",\"thinkingEffort\":\"high\"}}\n{{\"type\":\"turn.prompt\",\"input\":[{{\"type\":\"text\",\"text\":{prompt}}}],\"origin\":{{\"kind\":\"system_trigger\"}}}}\n{{\"type\":\"context.append_loop_event\",\"event\":{{\"type\":\"content.part\",\"stepUuid\":\"s1\",\"part\":{{\"type\":\"text\",\"text\":{response}}}}}}}\n{{\"type\":\"context.append_loop_event\",\"event\":{{\"type\":\"step.end\",\"uuid\":\"s1\"}}}}\n",
            profile = serde_json::to_string(profile).unwrap(),
            prompt = serde_json::to_string(prompt).unwrap(),
            response = serde_json::to_string(response).unwrap(),
        ),
    )
    .unwrap();
}

#[test]
fn refresh_publishes_the_state_title_as_session_preview() {
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("s1");
    let path = session.join("agents/main/wire.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{\"type\":\"metadata\"}\n").unwrap();
    std::fs::write(session.join("state.json"), r#"{"title":"  Stable task  "}"#).unwrap();
    let cache = dir.path().join("prices.json");
    let ctx = LocalContextRefreshCtx {
        agent_id: "s1",
        model_hint: None,
        current_transcript_path: None,
        prior_transcript_path: None,
        prior_transcript_stat: None,
        prior_spend_fold: None,
        shared_pricing_cache_path: &cache,
    };

    let refresh =
        refresh_wire_path(&path, "s1", TranscriptStat::from_path(&path).unwrap(), &ctx).unwrap();
    assert_eq!(
        refresh.context.session_preview.as_set().map(String::as_str),
        Some("Stable task")
    );

    std::fs::write(session.join("state.json"), r#"{"title":"  "}"#).unwrap();
    assert_eq!(subagents::session_title(&session), None);
    std::fs::remove_file(session.join("state.json")).unwrap();
    assert_eq!(subagents::session_title(&session), None);
}

#[test]
fn session_title_drops_kimis_pre_prompt_placeholder() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("state.json"), r#"{"title":"New Session"}"#).unwrap();

    assert_eq!(subagents::session_title(dir.path()), None);
}

#[test]
fn usage_records_drive_context_spend_and_additive_scopes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions/wd/s1/agents/main/wire.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        concat!(
            "{\"type\":\"metadata\",\"protocol_version\":\"1.4\"}\n",
            "{\"type\":\"llm.request\",\"time\":1769999999000,\"provider\":\"moonshot\",\"model\":\"kimi-k2.5\",\"modelAlias\":\"kimi-code/kimi-for-coding\",\"thinkingEffort\":\"high\"}\n",
            "{\"type\":\"usage.record\",\"time\":1770000000000,\"model\":\"moonshot/kimi-k2.5\",\"usageScope\":\"session\",\"usage\":{\"inputOther\":999}}\n",
            "{\"type\":\"usage.record\",\"time\":1770000001000,\"model\":\"moonshot/kimi-k2.5\",\"usageScope\":\"turn\",\"usage\":{\"inputOther\":40000,\"output\":20,\"inputCacheRead\":10000,\"inputCacheCreation\":30}}\n",
            "{\"type\":\"context.append_loop_event\",\"time\":1770000001001,\"event\":{\"type\":\"step.end\",\"uuid\":\"step-1\",\"usage\":{\"inputOther\":40000,\"output\":20,\"inputCacheRead\":10000,\"inputCacheCreation\":30}}}\n",
            "{\"type\":\"usage.record\",\"time\":1770000001002,\"model\":\"moonshot/kimi-k2.5\",\"usageScope\":\"session\",\"usage\":{\"inputOther\":5}}\n"
        ),
    )
    .unwrap();
    let records = wire::records_from_bytes(&std::fs::read(&path).unwrap());
    assert_eq!(wire::usage_records(&records).len(), 3);
    let stat = TranscriptStat::from_path(&path).unwrap();
    let cache = dir.path().join("prices.json");
    let ctx = LocalContextRefreshCtx {
        agent_id: "s1",
        model_hint: None,
        current_transcript_path: None,
        prior_transcript_path: None,
        prior_transcript_stat: None,
        prior_spend_fold: None,
        shared_pricing_cache_path: &cache,
    };
    let refresh = refresh_wire_path(&path, "s1", stat, &ctx).unwrap();
    let tokens = refresh.context.tokens.clone().into_value().unwrap();
    assert_eq!(tokens.used_percentage, Some(19));
    assert_eq!(
        tokens.current_usage.unwrap().cache_read_input_tokens,
        Some(10_000)
    );
    assert_eq!(
        refresh.context.model_id.as_set().map(String::as_str),
        Some("kimi-for-coding")
    );
    assert_eq!(
        refresh.context.effort.as_set().map(String::as_str),
        Some("high")
    );
    assert!(
        refresh
            .context
            .cost
            .into_set()
            .unwrap()
            .total_cost_usd
            .unwrap()
            > 0.0
    );
    assert_eq!(refresh.transcript_path.as_deref(), path.to_str());

    let parsed = spend::parse(&path, None, &super::super::PriceBook::embedded());
    let snapshot = wire::WireSnapshot::read(&path).unwrap();
    let snapshot_parsed =
        spend::parse_snapshot(&path, &snapshot, &super::super::PriceBook::embedded());
    assert_eq!(snapshot_parsed.entries, parsed.entries);
    assert_eq!(
        snapshot.consumed_offset(),
        std::fs::metadata(&path).unwrap().len()
    );
    assert_eq!(parsed.entries.len(), 3);
    assert_eq!(
        parsed.entries[1].model.as_deref(),
        Some("moonshot/kimi-k2.5")
    );
    assert_eq!(parsed.entries[1].thread_id.as_deref(), Some("s1"));
    assert!(parsed.entries.iter().all(|entry| entry.cost_usd > 0.0));
}

#[test]
fn wire_snapshot_tail_is_record_aligned_oversized_and_torn_safe() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wire.jsonl");
    let usage = |time: u64, padding: usize| {
        serde_json::json!({
            "type": "usage.record",
            "time": time,
            "model": "moonshot/kimi-k2.5",
            "usageScope": "turn",
            "usage": { "inputOther": time },
            "padding": "x".repeat(padding),
        })
        .to_string()
    };
    let early = usage(1, 0);
    let oversized = usage(2, 70_000);
    let complete = format!("{early}\n{oversized}\n");
    std::fs::write(&path, format!("{complete}{{\"type\":\"usage.record\"")).unwrap();

    let snapshot = wire::WireSnapshot::read(&path).unwrap();
    let tail_usage = wire::usage_records(snapshot.tail_records());
    assert_eq!(tail_usage.len(), 1);
    assert_eq!(tail_usage[0].1.usage.input_other, Some(2));
    assert_eq!(snapshot.consumed_offset(), complete.len() as u64);
    assert_eq!(wire::usage_records(snapshot.records()).len(), 2);

    let latest = usage(3, 0);
    let complete = format!("{early}\n{oversized}\n{latest}\n");
    std::fs::write(&path, format!("{complete}{{\"type\":\"usage.record\"")).unwrap();
    let snapshot = wire::WireSnapshot::read(&path).unwrap();
    let tail_usage = wire::usage_records(snapshot.tail_records());
    assert_eq!(tail_usage.len(), 1);
    assert_eq!(tail_usage[0].1.usage.input_other, Some(3));
    assert_eq!(snapshot.consumed_offset(), complete.len() as u64);
    assert_eq!(wire::usage_records(snapshot.records()).len(), 3);
}

#[test]
fn compaction_and_effective_model_config_drive_context() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(
        &config,
        concat!(
            "default_model = \"small\"\n",
            "[models.small]\nmax_context_size = 64000\n",
            "[models.large]\nmax_context_size = 128000\n",
            "[models.large.overrides]\nmax_context_size = 96000\n",
        ),
    )
    .unwrap();
    assert_eq!(
        configured_context_window_at(&config, Some("large")),
        Some(96_000)
    );

    let records = wire::records_from_bytes(
        concat!(
            "{\"type\":\"config.update\",\"time\":1,\"modelAlias\":\"large\",\"thinkingEffort\":\"high\"}\n",
            "{\"type\":\"config.update\",\"time\":1.5,\"thinkingEffort\":\"low\"}\n",
            "{\"type\":\"context.append_loop_event\",\"time\":2,\"event\":{\"type\":\"step.end\",\"uuid\":\"a\",\"usage\":{\"inputOther\":50000,\"inputCacheRead\":30000,\"inputCacheCreation\":5000,\"output\":5000}}}\n",
            "{\"type\":\"context.append_loop_event\",\"time\":3,\"event\":{\"type\":\"step.end\",\"uuid\":\"b\",\"usage\":{}}}\n",
            "{\"type\":\"context.clear\",\"time\":4}\n",
            "{\"type\":\"context.append_loop_event\",\"time\":5,\"event\":{\"type\":\"step.end\",\"uuid\":\"c\",\"usage\":{\"inputOther\":20000}}}\n",
            "{\"type\":\"context.apply_compaction\",\"time\":6,\"tokensBefore\":20000,\"tokensAfter\":12000}\n",
        )
        .as_bytes(),
    );
    let attribution = wire::effective_attribution(&records);
    assert_eq!(attribution.display_model().as_deref(), Some("large"));
    assert_eq!(attribution.thinking_effort.as_deref(), Some("low"));
    assert_eq!(wire::latest_context_tokens(&records), Some(12_000));
}

#[test]
fn wire_without_usage_emits_fresh_sentinel() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wire.jsonl");
    std::fs::write(
        &path,
        "{\"type\":\"context.append_loop_event\",\"time\":1,\"event\":{\"type\":\"tool.result\"}}\n",
    )
    .unwrap();
    let stat = TranscriptStat::from_path(&path).unwrap();
    let cache = dir.path().join("prices.json");
    let ctx = LocalContextRefreshCtx {
        agent_id: "s1",
        model_hint: None,
        current_transcript_path: None,
        prior_transcript_path: None,
        prior_transcript_stat: None,
        prior_spend_fold: None,
        shared_pricing_cache_path: &cache,
    };
    let tokens = refresh_wire_path(&path, "s1", stat, &ctx)
        .unwrap()
        .context
        .tokens
        .into_value()
        .unwrap();
    assert_eq!(tokens.used_percentage, None);
    assert!(tokens.current_usage.unwrap().is_zero());
}

#[test]
fn current_wire_reconstructs_visible_conversation_without_duplicate_prompts() {
    let lines = concat!(
        "{\"type\":\"turn.prompt\",\"time\":1770000000000,\"input\":[{\"type\":\"text\",\"text\":\"hello\"}],\"origin\":{\"kind\":\"user\"}}\n",
        "{\"type\":\"context.append_message\",\"time\":1770000000001,\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n",
        "{\"type\":\"context.append_loop_event\",\"time\":1770000000100,\"event\":{\"type\":\"step.begin\",\"uuid\":\"s1\"}}\n",
        "{\"type\":\"context.append_loop_event\",\"time\":1770000000200,\"event\":{\"type\":\"content.part\",\"uuid\":\"p1\",\"stepUuid\":\"s1\",\"part\":{\"type\":\"think\",\"think\":\"secret\"}}}\n",
        "{\"type\":\"context.append_loop_event\",\"time\":1770000000300,\"event\":{\"type\":\"content.part\",\"uuid\":\"p2\",\"stepUuid\":\"s1\",\"part\":{\"type\":\"text\",\"text\":\"first\"}}}\n",
        "{\"type\":\"context.append_loop_event\",\"time\":1770000000400,\"event\":{\"type\":\"tool.call\",\"stepUuid\":\"s1\",\"name\":\"Bash\"}}\n",
        "{\"type\":\"context.append_loop_event\",\"time\":1770000000500,\"event\":{\"type\":\"step.end\",\"uuid\":\"s1\"}}\n",
        "{\"type\":\"context.append_message\",\"time\":1770000000600,\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"injected\"}]}}\n",
        "{\"type\":\"turn.steer\",\"input\":[{\"type\":\"text\",\"text\":\"continue\"}],\"origin\":{\"kind\":\"user\"}}\n",
        "{\"type\":\"turn.prompt\",\"time\":1770000000700,\"input\":[{\"type\":\"text\",\"text\":\"<system-reminder>hidden\"}],\"origin\":{\"kind\":\"user\"}}\n",
        "{\"type\":\"turn.prompt\",\"time\":1770000000800,\"input\":[{\"type\":\"text\",\"text\":\"injected prompt\"}],\"origin\":{\"kind\":\"injection\",\"variant\":\"hook\"}}\n"
    );
    let messages = KimiAdapter.parse_transcript_messages(lines);
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0].text, "hello");
    assert_eq!(messages[1].text, "first");
    assert_eq!(messages[2].text, "injected");
    assert_eq!(messages[3].text, "continue");
    assert!(messages[3].at.is_none());
}

#[test]
fn unknown_and_malformed_wire_records_do_not_block_following_facts() {
    let lines = concat!(
        "{\"type\":\"future.record\",\"time\":1,\"payload\":{\"nested\":true}}\n",
        "{\"type\":\"turn.prompt\",\"time\":2,\"input\":\"malformed\",\"origin\":{\"kind\":\"user\"}}\n",
        "{\"type\":\"context.append_loop_event\",\"time\":3,\"event\":\"malformed\"}\n",
        "{\"type\":\"usage.record\",\"time\":4,\"usage\":\"malformed\"}\n",
        "{\"type\":\"turn.prompt\",\"time\":5,\"input\":[{\"type\":\"text\",\"text\":\"valid prompt\"}],\"origin\":{\"kind\":\"user\"}}\n",
        "{\"type\":\"context.append_loop_event\",\"time\":6,\"event\":{\"type\":\"content.part\",\"stepUuid\":\"s1\",\"part\":{\"type\":\"text\",\"text\":\"valid answer\"}}}\n",
        "{\"type\":\"context.append_loop_event\",\"time\":7,\"event\":{\"type\":\"step.end\",\"uuid\":\"s1\",\"usage\":{\"inputOther\":10}}}\n",
        "{\"type\":\"usage.record\",\"time\":8,\"model\":\"moonshot/kimi-k2.5\",\"usageScope\":\"turn\",\"usage\":{\"inputOther\":10}}\n",
    );

    let records = wire::records_from_bytes(lines.as_bytes());
    assert!(matches!(records[0].event, wire::WireEvent::Unknown));
    assert!(matches!(records[1].event, wire::WireEvent::Unknown));
    assert!(matches!(
        records[2].event,
        wire::WireEvent::AppendLoopEvent(wire::LoopEvent::Other)
    ));
    assert!(matches!(records[3].event, wire::WireEvent::Unknown));
    assert!(matches!(
        records[4].event,
        wire::WireEvent::Prompt {
            kind: wire::PromptKind::Prompt,
            ..
        }
    ));
    let messages = transcript::normalize(&records);
    assert_eq!(
        messages
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        ["valid prompt", "valid answer"]
    );
    assert_eq!(wire::latest_context_tokens(&records), Some(10));
    assert_eq!(wire::usage_records(&records).len(), 1);
}

#[test]
fn interleaved_assistant_steps_keep_completion_and_flush_order() {
    let lines = concat!(
        "{\"type\":\"context.append_loop_event\",\"time\":1,\"event\":{\"type\":\"step.begin\",\"uuid\":\"pending\"}}\n",
        "{\"type\":\"context.append_loop_event\",\"time\":2,\"event\":{\"type\":\"content.part\",\"stepUuid\":\"pending\",\"part\":{\"type\":\"text\",\"text\":\"pending first\"}}}\n",
        "{\"type\":\"context.append_loop_event\",\"time\":3,\"event\":{\"type\":\"step.begin\",\"uuid\":\"completed\"}}\n",
        "{\"type\":\"context.append_loop_event\",\"time\":4,\"event\":{\"type\":\"content.part\",\"stepUuid\":\"completed\",\"part\":{\"type\":\"text\",\"text\":\"completed first\"}}}\n",
        "{\"type\":\"context.append_loop_event\",\"time\":5,\"event\":{\"type\":\"step.end\",\"uuid\":\"completed\"}}\n",
        "{\"type\":\"turn.prompt\",\"time\":6,\"input\":[{\"type\":\"text\",\"text\":\"next\"}],\"origin\":{\"kind\":\"user\"}}\n",
        "{\"type\":\"context.append_loop_event\",\"time\":7,\"event\":{\"type\":\"content.part\",\"stepUuid\":\"orphan\",\"part\":{\"type\":\"text\",\"text\":\"orphan\"}}}\n",
        "{\"type\":\"context.append_loop_event\",\"time\":8,\"event\":{\"type\":\"step.begin\",\"uuid\":\"last\"}}\n",
        "{\"type\":\"context.append_loop_event\",\"time\":9,\"event\":{\"type\":\"content.part\",\"stepUuid\":\"last\",\"part\":{\"type\":\"text\",\"text\":\"last\"}}}\n",
    );

    let messages = transcript::parse_messages(lines);
    assert_eq!(
        messages
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        ["completed first", "pending first", "next", "orphan", "last"]
    );
}

#[test]
fn wire_timestamps_require_positive_in_range_milliseconds() {
    let lines = concat!(
        "{\"type\":\"turn.prompt\",\"time\":0,\"input\":[{\"type\":\"text\",\"text\":\"zero\"}],\"origin\":{\"kind\":\"user\"}}\n",
        "{\"type\":\"turn.prompt\",\"time\":-1,\"input\":[{\"type\":\"text\",\"text\":\"negative\"}],\"origin\":{\"kind\":\"user\"}}\n",
        "{\"type\":\"turn.prompt\",\"time\":1e30,\"input\":[{\"type\":\"text\",\"text\":\"overflow\"}],\"origin\":{\"kind\":\"user\"}}\n",
        "{\"type\":\"turn.prompt\",\"time\":1770000000000,\"input\":[{\"type\":\"text\",\"text\":\"valid\"}],\"origin\":{\"kind\":\"user\"}}\n",
    );

    let messages = transcript::parse_messages(lines);
    assert!(messages[..3].iter().all(|message| message.at.is_none()));
    assert!(messages[3].at.is_some());
}

#[test]
fn incremental_assistant_pages_keep_torn_lines_and_accept_mid_step_pages() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wire.jsonl");
    let begin = "{\"type\":\"context.append_loop_event\",\"event\":{\"type\":\"step.begin\",\"uuid\":\"s1\"}}\n";
    let first = "{\"type\":\"context.append_loop_event\",\"event\":{\"type\":\"content.part\",\"stepUuid\":\"s1\",\"part\":{\"type\":\"text\",\"text\":\"first\"}}}\n";
    let torn = "{\"type\":\"context.append_loop_event\",\"event\":{\"type\":\"content.part\",\"stepUuid\":\"s1\",\"part\":{\"type\":\"text\",\"text\":\"sec";
    std::fs::write(&path, format!("{begin}{first}{torn}")).unwrap();

    let page = KimiAdapter
        .read_assistant_transcript_page(
            &path,
            None,
            crate::agents::transcript::TranscriptPosition::START,
        )
        .unwrap();
    assert_eq!(page.messages, ["first"]);

    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    file.write_all(b"ond\"}}}\n").unwrap();
    let page = KimiAdapter
        .read_assistant_transcript_page(&path, None, page.next)
        .unwrap();
    assert_eq!(page.messages, ["second"]);
}

#[test]
fn latest_answer_cannot_cross_a_newer_unanswered_prompt() {
    let completed = concat!(
        "{\"type\":\"turn.prompt\",\"input\":[{\"type\":\"text\",\"text\":\"first\"}],\"origin\":{\"kind\":\"user\"}}\n",
        "{\"type\":\"context.append_loop_event\",\"event\":{\"type\":\"content.part\",\"stepUuid\":\"s1\",\"part\":{\"type\":\"text\",\"text\":\"answer\"}}}\n",
    );
    assert_eq!(
        transcript::latest_assistant(completed).as_deref(),
        Some("answer")
    );
    let failed = format!(
        "{completed}{{\"type\":\"turn.prompt\",\"input\":[{{\"type\":\"text\",\"text\":\"second\"}}],\"origin\":{{\"kind\":\"user\"}}}}\n"
    );
    assert_eq!(transcript::latest_assistant(&failed), None);
}

#[test]
fn prior_transcript_path_must_be_the_bound_main_agent_wire() {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("s1/agents/main/wire.jsonl");
    let child = dir.path().join("s1/agents/agent-0/wire.jsonl");
    std::fs::create_dir_all(main.parent().unwrap()).unwrap();
    std::fs::create_dir_all(child.parent().unwrap()).unwrap();
    std::fs::write(&main, "").unwrap();
    std::fs::write(&child, "").unwrap();

    assert_eq!(
        KimiAdapter.session_transcript("s1", Some(&main)).as_deref(),
        Some(main.as_path())
    );
    assert!(!valid_main_wire(&child, "s1"));
    assert!(!valid_main_wire(&main, "other"));
}

#[test]
fn live_cost_prices_the_full_file_outside_the_bounded_tail() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s1/agents/main/wire.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let early = "{\"type\":\"usage.record\",\"time\":1770000000000,\"model\":\"moonshot/kimi-k2.5\",\"usageScope\":\"turn\",\"usage\":{\"inputOther\":1000,\"output\":100}}\n";
    let padding = format!(
        "{{\"type\":\"tools.update_store\",\"payload\":\"{}\"}}\n",
        "x".repeat(70_000)
    );
    std::fs::write(&path, format!("{early}{padding}")).unwrap();
    let stat = TranscriptStat::from_path(&path).unwrap();
    let cache = dir.path().join("prices.json");
    let ctx = LocalContextRefreshCtx {
        agent_id: "s1",
        model_hint: None,
        current_transcript_path: None,
        prior_transcript_path: Some(path.to_str().unwrap()),
        prior_transcript_stat: None,
        prior_spend_fold: None,
        shared_pricing_cache_path: &cache,
    };

    let refresh = refresh_wire_path(&path, "s1", stat, &ctx).unwrap();
    assert!(
        refresh
            .context
            .cost
            .into_set()
            .unwrap()
            .total_cost_usd
            .unwrap()
            > 0.0
    );
    assert!(
        refresh
            .context
            .tokens
            .into_value()
            .unwrap()
            .current_usage
            .unwrap()
            .is_zero()
    );
}

#[test]
fn unknown_alias_costs_zero_instead_of_guessing_k25() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s1/agents/main/wire.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        concat!(
            "{\"type\":\"llm.request\",\"time\":1770000000000,\"provider\":\"custom\",\"model\":\"future-model\",\"modelAlias\":\"kimi-code/future\"}\n",
            "{\"type\":\"usage.record\",\"time\":1770000001000,\"model\":\"future\",\"usageScope\":\"session\",\"usage\":{\"inputOther\":1000,\"output\":100}}\n",
        ),
    )
    .unwrap();

    let parsed = spend::parse(&path, None, &super::super::PriceBook::embedded());
    assert_eq!(parsed.entries.len(), 1);
    assert_eq!(parsed.entries[0].model.as_deref(), Some("future"));
    assert_eq!(parsed.entries[0].cost_usd, 0.0);
    assert!(parsed.unknown_models.contains_key("future"));
}

#[test]
fn request_attribution_prices_alias_usage_and_history_groups_the_turn() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s1/agents/main/wire.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        concat!(
            "{\"type\":\"turn.prompt\",\"time\":1770000000000,\"input\":[{\"type\":\"text\",\"text\":\"fix history\"}],\"origin\":{\"kind\":\"user\"}}\n",
            "{\"type\":\"llm.request\",\"time\":1770000000100,\"provider\":\"moonshot\",\"model\":\"kimi-k2.5\",\"modelAlias\":\"kimi-code/kimi-for-coding\"}\n",
            "{\"type\":\"usage.record\",\"time\":1770000000200,\"model\":\"kimi-for-coding\",\"usageScope\":\"turn\",\"usage\":{\"inputOther\":100,\"output\":50,\"inputCacheRead\":10,\"inputCacheCreation\":5}}\n",
            "{\"type\":\"usage.record\",\"time\":0,\"model\":\"moonshot/kimi-k2.5\",\"usageScope\":\"session\",\"usage\":{\"inputOther\":999}}\n",
            "{\"type\":\"usage.record\",\"model\":\"moonshot/kimi-k2.5\",\"usageScope\":\"session\",\"usage\":{\"inputOther\":999}}\n",
            "{\"type\":\"context.append_loop_event\",\"time\":1770000000300,\"event\":{\"type\":\"content.part\",\"stepUuid\":\"s1\",\"part\":{\"type\":\"text\",\"text\":\"done\"}}}\n",
        ),
    )
    .unwrap();

    let messages = KimiAdapter.read_transcript_messages(&path, None).unwrap();
    let parsed = spend::parse(&path, None, &super::super::PriceBook::embedded());
    assert_eq!(parsed.entries.len(), 1);
    assert_eq!(
        parsed.entries[0].model.as_deref(),
        Some("moonshot/kimi-k2.5")
    );
    assert!(parsed.entries[0].cost_usd > 0.0);
    let turns = crate::agents::turns::session_turns(&messages, &parsed.entries, "s1", false);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].prompt, "fix history");
    assert_eq!(turns[0].api_calls, 1);
    assert_eq!(turns[0].fresh_input, 100);
    assert_eq!(turns[0].output, 50);
    assert_eq!(turns[0].outcome, crate::agents::turns::TurnOutcome::Done);
}

#[test]
fn refresh_triggers_seed_and_stat_gate_the_stable_transcript_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s1/agents/main/wire.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        "{\"type\":\"context.append_loop_event\",\"time\":1770000000000,\"event\":{\"type\":\"step.end\",\"uuid\":\"s1\",\"usage\":{\"inputOther\":100}}}\n",
    )
    .unwrap();
    let cache = dir.path().join("prices.json");
    let path_text = path.to_string_lossy().into_owned();
    let ctx = LocalContextRefreshCtx {
        agent_id: "s1",
        model_hint: None,
        current_transcript_path: None,
        prior_transcript_path: Some(&path_text),
        prior_transcript_stat: None,
        prior_spend_fold: None,
        shared_pricing_cache_path: &cache,
    };

    assert!(
        KimiAdapter
            .local_context_refresh(RefreshTrigger::Hook("PreToolUse"), &ctx)
            .is_none()
    );
    let refresh = KimiAdapter
        .local_context_refresh(RefreshTrigger::Hook("SessionStart"), &ctx)
        .unwrap();
    assert_eq!(refresh.transcript_path.as_deref(), Some(path_text.as_str()));
    let stat = refresh.transcript_stat.unwrap();
    let unchanged = LocalContextRefreshCtx {
        prior_transcript_stat: Some(&stat),
        prior_spend_fold: None,
        ..ctx
    };
    assert!(
        KimiAdapter
            .local_context_refresh(RefreshTrigger::Hook("StopFailure"), &unchanged)
            .is_none()
    );
}

#[test]
fn quota_parser_accepts_nested_remaining_and_reset_spellings() {
    let snapshot = oauth_usage::parse_response(
        r#"{"limits":[{"detail":{"limit":100,"remaining":25,"resetAt":"2030-01-01T00:00:00Z"},"window":{"duration":5,"timeUnit":"HOUR"}}],"boosterWallet":{"balance":{"type":"BOOSTER","amount":500000000,"amountLeft":125000000},"monthlyChargeLimitEnabled":true,"monthlyChargeLimit":{"priceInCents":500,"currency":"USD"},"monthlyUsed":{"priceInCents":125,"currency":"USD"}}}"#,
    )
    .unwrap();
    let window = &snapshot.rate_limits.as_ref().unwrap().windows[0];
    assert_eq!(window.used_percentage, Some(75));
    assert_eq!(window.duration_mins, Some(300));
    assert_eq!(
        snapshot.extra_credits,
        Some(super::super::ExtraCredits::known(
            Some(1.25),
            Some(1.25),
            Some(5.0)
        ))
    );
}
