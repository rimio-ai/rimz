use super::*;
use serde_json::json;

fn at() -> Timestamp {
    Timestamp::from_second(1_794_946_800).unwrap()
}

#[test]
fn observe_maps_the_agent_list_onto_child_fields() {
    let report = observe(
        &json!({
            "v": MOD_VERSION,
            "event": "turn.complete",
            "session_id": "sess-1",
            "transcript_path": "/t/sess-1.jsonl",
            "claude_version": "2.1.326",
            "agent_id": "aaf8677e2225c6981",
            "future_field": {"ignored": true},
            "agents": [
                {"id": "aaf8677e2225c6981", "description": "Run printf child-ok",
                 "type": "general-purpose", "status": "completed", "kind": "local_agent",
                 "startTime": 1_794_946_718_622_i64, "model": "claude-haiku-5-5",
                 "cost": {"usd": 0.01},
                 "lastRequest": {"at": 1, "model": "claude-haiku-5-5", "effort": "low"}},
                {"id": "b2", "type": "Explore", "effort": "high",
                 "lastRequest": {"effort": "low"}},
                {"description": "no id"},
                "not an object"
            ]
        }),
        at(),
    )
    .unwrap();
    assert_eq!(report.session_id, "sess-1");
    assert_eq!(report.mod_version, MOD_VERSION);
    assert_eq!(report.provider_version.as_deref(), Some("2.1.326"));
    assert_eq!(report.context.agent_version.as_deref(), Some("2.1.326"));
    assert_eq!(
        report.subagents,
        vec![
            SubagentObservation {
                agent_id: "aaf8677e2225c6981".into(),
                context: SubagentContext {
                    usage: None,
                    agent_type: Some("general-purpose".into()),
                    model: Some("claude-haiku-5-5".into()),
                    effort: Some("low".into()),
                    description: Some("Run printf child-ok".into()),
                    cost_usd: None,
                    started_at: Some(Timestamp::from_millisecond(1_794_946_718_622).unwrap()),
                    observed_at: at(),
                },
            },
            SubagentObservation {
                agent_id: "b2".into(),
                context: SubagentContext {
                    usage: None,
                    agent_type: Some("Explore".into()),
                    model: None,
                    effort: Some("high".into()),
                    description: None,
                    cost_usd: None,
                    started_at: None,
                    observed_at: at(),
                },
            },
        ],
        "the configured effort wins over the last request's"
    );
}

#[test]
fn observe_keeps_a_child_whose_optional_fields_surprise() {
    let report = observe(
        &json!({
            "v": MOD_VERSION,
            "event": "turn.complete",
            "session_id": "s",
            "agents": [
                {"id": "configured", "type": "Explore", "model": "m", "effort": 32000},
                {"id": "requested", "type": "Explore", "model": "m",
                 "lastRequest": {"effort": 2.5}},
                {"id": "odd", "type": "Explore", "model": "m", "effort": true,
                 "description": {"text": "x"}, "startTime": [1], "lastRequest": "x"},
                {"id": 7, "type": "Explore"}
            ]
        }),
        at(),
    )
    .unwrap();
    let children: Vec<_> = report
        .subagents
        .iter()
        .map(|child| {
            (
                child.agent_id.as_str(),
                child.context.agent_type.as_deref(),
                child.context.model.as_deref(),
                child.context.effort.as_deref(),
                child.context.description.as_deref(),
                child.context.started_at,
            )
        })
        .collect();
    assert_eq!(
        children,
        [
            (
                "configured",
                Some("Explore"),
                Some("m"),
                Some("32000"),
                None,
                None
            ),
            (
                "requested",
                Some("Explore"),
                Some("m"),
                Some("2.5"),
                None,
                None
            ),
            ("odd", Some("Explore"), Some("m"), None, None, None),
        ],
        "a surprising field drops alone; only an entry without a usable id drops"
    );
}

