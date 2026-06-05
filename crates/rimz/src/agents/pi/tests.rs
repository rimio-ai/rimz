use super::*;
use crate::agents::AgentHookClass;
use crate::agents::lifecycle::TurnPhase;
use crate::feed::{FeedKind, ResolutionMethod, Surface};
use crate::ids::WorkspaceId;
use serde_json::json;

#[test]
fn pi_classifies_the_blocking_gate_lifecycle_events_and_unknowns() {
    let tool_call = PiAdapter.classify_hook("tool_call", &Value::Null);
    assert_eq!(tool_call.class, AgentHookClass::BlockingFeed);
    assert_eq!(tool_call.feed_kind, Some(FeedKind::Permission));
    for event in LIFECYCLE_EVENTS {
        let classified = PiAdapter.classify_hook(event, &Value::Null);
        assert_eq!(classified.class, AgentHookClass::Lifecycle, "event {event}");
        assert_eq!(classified.feed_kind, None, "event {event} never blocks");
    }
    for event in ["PermissionRequest", "SessionStart", "bogus"] {
        let classified = PiAdapter.classify_hook(event, &Value::Null);
        assert_eq!(classified.class, AgentHookClass::Unknown, "event {event}");
    }
}

#[test]
fn pi_declares_its_surfaces() {
    let capabilities = PiAdapter.descriptor().capabilities;
    assert!(capabilities.blocking_feed);
    assert!(!capabilities.native_ask_ui);
    assert!(!capabilities.rate_limit_windows);
    assert!(!capabilities.subagents);
    assert!(!capabilities.background_tasks);
    assert!(capabilities.hook_install);
    assert!(PI_DESCRIPTOR.hook_install_unavailable.is_none());
}

#[test]
fn session_start_registers_with_worktree() {
    let observation = PiAdapter
        .observe_lifecycle(
            "session_start",
            &json!({ "session_id": "sess-1", "cwd": "/home/u/code/query-engine" }),
        )
        .expect("observation");
    assert_eq!(observation.agent_id.as_deref(), Some("sess-1"));
    assert_eq!(observation.signal, LifecycleSignal::Registered);
    assert_eq!(
        observation.worktree_path.as_deref(),
        Some("/home/u/code/query-engine"),
    );
    assert_eq!(observation.parent_agent_id, None);
}

#[test]
fn before_agent_start_starts_the_turn_with_the_sanitized_prompt() {
    let observation = PiAdapter
        .observe_lifecycle(
            "before_agent_start",
            &json!({ "session_id": "sess-1", "prompt": "  add a dark mode toggle  " }),
        )
        .expect("observation");
    assert_eq!(observation.signal, LifecycleSignal::TurnStarted);
    assert_eq!(
        observation.prompt.as_deref(),
        Some("add a dark mode toggle"),
    );
    assert_eq!(observation.task.as_deref(), Some("add a dark mode toggle"));
    // Harness control text never labels a row.
    let injected = PiAdapter
        .observe_lifecycle(
            "before_agent_start",
            &json!({ "session_id": "sess-1", "prompt": "<system-reminder>noise" }),
        )
        .expect("observation");
    assert_eq!(injected.prompt, None);
    assert_eq!(injected.task, None);
}

#[test]
fn agent_end_completes_the_turn_with_model_and_tokens() {
    let observation = PiAdapter
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
        observation.signal,
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        },
    );
    assert_eq!(observation.model.as_deref(), Some("gpt-5"));
    assert_eq!(observation.total_tokens, Some(4200));
}

