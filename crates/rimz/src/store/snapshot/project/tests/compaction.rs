use super::*;

fn lifecycle(source: &str, event_name: &str, signal: serde_json::Value) -> EventEnvelope {
    raw_lifecycle(
        source,
        serde_json::json!({
            "event_name": event_name,
            "agent_id": "sess-1",
            "signal": signal,
        }),
    )
}

fn lifecycle_at(
    source: &str,
    secs_after_epoch: i64,
    event_name: &str,
    signal: serde_json::Value,
) -> EventEnvelope {
    raw_lifecycle_at(
        source,
        secs_after_epoch,
        serde_json::json!({
            "event_name": event_name,
            "agent_id": "sess-1",
            "signal": signal,
        }),
    )
}

fn signal(name: &str) -> serde_json::Value {
    serde_json::json!({ "signal": name })
}

fn compaction_ended(auto: Option<bool>) -> serde_json::Value {
    let mut signal = signal("compaction_ended");
    if let Some(auto) = auto {
        signal["auto"] = serde_json::Value::Bool(auto);
    }
    signal
}

fn compaction_failed(auto: Option<bool>) -> serde_json::Value {
    let mut signal = compaction_ended(auto);
    signal["failed"] = serde_json::Value::Bool(true);
    signal
}

fn tool_used() -> serde_json::Value {
    serde_json::json!({ "signal": "tool_used", "mutates": true, "edits": true })
}

fn lifecycle_for_agent(
    agent_id: &str,
    event_name: &str,
    signal: serde_json::Value,
) -> EventEnvelope {
    raw_lifecycle(
        "codex",
        serde_json::json!({
            "event_name": event_name,
            "agent_id": agent_id,
            "signal": signal,
        }),
    )
}

#[test]
fn compacting_for_unknown_session_folds_to_nothing() {
    let compact = lifecycle_for_agent("fresh-compact", "PreCompact", signal("compacting"));

    assert!(reduce_agent_states(&[compact]).is_empty());
}

#[test]
fn compaction_ended_for_unknown_session_folds_to_nothing() {
    let post = lifecycle_for_agent("fresh-compact", "SessionStart", compaction_ended(None));

    assert!(reduce_agent_states(&[post]).is_empty());
}

#[test]
fn linked_compaction_end_seeds_and_carries_the_continuation() {
    let pane_id = "tmux:%compact";
    let owner_pid = 42;
    let mut launch = launch_event(
        "codex",
        AgentLaunchPayload {
            launch_id: Some(AgentSessionId::from("launch_coder")),
            launch: LaunchParams {
                checkout: Some("/repo".to_owned()),
                role: Some("coder".to_owned()),
                isolation: Some(crate::config::Isolation::Host),
                ..LaunchParams::default()
            },
            pane_id: Some(PaneId::parse(pane_id).expect("pane id")),
            runtime_owner: Some(RuntimeOwner::new(
                RuntimeOwnerKind::Agent,
                "launch_coder",
                owner_pid,
                Some("agent-start".to_owned()),
            )),
            ..launch_payload("launch_coder", "coder-card")
        },
    );
    launch.timestamp = epoch();
    let predecessor = raw_lifecycle_at(
        "codex",
        1,
        serde_json::json!({
            "event_name": "Stop",
            "agent_id": "predecessor",
            "agent_name": "coder-card",
            "pane_id": pane_id,
            "pane_process_start": "pane-start",
            "runtime_owner": {
                "kind": "agent",
                "subject_id": "predecessor",
                "pid": owner_pid,
                "process_start": "agent-start",
            },
            "signal": {
                "signal": "turn_ended",
                "errored": false,
                "parked_on_background": false
            },
        }),
    );
    let continuation = raw_lifecycle_at(
        "codex",
        2,
        serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "continuation",
            "compacted_from": "predecessor",
            "pane_id": pane_id,
            "pane_process_start": "pane-start",
            "runtime_owner": {
                "kind": "agent",
                "subject_id": "continuation",
                "pid": owner_pid,
                "process_start": "agent-start",
            },
            "signal": { "signal": "compaction_ended" },
        }),
    );
    let continued_turn = raw_lifecycle_at(
        "codex",
        3,
        serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "continuation",
            "signal": { "signal": "turn_started" },
        }),
    );

    let attach = EventEnvelope::agent_attached(
        workspace(),
        "session",
        &AgentKind::new_unchecked("codex"),
        AgentAttachPayload {
            epoch: None,
            login: None,
            record: None,
            tier: None,
            mode: None,
            effective_isolation: None,
            agent_id: AgentSessionId::from("predecessor"),
            launch_id: Some(AgentSessionId::from("launch_coder")),
            isolation: Some(crate::config::Isolation::Scoped),
            pane_id: PaneId::parse(pane_id).expect("pane id"),
            pane_pid: Some(owner_pid),
            runtime_owner: RuntimeOwner::new(
                RuntimeOwnerKind::Agent,
                "predecessor",
                owner_pid,
                Some("agent-start".to_owned()),
            ),
        },
    );
    let events = vec![launch, predecessor, attach];
    let attached = reduce_agent_states(&events);
    assert_eq!(
        attached
            .iter()
            .find(|agent| agent.agent_id == "predecessor")
            .unwrap()
            .isolation,
        Some(crate::config::Isolation::Scoped),
    );
    let mut events = events;
    events.push(lifecycle_for_agent(
        "predecessor",
        "PreCompact",
        signal("compacting"),
    ));
    events.push(continuation);
    let after_start = reduce_agent_states(&events);
    let predecessor_state = after_start
        .iter()
        .find(|agent| agent.agent_id == "predecessor")
        .unwrap();
    let continuation_state = after_start
        .iter()
        .find(|agent| agent.agent_id == "continuation")
        .unwrap();
    assert_eq!(
        continuation_state.compacted_from.as_deref(),
        Some("predecessor")
    );
    assert_eq!(
        continuation_state.registered_at, predecessor_state.registered_at,
        "the continuation inherits pane primacy"
    );
    assert_eq!(
        continuation_state.launch_id.as_deref(),
        Some("launch_coder")
    );
    assert_eq!(continuation_state.role.as_deref(), Some("coder"));
    assert_eq!(continuation_state.checkout.as_deref(), Some("/repo"));
    assert_eq!(
        predecessor_state.isolation,
        Some(crate::config::Isolation::Scoped)
    );
    assert_eq!(
        continuation_state.isolation,
        Some(crate::config::Isolation::Scoped)
    );

    events.push(continued_turn);
    let after_turn = reduce_agent_states(&events);
    let continuation_state = after_turn
        .iter()
        .find(|agent| agent.agent_id == "continuation")
        .unwrap();
    assert_eq!(continuation_state.checkout.as_deref(), Some("/repo"));
    assert_eq!(
        continuation_state.isolation,
        Some(crate::config::Isolation::Scoped)
    );
    assert_eq!(
        continuation_state.compacted_from.as_deref(),
        Some("predecessor"),
        "sparse later observations keep the predecessor link"
    );
}

#[test]
fn compacting_after_registration_still_stamps_the_head() {
    let registered = lifecycle_for_agent("session-a", "SessionStart", signal("registered"));
    let compact = lifecycle_for_agent("session-a", "PreCompact", signal("compacting"));

    let agents = reduce_agent_states(&[registered, compact]);

    assert_eq!(agents.len(), 1);
    assert!(agents[0].compacting_since.is_some());
}

#[test]
fn aborted_compaction_rotation_does_not_create_a_ghost_session() {
    let original = lifecycle_for_agent("session-a", "SessionStart", signal("registered"));
    let aborted_rotation = lifecycle_for_agent("session-b", "PreCompact", signal("compacting"));
    let replacement = lifecycle_for_agent("session-c", "SessionStart", signal("registered"));

    let agents = reduce_agent_states(&[original, aborted_rotation, replacement]);

    assert_eq!(agents.len(), 2);
    assert!(agents.iter().any(|agent| agent.agent_id == "session-a"));
    assert!(agents.iter().any(|agent| agent.agent_id == "session-c"));
    assert!(agents.iter().all(|agent| agent.agent_id != "session-b"));
}

