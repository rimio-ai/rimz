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
            login: None,
            record: None,
            tier: None,
            mode: None,
            effective_isolation: None,
            agent_id: AgentSessionId::from("predecessor"),
            launch_id: Some(AgentSessionId::from("launch_coder")),
            isolation: Some(crate::config::Isolation::Sandbox),
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
        Some(crate::config::Isolation::Sandbox),
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
    assert_eq!(
        predecessor_state.isolation,
        Some(crate::config::Isolation::Sandbox)
    );
    assert_eq!(
        continuation_state.isolation,
        Some(crate::config::Isolation::Sandbox)
    );

    events.push(continued_turn);
    let after_turn = reduce_agent_states(&events);
    let continuation_state = after_turn
        .iter()
        .find(|agent| agent.agent_id == "continuation")
        .unwrap();
    assert_eq!(
        continuation_state.isolation,
        Some(crate::config::Isolation::Sandbox)
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
    for (label, next) in [
        (
            "next tool signal",
            lifecycle(
                "claude",
                "PreToolUse",
                serde_json::json!({ "signal": "tool_used", "mutates": false, "edits": false }),
            ),
        ),
        (
            "expired display marker",
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
        let prompt = lifecycle_at("claude", 0, "UserPromptSubmit", signal("turn_started"));
        let compact = lifecycle_at("claude", 1, "PreCompact", signal("compacting"));
        let agents = reduce_agent_states(&[prompt, compact, next]);

        assert_eq!(agents[0].compaction_count, 1, "{label}");
        assert!(agents[0].compacting_since.is_none(), "{label}");
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
fn completed_compaction_retires_the_usage_measured_before_it() {
    let mut events = vec![
        lifecycle_reading("UserPromptSubmit", signal("turn_started"), full_reading()),
        lifecycle("claude", "PreCompact", signal("compacting")),
        // Claude's close hook re-reads the same transcript record.
        lifecycle_reading("PostCompact", compaction_ended(Some(false)), full_reading()),
    ];

    let agent = reduce_agent_states(&events).remove(0);
    assert_eq!(agent.usage, retired_usage());
    assert_eq!(agent.compaction_count, 1);
    assert_eq!(agent.retired_context_tokens, Some(85_000));

    events.push(lifecycle_reading(
        "SessionStart",
        signal("registered"),
        full_reading(),
    ));
    let agent = reduce_agent_states(&events).remove(0);
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

    assert_eq!(agent.usage.fresh_input_tokens, Some(5_000));
    assert_eq!(agent.usage.context_pct, Some(42));
    assert_eq!(agent.compaction_count, 0);
    assert_eq!(agent.retired_context_tokens, None);
}

#[test]
fn derived_close_shows_the_reading_its_closing_signal_carries() {
    let agent = reduce_agent_states(&[
        lifecycle_reading("UserPromptSubmit", signal("turn_started"), full_reading()),
        lifecycle("claude", "PreCompact", signal("compacting")),
        lifecycle_reading(
            "Stop",
            turn_ended(),
            serde_json::json!({ "total_tokens": 7_000 }),
        ),
    ])
    .remove(0);

    assert_eq!(agent.compaction_count, 1);
    assert_eq!(agent.retired_context_tokens, Some(85_000));
    assert_eq!(
        agent.usage,
        crate::agents::AgentUsageSummary {
            context_pct: Some(3),
            total_tokens: Some(7_000),
            ..retired_usage()
        }
    );
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
    agent.context = Some(context);

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
    let row = row_from_agent(&agent, epoch());
    assert_eq!(row.context_gauge_percent(), Some(3));
    assert_eq!(row.context_used_tokens(), None);
    assert_eq!(row.call_split(), None);
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
            lifecycle_reading("PostCompact", compaction_ended(Some(false)), closing),
        ])
        .remove(0);

        assert_eq!(agent.usage, expected, "{label}");
        assert_eq!(agent.retired_context_tokens, Some(85_000), "{label}");
        // The count stays on the bracket: with no open, none closed.
        assert_eq!(agent.compaction_count, 0, "{label}");
    }
}

#[test]
fn failed_end_without_an_open_bracket_retires_nothing() {
    let agent = reduce_agent_states(&[
        lifecycle_reading("UserPromptSubmit", signal("turn_started"), full_reading()),
        lifecycle("claude", "PostCompact", compaction_failed(Some(false))),
    ])
    .remove(0);

    assert_eq!(agent.usage.fresh_input_tokens, Some(5_000));
    assert_eq!(agent.retired_context_tokens, None);
}

#[test]
fn end_after_an_early_close_keeps_the_retired_value_and_the_count() {
    let agent = reduce_agent_states(&[
        lifecycle_reading("UserPromptSubmit", signal("turn_started"), full_reading()),
        lifecycle("claude", "PreCompact", signal("compacting")),
        lifecycle_reading(
            "PreToolUse",
            serde_json::json!({ "signal": "tool_used", "mutates": false, "edits": false }),
            full_reading(),
        ),
        lifecycle_reading("PostCompact", compaction_ended(Some(true)), full_reading()),
    ])
    .remove(0);

    assert_eq!(agent.compaction_count, 1);
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
