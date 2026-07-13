use super::*;

use crate::agents::lifecycle::{LifecycleState, TurnPhase, step};
use crate::agents::{AgentErr, AgentHookClass, AgentStatus};
use serde_json::json;

// Capability and coverage-table honesty is cross-checked against behavior for
// every adapter in `agents::conformance`; this slice only pins what is
// pi-specific behavior beyond those flags.

#[test]
fn pi_activity_filter_excludes_the_blocking_gate_and_launch_commands_build() {
    let descriptor = PiAdapter.descriptor();
    // Completed-work events touch activity; the blocking `tool_call` gate is
    // excluded so creating the ask never instantly un-blocks the row.
    assert!(descriptor.records_activity("tool_execution_end"));
    assert!(descriptor.records_activity("agent_end"));
    assert!(descriptor.records_activity("message_update"));
    assert!(descriptor.records_activity("turn_end"));
    assert!(!descriptor.records_activity("tool_call"));
    assert!(!descriptor.records_activity("session_shutdown"));

    assert_eq!(
        PiAdapter.resume_command("0199aaf2", Path::new("/tmp")),
        Some(vec![
            "pi".to_owned(),
            "--session".to_owned(),
            "0199aaf2".to_owned(),
        ])
    );
    assert_eq!(
        PiAdapter.fork_command("0199aaf2", Path::new("/tmp")),
        Some(vec![
            "pi".to_owned(),
            "--fork".to_owned(),
            "0199aaf2".to_owned(),
        ])
    );
    assert_eq!(
        PiAdapter.launch_command(&[], None),
        Some(vec!["pi".to_owned()])
    );
    assert_eq!(
        PiAdapter.launch_command(
            &["--model".to_owned(), "large".to_owned()],
            Some("review this"),
        ),
        Some(vec![
            "pi".to_owned(),
            "--model".to_owned(),
            "large".to_owned(),
            "--".to_owned(),
            "review this".to_owned(),
        ])
    );
}

#[test]
fn pi_render_preset_maps_model_and_thinking() {
    use crate::agents::{LaunchPreset, PresetErr};

    assert_eq!(
        PiAdapter.render_preset(&LaunchPreset {
            model: Some("openai/gpt-4o".to_owned()),
            effort: Some("high".to_owned()),
            ..Default::default()
        }),
        Ok(vec![
            "--model".to_owned(),
            "openai/gpt-4o".to_owned(),
            "--thinking".to_owned(),
            "high".to_owned(),
        ])
    );
    assert_eq!(
        PiAdapter.render_preset(&LaunchPreset {
            system_prompt_file: Some(Path::new("/abs/prompt.md").to_path_buf()),
            ..Default::default()
        }),
        Err(PresetErr::UnsupportedField {
            agent: "pi",
            field: "system-prompt-file",
        })
    );
    assert_eq!(
        PiAdapter.render_preset(&LaunchPreset {
            append_system_prompt_file: Some(Path::new("/abs/append.md").to_path_buf()),
            ..Default::default()
        }),
        Err(PresetErr::UnsupportedField {
            agent: "pi",
            field: "append-system-prompt-file",
        })
    );
    assert!(
        PiAdapter
            .render_preset(&LaunchPreset::default())
            .expect("empty preset is valid")
            .is_empty()
    );
}