#[test]
fn compaction_end_clears_marker_and_counts_completed_brackets() {
    let prompt = lifecycle("claude", "UserPromptSubmit", signal("turn_started"));
    let compact = lifecycle("claude", "PreCompact", signal("compacting"));
    let post = lifecycle("claude", "PostCompact", compaction_ended(Some(false)));
    let next_prompt = lifecycle("claude", "UserPromptSubmit", signal("turn_started"));
    let second_compact = lifecycle("claude", "PreCompact", signal("compacting"));
    let second_post = lifecycle("claude", "PostCompact", compaction_ended(Some(true)));

    let agents = reduce_agent_states(&[prompt.clone(), compact.clone(), post.clone()]);
    assert_eq!(agents[0].status, AgentStatus::Idle);
    assert_eq!(agents[0].phase, TurnPhase::Idle);
    assert!(agents[0].compacting_since.is_none());

    for (label, events, expected_count) in [
        (
            "one completed bracket",
            vec![prompt.clone(), compact.clone(), post.clone()],
            1,
        ),
        (
            "non-compaction lifecycle events carry the count forward",
            vec![
                prompt.clone(),
                compact.clone(),
                post.clone(),
                next_prompt.clone(),
            ],
            1,
        ),
        (
            "two completed brackets",
            vec![
                prompt,
                compact,
                post,
                next_prompt,
                second_compact,
                second_post,
            ],
            2,
        ),
    ] {
        assert_eq!(
            reduce_agent_states(&events)[0].compaction_count,
            expected_count,
            "{label}",
        );
    }
}

#[test]
fn manual_compaction_resumes_success_and_advances_the_turn_boundary() {
    let prompt = lifecycle_at("claude", 1, "UserPromptSubmit", signal("turn_started"));
    let stop = lifecycle_at(
        "claude",
        2,
        "Stop",
        serde_json::json!({
            "signal": "turn_ended",
            "errored": false,
            "parked_on_background": false
        }),
    );
    let compact = lifecycle_at("claude", 3, "PreCompact", signal("compacting"));
    let post = lifecycle_at("claude", 4, "PostCompact", compaction_ended(Some(false)));
    let expected_boundary = post.timestamp;

    let agents = reduce_agent_states(&[prompt, stop, compact, post]);

    assert_eq!(agents[0].status, AgentStatus::Success);
    assert_eq!(agents[0].phase, TurnPhase::Idle);
    assert!(agents[0].compacting_since.is_none());
    assert_eq!(agents[0].compaction_count, 1);
    assert_eq!(agents[0].turn_started_at, Some(expected_boundary));
}

#[test]
fn failed_compaction_preserves_the_turn_boundary() {
    let prompt = lifecycle_at("pi", 1, "before_agent_start", signal("turn_started"));
    let compact = lifecycle_at("pi", 3, "session_before_compact", signal("compacting"));
    let failed = lifecycle_at(
        "pi",
        4,
        "session_compact_failed",
        compaction_failed(Some(false)),
    );
    let expected_boundary = prompt.timestamp;

    let agents = reduce_agent_states(&[prompt, compact, failed]);

    assert_eq!(agents[0].status, AgentStatus::Running);
    assert!(agents[0].compacting_since.is_none());
    assert_eq!(agents[0].compaction_count, 0);
    assert_eq!(agents[0].turn_started_at, Some(expected_boundary));
}

#[test]
fn compaction_end_resumes_interrupted_turn_for_auto_or_unknown_edges() {
    for (source, prompt_event, edit_event, compact_event, post_event, end_signal) in [
        (
            "codex",
            "UserPromptSubmit",
            "PostToolUse",
            "PreCompact",
            "PostCompact",
            compaction_ended(Some(true)),
        ),
        (
            "pi",
            "before_agent_start",
            "tool_execution_end",
            "session_before_compact",
            "session_compact",
            compaction_ended(None),
        ),
    ] {
        let prompt = lifecycle(source, prompt_event, signal("turn_started"));
        let edit = lifecycle(source, edit_event, tool_used());
        let compact = lifecycle(source, compact_event, signal("compacting"));
        let post = lifecycle(source, post_event, end_signal);

        let agents = reduce_agent_states(&[prompt, edit, compact, post]);
        assert_eq!(agents[0].status, AgentStatus::Running, "{source}");
        assert_eq!(agents[0].phase, TurnPhase::Acting, "{source}");
        assert!(agents[0].compacting_since.is_none(), "{source}");
    }
}

#[test]
fn next_lifecycle_signal_closes_missed_compaction_bracket() {
    for (label, next_derived, next_native) in [
        (
            "next tool signal",
            lifecycle(
                "droid",
                "PostToolUse",
                serde_json::json!({ "signal": "tool_used", "mutates": false, "edits": false }),
            ),
            lifecycle(
                "claude",
                "PreToolUse",
                serde_json::json!({ "signal": "tool_used", "mutates": false, "edits": false }),
            ),
        ),
        (
            "expired display marker",
            lifecycle_at(
                "droid",
                crate::agents::COMPACTING_WINDOW_SECS + 5,
                "Stop",
                serde_json::json!({
                    "signal": "turn_ended",
                    "errored": false,
                    "parked_on_background": false
                }),
            ),
            lifecycle_at(
                "claude",
                crate::agents::COMPACTING_WINDOW_SECS + 5,
                "Stop",
                serde_json::json!({
                    "signal": "turn_ended",
                    "errored": false,
                    "parked_on_background": false
                }),
            ),
        ),
    ] {
        // Derived kinds have no completion signal, so the next signal closes and counts.
        let derived = reduce_agent_states(&[
            lifecycle_at("droid", 0, "UserPromptSubmit", signal("turn_started")),
            lifecycle_at("droid", 1, "PreCompact", signal("compacting")),
            next_derived,
        ]);
        assert_eq!(derived[0].compaction_count, 1, "derived {label}");
        assert!(derived[0].compacting_since.is_none(), "derived {label}");

        // Native kinds lower the head on the next signal without counting or stamping.
        let native = reduce_agent_states(&[
            lifecycle_at("claude", 0, "UserPromptSubmit", signal("turn_started")),
            lifecycle_at("claude", 1, "PreCompact", signal("compacting")),
            next_native,
        ]);
        assert_eq!(native[0].compaction_count, 0, "native {label}");
        assert_eq!(native[0].compacted_at, None, "native {label}");
        assert!(native[0].compacting_since.is_none(), "native {label}");
    }
}

#[test]
fn turn_started_at_is_stamped_when_progress_reconciles_a_turn_open() {
    let registered = lifecycle_at("claude", 0, "SessionStart", signal("registered"));
    let tool = lifecycle_at("claude", 5, "PostToolUse", tool_used());

    let agents = reduce_agent_states(&[registered, tool]);
    assert_eq!(
        agents[0].turn_started_at,
        Some(Timestamp::from_second(epoch().as_second() + 5).unwrap())
    );
}

/// A lifecycle event carrying a usage reading beside its signal.
fn lifecycle_reading(
    event_name: &str,
    signal: serde_json::Value,
    reading: serde_json::Value,
) -> EventEnvelope {
    let mut params = serde_json::json!({
        "event_name": event_name,
        "agent_id": "sess-1",
        "signal": signal,
    });
    for (key, value) in reading.as_object().expect("reading object") {
        params[key] = value.clone();
    }
    raw_lifecycle("claude", params)
}

/// 85k input-side tokens of a 200k window, measured before the compaction.
fn full_reading() -> serde_json::Value {
    serde_json::json!({
        "context_window": 200_000,
        "total_tokens": 85_900,
        "cache_read_input_tokens": 80_000,
        "fresh_input_tokens": 5_000,
        "output_tokens": 900,
    })
}

fn turn_ended() -> serde_json::Value {
    serde_json::json!({
        "signal": "turn_ended",
        "errored": false,
        "parked_on_background": false
    })
}

