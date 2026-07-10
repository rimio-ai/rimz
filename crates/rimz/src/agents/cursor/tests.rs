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
        step(None, &registered.signal).next.status,
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
    let running = step(None, &prompt.signal).next;
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

    for (status, errored) in [("completed", false), ("aborted", true), ("error", true)] {
        let observation = CursorAdapter
            .observe_lifecycle(
                "stop",
                &json!({ "conversation_id": "conv-1", "status": status }),
            )
            .expect("stop observation");
        assert_eq!(
            observation.signal,
            LifecycleSignal::TurnEnded {
                errored,
                parked_on_background: false,
            }
        );
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
    let transition = step(Some(&running), &compacting.signal);
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

    let report = install::install_into(&path).expect("install");
    assert!(report.merged);
    assert_eq!(report.installed_events.len(), WIRED_EVENTS.len());
    assert!(install::hooks_installed_at(&path));
    let once = std::fs::read_to_string(&path).unwrap();
    install::install_into(&path).expect("second install");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), once);

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

    let preview = install::preview_at(&path).expect("preview");
    assert_eq!(preview.candidate_config, once);
    let uninstall = install::uninstall_from(&path).expect("uninstall");
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
}

#[test]
fn incomplete_and_malformed_hook_configs_are_detected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hooks.json");
    install::install_into(&path).expect("install");
    let mut root: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    root["hooks"].as_object_mut().unwrap().remove("stop");
    std::fs::write(&path, serde_json::to_vec_pretty(&root).unwrap()).unwrap();
    assert!(!install::hooks_installed_at(&path));

    std::fs::write(&path, "{").unwrap();
    assert!(matches!(
        install::install_into(&path),
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
