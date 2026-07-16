use super::*;

use crate::agents::lifecycle::{TurnPhase, step};
use crate::agents::{AgentErr, AgentHookClass, AgentStatus, LaunchPreset, PresetErr};
use serde_json::json;

#[test]
fn lifecycle_maps_identity_prompt_tools_outcomes_and_compaction() {
    let registered = CursorAdapter
        .observe_lifecycle(
            "sessionStart",
            &json!({
                "conversation_id": "conv-1",
                "model": "legacy-model",
                "model_id": "cursor/model",
                "model_params": [
                    { "id": "context", "value": "200k" },
                    { "id": "effort", "value": "high" },
                    { "id": "future", "value": "kept-tolerant" }
                ],
                "transcript_path": "/tmp/transcript.jsonl"
            }),
        )
        .expect("registered observation");
    assert_eq!(registered.agent_id.as_deref(), Some("conv-1"));
    assert_eq!(registered.signal, LifecycleSignal::Registered);
    assert_eq!(registered.launch.model.as_deref(), Some("cursor/model"));
    assert_eq!(registered.launch.effort.as_deref(), Some("high"));
    assert_eq!(
        registered.transcript_path.as_deref(),
        Some("/tmp/transcript.jsonl")
    );
    assert_eq!(
        step(None, None, &registered.signal).next.status,
        AgentStatus::Idle
    );

    let prompt = CursorAdapter
        .observe_lifecycle(
            "beforeSubmitPrompt",
            &json!({ "conversation_id": "conv-1", "prompt": "  fix auth  " }),
        )
        .expect("prompt observation");
    assert_eq!(prompt.task.as_deref(), Some("fix auth"));
    assert_eq!(prompt.prompt.as_deref(), Some("fix auth"));
    let running = step(None, None, &prompt.signal).next;
    assert_eq!(running.status, AgentStatus::Running);
    assert_eq!(running.phase, TurnPhase::Reasoning);

    for (tool, edits) in [
        ("Write", Some(true)),
        ("Shell", Some(false)),
        ("Read", None),
    ] {
        let observation = CursorAdapter.observe_lifecycle(
            "postToolUse",
            &json!({ "conversation_id": "conv-1", "tool_name": tool, "cwd": "/work" }),
        );
        assert_eq!(
            observation.map(|observation| observation.signal),
            edits.map(|edits| LifecycleSignal::ToolUsed {
                mutates: true,
                edits,
                native_key: None,
            }),
            "{tool}",
        );
    }
    assert!(
        CursorAdapter
            .observe_lifecycle(
                "postToolUseFailure",
                &json!({ "conversation_id": "conv-1", "tool_name": "Write" }),
            )
            .is_none()
    );

    for (status, signal) in [
        (
            "completed",
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
        ),
        ("aborted", LifecycleSignal::TurnInterrupted),
        (
            "error",
            LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: false,
            },
        ),
    ] {
        let observation = CursorAdapter
            .observe_lifecycle(
                "stop",
                &json!({ "conversation_id": "conv-1", "status": status }),
            )
            .expect("stop observation");
        assert_eq!(observation.signal, signal);
    }

    let compacting = CursorAdapter
        .observe_lifecycle(
            "preCompact",
            &json!({
                "conversation_id": "conv-1",
                "context_usage_percent": 83.6,
                "context_tokens": 167200,
                "context_window_size": 200000
            }),
        )
        .expect("compaction observation");
    assert_eq!(compacting.signal, LifecycleSignal::Compacting);
    assert_eq!(compacting.context_pct, Some(84));
    assert_eq!(compacting.context_window, Some(200_000));
    assert_eq!(compacting.total_tokens, None);
    let transition = step(Some(&running), None, &compacting.signal);
    assert!(transition.next.compacting);
    assert_eq!(transition.next.status, AgentStatus::Running);

    let ended = CursorAdapter
        .observe_lifecycle("sessionEnd", &json!({ "conversation_id": "conv-1" }))
        .expect("session end");
    assert_eq!(ended.signal, LifecycleSignal::Ended);
    assert!(CursorAdapter.ends_session("sessionEnd"));
}