#[test]
fn observe_refuses_envelopes_not_to_act_on() {
    for payload in [
        json!(null),
        json!([]),
        json!({}),
        json!({"v": "1.0.0", "event": "session.measure", "session_id": "s"}),
        json!({"v": "1.0.0", "event": "session.start"}),
        json!({"v": "1.0.0", "event": "session.start", "session_id": ""}),
        json!({"event": "session.start", "session_id": "s"}),
        json!({"v": 1, "event": "session.start", "session_id": "s"}),
    ] {
        assert_eq!(observe(&payload, at()), None, "{payload}");
    }
    let listless = observe(
        &json!({"v": "1.0.0", "event": "session.start", "session_id": "s", "agents": "x"}),
        at(),
    );
    assert_eq!(listless, None, "a malformed list is a malformed envelope");
    let bare = observe(
        &json!({"v": "1.0.0", "event": "session.start", "session_id": "s"}),
        at(),
    )
    .unwrap();
    assert!(bare.subagents.is_empty());
    assert_eq!(bare.provider_version, None);
}

/// `$.session.usage()` as the live probe saw it beside a statusline push
/// carrying the same figures.
fn probe_usage() -> Value {
    json!({
        "startedAt": 1_794_967_641_885_i64,
        "context": {"tokens": 38_328, "window": 1_000_000, "percent": 4},
        "rateLimits": [
            {"kind": "seven_day", "percentUsed": 45, "resetsAt": "2026-11-23T06:00:00.000Z"},
            {"kind": "five_hour", "percentUsed": 12.5},
            {"kind": "spend_limit", "percentUsed": 130, "resetsAt": "2026-12-01T00:00:00Z"},
            {"kind": "future_window", "percentUsed": 1}
        ],
        "cost": {"usd": 0.004_591_13}
    })
}

#[test]
fn observe_maps_the_usage_onto_the_statusline_fields() {
    let report = observe(
        &json!({"v": MOD_VERSION, "event": "tool.call", "session_id": "s",
                "usage": probe_usage()}),
        at(),
    )
    .unwrap();
    assert!(report.readings);
    assert!(
        !report.turn_report,
        "only a transcript read is a turn report"
    );
    let tokens = report.context.tokens.unwrap();
    assert_eq!(
        (
            tokens.current_context_tokens,
            tokens.context_window_size,
            tokens.used_percentage
        ),
        (Some(38_328), Some(1_000_000), Some(4))
    );
    assert_eq!(
        report.context.cost.unwrap().total_cost_usd,
        Some(0.004_591_13)
    );
    let statusline: super::super::statusline::StatuslinePayload =
        serde_json::from_value(json!({"rate_limits": {
            "seven_day": {"used_percentage": 45.0, "resets_at": 1_795_413_600},
            "five_hour": {"used_percentage": 12.5},
            "spend_limit": {"used_percentage": 130.0, "resets_at": 1_796_083_200}
        }}))
        .unwrap();
    let mut from_statusline = statusline
        .into_context("claude", at())
        .rate_limits
        .unwrap()
        .windows;
    let mut from_mod = report.context.rate_limits.unwrap().windows;
    from_statusline.sort_by_key(RateLimitWindow::key);
    from_mod.sort_by_key(RateLimitWindow::key);
    assert_eq!(
        from_mod, from_statusline,
        "ISO `resetsAt` lands on the instant the statusline's epoch seconds name; an \
         unknown kind drops"
    );
    assert_eq!(report.context.fast, None, "fast comes from the transcript");
}

#[test]
fn observe_drops_a_usage_field_of_a_surprising_shape_alone() {
    for (usage, window, tokens, windows, cost) in [
        (json!("not an object"), None, None, 0, None),
        (
            json!({"context": {"window": "big", "tokens": 5}, "cost": {"usd": 0.5}}),
            None,
            Some(5),
            0,
            Some(0.5),
        ),
        (
            json!({"context": {"window": 200_000}, "rateLimits": {"kind": "seven_day"},
                "cost": {"usd": "free"}}),
            Some(200_000),
            None,
            0,
            None,
        ),
        (
            json!({"rateLimits": [{"kind": 7}, "x", {"kind": "seven_day", "percentUsed": "a",
                "resetsAt": 3}], "cost": {"usd": -1.0}}),
            None,
            None,
            0,
            None,
        ),
        (
            json!({"rateLimits": [{"kind": "seven_day", "percentUsed": 3,
                "resetsAt": "not a time"}]}),
            None,
            None,
            1,
            None,
        ),
    ] {
        let report = observe(
            &json!({"v": MOD_VERSION, "event": "turn.complete", "session_id": "s",
                    "usage": usage, "reason": 7}),
            at(),
        )
        .expect("a surprising usage still checks in");
        let reading = report.context.tokens.as_ref();
        assert_eq!(
            reading.and_then(|tokens| tokens.context_window_size),
            window,
            "{usage}"
        );
        assert_eq!(report.readings, window.is_some(), "{usage}");
        assert_eq!(
            reading.and_then(|tokens| tokens.current_context_tokens),
            tokens,
            "{usage}"
        );
        assert_eq!(
            report
                .context
                .rate_limits
                .as_ref()
                .map_or(0, |limits| limits.windows.len()),
            windows,
            "{usage}"
        );
        assert_eq!(
            report.context.cost.and_then(|cost| cost.total_cost_usd),
            cost,
            "{usage}"
        );
    }
}

