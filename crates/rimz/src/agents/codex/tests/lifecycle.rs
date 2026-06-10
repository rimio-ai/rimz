use super::*;

#[test]
fn session_start_observes_idle() {
    let obs = CodexAdapter
        .observe_lifecycle("SessionStart", &json!({ "session_id": "sess-1" }))
        .unwrap();
    assert_eq!(obs.agent_id.as_deref(), Some("sess-1"));
    // Wired in, nothing asked yet — a plain startup registers fresh (not a
    // compaction), no task.
    assert_eq!(obs.signal, LifecycleSignal::Registered);
    assert_eq!(obs.task, None);
}

#[test]
fn session_start_source_maps_to_registration_or_compaction_end() {
    let compact = CodexAdapter
        .observe_lifecycle(
            "SessionStart",
            &json!({ "session_id": "sess-1", "source": "compact" }),
        )
        .unwrap();
    assert_eq!(
        compact.signal,
        LifecycleSignal::CompactionEnded { auto: None }
    );
    for source in ["startup", "resume", "clear", "future"] {
        let obs = CodexAdapter
            .observe_lifecycle(
                "SessionStart",
                &json!({ "session_id": "sess-1", "source": source }),
            )
            .unwrap();
        assert_eq!(
            obs.signal,
            LifecycleSignal::Registered,
            "{source} is not a compaction",
        );
    }
}

#[test]
fn lifecycle_classification_covers_installed_nonblocking_events() {
    let expected = INSTALLED_EVENTS
        .iter()
        .map(|(event, _)| *event)
        .filter(|event| *event != "PermissionRequest")
        .collect::<std::collections::BTreeSet<_>>();
    let actual = LIFECYCLE_EVENTS
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(actual, expected);
}

#[test]
fn compaction_pair_maps_to_lifecycle_signals() {
    for event in ["PreCompact", "PostCompact"] {
        let c = CodexAdapter.classify_hook(event, &json!({ "session_id": "sess-1" }));
        assert_eq!(c.class, AgentHookClass::Lifecycle, "{event}");
        assert_eq!(c.feed_kind, None, "{event}");
    }

    let pre = CodexAdapter
        .observe_lifecycle(
            "PreCompact",
            &json!({ "session_id": "sess-1", "trigger": "manual" }),
        )
        .unwrap();
    assert_eq!(pre.signal, LifecycleSignal::Compacting);

    let auto = CodexAdapter
        .observe_lifecycle(
            "PostCompact",
            &json!({ "session_id": "sess-1", "trigger": "auto" }),
        )
        .unwrap();
    assert_eq!(
        auto.signal,
        LifecycleSignal::CompactionEnded { auto: Some(true) }
    );

    let manual = CodexAdapter
        .observe_lifecycle(
            "PostCompact",
            &json!({ "session_id": "sess-1", "trigger": "manual" }),
        )
        .unwrap();
    assert_eq!(
        manual.signal,
        LifecycleSignal::CompactionEnded { auto: Some(false) }
    );

    for payload in [
        json!({ "session_id": "sess-1", "trigger": "future" }),
        json!({ "session_id": "sess-1" }),
    ] {
        let obs = CodexAdapter
            .observe_lifecycle("PostCompact", &payload)
            .unwrap();
        assert_eq!(
            obs.signal,
            LifecycleSignal::CompactionEnded { auto: None },
            "{payload}"
        );
    }
}

#[test]
fn user_prompt_submit_observes_running_with_prompt_task() {
    let obs = CodexAdapter
        .observe_lifecycle(
            "UserPromptSubmit",
            &json!({ "session_id": "sess-1", "prompt": "fix auth flow" }),
        )
        .unwrap();
    assert_eq!(obs.agent_id.as_deref(), Some("sess-1"));
    assert_eq!(obs.signal, LifecycleSignal::TurnStarted);
    assert_eq!(obs.task.as_deref(), Some("fix auth flow"));
}