#[test]
fn malformed_payloads_degrade_without_losing_the_event() {
    let observations: Vec<_> = [
        Value::Null,
        json!({ "status": "completed" }),
        json!({ "conversation_id": "  " }),
    ]
    .iter()
    .map(|payload| {
        CursorAdapter
            .observe_lifecycle("sessionStart", payload)
            .expect("event still maps")
    })
    .collect();
    assert!(
        observations
            .iter()
            .all(|observation| observation.agent_id.is_none())
    );
    insta::assert_json_snapshot!(observations, @r###"
    [
      {
        "signal": {
          "signal": "registered"
        }
      },
      {
        "signal": {
          "signal": "registered"
        }
      },
      {
        "signal": {
          "signal": "registered"
        }
      }
    ]
    "###);
}

#[test]
fn malformed_fields_preserve_identity_response_and_token_composition() {
    let payload = json!({
        "conversation_id": "conv-1",
        "model_id": "cursor/model",
        "model": 7,
        "model_params": [false, {"id": "effort", "value": "high"}, {"id": 9, "value": []}],
        "transcript_path": "/tmp/conv.jsonl",
        "status": "completed",
        "input_tokens": 0,
        "output_tokens": "12",
        "cache_read_tokens": 3,
        "cache_write_tokens": {},
        "context_tokens": 999
    });
    let observed = CursorAdapter
        .observe_lifecycle("stop", &payload)
        .expect("stop survives malformed siblings");
    assert_eq!(observed.agent_id.as_deref(), Some("conv-1"));
    assert_eq!(observed.launch.model.as_deref(), Some("cursor/model"));
    assert_eq!(observed.launch.effort.as_deref(), Some("high"));
    assert_eq!(observed.fresh_input_tokens, Some(0));
    assert_eq!(observed.output_tokens, Some(12));
    assert_eq!(observed.cache_read_input_tokens, Some(3));
    assert_eq!(observed.cache_write_input_tokens, None);
    assert_eq!(observed.total_tokens, None);

    assert_eq!(
        CursorAdapter.observe_assistant_message(
            "afterAgentResponse",
            &json!({"conversation_id": "conv-1", "text": "  safe final  ", "input_tokens": 9})
        ),
        Some("safe final".to_owned())
    );
    assert!(
        CursorAdapter
            .observe_assistant_message("stop", &json!({"text": "unsafe fallback"}))
            .is_none()
    );
}

#[test]
fn stop_input_total_subtracts_cache_without_underflow() {
    let observed = CursorAdapter
        .observe_lifecycle(
            "stop",
            &json!({
                "conversation_id": "conv-1",
                "status": "completed",
                "input_tokens": 22_725,
                "output_tokens": 26,
                "cache_read_tokens": 8_704,
                "cache_write_tokens": 0
            }),
        )
        .unwrap();
    assert_eq!(observed.fresh_input_tokens, Some(14_021));
    assert_eq!(observed.cache_read_input_tokens, Some(8_704));
    assert_eq!(observed.output_tokens, Some(26));

    let malformed = CursorAdapter
        .observe_lifecycle(
            "stop",
            &json!({
                "conversation_id": "conv-1",
                "status": "completed",
                "input_tokens": 3,
                "cache_read_tokens": 4,
                "cache_write_tokens": 5
            }),
        )
        .unwrap();
    assert_eq!(malformed.fresh_input_tokens, Some(0));
}

#[test]
fn turn_cost_prices_auto_explicit_and_fast_models() {
    let prices = PriceBook::embedded();
    let auto = CursorAdapter
        .price_turn_locally(
            "stop",
            &json!({
                "generation_id": " gen-1 ",
                "status": "completed",
                "model_id": "default",
                "input_tokens": 22_725,
                "output_tokens": 26,
                "cache_read_tokens": 8_704,
                "cache_write_tokens": 0
            }),
            &prices,
        )
        .unwrap();
    assert_eq!(auto.turn_id, "gen-1");
    assert!((auto.cost_usd - 0.019_858_25).abs() < 1e-12);
    let response = CursorAdapter
        .price_turn_locally(
            "afterAgentResponse",
            &json!({
                "generation_id": "gen-1",
                "model_id": "default",
                "input_tokens": 22_725,
                "output_tokens": 26,
                "cache_read_tokens": 8_704,
                "cache_write_tokens": 0
            }),
            &prices,
        )
        .unwrap();
    assert_eq!(response.cost_usd, auto.cost_usd);

    let payload = |model: &str| {
        json!({
            "generation_id": "gen-2",
            "status": "completed",
            "model_id": model,
            "input_tokens": 1_000,
            "output_tokens": 100,
            "cache_read_tokens": 0,
            "cache_write_tokens": 0
        })
    };
    let base = CursorAdapter
        .price_turn_locally("stop", &payload("gpt-5.4"), &prices)
        .unwrap();
    let fast = CursorAdapter
        .price_turn_locally("stop", &payload("gpt-5.4-fast"), &prices)
        .unwrap();
    assert!((fast.cost_usd - base.cost_usd * 2.0).abs() < 1e-12);
    assert!(
        CursorAdapter
            .price_turn_locally("stop", &payload("unknown-future-model"), &prices)
            .is_none()
    );
    assert!(
        CursorAdapter
            .price_turn_locally(
                "stop",
                &json!({"generation_id": "", "status": "completed", "model_id": "default", "input_tokens": 1}),
                &prices,
            )
            .is_none()
    );
    assert!(
        CursorAdapter
            .price_turn_locally("postToolUse", &payload("gpt-5.4"), &prices)
            .is_none()
    );
    assert!(
        CursorAdapter
            .price_turn_locally(
                "stop",
                &json!({"generation_id": "gen", "status": "running", "model_id": "default", "input_tokens": 1}),
                &prices,
            )
            .is_none()
    );
    assert!(
        CursorAdapter
            .observe_lifecycle(
                "afterAgentResponse",
                &json!({"conversation_id": "conv-1", "input_tokens": 1}),
            )
            .is_none(),
        "response pricing must not become a lifecycle or token source"
    );
}

#[test]
fn transcript_tail_reads_only_terminal_rows_and_resolves_exact_paths() {
    const SENTINEL: &str = "THINKING_SENTINEL_DO_NOT_INGEST";
    let fixture = include_str!("tests/fixtures/transcript.jsonl");
    assert!(
        fixture.contains(SENTINEL),
        "fixture exercises the privacy boundary"
    );
    assert_eq!(transcript::parse_terminal_for_test(fixture), None);
    let healed = format!("{}}}\n", fixture.trim_end());
    assert_eq!(
        transcript::parse_terminal_for_test(&healed),
        Some("complete")
    );
    assert!(CursorAdapter.parse_transcript_messages(fixture).is_empty());
    let page = CursorAdapter.stream_assistant_messages(fixture);
    assert!(page.is_empty());

    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project-a/agent-transcripts/conv-1");
    std::fs::create_dir_all(&project).unwrap();
    let discovered = project.join("conv-1.jsonl");
    std::fs::write(&discovered, fixture).unwrap();
    assert_eq!(
        transcript::discover_under(dir.path(), "conv-1"),
        Some(discovered.clone())
    );
    assert!(transcript::discover_under(dir.path(), "../conv-1").is_none());

    let project_b = dir.path().join("project-b/agent-transcripts/conv-1");
    std::fs::create_dir_all(&project_b).unwrap();
    std::fs::write(project_b.join("conv-1.jsonl"), fixture).unwrap();
    assert!(transcript::discover_under(dir.path(), "conv-1").is_none());

    let current = dir.path().join("current.jsonl");
    let prior = dir.path().join("prior.jsonl");
    std::fs::write(&current, fixture).unwrap();
    std::fs::write(&prior, fixture).unwrap();
    assert_eq!(
        transcript::resolve_transcript("conv-1", Some(&current), Some(&prior)),
        Some(current)
    );
}

#[test]
fn transcript_recovery_requires_the_terminal_row_to_be_last() {
    let terminal = "{\"type\":\"turn_ended\",\"status\":\"success\"}";
    for later in [
        "{\"role\":\"user\",\"message\":{\"content\":[]}}",
        "{\"role\":\"assistant\",\"message\":{\"content\":[]}}",
        "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_call\",\"name\":\"Read\"}]}}",
        "{\"type\":\"future_record\"}",
        "not-json",
    ] {
        let tail = format!("{terminal}\n{later}\n");
        assert_eq!(transcript::parse_terminal_for_test(&tail), None, "{later}");

        let healed = format!("{tail}{terminal}\n");
        assert_eq!(
            transcript::parse_terminal_for_test(&healed),
            Some("complete"),
            "{later}",
        );
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conv-1.jsonl");
    std::fs::write(&path, format!("{terminal}\n{{\"role\":\"user\"")).unwrap();
    let path_string = path.to_string_lossy().into_owned();
    let pricing = dir.path().join("pricing-cache.json");
    let refresh = transcript::refresh(&crate::agents::LocalContextRefreshCtx {
        agent_id: "conv-1",
        model_hint: None,
        current_transcript_path: Some(&path_string),
        prior_transcript_path: None,
        prior_transcript_stat: None,
        shared_pricing_cache_path: &pricing,
    })
    .expect("torn transcript still registers its path");
    assert!(refresh.turn_complete.is_none());

    std::fs::write(&path, format!("{terminal}\n{terminal}\n")).unwrap();
    let healed = transcript::refresh(&crate::agents::LocalContextRefreshCtx {
        agent_id: "conv-1",
        model_hint: None,
        current_transcript_path: Some(&path_string),
        prior_transcript_path: None,
        prior_transcript_stat: refresh.transcript_stat.as_ref(),
        shared_pricing_cache_path: &pricing,
    })
    .expect("new complete terminal refresh");
    assert!(healed.turn_complete.is_some());
}

#[test]
fn transcript_refresh_registers_live_file_and_recovers_interruption() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conv-1.jsonl");
    std::fs::write(&path, "{\"role\":\"user\",\"message\":{\"content\":[]}}\n").unwrap();
    let path_string = path.to_string_lossy().into_owned();
    let pricing = dir.path().join("pricing-cache.json");
    let first = transcript::refresh(&crate::agents::LocalContextRefreshCtx {
        agent_id: "conv-1",
        model_hint: Some("cursor/model"),
        current_transcript_path: Some(&path_string),
        prior_transcript_path: None,
        prior_transcript_stat: None,
        shared_pricing_cache_path: &pricing,
    })
    .expect("file identity refresh");
    assert_eq!(first.transcript_path.as_deref(), Some(path_string.as_str()));
    assert_eq!(first.model_id.as_deref(), Some("cursor/model"));
    assert!(first.turn_complete.is_none());
    assert!(first.turn_interrupted.is_none());
    assert!(first.turn_error.is_none());

    std::fs::write(
        &path,
        "{\"role\":\"user\",\"message\":{\"content\":[]}}\n{\"type\":\"turn_ended\",\"status\":\"aborted\"}\n",
    )
    .unwrap();
    let interrupted = transcript::refresh(&crate::agents::LocalContextRefreshCtx {
        agent_id: "conv-1",
        model_hint: None,
        current_transcript_path: Some(&path_string),
        prior_transcript_path: None,
        prior_transcript_stat: first.transcript_stat.as_ref(),
        shared_pricing_cache_path: &pricing,
    })
    .expect("changed transcript refresh");
    assert!(interrupted.turn_interrupted.is_some());
    assert!(interrupted.tokens.is_none());
    assert!(interrupted.cost.is_none());
}

#[test]
fn transcript_refresh_recovers_a_same_path_whole_file_rewrite() {
    const SENTINEL: &str = "THINKING_SENTINEL_DO_NOT_INGEST";
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conv-1.jsonl");
    let one_turn = concat!(
        "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"first\"}]}}\n",
        "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"first answer\"}]}}\n",
        "{\"type\":\"turn_ended\",\"status\":\"success\"}\n",
    );
    std::fs::write(&path, one_turn).unwrap();
    let path_string = path.to_string_lossy().into_owned();
    let pricing = dir.path().join("pricing-cache.json");
    let first = transcript::refresh(&crate::agents::LocalContextRefreshCtx {
        agent_id: "conv-1",
        model_hint: None,
        current_transcript_path: Some(&path_string),
        prior_transcript_path: None,
        prior_transcript_stat: None,
        shared_pricing_cache_path: &pricing,
    })
    .expect("first completed snapshot");
    let first_complete = first.turn_complete.expect("first terminal marker");
    let first_stat = first.transcript_stat.expect("first transcript stat");

    let two_turns = concat!(
        "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"first\"}]}}\n",
        "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"first answer\"}]}}\n",
        "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"second\"}]}}\n",
        "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"<sentinel>\"}]}}\n",
        "{\"type\":\"turn_ended\",\"status\":\"success\"}\n",
    )
    .replace("<sentinel>", SENTINEL);
    std::fs::write(&path, &two_turns).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60)),
        )
        .unwrap();

    let rewritten = transcript::refresh(&crate::agents::LocalContextRefreshCtx {
        agent_id: "conv-1",
        model_hint: None,
        current_transcript_path: Some(&path_string),
        prior_transcript_path: None,
        prior_transcript_stat: Some(&first_stat),
        shared_pricing_cache_path: &pricing,
    })
    .expect("whole-file rewrite refresh");
    assert_ne!(rewritten.transcript_stat, Some(first_stat));
    assert!(
        rewritten
            .turn_complete
            .is_some_and(|at| at > first_complete)
    );
    assert!(
        CursorAdapter
            .parse_transcript_messages(&two_turns)
            .is_empty()
    );
    assert!(
        CursorAdapter
            .stream_assistant_messages(&two_turns)
            .is_empty()
    );
}