#[test]
fn context_gauge_rides_every_envelope_payload_first() {
    // The extension stamps the gauge on every event; the adapter reads it
    // straight off the payload — here a mid-turn registration.
    let observation = PiAdapter
        .observe_lifecycle(
            "session_start",
            &json!({
                "session_id": "sess-1",
                "model": "gpt-5.5",
                "effort": "medium",
                "context_pct": 3,
                "context_window": 272_000,
                "total_tokens": 8160,
            }),
        )
        .expect("observation");
    assert_eq!(observation.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(observation.effort.as_deref(), Some("medium"));
    assert_eq!(observation.context_pct, Some(3));
    assert_eq!(observation.context_window, Some(272_000));
    assert_eq!(observation.total_tokens, Some(8160));

    // No payload gauge means no gauge — there is no transcript fallback.
    let bare = PiAdapter
        .observe_lifecycle("session_start", &json!({ "session_id": "sess-1" }))
        .expect("observation");
    assert_eq!(bare.context_pct, None);
    assert_eq!(bare.context_window, None);
    assert_eq!(bare.total_tokens, None);

    // The shared override helper clamps a wire glitch to a sane percent.
    let clamped = PiAdapter
        .observe_lifecycle(
            "session_start",
            &json!({ "session_id": "sess-1", "context_pct": 150 }),
        )
        .expect("observation");
    assert_eq!(clamped.context_pct, Some(100));
}

#[test]
fn agent_end_carries_the_in_band_error_bit() {
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
fn tool_execution_end_maps_the_mutating_subset() {
    // `edit` writes files — the acting transition.
    let edit = PiAdapter
        .observe_lifecycle(
            "tool_execution_end",
            &json!({ "session_id": "sess-1", "tool_name": "edit" }),
        )
        .expect("observation");
    assert_eq!(
        edit.signal,
        LifecycleSignal::ToolUsed {
            mutates: true,
            edits: true,
        },
    );
    // `bash` mutates without editing — the reasoning phase survives.
    let bash = PiAdapter
        .observe_lifecycle(
            "tool_execution_end",
            &json!({ "session_id": "sess-1", "tool_name": "bash" }),
        )
        .expect("observation");
    assert_eq!(
        bash.signal,
        LifecycleSignal::ToolUsed {
            mutates: true,
            edits: false,
        },
    );
    // Read-only tools stay silent.
    assert_eq!(
        PiAdapter.observe_lifecycle(
            "tool_execution_end",
            &json!({ "session_id": "sess-1", "tool_name": "read" }),
        ),
        None,
    );
}

/// The descriptor's `edits` split drives the shared phase machine: the
/// first `edit` of a running turn moves reasoning → acting.
#[test]
fn an_edit_tool_ends_the_reasoning_phase() {
    use crate::agents::lifecycle::{LifecycleState, step};
    use crate::feed::AgentStatus;
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
    let next = step(Some(&running), &edit.signal);
    assert_eq!(next.next.phase, TurnPhase::Acting);
}

#[test]
fn compaction_and_shutdown_signals() {
    let compacting = PiAdapter
        .observe_lifecycle("session_before_compact", &json!({ "session_id": "sess-1" }))
        .expect("observation");
    assert_eq!(compacting.signal, LifecycleSignal::Compacting);
    let ended = PiAdapter
        .observe_lifecycle("session_shutdown", &json!({ "session_id": "sess-1" }))
        .expect("observation");
    assert_eq!(ended.signal, LifecycleSignal::Ended);
}

#[test]
fn unrecognized_events_observe_nothing() {
    assert_eq!(
        PiAdapter.observe_lifecycle("tool_call", &json!({ "session_id": "sess-1" })),
        None,
    );
    assert_eq!(PiAdapter.observe_lifecycle("bogus", &json!({})), None);
}

#[test]
fn session_boundaries_end_and_move_on() {
    assert!(PiAdapter.ends_session("session_shutdown"));
    assert!(!PiAdapter.ends_session("agent_end"));
    assert!(PiAdapter.moves_on("before_agent_start"));
    assert!(PiAdapter.moves_on("agent_end"));
    assert!(!PiAdapter.moves_on("session_start"));
}

#[test]
fn progress_events_touch_the_activity_heartbeat() {
    let descriptor = PiAdapter.descriptor();
    for event in [
        "session_start",
        "before_agent_start",
        "agent_end",
        "tool_execution_end",
    ] {
        assert!(descriptor.records_activity(event), "event {event}");
    }
    // The blocking gate races the ask it creates; a shutdown is an end,
    // not progress.
    assert!(!descriptor.records_activity("tool_call"));
    assert!(!descriptor.records_activity("session_shutdown"));
    assert!(!descriptor.records_activity("session_before_compact"));
}

#[test]
fn resume_command_is_pi_with_the_session_id() {
    assert_eq!(
        PiAdapter.resume_command("0199aaf2", Path::new("/tmp")),
        Some(vec![
            "pi".to_owned(),
            "--session".to_owned(),
            "0199aaf2".to_owned(),
        ]),
    );
}

/// Empty stdout is pi's neutral: the extension's child is fire-and-forget
/// and nothing reads it. Golden so the shape never drifts.
#[test]
fn render_neutral_prints_nothing() {
    let rendered = PiAdapter.render_neutral("agent_end").unwrap();
    insta::assert_snapshot!(format!("{rendered:?}"), @"None");
}

fn permission_item() -> FeedItem {
    crate::agents::testkit::feed_item(FeedKind::Permission, "pi")
}

#[test]
fn permission_allow_shape_is_pinned() {
    let resolution = Resolution::new(json!({ "choice": "allow" }), ResolutionMethod::HookBridge);
    let rendered = PiAdapter
        .render_decision(&permission_item(), &resolution)
        .unwrap();
    insta::assert_json_snapshot!(rendered, @"{}");
}

#[test]
fn permission_allow_ignores_updated_input() {
    // Pi mutates tool args only in-process; the bridge can't, so an
    // updatedInput riding the resolution renders as a plain allow.
    let resolution = Resolution::new(
        json!({ "choice": "allow", "updatedInput": { "command": "ls -la" } }),
        ResolutionMethod::HookBridge,
    );
    let rendered = PiAdapter
        .render_decision(&permission_item(), &resolution)
        .unwrap();
    insta::assert_json_snapshot!(rendered, @"{}");
}

#[test]
fn permission_deny_with_reason_shape_is_pinned() {
    let mut resolution = Resolution::new(json!({ "choice": "deny" }), ResolutionMethod::HookBridge);
    resolution.reason = Some("rm -rf is not on the allowlist".to_owned());
    let rendered = PiAdapter
        .render_decision(&permission_item(), &resolution)
        .unwrap();
    insta::assert_json_snapshot!(rendered, @r###"
        {
          "block": true,
          "reason": "rm -rf is not on the allowlist"
        }
        "###);
}

#[test]
fn permission_deny_reads_decision_reason_then_defaults() {
    let resolution = Resolution::new(
        json!({ "choice": "deny", "reason": "policy says no" }),
        ResolutionMethod::HookBridge,
    );
    let rendered = PiAdapter
        .render_decision(&permission_item(), &resolution)
        .unwrap();
    insta::assert_json_snapshot!(rendered, @r###"
        {
          "block": true,
          "reason": "policy says no"
        }
        "###);

    let bare = Resolution::new(json!({ "choice": "deny" }), ResolutionMethod::HookBridge);
    let rendered = PiAdapter
        .render_decision(&permission_item(), &bare)
        .unwrap();
    insta::assert_json_snapshot!(rendered, @r###"
        {
          "block": true,
          "reason": "denied by resolver"
        }
        "###);
}

#[test]
fn non_permission_kinds_refuse_to_render() {
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
    let err = PiAdapter.render_decision(&item, &resolution).unwrap_err();
    assert!(matches!(err, AgentErr::Render { agent: "pi", .. }));
}

#[test]
fn install_round_trip_owns_the_marked_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("extensions").join("rimz.ts");

    let report = install_into(&path).unwrap();
    assert_eq!(report.agent, "pi");
    assert!(!report.merged, "fresh install creates the file");
    assert_eq!(report.installed_events, installed_event_names());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), EXTENSION_SOURCE);
    assert!(hooks_installed_at(&path));

    // Re-install over a *marked* file — however edited since — reclaims
    // it verbatim. The marker on line one is what says "Rimz wrote this".
    std::fs::write(&path, "// still _rimz_managed\n// user tweak\n").unwrap();
    let again = install_into(&path).unwrap();
    assert!(again.merged, "reclaimed an existing managed file");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), EXTENSION_SOURCE);

    let removed = uninstall_from(&path).unwrap();
    assert!(removed.existed);
    assert_eq!(removed.removed_events, installed_event_names());
    assert!(!path.exists());
    assert!(!hooks_installed_at(&path));

    // Uninstall on a missing file is a clean no-op.
    let missing = uninstall_from(&path).unwrap();
    assert!(!missing.existed);
    assert!(missing.removed_events.is_empty());
}

