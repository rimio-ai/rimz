use super::*;

use crate::agents::lifecycle::{LifecycleState, TurnPhase, step};
use crate::feed::{AgentStatus, FeedKind, ResolutionMethod, Surface};
use crate::ids::WorkspaceId;
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
            "review this".to_owned(),
        ])
    );
}

#[test]
fn pi_render_preset_rejects_unsupported_launch_fields() {
    use crate::agents::{LaunchPreset, PresetErr};

    assert_eq!(
        PiAdapter.render_preset(&LaunchPreset {
            effort: Some("high".to_owned()),
            ..Default::default()
        }),
        Err(PresetErr::UnsupportedField {
            agent: "pi",
            field: "effort",
        })
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
    assert_eq!(started.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(started.effort.as_deref(), Some("medium"));
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

    let clean = PiAdapter
        .observe_lifecycle(
            "agent_end",
            &json!({
                "session_id": "sess-1",
                "stop_reason": "stop",
                "model": "gpt-5",
                "total_tokens": 4200,
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
    assert_eq!(clean.model.as_deref(), Some("gpt-5"));
    assert_eq!(clean.total_tokens, Some(4200));

    for payload in [
        json!({ "session_id": "sess-1", "stop_reason": "aborted" }),
        json!({ "session_id": "sess-1", "stop_reason": "error" }),
        json!({ "session_id": "sess-1", "stop_reason": "stop", "error_message": "boom" }),
    ] {
        let observation = PiAdapter
            .observe_lifecycle("agent_end", &payload)
            .expect("observation");
        assert_eq!(
            observation.signal,
            LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: false,
            },
            "payload {payload}",
        );
    }
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
    let compacted = PiAdapter
        .observe_lifecycle("session_compact", &json!({ "session_id": "sess-1" }))
        .expect("observation");
    assert_eq!(
        compacted.signal,
        LifecycleSignal::CompactionEnded { auto: None }
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
    assert!(PiAdapter.moves_on("agent_end"));
    assert!(!PiAdapter.moves_on("session_start"));
}

fn permission_item() -> FeedItem {
    crate::agents::testkit::feed_item(FeedKind::Permission, "pi")
}

#[test]
fn permission_and_neutral_decision_shapes_are_pinned() {
    let rendered = PiAdapter.render_neutral("agent_end").unwrap();
    insta::assert_snapshot!(format!("{rendered:?}"), @"None");

    for resolution in [
        Resolution::new(json!({ "choice": "allow" }), ResolutionMethod::HookBridge),
        Resolution::new(
            json!({ "choice": "allow", "updatedInput": { "command": "ls -la" } }),
            ResolutionMethod::HookBridge,
        ),
    ] {
        let rendered = PiAdapter
            .render_decision(&permission_item(), &resolution)
            .unwrap();
        assert_eq!(rendered, json!({}));
    }

    let mut reason_field =
        Resolution::new(json!({ "choice": "deny" }), ResolutionMethod::HookBridge);
    reason_field.reason = Some("rm -rf is not on the allowlist".to_owned());
    let rendered = PiAdapter
        .render_decision(&permission_item(), &reason_field)
        .unwrap();
    insta::assert_json_snapshot!(rendered, @r###"
        {
          "block": true,
          "reason": "rm -rf is not on the allowlist"
        }
        "###);

    for (resolution, expected_reason) in [
        (
            Resolution::new(
                json!({ "choice": "deny", "reason": "policy says no" }),
                ResolutionMethod::HookBridge,
            ),
            "policy says no",
        ),
        (
            Resolution::new(json!({ "choice": "deny" }), ResolutionMethod::HookBridge),
            "denied by resolver",
        ),
    ] {
        let rendered = PiAdapter
            .render_decision(&permission_item(), &resolution)
            .unwrap();
        assert_eq!(rendered["reason"], expected_reason);
        assert_eq!(rendered["block"], true);
    }

    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/rimz-test"));
    let item = FeedItem::new(
        workspace,
        Surface::Bridge,
        FeedKind::PlanApproval,
        "approve?",
        "pi",
        "agent-hook",
    );
    let resolution = Resolution::new(json!({ "choice": "allow" }), ResolutionMethod::HookBridge);
    assert!(matches!(
        PiAdapter.render_decision(&item, &resolution).unwrap_err(),
        AgentErr::Render { agent: "pi", .. }
    ));
}

#[test]
fn install_preview_and_uninstall_only_own_managed_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("extensions").join("rimz.ts");

    let report = install_into(&path).unwrap();
    assert_eq!(report.agent, "pi");
    assert!(!report.merged);
    assert_eq!(report.installed_events, installed_event_names());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), EXTENSION_SOURCE);
    assert!(hooks_installed_at(&path));

    std::fs::write(&path, "// still _rimz_managed\n// user tweak\n").unwrap();
    assert!(install_into(&path).unwrap().merged);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), EXTENSION_SOURCE);

    let preview = preview_install_at(&path).unwrap();
    assert_eq!(preview.agent, "pi");
    assert!(preview.merged);
    assert_eq!(preview.candidate_config, EXTENSION_SOURCE);

    let removed = uninstall_from(&path).unwrap();
    assert!(removed.existed);
    assert_eq!(removed.removed_events, installed_event_names());
    assert!(!path.exists());
    assert!(!hooks_installed_at(&path));
    assert!(!uninstall_from(&path).unwrap().existed);

    let user_path = dir.path().join("user.ts");
    std::fs::write(&user_path, "// the user's own extension\n").unwrap();
    assert!(matches!(
        install_into(&user_path).unwrap_err(),
        AgentErr::Install { agent: "pi", .. }
    ));
    assert!(matches!(
        preview_install_at(&user_path).unwrap_err(),
        AgentErr::Install { agent: "pi", .. }
    ));
    let report = uninstall_from(&user_path).unwrap();
    assert!(report.existed);
    assert!(report.removed_events.is_empty());
    assert_eq!(
        std::fs::read_to_string(&user_path).unwrap(),
        "// the user's own extension\n"
    );
    assert!(!hooks_installed_at(&user_path));
}

#[test]
fn extension_source_wires_every_event() {
    assert!(EXTENSION_SOURCE.contains("_rimz_managed"));
    assert!(EXTENSION_SOURCE.contains(r#"["hooks", "feed", "--source", "pi"]"#));
    assert!(EXTENSION_SOURCE.contains("RIMZ_AGENT_PID"));
    assert!(EXTENSION_SOURCE.contains("RIMZ_BIN"));
    assert!(EXTENSION_SOURCE.contains("getContextUsage"));
    assert!(EXTENSION_SOURCE.contains("Math.round"));
    for event in WIRED_EVENTS {
        assert!(
            EXTENSION_SOURCE.contains(&format!("pi.on(\"{event}\"")),
            "extension registers {event}",
        );
    }
    assert!(EXTENSION_SOURCE.contains("block: true"));
    assert!(EXTENSION_SOURCE.contains(r#"ev?.reason === "reload""#));
}
