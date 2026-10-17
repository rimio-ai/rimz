use super::*;

#[test]
fn workflow_wait_counts_render_and_legacy_routes_still_parse() {
    for count in [None, Some(7)] {
        let mut wire = serde_json::json!({
            "at":"2026-01-01T00:00:00Z", "assist":"workflow_route",
            "run":"wf_0190b8172c0070008000000000000001", "workflow":"review",
            "node":"pr", "outcome":"pending", "to":"@wait"
        });
        if let Some(count) = count {
            wire["count"] = serde_json::json!(count);
        }
        let record: AssistRecord = serde_json::from_value(wire).unwrap();
        let stats = AssistStats::from_records("all", vec![record]);
        assert_eq!(stats.rollup.workflow_routes, 1);
        let line = benefit_line(&stats.events[0], &jiff::tz::TimeZone::UTC);
        if count.is_some() {
            assert!(
                line.contains("workflow review pr waited pending ×7"),
                "{line}"
            );
            assert_eq!(serde_json::to_value(&stats.events[0]).unwrap()["count"], 7);
        } else {
            assert!(line.contains("said pending → @wait"), "{line}");
            assert!(
                serde_json::to_value(&stats.events[0])
                    .unwrap()
                    .get("count")
                    .is_none()
            );
        }
    }
}

#[test]
fn molts_count_and_render_both_agents_and_the_inherited_generation() {
    let record = AssistRecord {
        at: Timestamp::UNIX_EPOCH,
        assist: Assist::Molt {
            kind: AgentKind::new_unchecked("claude"),
            agent_id: "old-session".into(),
            label: "@fox".into(),
            successor_kind: AgentKind::new_unchecked("codex"),
            successor_id: "launch_new".into(),
            successor_label: "@otter".into(),
            generation: Some(3),
        },
    };
    let stats = AssistStats::from_records("all", vec![record]);
    assert_eq!(serde_json::to_value(&stats.rollup).unwrap()["molts"], 1);
    assert_effect_counts(&stats);
    assert!(
        category_rows(&stats.rollup)
            .iter()
            .any(|row| row.contains("Molts:"))
    );
    let line = benefit_line(&stats.events[0], &jiff::tz::TimeZone::UTC);
    assert!(line.contains("⤳ @fox molted into @otter"), "{line}");
    let line = forensic_line(&stats.events[0], &jiff::tz::TimeZone::UTC);
    for text in [
        "claude",
        "old-session",
        "codex",
        "launch_new",
        "generation 3",
    ] {
        assert!(line.contains(text), "{line}");
    }
    let event = serde_json::to_value(&stats.events[0]).unwrap();
    assert_eq!(event["assist"], "molt");
    assert_eq!(event["generation"], 3);
    assert_eq!(event["successor_id"], "launch_new");
}

fn deepenings(stats: &AssistStats) -> usize {
    stats
        .events
        .iter()
        .filter(|event| {
            matches!(
                event,
                AssistEvent::Hibernate {
                    frozen: false,
                    reclaimed_bytes: Some(_),
                    ..
                }
            )
        })
        .count()
}

/// Per kind, the effect count plus the held count equals the events of that kind.
fn assert_effect_counts(stats: &AssistStats) {
    let rollup = serde_json::to_value(&stats.rollup).unwrap();
    let mut events = BTreeMap::<&str, usize>::new();
    for event in &stats.events {
        *events.entry(event.count_key()).or_default() += 1;
    }
    for (key, total) in events {
        let mut counted = usize::try_from(rollup[key].as_u64().unwrap()).unwrap();
        if key == "redeems" {
            counted -= stats.rollup.manual_redeems;
        }
        if key == "hibernations" {
            counted += deepenings(stats);
        }
        let held = stats.rollup.held.get(key).copied().unwrap_or(0);
        assert_eq!(counted + held, total, "{key}");
    }
    let held = stats.rollup.held.values().sum::<usize>();
    assert_eq!(stats.timeline(0, false).0.len() + held, stats.events.len());
    assert_eq!(stats.timeline(0, true).0.len(), stats.events.len());
}