const PROMPT: &str = r#"{"type":"user","timestamp":"2026-11-18T02:08:30.000Z","message":{"role":"user","content":"run it"}}"#;
const INTERRUPTED: &str = r#"{"type":"user","timestamp":"2026-11-18T02:08:34.490Z","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user for tool use]"}]}}"#;
const DIED: &str = r#"{"type":"assistant","isApiErrorMessage":true,"timestamp":"2026-11-18T02:08:35.000Z","message":{"content":[{"type":"text","text":"API Error: Overloaded"}]}}"#;
const FAST_REQUEST: &str = r#"{"type":"assistant","timestamp":"2026-11-18T02:08:31.000Z","message":{"model":"claude-opus-5-5","usage":{"input_tokens":3,"cache_read_input_tokens":20000,"output_tokens":9,"speed":"fast"}}}"#;

/// Scan `envelope` over the tails `reads` hands out in turn, counting the
/// reads and pauses.
fn scan(envelope: Value, reads: &[Option<&str>]) -> (ModReport, usize, usize) {
    let mut report = observe(&envelope, at()).unwrap();
    let mut tails = reads.iter();
    let (mut read, mut paused) = (0, 0);
    scan_transcript(
        &envelope,
        &mut report,
        |path| {
            assert_eq!(path, Path::new("/t/s.jsonl"));
            read += 1;
            tails.next().copied().flatten().map(str::to_owned)
        },
        || paused += 1,
    );
    (report, read, paused)
}

fn envelope(event: &str, extra: Value) -> Value {
    let mut envelope = json!({"v": MOD_VERSION, "event": event, "session_id": "s",
                              "transcript_path": "/t/s.jsonl"});
    envelope
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    envelope
}

#[test]
fn scan_stores_what_the_resting_turn_scan_finds() {
    let interrupted = format!("{FAST_REQUEST}\n{INTERRUPTED}\n");
    let (report, read, paused) = scan(envelope("tool.call", json!({})), &[Some(&interrupted)]);
    assert_eq!((read, paused), (1, 0));
    assert!(report.turn_report);
    assert_eq!(
        report.context.fast,
        Some(true),
        "the newest request ran fast"
    );
    assert_eq!(
        report.context.settle,
        super::super::statusline::detect_turn_interrupted(&interrupted)
            .map(|at| TurnSettle::new(at, TurnSettleOutcome::Interrupted)),
    );
    assert!(report.context.settle.is_some());
    assert_eq!(report.context.turn_error, None);

    let died = format!("{PROMPT}\n{DIED}\n");
    let (report, _, _) = scan(
        envelope("turn.complete", json!({"reason": "answer"})),
        &[Some(&died)],
    );
    assert_eq!(
        report.context.turn_error,
        super::super::statusline::detect_turn_error(&died)
    );
    assert!(report.context.turn_error.is_some());
    assert_eq!(report.context.settle, None);

    let (report, read, _) = scan(envelope("session.start", json!({})), &[Some(&interrupted)]);
    assert_eq!(read, 1, "every report that names a transcript scans it");
    assert!(report.context.settle.is_some());
    assert!(!report.turn_report, "a session start is no turn report");
}

#[test]
fn an_aborted_turn_end_rereads_until_the_interruption_lands() {
    let aborted = envelope("turn.complete", json!({"reason": "aborted"}));
    let lands = format!("{PROMPT}\n{INTERRUPTED}\n");
    let (report, read, paused) = scan(aborted.clone(), &[Some(PROMPT), Some(PROMPT), Some(&lands)]);
    assert_eq!((read, paused), (3, 2), "the read stops at the first marker");
    assert!(report.context.settle.is_some());

    let (report, read, paused) = scan(aborted.clone(), &[Some(PROMPT); 9]);
    assert_eq!(
        (read, paused),
        (1 + ABORT_REREADS as usize, ABORT_REREADS as usize),
        "bounded: {ABORT_REREADS} more reads"
    );
    assert_eq!(
        report.context.settle, None,
        "the reason itself is no marker"
    );
    assert_eq!(report.context.turn_error, None);
    assert!(report.turn_report);

    let (_, read, _) = scan(
        envelope(
            "turn.complete",
            json!({"reason": "aborted", "agent_id": "child"}),
        ),
        &[Some(PROMPT); 9],
    );
    assert_eq!(read, 1, "a child's turn end never decides the root's");
    let (_, read, _) = scan(
        envelope("turn.complete", json!({"reason": "answer"})),
        &[Some(PROMPT); 9],
    );
    assert_eq!(read, 1);
}