fn retired_usage() -> crate::agents::AgentUsageSummary {
    crate::agents::AgentUsageSummary {
        context_window: Some(200_000),
        ..Default::default()
    }
}

#[test]
fn compaction_preserves_the_published_capacity_beside_its_window() {
    let mut params = full_reading();
    params["context_capacity"] = serde_json::json!(400_000);
    params["agent_id"] = serde_json::json!("sess-1");
    params["signal"] = signal("registered");
    let agent = reduce_agent_states(&[
        raw_lifecycle("opencode", params),
        lifecycle("opencode", "compaction_started", signal("compacting")),
        lifecycle("opencode", "compaction_ended", compaction_ended(None)),
    ])
    .remove(0);
    assert_eq!(agent.usage.context_window, Some(200_000));
    assert_eq!(agent.usage.context_capacity, Some(400_000));
    assert_eq!(agent.usage.total_tokens, None);
    assert_eq!(agent.usage.fresh_input_tokens, None);
}

#[test]
fn codex_estimate_keeps_the_gap_until_a_model_call_reports_its_split() {
    let before = serde_json::json!({
        "context_window": 372_400, "total_tokens": 16_736,
        "cache_read_input_tokens": 0, "cache_write_input_tokens": 0,
        "fresh_input_tokens": 16_729, "output_tokens": 7,
    });
    let estimate = serde_json::json!({
        "context_window": 372_400, "total_tokens": 19_359,
        "cache_read_input_tokens": 0, "cache_write_input_tokens": 0,
        "fresh_input_tokens": 0, "output_tokens": 0,
    });
    let reading = |name: &str, signal, usage: serde_json::Value| {
        let mut params = usage;
        params["event_name"] = name.into();
        params["agent_id"] = "sess-1".into();
        params["signal"] = signal;
        raw_lifecycle("codex", params)
    };
    let mut events = vec![
        lifecycle("codex", "SessionStart", signal("registered")),
        lifecycle("codex", "UserPromptSubmit", signal("turn_started")),
        reading("Stop", turn_ended(), before.clone()),
        reading("PreCompact", signal("compacting"), before),
        reading(
            "PostCompact",
            compaction_ended(Some(false)),
            estimate.clone(),
        ),
    ];
    let agent = reduce_agent_states(&events).remove(0);
    assert!(agent.compacted_unmeasured);
    assert_eq!(agent.compaction_count, 1);
    assert_eq!(agent.usage.context_pct, Some(5));
    assert_eq!(agent.usage.input_context_tokens(), None);
    assert!(agent.compacted_awaiting_prompt.is_some());

    events.push(reading(
        "UserPromptSubmit",
        signal("turn_started"),
        estimate.clone(),
    ));
    let agent = reduce_agent_states(&events).remove(0);
    assert!(agent.compacted_unmeasured);
    assert_eq!(agent.usage.context_pct, Some(5));
    assert!(agent.compacted_awaiting_prompt.is_none());

    let mut delayed_echo = events.clone();
    delayed_echo.push(reading(
        "SessionStart",
        serde_json::json!({ "signal": "compaction_ended", "echo": true }),
        estimate,
    ));
    let mut agent = reduce_agent_states(&delayed_echo).remove(0);
    assert!(agent.compacted_unmeasured);
    assert_eq!(
        agent.retired_context_tokens,
        Some(16_729),
        "a re-retired estimate keeps the pre-compaction count"
    );
    assert_eq!(agent.usage.context_pct, Some(5));
    assert_eq!(agent.compaction_count, 1);
    // Codex's sidecar still holds the reading from before the compaction.
    let mut context = crate::agents::AgentContext::new("codex", epoch());
    context.tokens = Some(crate::agents::AgentTokenUsage {
        context_window_size: Some(372_400),
        used_percentage: Some(4),
        current_usage: Some(crate::agents::AgentCurrentUsage {
            input_tokens: Some(16_729),
            ..Default::default()
        }),
        ..Default::default()
    });
    agent.attach_context(Some(context));
    let card = crate::store::snapshot::row::AgentCard::from_agent(&agent, epoch());
    assert!(card.compacted_unmeasured, "the stale sidecar ends no gap");
    assert_eq!(card.context_gauge_percent(), Some(5));

    events.push(reading(
        "Stop",
        turn_ended(),
        serde_json::json!({
            "fresh_input_tokens": 20_000, "cache_read_input_tokens": 10_000,
            "cache_write_input_tokens": 0, "total_tokens": 30_010, "output_tokens": 10,
        }),
    ));
    let agent = reduce_agent_states(&events).remove(0);
    assert!(!agent.compacted_unmeasured);
    assert_eq!(agent.usage.context_pct, Some(8));
}

#[test]
fn claude_compaction_hooks_with_the_same_widened_reading_keep_usage_retired() {
    let reading = serde_json::json!({
        "total_tokens": 100_500,
        "fresh_input_tokens": 100_000,
        "cache_read_input_tokens": 0,
        "cache_write_input_tokens": 0,
        "output_tokens": 500,
    });
    let events = [
        lifecycle("claude", "SessionStart", signal("registered")),
        lifecycle_reading("PreCompact", signal("compacting"), reading.clone()),
        lifecycle_reading("PostCompact", compaction_ended(Some(false)), reading),
    ];
    let agent = reduce_agent_states(&events).remove(0);
    assert!(agent.compacted_unmeasured);
    assert_eq!(agent.retired_context_tokens, Some(100_000));
    assert_eq!(agent.usage.total_tokens, None);
    assert_eq!(agent.usage.context_pct, None);
}

#[test]
fn claude_gap_survives_the_retired_split_echo_and_prompt_until_tool_usage() {
    for echo in [serde_json::json!({}), compacted_reading()] {
        let mut events = vec![
            lifecycle_reading("UserPromptSubmit", signal("turn_started"), full_reading()),
            lifecycle("claude", "PreCompact", signal("compacting")),
            lifecycle_reading("PostCompact", compaction_ended(Some(false)), full_reading()),
        ];
        assert!(reduce_agent_states(&events)[0].compacted_unmeasured);
        events.push(lifecycle_reading(
            "SessionStart",
            compaction_ended(None),
            echo,
        ));
        assert!(reduce_agent_states(&events)[0].compacted_unmeasured);
        events.push(lifecycle(
            "claude",
            "UserPromptSubmit",
            signal("turn_started"),
        ));
        assert!(reduce_agent_states(&events)[0].compacted_unmeasured);
        events.push(lifecycle_reading(
            "PostToolUse",
            tool_used(),
            serde_json::json!({
                "fresh_input_tokens": 6_000, "cache_read_input_tokens": 1_000,
            }),
        ));
        assert!(!reduce_agent_states(&events)[0].compacted_unmeasured);
    }
}

#[test]
fn automatic_mid_turn_compaction_waits_for_a_nonzero_input_split() {
    let mut events = vec![
        lifecycle_reading("UserPromptSubmit", signal("turn_started"), full_reading()),
        lifecycle("claude", "PreCompact", signal("compacting")),
        lifecycle_reading(
            "PostCompact",
            compaction_ended(Some(true)),
            compacted_reading(),
        ),
    ];
    let agent = reduce_agent_states(&events).remove(0);
    assert!(agent.compacted_unmeasured);
    assert_eq!(agent.status, AgentStatus::Running);
    assert!(agent.compacted_awaiting_prompt.is_none());
    events.push(lifecycle_reading(
        "PostToolUse",
        tool_used(),
        serde_json::json!({
            "fresh_input_tokens": 6_000,
        }),
    ));
    assert!(!reduce_agent_states(&events)[0].compacted_unmeasured);
}