#[test]
fn model_alias_moves_fold_and_render() {
    let record: AssistRecord = serde_json::from_value(serde_json::json!({
        "at": "2026-01-01T00:00:00Z", "assist": "model_alias", "kind": "codex",
        "login": "codex@default", "alias": "sol", "from": "gpt-6-sol", "to": "gpt-6.1-sol"
    }))
    .unwrap();
    let stats = AssistStats::from_records("all", vec![record]);
    assert_effect_counts(&stats);
    assert!(
        category_rows(&stats.rollup)
            .iter()
            .any(|row| row.contains("Model aliases:"))
    );
    let line = benefit_line(&stats.events[0], &jiff::tz::TimeZone::UTC);
    assert!(line.contains("sol now resolves to gpt-6.1-sol (was gpt-6-sol)"));
}

#[test]
fn resident_launch_assists_roll_up_and_render() {
    let record = serde_json::from_value::<AssistRecord>(serde_json::json!({
        "at": "2026-01-01T00:00:00Z", "assist": "resident_launch",
        "task": "fixer", "checkout": "/repo-worktrees/auth", "stopped": ["@coder", "@sweep"],
        "condition": {"when":"ci=passed", "hold":null, "held_ms":0, "readings":{"ci":"passed"}},
        "handles": ["@otter", "@fox"]
    }))
    .expect("resident launch assist wire format");
    let stats = AssistStats::from_records("all", vec![record]);
    assert_effect_counts(&stats);
    let json = serde_json::to_value(&stats).unwrap();
    assert_eq!(json["rollup"]["resident_launches"], 1);
    assert_eq!(json["events"][0]["condition"]["readings"]["ci"], "passed");
    assert_eq!(
        json["events"][0]["stopped"],
        serde_json::json!(["@coder", "@sweep"])
    );
    assert!(
        category_rows(&stats.rollup)
            .join(" ")
            .contains("Resident launches:")
    );
    let line = forensic_line(&stats.events[0], &jiff::tz::TimeZone::UTC);
    for fact in [
        "fixer",
        "@otter",
        "@fox",
        "/repo-worktrees/auth",
        "took over from @coder, @sweep",
        "ci=passed",
    ] {
        assert!(line.contains(fact), "{line}");
    }
}

#[test]
fn check_decline_assists_roll_up_and_render_reason_and_cost() {
    let mut wire = serde_json::json!({
        "at": "2026-01-01T00:00:00Z", "assist": "check_decline", "task": "fixer",
        "checkout": "/repo-worktrees/auth", "profile": "haiku", "kind": "claude",
        "reason": "No actionable work.", "cost_usd": 0.01
    });
    let decoded = serde_json::from_value::<AssistRecord>(wire.clone());
    assert!(
        decoded.is_ok(),
        "decline assist must be readable: {decoded:?}"
    );
    let record = decoded.unwrap();
    wire["assist"] = "judge_decline".into();
    assert_eq!(serde_json::to_value(&record).unwrap(), wire);
    let stats = AssistStats::from_records("all", vec![record]);
    assert_effect_counts(&stats);
    let json = serde_json::to_value(&stats).unwrap();
    assert_eq!(json["rollup"]["judge_declines"], 1);
    assert_eq!(json["events"][0]["cost_usd"], 0.01);
    assert!(
        category_rows(&stats.rollup)
            .join(" ")
            .contains("Judge declines:")
    );
    let line = forensic_line(&stats.events[0], &jiff::tz::TimeZone::UTC);
    for fact in [
        "fixer",
        "haiku",
        "No actionable work.",
        "/repo-worktrees/auth",
        "$0.01",
    ] {
        assert!(line.contains(fact), "{line}");
    }
}

#[test]
fn resident_launch_assist_written_before_takeover_still_loads() {
    let record = serde_json::from_value::<AssistRecord>(serde_json::json!({
        "at": "2026-01-01T00:00:00Z", "assist": "resident_launch",
        "task": "fixer", "checkout": "/repo-worktrees/auth", "stopped_team": "forge",
        "handles": ["@otter"]
    }))
    .expect("resident launch assist written with the old field");
    let stats = AssistStats::from_records("all", vec![record]);
    assert_effect_counts(&stats);
    let json = serde_json::to_value(&stats).unwrap();
    assert!(json["events"][0].get("stopped").is_none(), "{json}");
    let line = forensic_line(&stats.events[0], &jiff::tz::TimeZone::UTC);
    assert!(!line.contains("took over"), "{line}");
}