#[test]
fn every_wired_event_returns_cursor_neutral_json() {
    let neutrals: Vec<_> = WIRED_EVENTS
        .iter()
        .map(|event| {
            (
                *event,
                CursorAdapter
                    .render_neutral(event)
                    .expect("neutral render")
                    .expect("wired neutral"),
            )
        })
        .collect();
    insta::assert_json_snapshot!(neutrals, @r###"
    [
      [
        "sessionStart",
        {}
      ],
      [
        "beforeSubmitPrompt",
        {}
      ],
      [
        "postToolUse",
        {}
      ],
      [
        "postToolUseFailure",
        {}
      ],
      [
        "afterAgentResponse",
        {}
      ],
      [
        "stop",
        {}
      ],
      [
        "sessionEnd",
        {}
      ],
      [
        "preCompact",
        {}
      ]
    ]
    "###);
    assert_eq!(CursorAdapter.render_neutral("future").unwrap(), None);
}

#[test]
fn launch_modes_presets_resume_and_compaction_are_cursor_native() {
    assert_eq!(
        CursorAdapter.permission_args(PermissionMode::Ask),
        Vec::<String>::new()
    );
    assert_eq!(
        CursorAdapter.permission_args(PermissionMode::Plan),
        vec!["--mode=plan"]
    );
    assert_eq!(
        CursorAdapter.permission_args(PermissionMode::Auto),
        vec!["--auto-review"]
    );
    assert_eq!(
        CursorAdapter.permission_args(PermissionMode::Yolo),
        vec!["--force", "--sandbox", "disabled"]
    );
    assert_eq!(CursorAdapter.compact_command(), Some("/summarize"));

    let launch = CursorAdapter
        .launch_command(&["--auto-review".to_owned()], Some("fix it"))
        .expect("fresh interactive launch");
    assert_eq!(
        &launch[launch.len() - 3..],
        ["--auto-review", "--", "fix it"]
    );
    assert!(
        launch.iter().all(|arg| arg != "-p" && arg != "--print"),
        "RimZ supervised Cursor runs stay on the interactive hook transport: {launch:?}",
    );

    let preset = CursorAdapter
        .render_preset(&LaunchPreset {
            model: Some("cursor/model".to_owned()),
            ..Default::default()
        })
        .expect("model preset");
    assert_eq!(preset, vec!["--model", "cursor/model"]);
    assert_eq!(
        CursorAdapter.render_preset(&LaunchPreset {
            effort: Some("high".to_owned()),
            ..Default::default()
        }),
        Err(PresetErr::UnsupportedField {
            agent: "cursor",
            field: "effort",
        })
    );
    let resume = CursorAdapter
        .resume_command("conv-1", Path::new("/tmp"))
        .expect("resume command");
    assert_eq!(&resume[resume.len() - 2..], ["--resume", "conv-1"]);
    assert!(
        CursorAdapter
            .fork_command("conv-1", Path::new("/tmp"))
            .is_none()
    );
}

#[test]
fn hook_install_merges_idempotently_and_uninstalls_only_owned_entries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hooks.json");
    let config_path = dir.path().join("cli-config.json");
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "version": 99,
            "future": { "kept": true },
            "hooks": {
                "sessionStart": [{ "command": "user-hook" }],
                "futureEvent": [{ "command": "future-hook" }]
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let original_config = r#"{ "statusLine": { "type": "command", "command": "'/tmp/my status' --plain '$HOME'", "padding": 2, "updateIntervalMs": 500, "timeoutMs": 1000, "future": true }, "theme": "dark" }
"#;
    std::fs::write(&config_path, original_config).unwrap();

    let report = install::install_into(&path, &config_path).expect("install");
    assert!(report.files.iter().all(|file| file.existed));
    assert_eq!(report.installed_events.len(), WIRED_EVENTS.len());
    assert!(install::hooks_installed_at(&path));
    assert!(install::statusline_installed_at(&config_path));
    let once = std::fs::read_to_string(&path).unwrap();
    let config_once = std::fs::read_to_string(&config_path).unwrap();
    install::install_into(&path, &config_path).expect("second install");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), once);
    assert_eq!(std::fs::read_to_string(&config_path).unwrap(), config_once);

    let installed: Value = serde_json::from_str(&once).unwrap();
    assert_eq!(installed["version"], 1);
    assert_eq!(installed["future"]["kept"], true);
    assert_eq!(
        installed["hooks"]["sessionStart"][0]["command"],
        "user-hook"
    );
    assert_eq!(
        installed["hooks"]["futureEvent"][0]["command"],
        "future-hook"
    );
    assert!(install::managed_artifacts_at(&path));
    assert!(install::statusline_artifact_at(&config_path));
    let installed_config: Value = serde_json::from_str(&config_once).unwrap();
    assert_eq!(installed_config["statusLine"]["padding"], 2);
    assert_eq!(installed_config["statusLine"]["updateIntervalMs"], 500);
    assert!(installed_config["statusLine"].get("future").is_none());
    assert_eq!(
        crate::agents::managed_statusline::wrapped_command(
            installed_config.as_object().unwrap(),
            &STATUS_LINE,
        )
        .as_deref(),
        Some("'/tmp/my status' --plain '$HOME'")
    );

    let preview = install::preview_at(&path, &config_path).expect("preview");
    assert_eq!(preview.files[0].candidate, once);
    assert_eq!(preview.files[1].candidate, config_once);
    let uninstall = install::uninstall_from(&path, &config_path).expect("uninstall");
    assert_eq!(uninstall.removed_events.len(), WIRED_EVENTS.len());
    assert!(!install::managed_artifacts_at(&path));
    let uninstalled: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        uninstalled["hooks"]["sessionStart"][0]["command"],
        "user-hook"
    );
    assert_eq!(
        uninstalled["hooks"]["futureEvent"][0]["command"],
        "future-hook"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&std::fs::read_to_string(config_path).unwrap()).unwrap(),
        serde_json::from_str::<Value>(original_config).unwrap()
    );
}