#[test]
fn pi_observes_lifecycle_enrichment_and_error_bits() {
    let started = PiAdapter
        .observe_lifecycle(
            "session_start",
            &json!({
                "session_id": "sess-1",
                "cwd": "/home/u/code/query-engine",
                "model": "gpt-5.5",
                "effort": "medium",
                "context_pct": 150,
                "context_window": 272_000,
                "total_tokens": 8160,
            }),
        )
        .expect("observation");
    assert_eq!(started.agent_id.as_deref(), Some("sess-1"));
    assert_eq!(started.signal, LifecycleSignal::Registered);
    assert_eq!(
        started.worktree_path.as_deref(),
        Some("/home/u/code/query-engine")
    );
    assert_eq!(started.launch.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(started.launch.effort.as_deref(), Some("medium"));
    assert_eq!(started.context_pct, Some(100));
    assert_eq!(started.context_window, Some(272_000));
    assert_eq!(started.total_tokens, Some(8160));
    assert_eq!(started.parent_agent_id, None);

    let prompt = PiAdapter
        .observe_lifecycle(
            "before_agent_start",
            &json!({ "session_id": "sess-1", "prompt": "  add a dark mode toggle  " }),
        )
        .expect("observation");
    assert_eq!(prompt.signal, LifecycleSignal::TurnStarted);
    assert_eq!(prompt.prompt.as_deref(), Some("add a dark mode toggle"));
    assert_eq!(prompt.task.as_deref(), Some("add a dark mode toggle"));

    let injected = PiAdapter
        .observe_lifecycle(
            "before_agent_start",
            &json!({ "session_id": "sess-1", "prompt": "<system-reminder>noise" }),
        )
        .expect("observation");
    assert_eq!(injected.prompt, None);
    assert_eq!(injected.task, None);

    let skill = PiAdapter
        .observe_lifecycle(
            "before_agent_start",
            &json!({
                "session_id": "sess-1",
                "prompt": "<skill name=\"merge\" Location=\"/home/u/.agents/skills/merge/SKILL.md\">\nmerge the branch\n</skill>"
            }),
        )
        .expect("observation");
    assert_eq!(skill.prompt, None);
    assert_eq!(skill.task, None);

    let clean = PiAdapter
        .observe_lifecycle(
            "agent_settled",
            &json!({
                "session_id": "sess-1",
                "stop_reason": "stop",
                "model": "gpt-5",
                "total_tokens": 4200,
                "input_tokens": 100,
                "cache_write_input_tokens": 40,
                "cache_read_input_tokens": 30,
                "output_tokens": 20,
            }),
        )
        .expect("observation");
    assert_eq!(
        clean.signal,
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        }
    );
    assert_eq!(clean.launch.model.as_deref(), Some("gpt-5"));
    assert_eq!(clean.total_tokens, Some(4200));
    assert_eq!(clean.fresh_input_tokens, Some(100));
    assert_eq!(clean.cache_write_input_tokens, Some(40));
    assert_eq!(clean.cache_read_input_tokens, Some(30));
    assert_eq!(clean.output_tokens, Some(20));

    for (payload, expected) in [
        (
            json!({ "session_id": "sess-1", "stop_reason": "aborted" }),
            LifecycleSignal::TurnInterrupted,
        ),
        (
            json!({ "session_id": "sess-1", "stop_reason": "error" }),
            LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: false,
            },
        ),
        (
            json!({ "session_id": "sess-1", "stop_reason": "stop", "error_message": "boom" }),
            LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: false,
            },
        ),
    ] {
        let observation = PiAdapter
            .observe_lifecycle("agent_settled", &payload)
            .expect("observation");
        assert_eq!(observation.signal, expected, "payload {payload}",);
    }
}

#[test]
fn pi_carries_final_assistant_text_through_the_settled_boundary() {
    let payload = json!({
        "session_id": "sess-1",
        "last_assistant_message": "  Fixed the parser.  "
    });
    let observation = PiAdapter
        .observe_lifecycle("agent_settled", &payload)
        .expect("settled observation");

    assert_eq!(
        PiAdapter
            .last_assistant_message("agent_settled", &payload, &observation)
            .as_deref(),
        Some("Fixed the parser.")
    );
    assert_eq!(
        PiAdapter.last_assistant_message("agent_end", &payload, &observation),
        None,
        "agent_end is enrichment-only and must not complete output early"
    );
}