#[test]
fn launch_retries_count_relaunches_and_preserve_spawn_failures() {
    let records = [(false, 999), (false, 1000), (true, 17_250)]
        .into_iter()
        .map(|(relaunched, startup_ms)| {
            serde_json::from_value::<AssistRecord>(serde_json::json!({
                "at": "2026-01-01T00:00:00Z", "assist": "launch_retry",
                "kind": "codex", "label": "@otter",
                "run_id": "run_0123456789abcdef0123456789abcdef",
                "exit_code": 1, "startup_ms": startup_ms, "relaunched": relaunched,
                "error": if relaunched { None } else { Some("codex: not found\ncaused by") }
            }))
            .expect("launch retry assist wire format")
        })
        .collect();
    let stats = AssistStats::from_records("all", records);
    assert_effect_counts(&stats);
    let json = serde_json::to_value(&stats).unwrap();
    assert_eq!(json["rollup"]["launch_retries"], 1);
    assert_eq!(json["events"][0]["assist"], "launch_retry");
    assert_eq!(json["events"][0]["exit_code"], 1);
    assert_eq!(json["events"][0]["startup_ms"], 17_250);
    assert_eq!(
        json["events"][0]["run_id"],
        "run_0123456789abcdef0123456789abcdef"
    );
    assert_eq!(category_rows(&stats.rollup).len(), 1);
    assert!(category_rows(&stats.rollup)[0].contains("Launch retry:"));
    let lines = stats
        .events
        .iter()
        .map(|event| benefit_line(event, &jiff::tz::TimeZone::UTC))
        .collect::<Vec<_>>();
    assert!(
        lines.iter().any(|line| line.contains(
            "@otter codex exited with code 1 after 17s before its session opened — relaunched, attempt 1"
        )),
        "{lines:?}"
    );
    assert_eq!(json["events"][2]["startup_ms"], 999);
    for startup in ["<1s", "1s"] {
        let failed = format!(
            "after {startup} before its session opened — relaunch failed, attempt 1 (codex: not found)"
        );
        assert!(lines.iter().any(|line| line.contains(&failed)), "{lines:?}");
    }
    let forensic = forensic_line(&stats.events[0], &jiff::tz::TimeZone::UTC);
    assert!(
        forensic.contains("run run_0123456789abcdef0123456789abcdef"),
        "{forensic}"
    );

    let rootless = serde_json::from_value::<AssistRecord>(serde_json::json!({
        "at": "2026-01-02T00:00:00Z", "assist": "launch_retry", "kind": "codex",
        "label": "@fox", "attempt": 2, "exit_code": 1, "startup_ms": 400,
        "relaunched": true
    }))
    .expect("a launch retry without a run");
    let stats = AssistStats::from_records("all", vec![rootless]);
    assert_effect_counts(&stats);
    let json = serde_json::to_value(&stats).unwrap();
    assert_eq!(json["rollup"]["launch_retries"], 1);
    assert_eq!(json["events"][0]["attempt"], 2);
    assert!(json["events"][0].get("run_id").is_none(), "{json}");
    let line = benefit_line(&stats.events[0], &jiff::tz::TimeZone::UTC);
    assert!(
        line.contains("@fox codex exited with code 1 after <1s before its session opened — relaunched, attempt 2"),
        "{line}"
    );
    let forensic = forensic_line(&stats.events[0], &jiff::tz::TimeZone::UTC);
    assert!(forensic.ends_with("relaunched true"), "{forensic}");
    assert!(!forensic.contains("run "), "{forensic}");

    let failed = serde_json::from_value::<AssistRecord>(serde_json::json!({
        "at": "2026-01-03T00:00:00Z", "assist": "launch_retry", "kind": "codex",
        "label": "@fox", "attempt": 3, "exit_code": 1, "startup_ms": 400,
        "relaunched": false, "error": "codex: not found"
    }))
    .expect("a failed launch retry without a run");
    let stats = AssistStats::from_records("all", vec![failed]);
    assert_effect_counts(&stats);
    let line = benefit_line(&stats.events[0], &jiff::tz::TimeZone::UTC);
    assert!(
        line.ends_with("— relaunch failed, attempt 3 (codex: not found)"),
        "{line}"
    );
}