#[test]
fn unmarked_user_extension_is_never_clobbered() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rimz.ts");
    std::fs::write(&path, "// the user's own extension\n").unwrap();

    // Install and preview both refuse, and the file is untouched.
    let install_err = install_into(&path).unwrap_err();
    assert!(matches!(install_err, AgentErr::Install { agent: "pi", .. }));
    let preview_err = preview_install_at(&path).unwrap_err();
    assert!(matches!(preview_err, AgentErr::Install { agent: "pi", .. }));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "// the user's own extension\n",
    );

    // Uninstall leaves the user's file in place and reports it removed
    // nothing.
    let report = uninstall_from(&path).unwrap();
    assert!(report.existed);
    assert!(report.removed_events.is_empty());
    assert!(path.exists());
}

#[test]
fn preview_carries_the_embedded_source_without_touching_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rimz.ts");
    let preview = preview_install_at(&path).unwrap();
    assert_eq!(preview.agent, "pi");
    assert_eq!(preview.planned_events, installed_event_names());
    assert_eq!(preview.original_config, None);
    assert_eq!(preview.candidate_config, EXTENSION_SOURCE);
    assert!(!preview.merged);
    assert_eq!(preview.status_line_change, None);
    assert_eq!(preview.subagent_status_line_change, None);
    assert!(!path.exists(), "preview never writes");

    std::fs::write(&path, "// _rimz_managed (an older build)\n").unwrap();
    let over = preview_install_at(&path).unwrap();
    assert!(
        over.merged,
        "an existing managed file is reported reclaimed"
    );
    assert_eq!(
        over.original_config.as_deref(),
        Some("// _rimz_managed (an older build)\n"),
    );
}