#[test]
fn legacy_hook_install_is_detected_and_repaired_additively() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hooks.json");
    let config_path = dir.path().join("cli-config.json");
    let legacy_events: Vec<_> = WIRED_EVENTS
        .iter()
        .copied()
        .filter(|event| *event != "afterAgentResponse")
        .collect();
    let mut hooks = serde_json::Map::new();
    for event in &legacy_events {
        hooks.insert(
            (*event).to_owned(),
            json!([
                { "command": format!("user-{event}-hook") },
                { "command": RIMZ_HOOK_COMMAND }
            ]),
        );
    }
    hooks.insert(
        "futureEvent".to_owned(),
        json!([{ "command": "future-user-hook" }]),
    );
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "version": 1,
            "hooks": hooks,
            "future": { "kept": true }
        }))
        .unwrap(),
    )
    .unwrap();

    assert!(!install::hooks_installed_at(&path));
    let preview = install::preview_at(&path, &config_path).expect("legacy repair preview");
    let candidate: Value = serde_json::from_str(&preview.files[0].candidate).unwrap();
    assert_eq!(candidate["future"]["kept"], true);
    assert_eq!(
        candidate["hooks"]["futureEvent"][0]["command"],
        "future-user-hook"
    );
    for event in WIRED_EVENTS {
        let entries = candidate["hooks"][event].as_array().unwrap();
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry["command"] == RIMZ_HOOK_COMMAND)
                .count(),
            1,
            "{event}",
        );
        if legacy_events.contains(event) {
            assert!(
                entries.iter().any(|entry| {
                    entry["command"] == Value::String(format!("user-{event}-hook"))
                })
            );
        }
    }

    install::install_into(&path, &config_path).expect("repair legacy install");
    assert!(install::hooks_installed_at(&path));
    assert!(install::statusline_installed_at(&config_path));
    let once = std::fs::read(&path).unwrap();
    install::install_into(&path, &config_path).expect("second repair install");
    assert_eq!(std::fs::read(&path).unwrap(), once);
}