#[test]
fn pi_observes_rich_context_from_the_extension_envelope() {
    let context = normalized_context(json!({
        "model": "gpt-5.5",
        "session_name": "Parser cleanup",
        "effort": "high",
        "context_pct": 42,
        "context_window": 272_000,
        "total_tokens": 114_000,
        "total_cost_usd": 0.125,
        "input_tokens": 10,
        "cache_write_input_tokens": 4,
        "cache_read_input_tokens": 30,
        "output_tokens": 2,
        "rate_limits": [
            {
                "used_percentage": 72,
                "resets_at": 1_700_018_000i64,
                "duration_mins": 300,
                "observed_at": 1_700_000_000i64
            },
            {
                "used_percentage": 35,
                "resets_at": 1_700_604_800i64,
                "duration_mins": 10_080,
                "observed_at": 1_700_000_000i64
            }
        ]
    }))
    .expect("rich context");
    insta::assert_json_snapshot!(context, @r###"
        {
          "source": "pi",
          "session_name": "Parser cleanup",
          "model_id": "gpt-5.5",
          "effort": "high",
          "cost": {
            "total_cost_usd": 0.125
          },
          "tokens": {
            "context_window_size": 272000,
            "used_percentage": 42,
            "current_usage": {
              "input_tokens": 10,
              "output_tokens": 2,
              "cache_creation_input_tokens": 4,
              "cache_read_input_tokens": 30
            }
          },
          "rate_limits": {
            "windows": [
              {
                "used_percentage": 72,
                "resets_at": "2023-11-15T03:13:20Z",
                "duration_mins": 300,
                "observed_at": "2023-11-14T22:13:20Z"
              },
              {
                "used_percentage": 35,
                "resets_at": "2023-11-21T22:13:20Z",
                "duration_mins": 10080,
                "observed_at": "2023-11-14T22:13:20Z"
              }
            ]
          },
          "observed_at": "2023-11-14T22:13:20Z"
        }
        "###);

    let without_cost = normalized_context(json!({
        "context_pct": 7,
        "context_window": 128_000,
        "input_tokens": 9
    }))
    .expect("context without cost");
    assert!(without_cost.cost.is_none());
    assert_eq!(
        without_cost.tokens.as_ref().unwrap().used_percentage,
        Some(7)
    );

    let without_rate_limits = normalized_context(json!({
        "context_pct": 12,
        "context_window": 128_000,
        "input_tokens": 6,
        "output_tokens": 1
    }))
    .expect("context without windows");
    assert!(without_rate_limits.rate_limits.is_none());
    assert_eq!(
        without_rate_limits
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.current_usage.as_ref())
            .and_then(|usage| usage.input_tokens),
        Some(6)
    );

    let zero_split = normalized_context(json!({
        "context_pct": 0,
        "context_window": 128_000,
        "input_tokens": 0,
        "cache_write_input_tokens": 0,
        "cache_read_input_tokens": 0,
        "output_tokens": 0
    }))
    .expect("zero split still carries the window");
    assert!(
        zero_split.tokens.as_ref().unwrap().current_usage.is_none(),
        "all-zero token split drops the per-call breakdown"
    );

    assert!(
        PiAdapter
            .observe_context("pi", &json!({ "context_window": "not a number" }))
            .is_none(),
        "malformed context payloads degrade to no enrichment"
    );
}

#[test]
fn model_select_is_enrichment_only() {
    let payload = json!({ "session_id": "s", "model": "gpt-5.5", "effort": "high" });
    assert_eq!(
        PiAdapter.classify_hook("model_select", &payload).class,
        AgentHookClass::Lifecycle
    );
    assert!(
        PiAdapter
            .observe_lifecycle("model_select", &payload)
            .is_none()
    );
    assert_eq!(
        PiAdapter
            .observe_context("pi", &payload)
            .unwrap()
            .model_id
            .as_deref(),
        Some("gpt-5.5")
    );
}

fn normalized_context(payload: serde_json::Value) -> Option<AgentContext> {
    let mut context = PiAdapter.observe_context("pi", &payload)?;
    context.observed_at = jiff::Timestamp::from_second(1_700_000_000).unwrap();
    Some(context)
}