#[test]
fn hooks_installed_requires_the_managed_marker() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rimz.ts");
    assert!(!hooks_installed_at(&path), "missing file is not installed");
    std::fs::write(&path, "export default function user(pi) {}\n").unwrap();
    assert!(
        !hooks_installed_at(&path),
        "a user's own extension at the path is not Rimz's",
    );
}

/// The embedded extension and this adapter agree: the marker, the feed
/// command, and every wired event registration are present in the source.
#[test]
fn extension_source_wires_every_event() {
    assert!(EXTENSION_SOURCE.contains("_rimz_managed"));
    assert!(EXTENSION_SOURCE.contains(r#"["hooks", "feed", "--source", "pi"]"#));
    assert!(EXTENSION_SOURCE.contains("RIMZ_AGENT_PID"));
    // The hook child honours a binary override so tests and unusual
    // PATHs can pin the rimz the extension spawns.
    assert!(EXTENSION_SOURCE.contains("RIMZ_BIN"));
    // The gauge rides every envelope, rounded to the integers the
    // adapter parses.
    assert!(EXTENSION_SOURCE.contains("getContextUsage"));
    assert!(EXTENSION_SOURCE.contains("Math.round"));
    for event in WIRED_EVENTS {
        assert!(
            EXTENSION_SOURCE.contains(&format!("pi.on(\"{event}\"")),
            "extension registers {event}",
        );
    }
    // The blocking gate renders pi's ToolCallEventResult deny shape.
    assert!(EXTENSION_SOURCE.contains("block: true"));
    // The /reload shutdown is skipped — its tombstone would race the
    // same-id re-register.
    assert!(EXTENSION_SOURCE.contains(r#"ev?.reason === "reload""#));
}