#[test]
fn a_keepalive_reply_is_a_real_reading_but_its_open_keeps_the_gap() {
    let ping = "Type: CACHE_KEEPALIVE\nFrom: @rimz\nContent:\nCache keepalive, no action needed.";
    let mut events = vec![
        lifecycle_reading("UserPromptSubmit", signal("turn_started"), full_reading()),
        lifecycle("claude", "PreCompact", signal("compacting")),
        lifecycle("claude", "PostCompact", compaction_ended(Some(false))),
        raw_lifecycle(
            "claude",
            serde_json::json!({
                "event_name": "UserPromptSubmit", "agent_id": "sess-1",
                "prompt": ping,
                "signal": { "signal": "turn_started" },
            }),
        ),
    ];
    assert!(reduce_agent_states(&events)[0].compacted_unmeasured);
    events.push(lifecycle_reading(
        "Stop",
        turn_ended(),
        serde_json::json!({
            "fresh_input_tokens": 6_000,
        }),
    ));
    let agent = reduce_agent_states(&events).remove(0);
    assert!(!agent.compacted_unmeasured);
    assert!(
        agent.compacted_awaiting_prompt.is_some(),
        "a ping is not a prompt"
    );
}

#[test]
fn linked_continuation_without_a_prior_enters_the_gap() {
    for (label, sig) in [
        (
            "flagged echo",
            serde_json::json!({ "signal": "compaction_ended", "echo": true }),
        ),
        (
            "unflagged legacy",
            serde_json::json!({ "signal": "compaction_ended" }),
        ),
    ] {
        let opening = raw_lifecycle_at(
            "codex",
            1,
            serde_json::json!({
                "event_name": "SessionStart", "agent_id": "continuation",
                "compacted_from": "predecessor",
                "signal": sig,
            }),
        );
        let agent = reduce_agent_states(std::slice::from_ref(&opening)).remove(0);
        assert!(agent.compacted_unmeasured, "{label}");
        assert_eq!(agent.compaction_count, 0, "{label}");
        assert!(agent.compacted_at.is_some(), "{label}");
        assert_eq!(agent.compacted_awaiting_prompt, None, "{label}");

        // A later non-echo end immediately restating the creating compaction must not count.
        let restated = raw_lifecycle_at(
            "codex",
            2,
            serde_json::json!({
                "event_name": "PostCompact", "agent_id": "continuation",
                "signal": compaction_ended(Some(true)),
            }),
        );
        let after_restate = reduce_agent_states(&[opening.clone(), restated.clone()]).remove(0);
        assert_eq!(after_restate.compaction_count, 0, "{label} restated");

        // A genuinely new compaction on the successor row counts.
        let new_open = raw_lifecycle_at(
            "codex",
            3,
            serde_json::json!({
                "event_name": "PreCompact", "agent_id": "continuation",
                "signal": signal("compacting"),
            }),
        );
        let new_close = raw_lifecycle_at(
            "codex",
            4,
            serde_json::json!({
                "event_name": "PostCompact", "agent_id": "continuation",
                "signal": compaction_ended(Some(true)),
            }),
        );
        let after_new = reduce_agent_states(&[opening, restated, new_open, new_close]).remove(0);
        assert_eq!(after_new.compaction_count, 1, "{label} new compaction");
    }
}

#[test]
fn completed_compaction_retires_the_usage_measured_before_it() {
    let mut events = vec![
        lifecycle_reading("UserPromptSubmit", signal("turn_started"), full_reading()),
        lifecycle("claude", "PreCompact", signal("compacting")),
        // Claude's close hook re-reads the same transcript record.
        lifecycle_reading("PostCompact", compaction_ended(Some(false)), full_reading()),
    ];

    let agent = reduce_agent_states(&events).remove(0);
    assert!(agent.compacted_unmeasured);
    assert_eq!(agent.usage, retired_usage());
    assert_eq!(agent.compaction_count, 1);
    assert_eq!(agent.retired_context_tokens, Some(85_000));

    events.push(lifecycle_reading(
        "SessionStart",
        signal("registered"),
        full_reading(),
    ));
    let agent = reduce_agent_states(&events).remove(0);
    assert!(agent.compacted_unmeasured, "registration preserves the gap");
    assert_eq!(
        agent.usage,
        retired_usage(),
        "a re-report of the retired reading stays withheld"
    );

    events.push(lifecycle_reading(
        "Stop",
        turn_ended(),
        serde_json::json!({ "fresh_input_tokens": 6_000, "output_tokens": 40 }),
    ));
    let agent = reduce_agent_states(&events).remove(0);
    assert!(!agent.compacted_unmeasured);
    assert_eq!(agent.usage.fresh_input_tokens, Some(6_000));
    assert_eq!(agent.usage.cache_read_input_tokens, None);
    assert_eq!(agent.usage.output_tokens, Some(40));
    assert_eq!(agent.usage.context_pct, Some(3));
    assert_eq!(agent.retired_context_tokens, Some(85_000));
}

#[test]
fn failed_compaction_keeps_the_usage_and_retires_nothing() {
    let agent = reduce_agent_states(&[
        lifecycle_reading("UserPromptSubmit", signal("turn_started"), full_reading()),
        lifecycle("claude", "PreCompact", signal("compacting")),
        lifecycle("claude", "PostCompact", compaction_failed(Some(false))),
    ])
    .remove(0);

    assert!(!agent.compacted_unmeasured);
    assert_eq!(agent.usage.fresh_input_tokens, Some(5_000));
    assert_eq!(agent.usage.context_pct, Some(42));
    assert_eq!(agent.compaction_count, 0);
    assert_eq!(agent.retired_context_tokens, None);
}

#[test]
fn derived_close_shows_the_reading_its_closing_signal_carries() {
    let reading_for =
        |kind: &str, event_name: &str, sig: serde_json::Value, reading: serde_json::Value| {
            let mut params = serde_json::json!({
                "event_name": event_name,
                "agent_id": "sess-1",
                "signal": sig,
            });
            for (key, value) in reading.as_object().expect("reading object") {
                params[key] = value.clone();
            }
            raw_lifecycle(kind, params)
        };
    for kind in ["droid", "cursor", "copilot", "unregistered-plugin"] {
        let agent = reduce_agent_states(&[
            reading_for(
                kind,
                "UserPromptSubmit",
                signal("turn_started"),
                full_reading(),
            ),
            lifecycle(kind, "PreCompact", signal("compacting")),
            reading_for(
                kind,
                "Stop",
                turn_ended(),
                serde_json::json!({ "total_tokens": 7_000 }),
            ),
        ])
        .remove(0);

        assert_eq!(agent.compaction_count, 1, "{kind}");
        assert!(agent.compacted_at.is_some(), "{kind}");
        assert_eq!(agent.retired_context_tokens, Some(85_000), "{kind}");
        assert_eq!(
            agent.usage,
            crate::agents::AgentUsageSummary {
                context_pct: Some(3),
                total_tokens: Some(7_000),
                ..retired_usage()
            },
            "{kind}"
        );
    }
}

#[test]
fn second_compaction_without_a_reading_keeps_the_retired_value() {
    let agent = reduce_agent_states(&[
        lifecycle_reading("UserPromptSubmit", signal("turn_started"), full_reading()),
        lifecycle("claude", "PreCompact", signal("compacting")),
        lifecycle("claude", "PostCompact", compaction_ended(Some(false))),
        lifecycle("claude", "PreCompact", signal("compacting")),
        lifecycle("claude", "PostCompact", compaction_ended(Some(false))),
        lifecycle_reading("SessionStart", signal("registered"), full_reading()),
    ])
    .remove(0);

    assert_eq!(agent.compaction_count, 2);
    assert_eq!(agent.retired_context_tokens, Some(85_000));
    assert_eq!(agent.usage, retired_usage());
}