#[test]
fn pi_tool_compaction_shutdown_and_unknown_events_map_cleanly() {
    for (tool_name, expected) in [
        (
            "edit",
            Some(LifecycleSignal::ToolUsed {
                mutates: true,
                edits: true,
            }),
        ),
        (
            "bash",
            Some(LifecycleSignal::ToolUsed {
                mutates: true,
                edits: false,
            }),
        ),
        ("read", None),
    ] {
        let observed = PiAdapter.observe_lifecycle(
            "tool_execution_end",
            &json!({ "session_id": "sess-1", "tool_name": tool_name }),
        );
        assert_eq!(observed.map(|obs| obs.signal), expected, "{tool_name}");
    }

    let running = LifecycleState {
        status: AgentStatus::Running,
        phase: TurnPhase::Reasoning,
        compacting: false,
    };
    let edit = PiAdapter
        .observe_lifecycle(
            "tool_execution_end",
            &json!({ "session_id": "sess-1", "tool_name": "edit" }),
        )
        .expect("observation");
    assert_eq!(
        step(Some(&running), &edit.signal).next.phase,
        TurnPhase::Acting
    );

    let compacting = PiAdapter
        .observe_lifecycle("session_before_compact", &json!({ "session_id": "sess-1" }))
        .expect("observation");
    assert_eq!(compacting.signal, LifecycleSignal::Compacting);
    for (reason, expected) in [
        (Some("manual"), Some(false)),
        (Some("threshold"), Some(true)),
        (Some("overflow"), Some(true)),
        (Some("future"), None),
        (None, None),
    ] {
        let mut payload = json!({ "session_id": "sess-1" });
        if let Some(reason) = reason {
            payload["compaction_reason"] = json!(reason);
        }
        let compacted = PiAdapter
            .observe_lifecycle("session_compact", &payload)
            .expect("observation");
        assert_eq!(
            compacted.signal,
            LifecycleSignal::CompactionEnded { auto: expected },
            "{reason:?}"
        );
    }
    assert_eq!(
        PiAdapter.observe_lifecycle(
            "agent_end",
            &json!({ "session_id": "sess-1", "stop_reason": "error" }),
        ),
        None
    );
    let settled = PiAdapter
        .observe_lifecycle(
            "agent_settled",
            &json!({ "session_id": "sess-1", "stop_reason": "error" }),
        )
        .expect("observation");
    assert_eq!(
        settled.signal,
        LifecycleSignal::TurnEnded {
            errored: true,
            parked_on_background: false,
        }
    );
    let ended = PiAdapter
        .observe_lifecycle("session_shutdown", &json!({ "session_id": "sess-1" }))
        .expect("observation");
    assert_eq!(ended.signal, LifecycleSignal::Ended);

    assert_eq!(
        PiAdapter.observe_lifecycle("tool_call", &json!({ "session_id": "sess-1" })),
        None
    );
    assert_eq!(PiAdapter.observe_lifecycle("bogus", &json!({})), None);

    // The session-end and moved-on predicates track those signals: only a real
    // shutdown ends the session, and only the turn boundaries move the row on.
    assert!(PiAdapter.ends_session("session_shutdown"));
    assert!(!PiAdapter.ends_session("agent_end"));
    assert!(PiAdapter.moves_on("before_agent_start"));
    assert!(!PiAdapter.moves_on("agent_end"));
    assert!(PiAdapter.moves_on("agent_settled"));
    assert!(!PiAdapter.moves_on("session_start"));
}

#[test]
fn neutral_decision_shape_is_pinned() {
    let rendered = PiAdapter.render_neutral("agent_end").unwrap();
    insta::assert_snapshot!(format!("{rendered:?}"), @"None");
}