#[test]
fn subagent_start_observes_child_id_and_type() {
    let obs = CodexAdapter
        .observe_lifecycle(
            "SubagentStart",
            &json!({
                "session_id": "sess-parent",
                "agent_id": "child-thread-1",
                "agent_type": "review",
            }),
        )
        .unwrap();

    assert_eq!(obs.agent_id.as_deref(), Some("child-thread-1"));
    assert_eq!(obs.signal, LifecycleSignal::SubagentStarted);
    assert_eq!(obs.task.as_deref(), Some("review"));
    // The child keys off `agent_id`; the payload's `session_id` is its parent
    // root, captured so the sidebar can nest it.
    assert_eq!(obs.parent_agent_id.as_deref(), Some("sess-parent"));
}

#[test]
fn subagent_stop_resolves_success_child_id() {
    let obs = CodexAdapter
        .observe_lifecycle(
            "SubagentStop",
            &json!({
                "session_id": "sess-parent",
                "agent_id": "child-thread-1",
                "agent_type": "review",
            }),
        )
        .unwrap();

    assert_eq!(obs.agent_id.as_deref(), Some("child-thread-1"));
    // Codex reports no subagent error signal, so a stop is always clean.
    assert_eq!(
        obs.signal,
        LifecycleSignal::SubagentStopped { errored: false }
    );
    // The type label persists across stop so a finished child stays labeled
    // while it lingers in the parent's list.
    assert_eq!(obs.task.as_deref(), Some("review"));
    assert_eq!(obs.parent_agent_id.as_deref(), Some("sess-parent"));
}

#[test]
fn foreign_child_root_lifecycle_events_are_dropped() {
    // A non-Subagent* event carrying a distinct `agent_id` fired inside a
    // subagent is dropped rather than keyed as a parentless phantom root.
    // Latent today — Codex stamps `agent_id` only on Subagent* — but the root
    // arm shares Claude's rule so the door stays closed.
    for (event, payload, why) in [
        (
            "PostToolUse",
            json!({
                "session_id": "sess-parent",
                "agent_id": "child-thread-1",
                "tool_name": "shell",
            }),
            "a foreign-id tool event never creates a parentless root row",
        ),
        (
            "PreToolUse",
            json!({
                "session_id": "sess-parent",
                "agent_id": "child-thread-1",
                "tool_name": "shell",
            }),
            "a foreign-id pre-tool event never creates a root row",
        ),
        (
            "PostCompact",
            json!({
                "session_id": "sess-parent",
                "agent_id": "child-thread-1",
                "trigger": "auto",
            }),
            "a foreign-id compaction end never creates a root row",
        ),
    ] {
        let obs = CodexAdapter.observe_lifecycle(event, &payload);
        assert!(obs.is_none(), "{event}: {why}");
    }
}

#[test]
fn root_post_tool_use_without_foreign_id_is_observed() {
    let root = CodexAdapter
        .observe_lifecycle(
            "PostToolUse",
            &json!({ "session_id": "sess-parent", "tool_name": "shell" }),
        )
        .unwrap();
    assert_eq!(root.agent_id.as_deref(), Some("sess-parent"));
    assert_eq!(root.parent_agent_id, None);
}

#[test]
fn pre_tool_use_observes_proof_of_work() {
    let obs = CodexAdapter
        .observe_lifecycle(
            "PreToolUse",
            &json!({ "session_id": "sess-1", "tool_name": "shell" }),
        )
        .unwrap();
    assert_eq!(
        obs.signal,
        LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
        }
    );
}

#[test]
fn non_mutating_post_tool_use_stays_out_of_lifecycle() {
    let obs = CodexAdapter.observe_lifecycle(
        "PostToolUse",
        &json!({ "session_id": "sess-1", "tool_name": "read" }),
    );
    assert!(
        obs.is_none(),
        "PostToolUse only emits ToolUsed from the mutating arm"
    );
}

#[test]
fn clean_stop_observes_success() {
    let obs = CodexAdapter
        .observe_lifecycle("Stop", &json!({ "session_id": "sess-1" }))
        .unwrap();
    assert_eq!(
        obs.signal,
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        }
    );
}

#[test]
fn errored_stop_observes_failed() {
    let obs = CodexAdapter
        .observe_lifecycle(
            "Stop",
            &json!({ "session_id": "sess-1", "status": "failed" }),
        )
        .unwrap();
    assert_eq!(
        obs.signal,
        LifecycleSignal::TurnEnded {
            errored: true,
            parked_on_background: false,
        }
    );
}