#[test]
fn card_after_a_completed_compaction_reads_only_the_fresh_sidecar() {
    let mut agent = reduce_agent_states(&[
        lifecycle_reading("UserPromptSubmit", signal("turn_started"), full_reading()),
        lifecycle("claude", "PreCompact", signal("compacting")),
        lifecycle_reading("PostCompact", compaction_ended(Some(false)), full_reading()),
    ])
    .remove(0);
    let mut context = crate::agents::AgentContext::new("claude", epoch());
    context.tokens = Some(crate::agents::AgentTokenUsage {
        context_window_size: Some(200_000),
        ..Default::default()
    });
    agent.attach_context(Some(context));

    let row = row_from_agent(&agent, epoch());
    assert_eq!(row.context_gauge_percent(), None);
    assert_eq!(row.context_used_tokens(), None);
    assert_eq!(row.call_split(), None);
    // The idle-compact predicate and both thresholds read this occupancy.
    assert_eq!(agent.occupied_context_tokens(), None);
    assert!(!crate::store::message::AutoCompact::Percent(40).triggered(&agent));
    assert!(!crate::store::message::AutoCompact::Tokens(50_000).triggered(&agent));

    let tokens = agent.context.as_mut().and_then(|c| c.tokens.as_mut());
    tokens.expect("sidecar tokens").used_percentage = Some(3);
    agent.attach_context(agent.context.clone());
    let row = row_from_agent(&agent, epoch());
    assert_eq!(row.context_gauge_percent(), None);
    assert_eq!(row.context_used_tokens(), None);
    assert_eq!(row.call_split(), None);

    let tokens = agent
        .context
        .as_mut()
        .and_then(|c| c.tokens.as_mut())
        .unwrap();
    tokens.current_usage = Some(crate::agents::AgentCurrentUsage {
        input_tokens: Some(6_000),
        ..Default::default()
    });
    agent.attach_context(agent.context.clone());
    let row = row_from_agent(&agent, epoch());
    assert_eq!(row.context_gauge_percent(), Some(3));
    assert_eq!(row.context_used_tokens(), Some(6_000));
    assert_eq!(
        row.call_split(),
        Some(crate::store::snapshot::RowCallSplit {
            cache_read: 0,
            cache_write: 0,
            fresh_input: 6_000,
            output: 0,
        }),
        "the fresh sidecar's call is the composition"
    );
}

/// What Claude reports once the boundary record is on disk: a bare total.
fn compacted_reading() -> serde_json::Value {
    serde_json::json!({ "total_tokens": 5_717 })
}

#[test]
fn successful_end_without_an_open_bracket_retires_the_usage() {
    for (label, closing, expected) in [
        (
            "the close reports the compacted total",
            compacted_reading(),
            crate::agents::AgentUsageSummary {
                context_pct: Some(2),
                total_tokens: Some(5_717),
                ..retired_usage()
            },
        ),
        (
            "the close re-reports the old split",
            full_reading(),
            retired_usage(),
        ),
    ] {
        // The `PreCompact` that opens the bracket was lost.
        let agent = reduce_agent_states(&[
            lifecycle_reading("UserPromptSubmit", signal("turn_started"), full_reading()),
            lifecycle_reading(
                "PostCompact",
                compaction_ended(Some(false)),
                closing.clone(),
            ),
        ])
        .remove(0);

        assert_eq!(agent.usage, expected, "{label}");
        assert!(agent.compacted_unmeasured, "{label}");
        assert_eq!(agent.retired_context_tokens, Some(85_000), "{label}");
        assert_eq!(agent.compaction_count, 1, "{label}");
    }
}

#[test]
fn in_turn_automatic_close_clears_unprompted_latch_but_parked_keeps_it() {
    use crate::store::event::EventEnvelope;
    use crate::store::message::{DeliveryGate, MessageBody, MessageRecord, MessageStatus};

    let sent_command = |source: &str, secs: i64| {
        let state = crate::agents::AgentState::stub(source, "sess-1", AgentStatus::Running);
        let mut command = MessageRecord::new(
            workspace(),
            &state,
            "/compact".to_owned(),
            DeliveryGate::Done,
        )
        .with_body(MessageBody::Command);
        command.status = MessageStatus::Sent;
        let mut envelope = EventEnvelope::message_event(
            &command,
            "session",
            crate::store::event::MessageEventMethod::Sent,
            None,
        );
        envelope.timestamp = Timestamp::from_second(secs).unwrap();
        envelope
    };

    // Open Running turn: command Sent sets the unprompted latch, and an unbracketed
    // automatic close clears it and counts the compaction once.
    let mut open_events = vec![
        lifecycle_at("antigravity", 1, "PreInvocation", signal("turn_started")),
        sent_command("antigravity", 2),
        lifecycle_at(
            "antigravity",
            3,
            "PostInvocation",
            compaction_ended(Some(true)),
        ),
    ];
    let after_open = reduce_agent_states(&open_events).remove(0);
    assert_eq!(after_open.status, AgentStatus::Running);
    assert_ne!(after_open.phase, TurnPhase::Parked);
    assert_eq!(after_open.compaction_count, 1);
    assert_eq!(after_open.compacted_awaiting_prompt, None);

    open_events.push(lifecycle_at(
        "antigravity",
        4,
        "SessionStart",
        compaction_ended(None),
    ));
    let after_echo = reduce_agent_states(&open_events).remove(0);
    assert_eq!(after_echo.compaction_count, 1);
    assert_eq!(after_echo.compacted_awaiting_prompt, None);

    // Parked row: the automatic close keeps the row parked and does not clear the latch.
    let parked_events = vec![
        lifecycle_at("antigravity", 1, "PreInvocation", signal("turn_started")),
        lifecycle_at(
            "antigravity",
            2,
            "Stop",
            serde_json::json!({
                "signal": "turn_ended",
                "errored": false,
                "parked_on_background": true
            }),
        ),
        sent_command("antigravity", 3),
        lifecycle_at(
            "antigravity",
            4,
            "PostInvocation",
            compaction_ended(Some(true)),
        ),
    ];
    let after_parked = reduce_agent_states(&parked_events).remove(0);
    assert_eq!(after_parked.status, AgentStatus::Running);
    assert_eq!(after_parked.phase, TurnPhase::Parked);
    assert_eq!(after_parked.compaction_count, 1);
    assert!(
        after_parked.compacted_awaiting_prompt.is_some(),
        "an automatic close on a parked row keeps the unprompted latch"
    );
}

#[test]
fn failed_end_without_an_open_bracket_retires_nothing() {
    let agent = reduce_agent_states(&[
        lifecycle_reading("UserPromptSubmit", signal("turn_started"), full_reading()),
        lifecycle("claude", "PostCompact", compaction_failed(Some(false))),
    ])
    .remove(0);

    assert!(!agent.compacted_unmeasured);
    assert_eq!(agent.usage.fresh_input_tokens, Some(5_000));
    assert_eq!(agent.retired_context_tokens, None);
    assert_eq!(agent.compaction_count, 0);
}

#[test]
fn end_after_an_early_close_keeps_the_retired_value_and_the_count() {
    let before_end = [
        lifecycle_reading("UserPromptSubmit", signal("turn_started"), full_reading()),
        lifecycle("claude", "PreCompact", signal("compacting")),
        lifecycle_reading(
            "PreToolUse",
            serde_json::json!({ "signal": "tool_used", "mutates": false, "edits": false }),
            full_reading(),
        ),
    ];
    let lowered = reduce_agent_states(&before_end).remove(0);
    assert!(lowered.compacting_since.is_none());
    assert_eq!(lowered.compaction_count, 0);
    assert_eq!(lowered.compacted_at, None);
    assert_eq!(lowered.retired_context_tokens, None);
    assert!(!lowered.compacted_unmeasured);
    assert_eq!(lowered.usage.fresh_input_tokens, Some(5_000));

    let mut events = before_end.to_vec();
    events.push(lifecycle_reading(
        "PostCompact",
        compaction_ended(Some(true)),
        full_reading(),
    ));
    let agent = reduce_agent_states(&events).remove(0);

    assert_eq!(agent.compaction_count, 1);
    assert!(agent.compacted_at.is_some());
    assert_eq!(agent.retired_context_tokens, Some(85_000));
    assert_eq!(agent.usage, retired_usage());
}

#[test]
fn second_end_of_one_compaction_keeps_the_fresh_figure() {
    // Claude reports every compaction's end twice, as `PostCompact` and as a
    // compact `SessionStart`; the second arrives with the bracket closed.
    let agent = reduce_agent_states(&[
        lifecycle_reading("UserPromptSubmit", signal("turn_started"), full_reading()),
        lifecycle("claude", "PreCompact", signal("compacting")),
        lifecycle_reading(
            "PostCompact",
            compaction_ended(Some(false)),
            compacted_reading(),
        ),
        lifecycle_reading("SessionStart", compaction_ended(None), compacted_reading()),
    ])
    .remove(0);

    assert_eq!(agent.compaction_count, 1);
    assert_eq!(agent.retired_context_tokens, Some(85_000));
    assert_eq!(agent.usage.total_tokens, Some(5_717));
    assert_eq!(agent.usage.context_pct, Some(2));
}