#[test]
fn an_unreadable_transcript_keeps_the_readings_and_sends_no_turn_report() {
    let (report, read, paused) = scan(
        envelope(
            "turn.complete",
            json!({"reason": "aborted", "usage": probe_usage()}),
        ),
        &[None],
    );
    assert_eq!((read, paused), (1, 0));
    assert!(report.readings && !report.turn_report);
    assert!(report.context.tokens.is_some());
    let mut pathless = envelope("tool.call", json!({}));
    pathless.as_object_mut().unwrap().remove("transcript_path");
    let (report, read, _) = scan(pathless, &[Some(INTERRUPTED)]);
    assert_eq!(read, 0);
    assert!(!report.turn_report);
}

/// `turn.step`'s `result.usage` for one main-loop request.
fn step_usage() -> Value {
    json!({"input_tokens": 3, "output_tokens": 9, "cache_read_input_tokens": 20_000,
           "cache_creation_input_tokens": 100, "model": "claude-opus-5-5"})
}

fn step_split() -> AgentCurrentUsage {
    AgentCurrentUsage {
        input_tokens: Some(3),
        output_tokens: Some(9),
        cache_creation_input_tokens: Some(100),
        cache_read_input_tokens: Some(20_000),
    }
}

#[test]
fn a_step_report_carries_the_split_model_version_and_count() {
    let report = observe(
        &json!({"v": MOD_VERSION, "event": "turn.step", "session_id": "s",
                "claude_version": "2.1.329", "usage": probe_usage(), "step": step_usage()}),
        at(),
    )
    .unwrap();
    let context = report.context;
    let tokens = context.tokens.as_ref().unwrap();
    assert_eq!(tokens.current_usage, Some(step_split()));
    assert_eq!(
        tokens.current_context_tokens,
        Some(20_103),
        "the count is the step's own input side, not the session's"
    );
    assert_eq!(tokens.context_window_size, Some(1_000_000));
    assert_eq!(context.model_id.as_deref(), Some("claude-opus-5-5"));
    assert_eq!(context.agent_version.as_deref(), Some("2.1.329"));
}

#[test]
fn a_step_reading_of_a_surprising_shape_drops_alone() {
    let without = |field: &str| {
        let mut step = step_usage();
        step.as_object_mut().unwrap().remove(field);
        step
    };
    let with = |field: &str, value: Value| {
        let mut step = step_usage();
        step[field] = value;
        step
    };
    let zero = json!({"input_tokens": 0, "output_tokens": 0, "cache_read_input_tokens": 0,
                      "cache_creation_input_tokens": 0, "model": "claude-opus-5-5"});
    for (step, agent_id, split, model) in [
        (without("cache_read_input_tokens"), None, false, true),
        (with("input_tokens", json!("3")), None, false, true),
        (with("output_tokens", json!(-1)), None, false, true),
        (with("model", json!("")), None, true, false),
        (with("model", json!(7)), None, true, false),
        (zero, None, false, true),
        (json!("not an object"), None, false, false),
        (step_usage(), Some("child"), false, false),
    ] {
        let report = observe(
            &json!({"v": MOD_VERSION, "event": "turn.step", "session_id": "s",
                    "claude_version": "2.1.329", "usage": probe_usage(), "step": step,
                    "agent_id": agent_id}),
            at(),
        )
        .expect("a surprising step still reports");
        let context = report.context;
        let tokens = context.tokens.as_ref().unwrap();
        assert_eq!(tokens.current_usage.is_some(), split, "{step}");
        assert_eq!(
            tokens.current_context_tokens,
            Some(if split { 20_103 } else { 38_328 }),
            "{step}"
        );
        assert_eq!(context.model_id.is_some(), model, "{step}");
        assert_eq!(context.agent_version.as_deref(), Some("2.1.329"));
        assert!(report.readings);
    }
}