#[test]
fn idle_stop_stats_count_stops_and_preserve_failures() {
    let records = [true, false]
        .into_iter()
        .map(|stopped| {
            serde_json::from_value::<AssistRecord>(serde_json::json!({
                "at": "2026-01-01T00:00:00Z", "assist": "idle_stop",
                "kind": "claude", "agent_id": "session-1", "label": "@coder",
                "idle_secs": 200, "idle_after_secs": 180, "requested_by": "@lead",
                "stopped": stopped,
                "error": if stopped { None } else { Some("no bound pane") }
            }))
            .expect("idle stop assist wire format")
        })
        .collect();
    let stats = AssistStats::from_records("all", records);
    assert_effect_counts(&stats);
    let json = serde_json::to_value(&stats).unwrap();
    assert_eq!(json["rollup"]["idle_stops"], 1);
    assert_eq!(json["events"][0]["assist"], "idle_stop");
    assert_eq!(json["events"][0]["requested_by"], "@lead");
    assert_eq!(category_rows(&stats.rollup).len(), 1);
    assert!(category_rows(&stats.rollup)[0].contains("Idle stop:"));
    let lines = stats
        .events
        .iter()
        .map(|event| forensic_line(event, &jiff::tz::TimeZone::UTC))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        lines.contains("■ @coder idle stopped after 200s (threshold 3m) — requested by @lead"),
        "{lines}"
    );
    assert!(lines.contains("agent session-1 · stopped true"), "{lines}");
    assert!(
        lines.contains(
            "@coder idle stop failed after 200s (threshold 3m) — requested by @lead (no bound pane)"
        ),
        "{lines}"
    );
}

#[test]
fn hibernation_stats_sum_reclaim_and_keep_the_last_hold_per_agent() {
    let records = [
        (true, None),
        (false, None),
        (false, Some("NoScope")),
        (false, Some("ForeignProcess")),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (frozen, hold))| AssistRecord {
        at: Timestamp::from_second(index as i64).unwrap(),
        assist: Assist::Hibernate {
            kind: AgentKind::new_unchecked("claude"),
            agent_id: "session".into(),
            label: "@coder".into(),
            idle_secs: 600,
            frozen,
            reclaimed_bytes: hold.is_none().then_some(2048),
            hold: hold.map(str::to_owned),
            error: None,
        },
    })
    .collect();
    let stats = AssistStats::from_records("all", records);
    assert_effect_counts(&stats);
    let json = serde_json::to_value(&stats).unwrap();
    assert_eq!(json["rollup"]["hibernations"], 1);
    assert_eq!(json["rollup"]["hibernate_reclaimed_bytes"], 4096);
    assert_eq!(json["rollup"]["reclaimed_bytes"], 0);
    assert_eq!(
        json["rollup"]["hibernate_holds"]["claude/session"],
        "ForeignProcess"
    );
    let lines = stats
        .events
        .iter()
        .map(|event| forensic_line(event, &jiff::tz::TimeZone::UTC))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        lines.contains("@coder hibernated, 2 KB reclaimed"),
        "{lines}"
    );
    assert!(lines.contains("ForeignProcess"), "{lines}");
    assert!(
        lines.contains("@coder hibernation deepened, 2 KB reclaimed"),
        "{lines}"
    );
    assert!(
        category_rows(&stats.rollup)
            .iter()
            .any(|row| row.contains("Hibernate:"))
    );
}