#[test]
fn manual_compaction_on_parked_row_keeps_parked_and_preserves_turn_clocks() {
    for source in ["claude", "antigravity"] {
        let prompt = lifecycle_at(source, 1, "UserPromptSubmit", signal("turn_started"));
        let edit = lifecycle_at(source, 2, "PostToolUse", tool_used());
        let parked_stop = lifecycle_at(
            source,
            3,
            "Stop",
            serde_json::json!({
                "signal": "turn_ended",
                "errored": false,
                "parked_on_background": true
            }),
        );
        let compact = lifecycle_at(source, 4, "PreCompact", signal("compacting"));
        let post = lifecycle_at(source, 5, "PostCompact", compaction_ended(Some(false)));

        let before = reduce_agent_states(&[prompt.clone(), edit.clone(), parked_stop.clone()]);
        let after = reduce_agent_states(&[prompt, edit, parked_stop, compact, post]);

        assert_eq!(after[0].status, AgentStatus::Running, "{source}");
        assert_eq!(after[0].phase, TurnPhase::Parked, "{source}");
        assert!(after[0].compacting_since.is_none(), "{source}");
        assert_eq!(after[0].compaction_count, 1, "{source}");
        assert_eq!(
            after[0].turn_started_at, before[0].turn_started_at,
            "{source}"
        );
        assert_eq!(
            after[0].user_turn_started_at, before[0].user_turn_started_at,
            "{source}"
        );
        assert_eq!(after[0].prior_turn, before[0].prior_turn, "{source}");
        assert_eq!(after[0].finished_at, before[0].finished_at, "{source}");
        assert_eq!(
            after[0].keepalive_since, before[0].keepalive_since,
            "{source}"
        );
    }
}

#[test]
fn stamped_retired_context_tokens_retires_sidecar_reading_and_hook_usage_still_wins() {
    use crate::agents::context::{AgentContext, AgentTokenUsage};

    let sidecar = |kind: &str, used: u64| {
        let mut ctx = AgentContext::new(kind, jiff::Timestamp::now());
        ctx.tokens = Some(AgentTokenUsage {
            context_window_size: Some(1_000_000),
            used_percentage: Some(((used * 100) / 1_000_000) as u8),
            remaining_percentage: Some(100 - ((used * 100) / 1_000_000) as u8),
            current_context_tokens: Some(used),
            current_usage: None,
            session_usage: None,
        });
        ctx
    };
    let stamped_event = |source: &str, event_name: &str, sig: serde_json::Value, retired: u64| {
        raw_lifecycle(
            source,
            serde_json::json!({
                "event_name": event_name,
                "agent_id": "sess-1",
                "signal": sig,
                "retired_context_tokens": retired,
            }),
        )
    };

    // Kind-agnostic: both antigravity and claude retire whatever the event carries when prior hook
    // usage gave none.
    for source in ["antigravity", "claude"] {
        // Explicit bracketed close.
        let mut agent = reduce_agent_states(&[
            lifecycle(source, "PreInvocation", signal("turn_started")),
            lifecycle(source, "PreInvocation", signal("compacting")),
            stamped_event(source, "Stop", compaction_ended(Some(false)), 26_700),
        ])
        .remove(0);
        assert_eq!(agent.retired_context_tokens, Some(26_700), "{source}");
        agent.attach_context(Some(sidecar(source, 26_700)));
        assert_eq!(agent.context_used_tokens(), None, "{source}");
        assert_eq!(agent.occupied_context_tokens(), None, "{source}");
        assert_eq!(agent.context_fill_pct(), None, "{source}");
        assert_eq!(agent.resolved_context_window(), Some(1_000_000), "{source}");
        agent.attach_context(Some(sidecar(source, 14_038)));
        assert_eq!(agent.context_used_tokens(), Some(14_038), "{source}");

        // Unbracketed explicit close.
        let unbracketed = reduce_agent_states(&[
            lifecycle(source, "PreInvocation", signal("turn_started")),
            stamped_event(source, "Stop", compaction_ended(Some(false)), 26_700),
        ])
        .remove(0);
        assert_eq!(unbracketed.retired_context_tokens, Some(26_700), "{source}");

        // On Native kinds, a bracket lowered by a non-end signal retires nothing.
        let lowered = reduce_agent_states(&[
            lifecycle(source, "PreInvocation", signal("turn_started")),
            lifecycle(source, "PreInvocation", signal("compacting")),
            stamped_event(source, "PreInvocation", signal("turn_started"), 26_700),
        ])
        .remove(0);
        assert_eq!(lowered.retired_context_tokens, None, "{source}");
        assert_eq!(lowered.compaction_count, 0, "{source}");

        // Second compaction after a new reading replaces the retired figure.
        let second = reduce_agent_states(&[
            lifecycle(source, "PreInvocation", signal("turn_started")),
            lifecycle(source, "PreInvocation", signal("compacting")),
            stamped_event(source, "Stop", compaction_ended(Some(false)), 26_700),
            lifecycle(source, "PreInvocation", signal("turn_started")),
            lifecycle(source, "PreInvocation", signal("compacting")),
            stamped_event(source, "Stop", compaction_ended(Some(false)), 41_000),
        ])
        .remove(0);
        assert_eq!(second.retired_context_tokens, Some(41_000), "{source}");

        // Failed compaction retires nothing even if stamped.
        let failed = reduce_agent_states(&[
            lifecycle(source, "PreInvocation", signal("turn_started")),
            lifecycle(source, "PreInvocation", signal("compacting")),
            stamped_event(source, "Stop", compaction_failed(Some(false)), 26_700),
        ])
        .remove(0);
        assert_eq!(failed.retired_context_tokens, None, "{source}");
    }

    // Hook-carried usage still wins over a stamped value.
    let hook_wins = reduce_agent_states(&[
        lifecycle_reading("UserPromptSubmit", signal("turn_started"), full_reading()),
        lifecycle("claude", "PreCompact", signal("compacting")),
        stamped_event(
            "claude",
            "PostCompact",
            compaction_ended(Some(false)),
            26_700,
        ),
    ])
    .remove(0);
    assert_eq!(hook_wins.retired_context_tokens, Some(85_000));
}

#[test]
fn native_non_end_signals_lower_head_without_counting_or_retiring() {
    let non_ends = [
        ("tool_used", "PostToolUse", tool_used()),
        ("turn_ended", "Stop", turn_ended()),
        (
            "turn_interrupted",
            "Interrupt",
            serde_json::json!({ "signal": "turn_interrupted" }),
        ),
        ("turn_started", "UserPromptSubmit", signal("turn_started")),
    ];
    for (label, event_name, sig) in non_ends {
        let mut params = serde_json::json!({
            "event_name": event_name,
            "agent_id": "sess-1",
            "signal": sig,
        });
        for (key, value) in full_reading().as_object().unwrap() {
            params[key] = value.clone();
        }
        let agent = reduce_agent_states(&[
            lifecycle_reading("UserPromptSubmit", signal("turn_started"), full_reading()),
            lifecycle("claude", "PreCompact", signal("compacting")),
            raw_lifecycle("claude", params),
        ])
        .remove(0);

        assert!(agent.compacting_since.is_none(), "{label}");
        assert_eq!(agent.compaction_count, 0, "{label}");
        assert_eq!(agent.compacted_at, None, "{label}");
        assert_eq!(agent.retired_context_tokens, None, "{label}");
        assert!(!agent.compacted_unmeasured, "{label}");
        assert_eq!(agent.usage.fresh_input_tokens, Some(5_000), "{label}");
    }
}