#[test]
fn expiry_predicates_match_observed_root_signals() {
    for (event, payload) in [
        ("SessionStart", json!({ "session_id": "sess-1" })),
        (
            "SessionStart",
            json!({ "session_id": "sess-1", "source": "compact" }),
        ),
        ("UserPromptSubmit", json!({ "session_id": "sess-1" })),
        ("Stop", json!({ "session_id": "sess-1" })),
        (
            "PostToolUse",
            json!({ "session_id": "sess-1", "tool_name": "shell" }),
        ),
        ("PreToolUse", json!({ "session_id": "sess-1" })),
        ("PreCompact", json!({ "session_id": "sess-1" })),
        ("PostCompact", json!({ "session_id": "sess-1" })),
    ] {
        let obs = CodexAdapter
            .observe_lifecycle(event, &payload)
            .unwrap_or_else(|| panic!("{event} should be observed"));
        assert_eq!(
            CodexAdapter.ends_session(event),
            matches!(obs.signal, LifecycleSignal::Ended),
            "{event} session-end predicate"
        );
        assert_eq!(
            CodexAdapter.moves_on(event),
            matches!(
                obs.signal,
                LifecycleSignal::TurnStarted | LifecycleSignal::TurnEnded { .. }
            ),
            "{event} moved-on predicate",
        );
    }
}

#[test]
fn classification_unchanged_for_unknown_event() {
    let c = CodexAdapter.classify_hook("WatItIs", &Value::Null);
    assert_eq!(c.class, AgentHookClass::Unknown);
    assert!(c.feed_kind.is_none());
}

#[test]
fn post_lifecycle_refresh_fires_on_turn_boundaries_only() {
    let ctx = crate::agents::LifecycleRefreshCtx {
        agent_id: "sess-1",
        workspace_id: "ws-1",
        model_hint: Some("gpt-5"),
    };
    let spawn = CodexAdapter
        .post_lifecycle_refresh("Stop", &ctx)
        .expect("Stop refreshes");
    assert_eq!(
        spawn.args,
        [
            "codex",
            "refresh-context",
            "--session-id",
            "sess-1",
            "--workspace-id",
            "ws-1",
            "--model",
            "gpt-5",
        ]
    );
    // No model hint → no --model flag.
    let bare = crate::agents::LifecycleRefreshCtx {
        model_hint: None,
        ..ctx
    };
    let spawn = CodexAdapter
        .post_lifecycle_refresh("SessionStart", &bare)
        .expect("SessionStart refreshes");
    assert!(!spawn.args.iter().any(|arg| arg == "--model"));
    // Per-tool events stay silent here — an app-server spawn per call is too frequent.
    // The cheap local transcript refresh is a separate inline lane.
    for event in ["PreToolUse", "PostToolUse", "SubagentStop", "Notification"] {
        assert!(
            CodexAdapter.post_lifecycle_refresh(event, &ctx).is_none(),
            "{event}"
        );
    }
}

#[test]
fn local_context_refresh_fires_for_progress_events_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-session.jsonl");
    std::fs::write(
        &path,
        "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5\"}}\n\
             {\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\
             \"last_token_usage\":{\"input_tokens\":500,\"cached_input_tokens\":300,\
             \"output_tokens\":20,\"total_tokens\":520},\
             \"model_context_window\":1000}}}\n",
    )
    .unwrap();
    let path = path.to_string_lossy().into_owned();
    let ctx = crate::agents::LocalContextRefreshCtx {
        agent_id: "sess-1",
        model_hint: Some("gpt-5"),
        prior_effort: None,
        prior_transcript_path: Some(&path),
        prior_transcript_stat: None,
    };

    let refresh = CodexAdapter
        .local_context_refresh("PostToolUse", &ctx)
        .expect("PostToolUse reads local transcript context");
    assert_eq!(
        refresh
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.used_percentage),
        Some(50)
    );
    assert!(
        CodexAdapter
            .local_context_refresh("PreToolUse", &ctx)
            .is_none()
    );
    assert!(
        CodexAdapter
            .local_context_refresh("PermissionRequest", &ctx)
            .is_none()
    );
}

#[test]
fn codex_hook_cap_is_shorter_than_claude_default() {
    use crate::agents::ClaudeAdapter;
    assert!(CodexAdapter.descriptor().hook_cap < ClaudeAdapter.descriptor().hook_cap);
}