#[test]
fn keepalive_stats_count_deliveries_and_preserve_failures() {
    let records = [(true, false), (false, false), (true, true)]
        .into_iter()
        .map(|(delivered, capped)| {
            let mut record = serde_json::json!({
                "at": "2026-01-01T00:00:00Z", "assist": "cache_keepalive",
                "kind": "claude", "agent_id": "session-1", "label": "@coder",
                "idle_secs": 3540, "waits": 2, "message_id": "msg_1",
                "delivered": delivered, "error": if delivered { None } else { Some("gate closed") }
            });
            if capped {
                record["capped"] = serde_json::json!(true);
            }
            serde_json::from_value::<AssistRecord>(record).expect("keepalive assist wire format")
        })
        .collect();
    let stats = AssistStats::from_records("all", records);
    assert_effect_counts(&stats);
    let json = serde_json::to_value(&stats).unwrap();
    assert_eq!(
        json["rollup"]["keepalives"], 2,
        "a capped delivery still counts"
    );
    assert_eq!(json["events"][0]["assist"], "cache_keepalive");
    let capped = json["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["capped"].clone())
        .collect::<Vec<_>>();
    assert_eq!(capped.iter().filter(|value| **value == true).count(), 1);
    assert_eq!(capped.iter().filter(|value| **value == false).count(), 2);
    assert!(
        category_rows(&stats.rollup)
            .join(" ")
            .contains("Keepalive:")
    );
    let lines = stats
        .events
        .iter()
        .map(|event| forensic_line(event, &jiff::tz::TimeZone::UTC))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(lines.contains("@coder cache keepalive after 59m — 2 waits pending"));
    assert!(lines.contains("message msg_1 · delivered true"));
    assert!(lines.contains("gate closed"));
    assert_eq!(lines.matches("2 waits pending, limit reached").count(), 1);
}

#[test]
fn keep_warm_pings_count_as_keepalives_and_name_their_horizon() {
    let record = |horizon: Option<u64>, waits: usize| {
        serde_json::from_value::<AssistRecord>(serde_json::json!({
            "at": "2026-01-01T00:00:00Z", "assist": "cache_keepalive",
            "kind": "codex", "agent_id": "session-1", "label": "@coder",
            "idle_secs": 1740, "waits": waits, "horizon_secs": horizon,
            "message_id": "msg_1", "delivered": true,
        }))
        .expect("keep-warm assist wire format")
    };
    let stats = AssistStats::from_records(
        "all",
        vec![
            record(Some(7200), 0),
            record(Some(5400), 1),
            record(None, 1),
        ],
    );
    assert_effect_counts(&stats);
    let json = serde_json::to_value(&stats).unwrap();
    assert_eq!(json["rollup"]["keepalives"], 3);
    // Newest first; equal stamps reverse the record order.
    assert_eq!(json["events"][2]["horizon_secs"], 7200);
    assert!(json["events"][0].get("horizon_secs").is_none());
    let lines = stats
        .events
        .iter()
        .map(|event| forensic_line(event, &jiff::tz::TimeZone::UTC))
        .collect::<Vec<_>>();
    assert!(
        lines[2].contains("@coder cache keepalive after 29m, holding warm (2h)")
            && !lines[2].contains("pending"),
        "{}",
        lines[2]
    );
    assert!(
        lines[1].contains("cache keepalive after 29m, holding warm (90m) — 1 wait pending"),
        "{}",
        lines[1]
    );
    assert!(lines[0].contains("cache keepalive after 29m — 1 wait pending"));
}

#[test]
fn flip_compaction_fold_and_render_preserve_skipped_attempts() {
    let records = [true, false]
        .into_iter()
        .map(|delivered| AssistRecord {
            at: Timestamp::from_second(if delivered { 0 } else { 60 }).expect("timestamp"),
            assist: Assist::FlipCompact {
                kind: AgentKind::new_unchecked("codex"),
                agent_id: AgentSessionId::from("session-1"),
                role: "coder".to_owned(),
                threshold: 180_000,
                from: delivered.then(|| "Implement".to_owned()),
                to: "Review".to_owned(),
                occupied_tokens: delivered.then_some(180_000),
                message_id: delivered.then(|| "msg_1".to_owned()),
                delivered,
                error: (!delivered).then(|| "already compacting\nmore detail".to_owned()),
            },
        })
        .collect::<Vec<_>>();
    for record in &records {
        let json = serde_json::to_string(record).expect("serialize");
        let decoded: AssistRecord = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(&decoded, record);
    }

    let stats = AssistStats::from_records("all", records);
    assert_effect_counts(&stats);
    assert_eq!(stats.rollup.compacts, 1);
    assert_eq!(stats.rollup.held.get("compacts"), Some(&1));
    assert_eq!(stats.timeline(0, true).0.len(), 2);
    assert!(
        category_rows(&stats.rollup)
            .join("\n")
            .contains("1 (1 held)")
    );
    assert_eq!(stats.events.len(), 2);
    let json = serde_json::to_value(&stats).expect("stats JSON");
    let skipped = &json["events"][0];
    assert_eq!(skipped["assist"], "flip_compact");
    assert_eq!(skipped["role"], "coder");
    assert_eq!(skipped["threshold"], 180_000);
    assert_eq!(skipped["delivered"], false);
    for field in ["message_id", "from", "occupied_tokens", "label"] {
        assert!(skipped.get(field).is_none(), "omits absent {field}");
    }
    assert_eq!(json["events"][1]["message_id"], "msg_1");

    let lines = stats
        .timeline(0, false)
        .0
        .into_iter()
        .map(|event| forensic_line(event, &jiff::tz::TimeZone::UTC))
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(lines, @"
    1970-01-01 00:00 ⌁ @coder flip compaction — Implement → Review — 180k ctx, threshold 180k · agent session-1 · message msg_1 · delivered true
    ");
}