#[test]
fn native_compaction_counts_and_retires_once_across_echo_and_legacy_orders() {
    let reading_at = |kind: &str,
                      secs: i64,
                      event_name: &str,
                      sig: serde_json::Value,
                      reading: serde_json::Value| {
        let mut params = serde_json::json!({
            "event_name": event_name,
            "agent_id": "sess-1",
            "signal": sig,
        });
        for (key, value) in reading.as_object().unwrap() {
            params[key] = value.clone();
        }
        raw_lifecycle_at(kind, secs, params)
    };
    let echo = || serde_json::json!({ "signal": "compaction_ended", "echo": true });
    let fresh_after = || serde_json::json!({ "fresh_input_tokens": 6_000, "output_tokens": 40 });
    let ts = |secs: i64| Timestamp::from_second(epoch().as_second() + secs).unwrap();

    // 1. Claude real order: PreCompact, echo SessionStart, then PostCompact (at rest and mid-turn).
    for (label, at_rest, auto) in [
        ("manual at rest", true, Some(false)),
        ("auto mid-turn", false, Some(true)),
    ] {
        let mut events = vec![reading_at(
            "claude",
            1,
            "UserPromptSubmit",
            signal("turn_started"),
            full_reading(),
        )];
        if at_rest {
            events.push(lifecycle_at("claude", 2, "Stop", turn_ended()));
        }
        events.push(lifecycle_at(
            "claude",
            3,
            "PreCompact",
            signal("compacting"),
        ));
        events.push(reading_at(
            "claude",
            4,
            "SessionStart",
            echo(),
            full_reading(),
        ));
        let after_echo = reduce_agent_states(&events).remove(0);
        assert!(after_echo.compacting_since.is_none(), "{label} after echo");
        assert_eq!(after_echo.compaction_count, 0, "{label} after echo");
        assert_eq!(after_echo.compacted_at, None, "{label} after echo");
        assert_eq!(
            after_echo.retired_context_tokens, None,
            "{label} after echo"
        );
        assert_eq!(
            after_echo.compacted_awaiting_prompt,
            at_rest.then_some(ts(4)),
            "{label} after echo"
        );

        events.push(reading_at(
            "claude",
            5,
            "PostCompact",
            compaction_ended(auto),
            compacted_reading(),
        ));
        let after_post = reduce_agent_states(&events).remove(0);
        assert_eq!(after_post.compaction_count, 1, "{label}");
        assert_eq!(after_post.compacted_at, Some(ts(5)), "{label}");
        assert_eq!(after_post.turn_ended_at, Some(ts(5)), "{label}");
        assert_eq!(after_post.retired_context_tokens, Some(85_000), "{label}");
        assert!(after_post.compacted_unmeasured, "{label}");
        assert_eq!(after_post.usage.total_tokens, Some(5_717), "{label}");
        assert_eq!(
            after_post.compacted_awaiting_prompt,
            at_rest.then_some(ts(5)),
            "{label}"
        );
    }

    // 2. Codex: PreCompact, PostCompact, then echo adjacent and with signals between.
    {
        let adjacent = reduce_agent_states(&[
            reading_at(
                "codex",
                1,
                "UserPromptSubmit",
                signal("turn_started"),
                full_reading(),
            ),
            lifecycle_at("codex", 2, "PreCompact", signal("compacting")),
            reading_at(
                "codex",
                3,
                "PostCompact",
                compaction_ended(Some(true)),
                compacted_reading(),
            ),
            reading_at("codex", 4, "SessionStart", echo(), compacted_reading()),
        ])
        .remove(0);
        assert_eq!(adjacent.compaction_count, 1);
        assert_eq!(adjacent.compacted_at, Some(ts(3)));
        assert_eq!(adjacent.retired_context_tokens, Some(85_000));
        assert_eq!(adjacent.usage.total_tokens, Some(5_717));

        let separated = reduce_agent_states(&[
            reading_at(
                "codex",
                1,
                "UserPromptSubmit",
                signal("turn_started"),
                full_reading(),
            ),
            lifecycle_at("codex", 2, "PreCompact", signal("compacting")),
            reading_at(
                "codex",
                3,
                "PostCompact",
                compaction_ended(Some(true)),
                compacted_reading(),
            ),
            lifecycle_at("codex", 4, "Stop", turn_ended()),
            reading_at(
                "codex",
                5,
                "UserPromptSubmit",
                signal("turn_started"),
                fresh_after(),
            ),
            reading_at("codex", 6, "SessionStart", echo(), fresh_after()),
        ])
        .remove(0);
        assert_eq!(separated.compaction_count, 1);
        assert_eq!(separated.compacted_at, Some(ts(3)));
        assert_eq!(separated.retired_context_tokens, Some(85_000));
        assert!(!separated.compacted_unmeasured);
        assert_eq!(separated.usage.fresh_input_tokens, Some(6_000));
        assert_eq!(separated.compacted_awaiting_prompt, None);
    }

    // 3. Reused summary (no PreCompact open): echo then PostCompact.
    {
        let reused = reduce_agent_states(&[
            reading_at(
                "claude",
                1,
                "UserPromptSubmit",
                signal("turn_started"),
                full_reading(),
            ),
            reading_at("claude", 2, "SessionStart", echo(), full_reading()),
            reading_at(
                "claude",
                3,
                "PostCompact",
                compaction_ended(Some(false)),
                compacted_reading(),
            ),
        ])
        .remove(0);
        assert_eq!(reused.compaction_count, 1);
        assert_eq!(reused.compacted_at, Some(ts(3)));
        assert_eq!(reused.turn_ended_at, Some(ts(3)));
        assert_eq!(reused.compacted_awaiting_prompt, Some(ts(3)));
        assert_eq!(reused.retired_context_tokens, Some(85_000));
        assert_eq!(reused.usage.total_tokens, Some(5_717));
    }

    // 4. Head lowered early by a tool report, then echo and PostCompact.
    {
        let early_lowered = reduce_agent_states(&[
            reading_at(
                "claude",
                1,
                "UserPromptSubmit",
                signal("turn_started"),
                full_reading(),
            ),
            lifecycle_at("claude", 2, "PreCompact", signal("compacting")),
            reading_at("claude", 3, "PostToolUse", tool_used(), full_reading()),
            reading_at("claude", 4, "SessionStart", echo(), full_reading()),
            reading_at(
                "claude",
                5,
                "PostCompact",
                compaction_ended(Some(true)),
                compacted_reading(),
            ),
        ])
        .remove(0);
        assert_eq!(early_lowered.compaction_count, 1);
        assert_eq!(early_lowered.compacted_at, Some(ts(5)));
        assert_eq!(early_lowered.retired_context_tokens, Some(85_000));
        assert_eq!(early_lowered.usage.total_tokens, Some(5_717));
    }

    // 5. Legacy unflagged records (echo omitted / false) with adjacent ends in every order.
    let legacy_cases = [
        (
            "legacy claude order (SessionStart then PostCompact)",
            vec![
                reading_at(
                    "claude",
                    1,
                    "UserPromptSubmit",
                    signal("turn_started"),
                    full_reading(),
                ),
                lifecycle_at("claude", 2, "PreCompact", signal("compacting")),
                reading_at(
                    "claude",
                    3,
                    "SessionStart",
                    compaction_ended(None),
                    compacted_reading(),
                ),
                reading_at(
                    "claude",
                    4,
                    "PostCompact",
                    compaction_ended(Some(true)),
                    compacted_reading(),
                ),
            ],
        ),
        (
            "legacy codex order (PostCompact then SessionStart)",
            vec![
                reading_at(
                    "codex",
                    1,
                    "UserPromptSubmit",
                    signal("turn_started"),
                    full_reading(),
                ),
                lifecycle_at("codex", 2, "PreCompact", signal("compacting")),
                reading_at(
                    "codex",
                    3,
                    "PostCompact",
                    compaction_ended(Some(true)),
                    compacted_reading(),
                ),
                reading_at(
                    "codex",
                    4,
                    "SessionStart",
                    compaction_ended(None),
                    compacted_reading(),
                ),
            ],
        ),
        (
            "legacy no open (SessionStart then PostCompact)",
            vec![
                reading_at(
                    "claude",
                    1,
                    "UserPromptSubmit",
                    signal("turn_started"),
                    full_reading(),
                ),
                reading_at(
                    "claude",
                    2,
                    "SessionStart",
                    compaction_ended(None),
                    compacted_reading(),
                ),
                reading_at(
                    "claude",
                    3,
                    "PostCompact",
                    compaction_ended(Some(false)),
                    compacted_reading(),
                ),
            ],
        ),
        (
            "legacy head lowered early then adjacent ends",
            vec![
                reading_at(
                    "claude",
                    1,
                    "UserPromptSubmit",
                    signal("turn_started"),
                    full_reading(),
                ),
                lifecycle_at("claude", 2, "PreCompact", signal("compacting")),
                reading_at("claude", 3, "PostToolUse", tool_used(), full_reading()),
                reading_at(
                    "claude",
                    4,
                    "SessionStart",
                    compaction_ended(None),
                    compacted_reading(),
                ),
                reading_at(
                    "claude",
                    5,
                    "PostCompact",
                    compaction_ended(Some(true)),
                    compacted_reading(),
                ),
            ],
        ),
    ];
    for (label, events) in legacy_cases {
        let agent = reduce_agent_states(&events).remove(0);
        assert_eq!(agent.compaction_count, 1, "{label}");
        assert!(agent.compacted_at.is_some(), "{label}");
        assert_eq!(agent.retired_context_tokens, Some(85_000), "{label}");
        assert_eq!(agent.usage.total_tokens, Some(5_717), "{label}");
    }
}