#[test]
fn install_preview_and_uninstall_only_own_managed_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("extensions").join("rimz.ts");

    let report = PI_MANAGED_SOURCE.install_into(&path).unwrap();
    assert_eq!(report.agent, "pi");
    assert!(!report.merged);
    assert_eq!(report.installed_events, managed_event_names());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), EXTENSION_SOURCE);
    assert!(PI_MANAGED_SOURCE.installed_at(&path));

    std::fs::write(&path, "// still _rimz_managed\n// user tweak\n").unwrap();
    assert!(PI_MANAGED_SOURCE.install_into(&path).unwrap().merged);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), EXTENSION_SOURCE);

    let preview = PI_MANAGED_SOURCE.preview_at(&path).unwrap();
    assert_eq!(preview.agent, "pi");
    assert!(preview.merged);
    assert_eq!(preview.candidate_config, EXTENSION_SOURCE);

    let removed = PI_MANAGED_SOURCE.uninstall_from(&path).unwrap();
    assert!(removed.existed);
    assert_eq!(removed.removed_events, managed_event_names());
    assert!(!path.exists());
    assert!(!PI_MANAGED_SOURCE.installed_at(&path));
    assert!(!PI_MANAGED_SOURCE.uninstall_from(&path).unwrap().existed);

    let user_path = dir.path().join("user.ts");
    std::fs::write(&user_path, "// the user's own extension\n").unwrap();
    assert!(matches!(
        PI_MANAGED_SOURCE.install_into(&user_path).unwrap_err(),
        AgentErr::Install { agent: "pi", .. }
    ));
    assert!(matches!(
        PI_MANAGED_SOURCE.preview_at(&user_path).unwrap_err(),
        AgentErr::Install { agent: "pi", .. }
    ));
    let report = PI_MANAGED_SOURCE.uninstall_from(&user_path).unwrap();
    assert!(report.existed);
    assert!(report.removed_events.is_empty());
    assert_eq!(
        std::fs::read_to_string(&user_path).unwrap(),
        "// the user's own extension\n"
    );
    assert!(!PI_MANAGED_SOURCE.installed_at(&user_path));
}

fn managed_event_names() -> Vec<String> {
    WIRED_EVENTS
        .iter()
        .map(|event| (*event).to_owned())
        .collect()
}

#[test]
fn extension_source_wires_every_event() {
    assert!(EXTENSION_SOURCE.contains("_rimz_managed"));
    assert!(EXTENSION_SOURCE.contains(r#"["hooks", "feed", "--source", "pi"]"#));
    assert!(EXTENSION_SOURCE.contains("RIMZ_AGENT_PID"));
    assert!(EXTENSION_SOURCE.contains("RIMZ_BIN"));
    assert!(EXTENSION_SOURCE.contains("PI_VERSION"));
    assert!(EXTENSION_SOURCE.contains("hasAgentSettled"));
    assert!(EXTENSION_SOURCE.contains("getContextUsage"));
    assert!(EXTENSION_SOURCE.contains("Math.round"));
    assert!(EXTENSION_SOURCE.contains("costBySession"));
    assert!(EXTENSION_SOURCE.contains("verdictBySession"));
    assert!(EXTENSION_SOURCE.contains("visibleAssistantText"));
    assert!(EXTENSION_SOURCE.contains("last_assistant_message"));
    assert!(EXTENSION_SOURCE.contains("total_cost_usd"));
    assert!(EXTENSION_SOURCE.contains("getBranch"));
    assert!(EXTENSION_SOURCE.contains("session_name"));
    assert!(EXTENSION_SOURCE.contains("messageSignature"));
    assert!(EXTENSION_SOURCE.contains("setTimeout"));
    assert!(EXTENSION_SOURCE.contains("cache_write_input_tokens"));
    assert!(EXTENSION_SOURCE.contains("rate_limits"));
    assert!(EXTENSION_SOURCE.contains("compaction_reason"));
    assert!(EXTENSION_SOURCE.contains("compaction_will_retry"));
    assert!(
        !EXTENSION_SOURCE.contains("addSessionCost(sessionId(ctx), last?.usage"),
        "agent_end's last message is the final turn_end usage and must not add cost again"
    );
    for event in WIRED_EVENTS {
        assert!(
            EXTENSION_SOURCE.contains(&format!("pi.on(\"{event}\"")),
            "extension registers {event}",
        );
    }
    assert!(EXTENSION_SOURCE.contains("block: true"));
    assert!(EXTENSION_SOURCE.contains(r#"ev?.reason === "reload""#));
}