#[test]
fn a_report_without_a_step_reads_the_model_and_a_matching_split_from_the_tail() {
    let tail_split = AgentCurrentUsage {
        input_tokens: Some(3),
        output_tokens: Some(9),
        cache_creation_input_tokens: Some(0),
        cache_read_input_tokens: Some(20_000),
    };
    let counted = |tokens: u64| json!({"context": {"tokens": tokens, "window": 1_000_000}});
    let (report, _, _) = scan(
        envelope("tool.call", json!({"usage": counted(20_003)})),
        &[Some(FAST_REQUEST)],
    );
    let tokens = report.context.tokens.as_ref().unwrap();
    assert_eq!(report.context.model_id.as_deref(), Some("claude-opus-5-5"));
    assert_eq!(tokens.current_usage, Some(tail_split));
    assert_eq!(tokens.current_context_tokens, Some(20_003));

    for usage in [counted(38_328), json!({"context": {"window": 1_000_000}})] {
        let (report, _, _) = scan(
            envelope("turn.complete", json!({"usage": usage})),
            &[Some(FAST_REQUEST)],
        );
        assert_eq!(report.context.model_id.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(
            report.context.tokens.unwrap().current_usage,
            None,
            "a split that does not match the report's count is no reading: {usage}"
        );
    }

    let mut step = step_usage();
    step["model"] = json!("claude-opus-5-6");
    let (report, _, _) = scan(
        envelope("turn.step", json!({"usage": counted(20_003), "step": step})),
        &[Some(FAST_REQUEST)],
    );
    let tokens = report.context.tokens.as_ref().unwrap();
    assert_eq!(report.context.model_id.as_deref(), Some("claude-opus-5-6"));
    assert_eq!(tokens.current_usage, Some(step_split()));
    let (report, _, _) = scan(
        envelope("turn.step", json!({"usage": counted(20_003), "step": "x"})),
        &[Some(FAST_REQUEST)],
    );
    assert_eq!(
        (
            report.context.model_id,
            report.context.tokens.unwrap().current_usage
        ),
        (None, None),
        "a step report never reads the tail"
    );
}

#[test]
fn manifest_and_module_carry_the_mod_version() {
    let manifest: serde_json::Value = serde_json::from_str(FILES[0].1).unwrap();
    assert_eq!(manifest["version"], MOD_VERSION);
    assert_eq!(manifest["name"], "rimz");
    let hooks: serde_json::Value = serde_json::from_str(FILES[1].1).unwrap();
    assert_eq!(hooks["modules"], serde_json::json!(["./rimz.ts"]));
    assert!(
        FILES[2]
            .1
            .contains(&format!("const MOD_VERSION = '{MOD_VERSION}'")),
        "the hooks module reports the version the manifest names"
    );
    assert_eq!(MOD_VERSION, "1.2.0");
    assert!(FILES[2].1.contains("on('turn.step'"));
}

#[test]
fn write_touches_only_files_that_differ() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("mods/claude");
    assert_eq!(
        outdated(&dir).len(),
        FILES.len(),
        "a missing folder is drift"
    );
    let first = write(&dir).unwrap();
    assert_eq!(first.len(), FILES.len());
    assert!(first.iter().all(|file| !file.existed));
    assert!(outdated(&dir).is_empty());
    assert!(
        write(&dir).unwrap().is_empty(),
        "a reinstall writes nothing"
    );

    let module = dir.join("hooks/rimz.ts");
    std::fs::write(&module, "stale").unwrap();
    std::fs::create_dir_all(dir.join(".claude-plugin/types")).unwrap();
    std::fs::write(
        dir.join(".claude-plugin/types/index.d.ts"),
        "laid by Claude",
    )
    .unwrap();
    assert_eq!(outdated(&dir), vec![module.clone()]);
    let refreshed = write(&dir).unwrap();
    assert_eq!(
        refreshed,
        vec![HookInstallFileReport {
            path: module.clone(),
            existed: true
        }]
    );
    assert!(dir.join(".claude-plugin/types/index.d.ts").is_file());

    std::fs::remove_file(&module).unwrap();
    std::fs::create_dir(&module).unwrap();
    assert_eq!(
        outdated(&dir),
        vec![module],
        "a mod file that cannot be read is stale"
    );
}