#[test]
fn soft_compact_mark_tracks_lowest_fold_fill_since_reminder_and_ignores_retired_or_blank_readings()
{
    let reading_at = |source: &str,
                      secs: i64,
                      event_name: &str,
                      sig: serde_json::Value,
                      reading: serde_json::Value| {
        let mut params = serde_json::json!({
            "event_name": event_name,
            "agent_id": "sess-1",
            "signal": sig,
        });
        for (key, value) in reading.as_object().unwrap() {
            params[key] = value.clone();
        }
        let mut event = raw_lifecycle(source, params);
        event.timestamp = Timestamp::from_second(secs).unwrap();
        event
    };
    let wrap_up_sent = |source: &str, secs: i64| {
        let mut event = EventEnvelope::new(
            WorkspaceId::from_project_root(std::path::Path::new("/repo")),
            "session",
            source,
            "message",
            "message.sent",
            serde_json::json!({
                "message_id": crate::ids::MessageId::new(),
                "kind": source,
                "agent_id": "sess-1",
                "gate": "any",
                "status": "sent",
                "body": "prompt",
                "forced": false,
                "text_len": 10,
                "attempts": 1,
                "soft_compact": "wrap_up",
            }),
        );
        event.timestamp = Timestamp::from_second(secs).unwrap();
        event
    };
    let token_reading = |tokens: u64| {
        serde_json::json!({
            "context_window": 200_000,
            "total_tokens": tokens,
            "fresh_input_tokens": tokens,
            "cache_read_input_tokens": 0,
            "cache_write_input_tokens": 0,
        })
    };

    // Pre-reminder usage never creates a soft_compact mark.
    let pre = reduce_agent_states(&[reading_at(
        "claude",
        1,
        "UserPromptSubmit",
        signal("turn_started"),
        token_reading(150_000),
    )])
    .remove(0);
    assert_eq!(pre.soft_compact, None);

    // After reminder Sent, blank/retired events leave low_water as None
    // (never inherited from pre-reminder usage).
    let retired_only = reduce_agent_states(&[
        reading_at(
            "claude",
            1,
            "UserPromptSubmit",
            signal("turn_started"),
            token_reading(50_000),
        ),
        wrap_up_sent("claude", 2),
        lifecycle_at("claude", 3, "PreCompact", signal("compacting")),
        lifecycle_at("claude", 4, "PostCompact", compaction_ended(Some(true))),
        // Stale re-report of retired 50k is cleared before updating low-water.
        reading_at(
            "claude",
            5,
            "PostToolUse",
            tool_used(),
            token_reading(50_000),
        ),
    ])
    .remove(0);
    let mark = retired_only
        .soft_compact
        .expect("mark stays after compaction");
    assert_eq!(mark.compactions, Some(0));
    assert_eq!(
        mark.low_water_tokens, None,
        "pre-reminder, blank, and retired readings lower nothing"
    );
    assert_eq!(mark.low_water_pct, None);

    // Post-reminder readings keep their minimum, ignore blank events,
    // and reset on the next reminder.
    let mut seq = vec![
        reading_at(
            "claude",
            1,
            "UserPromptSubmit",
            signal("turn_started"),
            token_reading(150_000),
        ),
        wrap_up_sent("claude", 2),
        reading_at(
            "claude",
            3,
            "PostToolUse",
            tool_used(),
            token_reading(160_000),
        ),
        lifecycle_at("claude", 4, "PreCompact", signal("compacting")),
        lifecycle_at("claude", 5, "PostCompact", compaction_ended(Some(true))),
        reading_at(
            "claude",
            6,
            "PostToolUse",
            tool_used(),
            token_reading(18_000),
        ),
        reading_at(
            "claude",
            7,
            "PostToolUse",
            tool_used(),
            token_reading(130_000),
        ),
    ];
    let lowered = reduce_agent_states(&seq).remove(0);
    let mark = lowered.soft_compact.expect("mark carries low-water");
    assert_eq!(mark.compactions, Some(0));
    assert_eq!(mark.low_water_tokens, Some(18_000));
    assert_eq!(mark.low_water_pct, None);

    // Sending a second reminder resets low-water to None.
    seq.push(wrap_up_sent("claude", 8));
    let reset = reduce_agent_states(&seq).remove(0);
    let mark = reset.soft_compact.expect("second reminder stamps mark");
    assert_eq!(mark.compactions, Some(1));
    assert_eq!(mark.low_water_tokens, None);
    assert_eq!(mark.low_water_pct, None);

    // Percent-only readings (Pi/OMP) track low_water_pct independently.
    let pi_pct = reduce_agent_states(&[
        reading_at(
            "pi",
            1,
            "agent_start",
            signal("turn_started"),
            serde_json::json!({ "context_pct": 82 }),
        ),
        wrap_up_sent("pi", 2),
        lifecycle_at("pi", 3, "session_compact", compaction_ended(Some(true))),
        reading_at(
            "pi",
            4,
            "tool_execution_end",
            tool_used(),
            serde_json::json!({ "context_pct": 12 }),
        ),
        reading_at(
            "pi",
            5,
            "tool_execution_end",
            tool_used(),
            serde_json::json!({ "context_pct": 79 }),
        ),
    ])
    .remove(0);
    let mark = pi_pct.soft_compact.expect("pi mark");
    assert_eq!(mark.low_water_tokens, None);
    assert_eq!(mark.low_water_pct, Some(12));

    // A linked continuation row resets compactions and both low-water fields.
    let mut continuation = raw_lifecycle(
        "codex",
        serde_json::json!({
            "event_name": "PostCompact",
            "agent_id": "sess-2",
            "compacted_from": "sess-1",
            "signal": compaction_ended(Some(false)),
        }),
    );
    continuation.timestamp = Timestamp::from_second(5).unwrap();
    let rows = reduce_agent_states(&[
        reading_at(
            "codex",
            1,
            "UserPromptSubmit",
            signal("turn_started"),
            token_reading(150_000),
        ),
        wrap_up_sent("codex", 2),
        reading_at(
            "codex",
            3,
            "PostToolUse",
            tool_used(),
            token_reading(155_000),
        ),
        continuation,
    ]);
    let successor = rows
        .iter()
        .find(|row| row.agent_id == "sess-2")
        .expect("continuation row");
    let mark = successor
        .soft_compact
        .as_ref()
        .expect("continuation inherits mark");
    assert_eq!(mark.compactions, None);
    assert_eq!(mark.low_water_tokens, None);
    assert_eq!(mark.low_water_pct, None);
}