#[test]
fn incomplete_and_malformed_hook_configs_are_detected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hooks.json");
    let config_path = dir.path().join("cli-config.json");
    install::install_into(&path, &config_path).expect("install");
    let mut root: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    root["hooks"].as_object_mut().unwrap().remove("stop");
    std::fs::write(&path, serde_json::to_vec_pretty(&root).unwrap()).unwrap();
    assert!(!install::hooks_installed_at(&path));

    std::fs::write(&path, "{").unwrap();
    assert!(matches!(
        install::install_into(&path, &config_path),
        Err(AgentErr::InstallParse {
            agent: "cursor",
            ..
        })
    ));
}

#[test]
fn hook_command_preserves_parent_pid_attribution() {
    assert_eq!(
        CursorAdapter.descriptor().bin_names,
        ["cursor-agent", "agent"]
    );
    assert!(!CursorAdapter.descriptor().bin_names.contains(&"cursor"));
    assert!(RIMZ_HOOK_COMMAND.starts_with("RIMZ_AGENT_PID=$PPID"));
    assert!(RIMZ_HOOK_COMMAND.contains("--source cursor"));
    assert_eq!(
        CursorAdapter
            .classify_hook("sessionStart", &json!({}))
            .class,
        AgentHookClass::Lifecycle
    );
    assert_eq!(
        CursorAdapter
            .classify_hook("subagentStart", &json!({}))
            .class,
        AgentHookClass::Unknown
    );
}
