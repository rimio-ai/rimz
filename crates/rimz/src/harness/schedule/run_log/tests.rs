use super::*;
use crate::store::workflow::RunEnding;
use std::io::Write as _;

#[test]
fn holder_survives_the_codec_and_caps_without_changing_legacy_records() {
    let legacy = serde_json::json!({
        "task": "busy", "at": "2026-01-01T00:25:00Z", "result": "overlapped"
    });
    let holder = serde_json::json!({"pid": 42424, "started_at": "2026-01-01T00:00:00Z"});
    let mut named = legacy.clone();
    named["holder"] = holder.clone();
    let mut locked = legacy.clone();
    locked["result"] = "completed".into();
    locked["run_lock"] = holder;
    let encoded: Vec<_> = [legacy.clone(), named.clone(), locked.clone()]
        .into_iter()
        .map(|value| {
            let record: LoopRunRecord = serde_json::from_value(value).unwrap();
            let bytes = serde_json::to_vec(&capped_record(&record)).unwrap();
            let decoded: LoopRunRecord = serde_json::from_slice(&bytes).unwrap();
            serde_json::to_value(decoded).unwrap()
        })
        .collect();
    assert_eq!(encoded, [legacy, named, locked]);
}

#[test]
fn workflow_skip_round_trips_as_a_neutral_result() {
    let value = serde_json::json!({"task":"review", "at":"2026-01-01T00:00:00Z", "result":"workflow_skipped", "run_id":"wf_existing"});
    let row = serde_json::from_value::<LoopRunRecord>(value.clone());
    assert!(row.is_ok(), "workflow skip is a persisted result: {row:?}");
    let row = row.unwrap();
    assert_eq!(row.result.label(), "skipped");
    assert_eq!(row.polarity(), None);
    assert_eq!(row.result.spawn_exit_code(), None);
    assert_eq!(
        serde_json::to_value(row).unwrap()["result"],
        value["result"]
    );
}

#[test]
fn stages_fold_legacy_judges_and_use_the_last_answer() {
    let legacy: LoopRunRecord = serde_json::from_value(serde_json::json!({
        "task": "guard", "at": "2026-01-01T00:00:00Z", "result": "check_skipped",
        "check": {"code": 0, "timed_out": false, "output": "old reason", "agent": {
            "profile": "haiku", "kind": "claude", "verdict": {"pass": true, "reason": "old reason"}
        }}
    }))
    .unwrap();
    let stages: Vec<_> = legacy.stages().collect();
    assert_eq!(
        stages.len(),
        1,
        "a legacy model check is one judge, not a shell stage"
    );
    assert!(
        matches!(&stages[0], RunStage::Judge(judge) if judge.reason.as_deref() == Some("old reason"))
    );
    assert_eq!(legacy.last_verdict(), Some(true));
    let row: LoopRunRecord = serde_json::from_value(serde_json::json!({
        "task": "guard", "at": "2026-01-01T00:00:00Z", "result": "check_skipped",
        "check": {"code": 1, "timed_out": false, "output": "shell failed"},
        "judge": [
            {"profile": "haiku", "kind": "claude", "verdict": true, "reason": "fixed"},
            {"profile": "sonnet", "kind": "claude", "error": "outage"}
        ]
    }))
    .unwrap();
    assert_eq!(row.stages().count(), 3);
    assert_eq!(
        row.last_verdict(),
        Some(true),
        "no answer does not replace the last verdict"
    );
    assert_eq!(row.polarity(), Some(RunPolarity::Good));
}

#[test]
fn every_judge_reason_and_error_is_capped() {
    let evidence = format!("{}tail", "界".repeat(CHECK_OUTPUT_CAP));
    let row: LoopRunRecord = serde_json::from_value(serde_json::json!({
        "task": "guard", "at": "2026-01-01T00:00:00Z", "result": "check_skipped",
        "judge": [
            {"profile": "haiku", "kind": "claude", "verdict": false, "reason": evidence, "error": evidence, "retry": evidence},
            {"profile": "sonnet", "kind": "claude", "reason": evidence, "error": evidence, "retry": evidence}
        ]
    })).unwrap();
    let capped = serde_json::to_value(capped_record(&row)).unwrap();
    assert_eq!(capped["judge"].as_array().map(Vec::len), Some(2));
    for judge in capped["judge"].as_array().unwrap() {
        for field in ["reason", "error", "retry"] {
            let text = judge[field].as_str().unwrap();
            assert!(
                text.len() <= CHECK_OUTPUT_CAP && text.ends_with("tail"),
                "{field}: {} bytes",
                text.len()
            );
        }
    }
}

#[test]
fn history_keeps_declines_and_refusals_and_counts_heard_since_acting() {
    use LoopRunResult::*;
    let mut rows = vec![record("task", 1, SignalSkipped)];
    for (index, result) in [
        Completed,
        CheckSkipped,
        CheckSkipped,
        BudgetSkipped,
        AccountSkipped,
        ThrottleSkipped,
        SurplusSkipped,
        Overlapped,
        TakeoverBlocked,
        TargetGone,
        Expired,
        Canceled,
    ]
    .into_iter()
    .enumerate()
    {
        let mut row = record("task", index as i64 + 2, result);
        if result == CheckSkipped {
            row.check = Some(
                serde_json::from_value(serde_json::json!({
                    "code": if index == 1 { 0 } else { 1 }, "timed_out": false, "output": ""
                }))
                .unwrap(),
            );
        }
        rows.push(row);
    }
    let mut sibling = record("task", 20, SignalSkipped);
    sibling.signal = Some(SignalRecord {
        name: "ci.passed".parse().unwrap(),
        payload: Map::new(),
    });
    rows.extend([sibling.clone(), sibling.clone()]);
    let history = TaskHistory::split(rows, None);
    assert_eq!(history.fires.len(), 12);
    let heard = history.heard.unwrap();
    assert_eq!(heard.count, 2);
    assert_eq!(heard.since, Some(Timestamp::from_second(3).unwrap()));
    assert_eq!(heard.latest.as_str(), "ci.passed");
    let history = TaskHistory::split(vec![sibling.clone(), sibling], None);
    assert!(history.fires.is_empty());
    assert_eq!(history.heard.as_ref().unwrap().count, 2);
    assert_eq!(history.heard.unwrap().since, None);
}

#[test]
fn history_arming_cutoff_applies_to_fires_and_heard() {
    let rows = vec![
        record("wait", 1, LoopRunResult::Completed),
        record("wait", 2, LoopRunResult::SignalSkipped),
        record("wait", 3, LoopRunResult::Failed),
    ];
    let history = TaskHistory::split(rows, Some(Timestamp::from_second(3).unwrap()));
    assert_eq!(history.records.len(), 1);
    assert_eq!(history.fires.len(), 1);
    assert!(history.heard.is_none());
}

#[test]
fn stats_counts_fires_not_heard_siblings() {
    let dir = tempfile::tempdir().unwrap();
    append_to(dir.path(), &record("wait", 10, LoopRunResult::Completed));
    append_to(
        dir.path(),
        &record("wait", 20, LoopRunResult::SignalSkipped),
    );
    let now = Timestamp::from_second(30)
        .unwrap()
        .to_zoned(jiff::tz::TimeZone::UTC);
    let stats = stats(dir.path(), &now, None);
    assert_eq!(stats["wait"].runs, 1);
}

#[test]
fn agent_check_evidence_is_bounded_in_the_persisted_record() {
    let dir = tempfile::tempdir().unwrap();
    let evidence = format!("{}tail", "界".repeat(CHECK_OUTPUT_CAP));
    let mut row = record("guard", 1, LoopRunResult::CheckSkipped);
    row.check = Some(serde_json::from_value::<CheckRecord>(serde_json::json!({
        "code": 1, "timed_out": false, "output": evidence,
        "agent": {"profile": "haiku", "kind": "claude", "verdict": {"pass": false, "reason": evidence}, "error": evidence}
    })).unwrap());
    row.check
        .as_mut()
        .unwrap()
        .agent
        .as_mut()
        .unwrap()
        .verdict
        .as_mut()
        .unwrap()
        .reason = evidence.clone();
    append_to(dir.path(), &row);
    let stored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(log_path(dir.path())).unwrap()).unwrap();
    let check = &stored["check"];
    for text in [
        &check["output"],
        &check["agent"]["verdict"]["reason"],
        &check["agent"]["error"],
    ] {
        let text = text.as_str().unwrap();
        assert!(
            text.len() <= CHECK_OUTPUT_CAP && text.ends_with("tail"),
            "stored evidence must be a bounded UTF-8 tail: {} bytes",
            text.len()
        );
    }
}

#[test]
fn skipped_check_cost_counts_and_old_check_rows_still_parse() {
    let dir = tempfile::tempdir().unwrap();
    let mut row = record("guard", 1, LoopRunResult::CheckSkipped);
    row.cost_usd = Some(0.25);
    row.check =
        Some(serde_json::from_str(r#"{"code":0,"timed_out":false,"output":"old row"}"#).unwrap());
    assert!(row.check.as_ref().unwrap().agent.is_none());
    assert!(
        serde_json::to_value(&row.check)
            .unwrap()
            .get("agent")
            .is_none()
    );
    append_to(dir.path(), &row);
    let now = Timestamp::from_second(10)
        .unwrap()
        .to_zoned(jiff::tz::TimeZone::UTC);
    assert_eq!(
        spend_on_local_day(&task_records(dir.path(), "guard", None), &now),
        0.25
    );
}

fn record(task: &str, second: i64, result: LoopRunResult) -> LoopRunRecord {
    LoopRunRecord {
        kind: None,
        judge: Vec::new(),
        checkout: None,
        worktree: None,
        task: task.to_owned(),
        root: None,
        at: Timestamp::from_second(second).expect("timestamp"),
        result,
        mode: None,
        duration_ms: None,
        throttle_wait_ms: None,
        late_ms: None,
        holder: None,
        run_lock: None,
        absorbed: 0,
        error: None,
        check: None,
        watch: None,
        signal: None,
        condition: None,
        message_id: None,
        run_id: None,
        blocking_run: None,
        transcript_path: None,
        last_message: None,
        target: None,
        cost_usd: None,
        input_tokens: None,
        output_tokens: None,
        acted: None,
    }
}

#[test]
fn acting_polarity_preserves_show_membership() {
    use LoopRunResult::*;
    for (results, expected) in [
        (
            vec![Completed, Delivered, Launched],
            Some(RunPolarity::Good),
        ),
        (
            vec![
                Errored,
                StartFailed,
                Failed,
                VerifyFailed,
                TimedOut,
                BudgetExceeded,
            ],
            Some(RunPolarity::Failure),
        ),
        (
            vec![
                BudgetSkipped,
                SurplusSkipped,
                AccountSkipped,
                ThrottleSkipped,
                Canceled,
                TargetGone,
                CheckSkipped,
                SignalSkipped,
                Expired,
                Overlapped,
                TakeoverBlocked,
            ],
            None,
        ),
    ] {
        for result in results {
            assert_eq!(record("task", 1, result).polarity(), expected, "{result:?}");
        }
    }
    for (code, timed_out, expected) in [
        (Some(0), false, Some(RunPolarity::Good)),
        (Some(0), true, None),
        (Some(1), false, None),
        (None, true, None),
    ] {
        let mut row = record("task", 1, CheckSkipped);
        row.check = Some(CheckRecord {
            duration_ms: None,
            agent: None,
            code,
            timed_out,
            output: String::new(),
            output_path: None,
        });
        assert_eq!(row.polarity(), expected);
    }
}

#[test]
fn acting_and_heard_survive_skips_rotation_and_root_filtering() {
    let dir = tempfile::tempdir().unwrap();
    let failed = record("task", 1, LoopRunResult::Failed);
    let completed = record("task", 2, LoopRunResult::Completed);
    crate::disk::rotating::append(&log_path(dir.path()), 1, &failed);
    crate::disk::rotating::append(&log_path(dir.path()), 1, &completed);
    let delivered = record("task", 3, LoopRunResult::Delivered);
    append_to(dir.path(), &delivered);
    let mut heard = record("task", 4, LoopRunResult::SignalSkipped);
    heard.signal = Some(SignalRecord {
        name: "ci.passed".parse().unwrap(),
        payload: Map::new(),
    });
    append_to(dir.path(), &heard);
    heard.at = Timestamp::from_second(5).unwrap();
    append_to(dir.path(), &heard);
    let mut legacy_skip = record("task", 6, LoopRunResult::SignalSkipped);
    append_to(dir.path(), &legacy_skip);
    legacy_skip.task = "only-heard".into();
    legacy_skip.signal = heard.signal.clone();
    append_to(dir.path(), &legacy_skip);
    let mut foreign = record("task", 7, LoopRunResult::Failed);
    foreign.root = Some(PathBuf::from("/other"));
    append_to(dir.path(), &foreign);
    let now = Timestamp::from_second(10)
        .unwrap()
        .to_zoned(jiff::tz::TimeZone::UTC);
    let stats = stats(dir.path(), &now, Some(Path::new("/project")));
    let task = &stats["task"];
    assert_eq!(
        task.acting,
        Some(ActingRun {
            record: delivered,
            polarity: RunPolarity::Good,
            streak: 2,
            since: Some(failed.at),
            failed_at: Vec::new(),
        })
    );
    assert_eq!(
        task.heard,
        Some(HeardSignal {
            signal: "ci.passed".parse().unwrap(),
            at: heard.at
        })
    );
    assert_eq!(task.last.as_ref().unwrap().result, LoopRunResult::Delivered);
    assert_eq!(task.runs, 3);
    assert_eq!(
        task.acting,
        acting_run(&task_records(
            dir.path(),
            "task",
            Some(Path::new("/project"))
        ))
    );
    assert!(stats["only-heard"].acting.is_none());
    assert!(stats["only-heard"].heard.is_some());
    assert_eq!(stats["only-heard"].runs, 0);
    assert!(stats["only-heard"].last.is_none());
}

#[test]
fn a_late_quiet_collection_counts_but_keeps_the_newer_last_fire() {
    let fires = super::super::fires::load;
    let key = format!("late-quiet-{}", crate::ids::WorkflowRunId::new());
    for second in [20, 10] {
        super::super::fires::note_quiet(&key, record("task", second, LoopRunResult::Completed))
            .unwrap();
    }
    let quiet = fires()[&key].quiet.clone().unwrap();
    assert_eq!(
        (quiet.count, quiet.streak, quiet.last.at.as_second()),
        (2, 2, 20)
    );
    super::super::fires::clear(&key).unwrap();
}

#[test]
fn in_flight_rows_merge_into_the_logged_rows_by_time() {
    let launched = |second| record("task", second, LoopRunResult::Launched);
    let logged = vec![
        record("task", 10, LoopRunResult::Completed),
        record("task", 30, LoopRunResult::Overlapped),
    ];
    let merged = merge_by_at(logged, vec![launched(5), launched(10), launched(20)]);
    let order = merged
        .iter()
        .map(|row| (row.at.as_second(), row.result))
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        [
            (5, LoopRunResult::Launched),
            (10, LoopRunResult::Completed),
            (10, LoopRunResult::Launched),
            (20, LoopRunResult::Launched),
            (30, LoopRunResult::Overlapped),
        ]
    );
}

#[test]
fn acting_streak_uses_log_order_and_resets_on_opposite_polarity() {
    let good = record("task", 30, LoopRunResult::Completed);
    let failed = record("task", 10, LoopRunResult::Failed);
    let error = record("task", 20, LoopRunResult::Errored);
    let neutral = record("task", 40, LoopRunResult::Canceled);
    assert_eq!(
        acting_run(&[
            good.clone(),
            failed.clone(),
            neutral.clone(),
            error.clone(),
            neutral
        ]),
        Some(ActingRun {
            record: error.clone(),
            polarity: RunPolarity::Failure,
            streak: 2,
            since: Some(good.at),
            failed_at: vec![failed.at, error.at],
        })
    );
    assert_eq!(acting_run(&[]), None);

    // A quiet fire between two failures ends the first one's streak, whatever the summary
    // stored: the failure after it reset the quiet streak.
    let quiet = QuietFires {
        count: 1,
        streak: 0,
        last: record("task", 15, LoopRunResult::Completed),
    };
    let after = with_quiet(acting_run(&[failed.clone(), error.clone()]), Some(&quiet)).unwrap();
    assert_eq!(
        (after.polarity, after.streak, after.since),
        (RunPolarity::Failure, 1, Some(quiet.last.at))
    );
    let older = QuietFires {
        last: record("task", 5, LoopRunResult::Completed),
        ..quiet
    };
    let after = with_quiet(acting_run(&[failed, error]), Some(&older)).unwrap();
    assert_eq!((after.streak, after.since), (2, None));
}

#[test]
fn start_failed_round_trips_and_has_no_spawn_exit_code() {
    let result = LoopRunResult::StartFailed;
    let encoded = serde_json::to_string(&result).expect("serialize result");
    assert_eq!(encoded, r#""start_failed""#);
    assert_eq!(
        serde_json::from_str::<LoopRunResult>(&encoded).expect("parse result"),
        result
    );
    assert_eq!(result.label(), "start failed");
    assert_eq!(result.spawn_exit_code(), None);
}

#[test]
fn scheduled_row_query_matches_task_root_mode_and_inclusive_time() {
    let root = Path::new("/project");
    let since = Timestamp::from_second(10).expect("timestamp");
    for (task, recorded_root, second, mode, expected) in [
        ("task", root, 10, Some(LoopRunMode::Scheduled), true),
        ("task", root, 11, Some(LoopRunMode::Scheduled), true),
        ("task", root, 9, Some(LoopRunMode::Scheduled), false),
        ("other", root, 10, Some(LoopRunMode::Scheduled), false),
        (
            "task",
            Path::new("/other"),
            10,
            Some(LoopRunMode::Scheduled),
            false,
        ),
        ("task", root, 10, Some(LoopRunMode::Manual), false),
        ("task", root, 10, None, false),
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(!has_scheduled_row_since(
            dir.path(),
            "task",
            "task",
            root,
            None,
            since
        ));
        let mut row = record(task, second, LoopRunResult::Expired);
        row.root = Some(recorded_root.to_path_buf());
        row.mode = mode;
        append_to(dir.path(), &row);
        assert_eq!(
            has_scheduled_row_since(dir.path(), "task", "task", root, None, since),
            expected,
            "{row:?}"
        );
    }
}

#[test]
fn append_then_stats_round_trips_records() {
    let dir = tempfile::tempdir().expect("tempdir");

    append_to(dir.path(), &record("wait", 10, LoopRunResult::Delivered));
    append_to(dir.path(), &record("wait", 12, LoopRunResult::TargetGone));

    let now = Timestamp::from_second(20)
        .expect("timestamp")
        .to_zoned(jiff::tz::TimeZone::UTC);
    let stats = stats(dir.path(), &now, None);
    let wait = stats.get("wait").expect("wait stats");
    assert_eq!(wait.runs, 2);
    assert_eq!(wait.acting.as_ref().unwrap().streak, 1);
    assert_eq!(
        wait.last.as_ref().unwrap().result,
        LoopRunResult::TargetGone
    );
}

#[test]
fn terminal_records_keep_durable_and_presentation_fields_separate() {
    for result in [
        LoopRunResult::Completed,
        LoopRunResult::Failed,
        LoopRunResult::VerifyFailed,
        LoopRunResult::TimedOut,
        LoopRunResult::BudgetSkipped,
        LoopRunResult::Overlapped,
        LoopRunResult::CheckSkipped,
        LoopRunResult::TargetGone,
        LoopRunResult::Expired,
        LoopRunResult::Errored,
        LoopRunResult::Canceled,
    ] {
        let record = LoopRunRecord::new("matrix", result, LoopRunMode::Scheduled, 17);
        assert_eq!(record.task, "matrix");
        assert_eq!(record.result, result);
        assert_eq!(record.mode, Some(LoopRunMode::Scheduled));
        assert_eq!(record.duration_ms, Some(17));
        assert_eq!(
            (
                record.error,
                record.check,
                record.run_id,
                record.transcript_path,
                record.last_message,
                record.target,
                record.cost_usd,
                record.input_tokens,
                record.output_tokens,
            ),
            (None, None, None, None, None, None, None, None, None)
        );
    }

    let check = CheckRecord {
        duration_ms: None,
        agent: None,
        output_path: None,
        code: Some(7),
        timed_out: false,
        output: "check output".to_owned(),
    };
    let mut record = LoopRunRecord::new("matrix", LoopRunResult::Failed, LoopRunMode::Manual, 19);
    record.error = Some("durable error".to_owned());
    record.check = Some(check.clone());
    record.run_id = Some("run_1".to_owned());
    record.transcript_path = Some("/tmp/transcript".to_owned());
    record.last_message = Some("last message".to_owned());
    record.target = Some("@coder".to_owned());
    record.cost_usd = Some(1.25);
    record.input_tokens = Some(10);
    record.output_tokens = Some(20);
    let presentation = LoopRunPresentation {
        failure_tail: Some("presentation tail".to_owned()),
        skip_reason: Some("presentation skip".to_owned()),
        streamed: true,
        exit_code: Some(7),
        ..LoopRunPresentation::default()
    };

    assert_eq!(record.error.as_deref(), Some("durable error"));
    assert_eq!(record.check, Some(check));
    assert_eq!(record.run_id.as_deref(), Some("run_1"));
    assert_eq!(record.transcript_path.as_deref(), Some("/tmp/transcript"));
    assert_eq!(record.last_message.as_deref(), Some("last message"));
    assert_eq!(record.target.as_deref(), Some("@coder"));
    assert_eq!(
        (record.cost_usd, record.input_tokens, record.output_tokens),
        (Some(1.25), Some(10), Some(20))
    );
    assert_eq!(
        presentation.failure_tail.as_deref(),
        Some("presentation tail")
    );
    assert_eq!(presentation.exit_code, Some(7));
    assert_eq!(
        presentation.skip_reason.as_deref(),
        Some("presentation skip")
    );
    assert!(presentation.streamed);
}

#[test]
fn verify_failed_run_status_keeps_its_distinct_loop_result() {
    assert_eq!(
        LoopRunResult::from(RunStatus::VerifyFailed),
        LoopRunResult::VerifyFailed
    );
    assert_eq!(LoopRunResult::VerifyFailed.label(), "verify failed");
}

#[test]
fn task_records_folds_log_generations() {
    let dir = tempfile::tempdir().expect("tempdir");
    let old = record("morning", 10, LoopRunResult::Completed);
    let new = record("other", 30, LoopRunResult::Completed);
    crate::disk::rotating::append(&log_path(dir.path()), 1, &old);
    crate::disk::rotating::append(&log_path(dir.path()), 1, &new);

    assert_eq!(task_records(dir.path(), "morning", None), vec![old]);
    assert_eq!(task_records(dir.path(), "other", None), vec![new]);
}

#[test]
fn daily_spend_uses_the_configured_local_day() {
    let now = "2026-06-02T00:30:00-04:00[America/New_York]"
        .parse::<Zoned>()
        .expect("zoned");
    let mut prior = record("wait", 0, LoopRunResult::Completed);
    prior.at = "2026-06-01T23:30:00Z".parse().expect("timestamp");
    prior.cost_usd = Some(3.0);
    let mut today = record("wait", 0, LoopRunResult::Completed);
    today.at = "2026-06-02T04:10:00Z".parse().expect("timestamp");
    today.cost_usd = Some(4.0);
    assert_eq!(spend_on_local_day(&[prior, today], &now), 4.0);
    let none = spend_on_local_day(&[], &now);
    assert!(none.is_sign_positive(), "{none}");
    assert_eq!(format!("${none:.2}"), "$0.00");
}

#[test]
fn cost_summary_uses_the_last_ten_costed_runs() {
    assert_eq!(cost_summary(&[]), TaskCostSummary::default());

    let mut records = vec![record("wait", 0, LoopRunResult::BudgetSkipped)];
    records[0].cost_usd = Some(f64::NAN);
    let mut first = record("wait", 1, LoopRunResult::Completed);
    first.cost_usd = Some(1.0);
    records.push(first);
    assert_eq!(cost_summary(&records).last_usd, Some(1.0));
    assert_eq!(cost_summary(&records).costed_runs, 1);

    for cost in 2..=12 {
        let mut costed = record("wait", cost, LoopRunResult::Completed);
        costed.cost_usd = Some(cost as f64);
        records.push(costed);
    }
    let summary = cost_summary(&records);
    assert_eq!(summary.last_usd, Some(12.0));
    assert_eq!(summary.avg_usd, Some(7.5));
    assert_eq!(summary.costed_runs, COST_WINDOW);
}

#[test]
fn stats_accumulates_only_same_local_day_spend() {
    let dir = tempfile::tempdir().expect("tempdir");
    let now = "2026-06-02T12:00:00-04:00[America/New_York]"
        .parse::<Zoned>()
        .expect("zoned");
    let path = dir.path().join("loop-runs.log.jsonl");
    std::fs::create_dir_all(path.parent().expect("log parent")).expect("log dir");
    let mut prior = record("wait", 0, LoopRunResult::Completed);
    prior.at = "2026-06-02T03:00:00Z".parse().expect("timestamp");
    prior.cost_usd = Some(3.0);
    let mut today = record("wait", 0, LoopRunResult::Completed);
    today.at = "2026-06-02T04:00:00Z".parse().expect("timestamp");
    today.cost_usd = Some(4.0);
    std::fs::write(
        path,
        format!(
            "{}\n{}\n",
            serde_json::to_string(&prior).expect("prior json"),
            serde_json::to_string(&today).expect("today json")
        ),
    )
    .expect("write run log");

    assert_eq!(stats(dir.path(), &now, None)["wait"].spend_today_usd, 4.0);
}

#[test]
fn held_stats_match_fresh_across_appends_rotation_and_roots() {
    let dir = tempfile::tempdir().unwrap();
    let root = Path::new("/room");
    let other = Path::new("/other");
    let now = "2026-06-02T12:00:00Z[UTC]".parse::<Zoned>().unwrap();
    let mut held = HeldStats::new(dir.path());
    for (i, cap) in [u64::MAX, u64::MAX, 1, u64::MAX].into_iter().enumerate() {
        let mut row = record("task", i as i64, LoopRunResult::Completed);
        row.root = match i {
            0 => None,
            1 => Some(other.into()),
            _ => Some(root.into()),
        };
        crate::disk::rotating::append(&log_path(dir.path()), cap, &row);
        for filter in [root, other] {
            assert_eq!(
                held.stats(filter, &now),
                &stats(dir.path(), &now, Some(filter))
            );
        }
    }
}

#[test]
fn held_stats_reset_on_local_day_and_time_zone_change() {
    let dir = tempfile::tempdir().unwrap();
    let root = Path::new("/room");
    let mut row = record("task", 0, LoopRunResult::Completed);
    row.at = "2026-06-02T23:00:00Z".parse().unwrap();
    row.cost_usd = Some(3.0);
    append_to(dir.path(), &row);
    row.at = "2026-06-03T01:00:00Z".parse().unwrap();
    row.cost_usd = Some(4.0);
    append_to(dir.path(), &row);
    let mut held = HeldStats::new(dir.path());
    for (time, spend) in [
        ("2026-06-02T12:00:00Z[UTC]", 3.0),
        ("2026-06-03T12:00:00Z[UTC]", 4.0),
        ("2026-06-03T12:00:00+02:00[Europe/Berlin]", 7.0),
    ] {
        let now = time.parse::<Zoned>().unwrap();
        assert_eq!(
            held.stats(root, &now)
                .get("task")
                .map(|s| s.spend_today_usd),
            Some(spend)
        );
    }
}

#[test]
fn daily_gate_reserves_the_next_runs_full_cap() {
    let dir = tempfile::tempdir().expect("tempdir");
    let now = "2026-06-02T12:00:00Z[UTC]".parse::<Zoned>().expect("zoned");
    let mut spent = record("bounded", 0, LoopRunResult::Completed);
    spent.at = now.timestamp();
    spent.cost_usd = Some(6.0);
    let path = log_path(dir.path());
    std::fs::create_dir_all(path.parent().expect("log parent")).expect("log dir");
    std::fs::write(
        path,
        format!("{}\n", serde_json::to_string(&spent).expect("record json")),
    )
    .expect("write run log");
    let entry = crate::config::TaskEntry {
        budget: Some("$5.00".to_owned()),
        budget_per_day: Some("$10.00".to_owned()),
        ..crate::config::TaskEntry::default()
    };

    let gate = daily_budget_gate(dir.path(), "bounded", &entry, &now)
        .expect("valid gate")
        .expect("next run does not fit");
    assert_eq!(gate.spend_usd, 6.0);
    assert_eq!(gate.reserved_usd, 5.0);
    assert_eq!(gate.cap_usd, 10.0);
}

#[test]
fn stats_folds_rotated_sibling_and_keeps_newest_last() {
    let dir = tempfile::tempdir().expect("tempdir");
    crate::disk::rotating::append(
        &log_path(dir.path()),
        1,
        &record("wait", 20, LoopRunResult::Completed),
    );
    crate::disk::rotating::append(
        &log_path(dir.path()),
        1,
        &record("wait", 10, LoopRunResult::Failed),
    );
    std::fs::OpenOptions::new()
        .append(true)
        .open(log_path(dir.path()))
        .expect("open active")
        .write_all(b"not json\n")
        .expect("append malformed line");

    let now = Timestamp::from_second(30)
        .expect("timestamp")
        .to_zoned(jiff::tz::TimeZone::UTC);
    let stats = stats(dir.path(), &now, None);
    let wait = stats.get("wait").expect("wait stats");
    assert_eq!(wait.runs, 2);
    assert_eq!(wait.acting.as_ref().unwrap().streak, 1);
    assert_eq!(wait.last.as_ref().unwrap().result, LoopRunResult::Completed);
}

#[test]
fn stats_tracks_matching_result_streak_across_rotated_and_current_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    crate::disk::rotating::append(
        &log_path(dir.path()),
        1,
        &record("wait", 10, LoopRunResult::Failed),
    );
    crate::disk::rotating::append(
        &log_path(dir.path()),
        1,
        &record("wait", 20, LoopRunResult::Failed),
    );

    let now = Timestamp::from_second(30)
        .expect("timestamp")
        .to_zoned(jiff::tz::TimeZone::UTC);
    let stats = stats(dir.path(), &now, None);
    let wait = stats.get("wait").expect("wait stats");
    assert_eq!(wait.runs, 2);
    assert_eq!(wait.acting.as_ref().unwrap().streak, 2);
    assert_eq!(wait.last.as_ref().unwrap().result, LoopRunResult::Failed);
}

#[test]
fn new_fields_round_trip_and_task_records_filter() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut with_detail = record("wait", 10, LoopRunResult::Failed);
    with_detail.mode = Some(LoopRunMode::Manual);
    with_detail.duration_ms = Some(123);
    with_detail.check = Some(CheckRecord {
        duration_ms: None,
        agent: None,
        output_path: Some(PathBuf::from("/tmp/wait.log")),
        code: Some(127),
        timed_out: false,
        output: "missing command".to_owned(),
    });
    with_detail.watch = Some(WatchVerdict::Exited {
        code: Some(127),
        elapsed_ms: 3_000,
    });
    with_detail.run_id = Some("run_0123456789abcdef0123456789abcdef".to_owned());
    with_detail.transcript_path = Some("/tmp/rimz/sessions/wait.jsonl".to_owned());
    with_detail.last_message = Some("last words".to_owned());
    with_detail.target = Some("@coder".to_owned());
    with_detail.worktree = Some(FireWorktree {
        path: PathBuf::from("/repo-worktrees/night-20260101-000000"),
        base_fallback: Some("origin unreachable; branched from local main".to_owned()),
    });
    let encoded = serde_json::to_value(&with_detail).unwrap();
    assert_eq!(
        encoded["worktree"],
        serde_json::json!({
            "path": "/repo-worktrees/night-20260101-000000",
            "base_fallback": "origin unreachable; branched from local main",
        })
    );
    append_to(dir.path(), &with_detail);
    append_to(dir.path(), &record("other", 11, LoopRunResult::Completed));

    assert_eq!(task_records(dir.path(), "wait", None), vec![with_detail]);
}

#[test]
fn throttle_vocabulary_is_durable_snake_case() {
    let mut held = record("wait", 0, LoopRunResult::ThrottleSkipped);
    held.throttle_wait_ms = Some(1_800_000);
    let encoded = serde_json::to_string(&held).unwrap();
    assert!(
        encoded.contains(r#""result":"throttle_skipped""#),
        "{encoded}"
    );
    assert!(
        encoded.contains(r#""throttle_wait_ms":1800000"#),
        "{encoded}"
    );
    assert_eq!(
        serde_json::from_str::<LoopRunRecord>(&encoded).unwrap(),
        held
    );
    assert_eq!(LoopRunResult::ThrottleSkipped.label(), "throttle skipped");
    assert_eq!(LoopRunResult::ThrottleSkipped.spawn_exit_code(), None);
    let capacity = serde_json::to_value(LoopRunResult::CapacityHeld).unwrap();
    assert_eq!(capacity, "capacity_held");
    assert_eq!(
        serde_json::from_value::<LoopRunResult>(capacity).unwrap(),
        LoopRunResult::CapacityHeld
    );
    assert_eq!(LoopRunResult::CapacityHeld.spawn_exit_code(), None);
}

#[test]
fn late_ms_is_encoded_only_for_a_late_fire() {
    let mut late = record("wait", 0, LoopRunResult::Launched);
    assert!(
        !serde_json::to_string(&late).unwrap().contains("late_ms"),
        "on time"
    );
    late.late_ms = Some(183_600_000);
    let encoded = serde_json::to_string(&late).unwrap();
    assert!(encoded.contains(r#""late_ms":183600000"#), "{encoded}");
    assert_eq!(
        serde_json::from_str::<LoopRunRecord>(&encoded).unwrap(),
        late
    );
}

#[test]
fn old_minimal_records_still_parse() {
    let line = r#"{"task":"wait","at":"1970-01-01T00:00:10Z","result":"completed"}"#;
    let record: LoopRunRecord = serde_json::from_str(line).expect("legacy record");
    assert_eq!(record.task, "wait");
    assert_eq!(record.result, LoopRunResult::Completed);
    assert_eq!(record.mode, None);
    assert_eq!(record.check, None);
    assert_eq!(record.transcript_path, None);
    assert_eq!(record.watch, None);
    assert_eq!(record.throttle_wait_ms, None);
    assert_eq!(record.worktree, None);
    assert!(!serde_json::to_string(&record).unwrap().contains("throttle"));
    assert!(!serde_json::to_string(&record).unwrap().contains("worktree"));
    let line = r#"{"task":"wait","at":"1970-01-01T00:00:10Z","result":"completed","check":{"code":0,"timed_out":false,"output":"ok"}}"#;
    let record: LoopRunRecord = serde_json::from_str(line).expect("legacy check record");
    assert_eq!(record.watch, None);
    assert_eq!(record.check.as_ref().unwrap().output_path, None);
    let encoded = serde_json::to_string(&record).unwrap();
    assert_eq!(
        serde_json::from_str::<LoopRunRecord>(&encoded).unwrap(),
        record
    );
}

#[test]
fn append_caps_forensic_fields() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut record = record("wait", 10, LoopRunResult::Errored);
    record.error = Some("e".repeat(ERROR_CAP + 20));
    record.last_message = Some("m".repeat(LAST_MESSAGE_CAP + 20));
    record.check = Some(CheckRecord {
        duration_ms: None,
        agent: None,
        output_path: None,
        code: Some(1),
        timed_out: false,
        output: "o".repeat(CHECK_OUTPUT_CAP + 20),
    });
    record.signal = Some(SignalRecord {
        name: "ci.failed".parse().unwrap(),
        payload: Map::from_iter([(
            "detail".to_owned(),
            Value::String("s".repeat(CHECK_OUTPUT_CAP + 20)),
        )]),
    });

    append_to(dir.path(), &record);
    let stored = task_records(dir.path(), "wait", None)
        .pop()
        .expect("stored record");
    assert_eq!(stored.error.expect("error").len(), ERROR_CAP);
    assert_eq!(stored.last_message.expect("last").len(), LAST_MESSAGE_CAP);
    assert_eq!(stored.check.expect("check").output.len(), CHECK_OUTPUT_CAP);
    assert!(
        stored
            .signal
            .expect("signal")
            .payload
            .contains_key("_truncated")
    );
}

#[test]
fn history_drops_named_launch_rows_and_keeps_each_runs_first_report() {
    let dir = tempfile::tempdir().unwrap();
    let run = |second, result, acted: Option<bool>| {
        let mut row = record("wf", second, result);
        row.run_id = Some("wf_1".to_owned());
        row.acted = acted;
        row.cost_usd = acted.map(|_| 2.0);
        row
    };
    let launch = run(1, LoopRunResult::Launched, None);
    let skipped = run(2, LoopRunResult::WorkflowSkipped, None);
    let report = run(3, LoopRunResult::Failed, Some(true));
    let again = run(4, LoopRunResult::Failed, Some(true));
    // An older binary logs the launch row after a fast run's report.
    let late = run(5, LoopRunResult::Launched, None);
    for row in [&launch, &skipped, &report, &again, &late] {
        append_to(dir.path(), row);
    }
    assert_eq!(
        task_records(dir.path(), "wf", None),
        vec![skipped, report],
        "a launch row naming a run is dropped, early or late, a skip naming the run is kept, \
         and the run's first report wins"
    );
    let now = "2026-06-02T12:00:00Z[UTC]".parse::<Zoned>().unwrap();
    let stats = &stats(dir.path(), &now, None)["wf"];
    assert_eq!(stats.runs, 2, "the skip and the run's report are two fires");
    assert_eq!(stats.last.as_ref().map(|row| row.at.as_second()), Some(3));
    assert_eq!(
        stats.last.as_ref().map(|row| row.result),
        Some(LoopRunResult::Failed),
        "a launch row never stands in for the run's outcome"
    );
}

#[test]
fn a_report_row_carries_the_facts_its_fire_sealed() {
    let mut launch = record("wf", 1, LoopRunResult::Launched);
    launch.absorbed = 2;
    launch.kind = Some(super::super::TaskKind::OneOff);
    launch.signal = Some(SignalRecord {
        name: "ci.failed".parse().unwrap(),
        payload: serde_json::Map::new(),
    });
    launch.run_lock = Some(RunLockInfo {
        pid: 7,
        started_at: Timestamp::from_second(1).unwrap(),
    });
    launch.worktree = Some(FireWorktree {
        path: PathBuf::from("/tree"),
        base_fallback: None,
    });
    let sealed = fire_facts(&launch);
    let run = crate::ids::WorkflowRunId::new();
    let facts = ReportFacts {
        task: "wf",
        run: &run,
        manual: false,
        created_at: Timestamp::from_second(1).unwrap(),
        terminal_at: Some(Timestamp::from_second(3).unwrap()),
        checkout: Path::new("/tree"),
        fire_facts: Some(&sealed),
    };
    let outcome: crate::store::workflow::RunOutcome =
        serde_json::from_value(serde_json::json!({"ending": {"kind": "failed"}, "acted": true}))
            .unwrap();
    let row = report_row(&facts, &outcome);
    assert_eq!(row.result, LoopRunResult::Failed);
    assert_eq!(row.at.as_second(), 3);
    assert_eq!(row.run_id, Some(run.to_string()));
    assert_eq!(
        (
            row.absorbed,
            row.kind,
            row.signal,
            row.run_lock,
            row.worktree
        ),
        (
            launch.absorbed,
            launch.kind,
            launch.signal,
            launch.run_lock,
            launch.worktree
        ),
        "the report keeps the fire's own facts"
    );
}

#[test]
fn workflow_daily_gate_holds_each_live_runs_reservation() {
    let dir = tempfile::tempdir().unwrap();
    let now = "2026-06-02T12:00:00Z[UTC]".parse::<Zoned>().unwrap();
    let mut spent = record("wf", 0, LoopRunResult::Completed);
    spent.at = now.timestamp();
    spent.cost_usd = Some(4.0);
    append_to(dir.path(), &spent);
    let entry = crate::config::TaskEntry {
        budget_per_day: Some("$10.00".to_owned()),
        ..crate::config::TaskEntry::default()
    };
    let gate =
        |reserved, live| reserved_budget_gate(dir.path(), "wf", &entry, &now, reserved, live);
    assert_eq!(
        gate(3.0, 3.0),
        Ok(None),
        "4 spent + 3 held + 3 next fits 10"
    );
    let held = gate(3.0, 3.5).unwrap().expect("4 + 3.5 + 3 exceeds 10");
    assert_eq!(
        (held.spend_usd, held.live_usd, held.reserved_usd),
        (4.0, 3.5, 3.0)
    );
    assert_eq!(
        held.reason(),
        "daily budget $10.00 cannot fund the next $3.00 run ($4.00 spent, $3.50 held by live runs)"
    );
    assert_eq!(
        gate(0.0, 5.5),
        Ok(None),
        "an unbounded graph is admitted below the cap"
    );
    assert_eq!(
        gate(0.0, 6.0)
            .unwrap()
            .map(DailyBudgetGate::reason)
            .as_deref(),
        Some("daily budget $10.00 exhausted ($4.00 spent, $6.00 held by live runs)")
    );
}

fn ended(
    ending: RunEnding,
    cost_usd: Option<f64>,
    acted: bool,
) -> crate::store::workflow::RunOutcome {
    crate::store::workflow::RunOutcome {
        ending,
        cost_usd,
        acted,
        reason: None,
        quiet: false,
    }
}

#[test]
fn a_workflow_report_strikes_or_resets_once_per_run() {
    let root = tempfile::tempdir().unwrap();
    let workspace = crate::workspace::WorkspaceResolver::resolve(root.path(), None).unwrap();
    let state = crate::StatePaths::for_project_root(root.path()).unwrap();
    let runtime = crate::RuntimePaths::for_state(&state).unwrap();
    crate::Store::open(state.clone(), runtime)
        .unwrap()
        .record_workspace(&workspace)
        .unwrap();
    let entry = crate::config::TaskEntry {
        check: Some("true".into()),
        every: Some("1h".into()),
        root: root.path().into(),
        ..crate::config::TaskEntry::default()
    };
    super::super::instances::insert(&state, "nightly", &entry).unwrap();
    let catalog = super::super::catalog::TaskCatalog::load_lenient(Some(root.path()));
    let key = catalog.for_run("nightly").unwrap().key("nightly");
    let run = |state: &str, visits: serde_json::Value| -> crate::store::workflow::RunRecord {
        serde_json::from_value(serde_json::json!({
            "version":crate::store::workflow::VERSION, "id":crate::ids::WorkflowRunId::new(), "workflow":"nightly",
            "origin":"inline", "task":"nightly", "spec_dir":"/spec", "spec_digest":"abc",
            "scope":{"kind":"worktree","path":root.path()}, "project_root":root.path(),
            "created_at":"2026-01-01T00:00:00Z", "state":state, "node":"agent",
            "entered_at":"2026-01-01T00:00:00Z", "visits":visits, "visit_grant":0,
            "deadline_fired":false, "next_seq":2, "terminal_at":"2026-01-01T00:01:00Z",
            "loop_report":"owed"
        }))
        .unwrap()
    };
    let failed = run("failed", serde_json::json!({"agent": 1}));
    for _ in 0..2 {
        report_run(&failed, &ended(RunEnding::Failed, Some(0.75), true)).unwrap();
        assert_eq!(
            super::super::strikes::load().get(&key),
            Some(&1),
            "a re-reported run strikes once"
        );
    }
    let rows = task_records(
        &crate::disk::paths::logs_dir(),
        "nightly",
        Some(root.path()),
    );
    assert_eq!(rows.len(), 1, "{rows:?}");
    let row = &rows[0];
    assert_eq!(
        (
            row.result,
            row.run_id.as_deref(),
            row.cost_usd,
            row.acted,
            row.duration_ms
        ),
        (
            LoopRunResult::Failed,
            Some(failed.id.to_string().as_str()),
            Some(0.75),
            Some(true),
            Some(60_000)
        )
    );
    report_run(
        &run("done", serde_json::json!({})),
        &ended(RunEnding::Done, None, false),
    )
    .unwrap();
    assert_eq!(
        super::super::strikes::load().get(&key),
        None,
        "a done run resets"
    );
    let rows = task_records(
        &crate::disk::paths::logs_dir(),
        "nightly",
        Some(root.path()),
    );
    assert_eq!(rows.last().map(|row| row.acted), Some(Some(false)));
    report_run(
        &run("canceled", serde_json::json!({})),
        &ended(RunEnding::Canceled, None, false),
    )
    .unwrap();
    report_run(&failed, &ended(RunEnding::Failed, None, true)).unwrap();
    assert_eq!(
        super::super::strikes::load().get(&key),
        None,
        "cancel is neutral"
    );

    // A quiet run collected again after a crash before its delete counts once, with another
    // task's quiet collection between the two.
    super::super::instances::insert(&state, "other", &entry).unwrap();
    let catalog = super::super::catalog::TaskCatalog::load_lenient(Some(root.path()));
    let other_key = catalog.for_run("other").unwrap().key("other");
    let quiet = run("done", serde_json::json!({}));
    let mut other = run("done", serde_json::json!({}));
    other.task = Some("other".into());
    let logged = || {
        task_records(
            &crate::disk::paths::logs_dir(),
            "nightly",
            Some(root.path()),
        )
    };
    let before = logged().len();
    let outcome = crate::store::workflow::RunOutcome {
        quiet: true,
        ..ended(RunEnding::Done, None, false)
    };
    for run in [&quiet, &other, &quiet] {
        assert!(matches!(
            report_run(run, &outcome),
            Ok(Collected::Forgotten)
        ));
    }
    let fires = super::super::fires::load();
    let count = |key: &str| {
        fires
            .get(key)
            .and_then(|fires| fires.quiet.as_ref())
            .map(|q| q.count)
    };
    assert_eq!(
        (count(&key), count(&other_key)),
        (Some(1), Some(1)),
        "{fires:?}"
    );
    assert_eq!(logged().len(), before, "a quiet run appends no row");

    // A run this RimZ ended as stranded, which decodes once ended, reports one row and moves
    // neither a strike nor the quiet streak, even with an outcome that says quiet.
    let quiet_before = super::super::fires::load().get(&key).cloned();
    assert!(
        quiet_before
            .as_ref()
            .and_then(|fires| fires.quiet.as_ref())
            .is_some_and(|q| q.streak > 0)
    );
    for outcome in [ended(RunEnding::Failed, None, false), outcome] {
        let mut stranded = run("failed", serde_json::json!({}));
        stranded.stranded = true;
        assert!(matches!(
            report_run(&stranded, &outcome),
            Ok(Collected::Reported(RunTransition::Recorded))
        ));
        let rows = logged();
        assert_eq!(
            rows.last().and_then(|row| row.run_id.as_deref()),
            Some(stranded.id.as_str())
        );
    }
    assert_eq!(logged().len(), before + 2);
    assert_eq!(
        super::super::strikes::load().get(&key),
        None,
        "a stranded run strikes no task"
    );
    assert_eq!(super::super::fires::load().get(&key).cloned(), quiet_before);
}

#[test]
fn a_run_that_gave_up_on_capacity_reports_a_neutral_throttle_skip() {
    let root = tempfile::tempdir().unwrap();
    let workspace = crate::workspace::WorkspaceResolver::resolve(root.path(), None).unwrap();
    let state = crate::StatePaths::for_project_root(root.path()).unwrap();
    let runtime = crate::RuntimePaths::for_state(&state).unwrap();
    crate::Store::open(state.clone(), runtime)
        .unwrap()
        .record_workspace(&workspace)
        .unwrap();
    let entry = crate::config::TaskEntry {
        agent: Some("claude".into()),
        prompt: Some("fix it".into()),
        every: Some("1h".into()),
        root: root.path().into(),
        ..crate::config::TaskEntry::default()
    };
    super::super::instances::insert(&state, "nightly", &entry).unwrap();
    let catalog = super::super::catalog::TaskCatalog::load_lenient(Some(root.path()));
    let key = catalog.for_run("nightly").unwrap().key("nightly");
    let failed: crate::store::workflow::RunRecord = serde_json::from_value(serde_json::json!({
        "version":crate::store::workflow::VERSION, "id":crate::ids::WorkflowRunId::new(), "workflow":"nightly",
        "origin":"inline", "task":"nightly", "spec_dir":"/spec", "spec_digest":"abc",
        "scope":{"kind":"worktree","path":root.path()}, "project_root":root.path(),
        "created_at":"2026-01-01T00:00:00Z", "state":"failed", "node":"@failed",
        "entered_at":"2026-01-01T00:59:59Z", "visits":{}, "visit_grant":0,
        "deadline_fired":false, "next_seq":2, "terminal_at":"2026-01-01T00:59:59Z",
        "loop_report":"owed"
    }))
    .unwrap();
    let gave_up = RunEnding::CapacityGaveUp {
        waited_ms: 3_600_000,
        reason: Some("2 agents open".into()),
    };
    report_run(&failed, &ended(gave_up, None, false)).unwrap();
    let rows = task_records(
        &crate::disk::paths::logs_dir(),
        "nightly",
        Some(root.path()),
    );
    assert_eq!(
        rows.iter()
            .map(|row| (row.result, row.error.as_deref()))
            .collect::<Vec<_>>(),
        [(
            LoopRunResult::ThrottleSkipped,
            Some("gave up waiting for capacity after 1h; 2 agents open")
        )]
    );
    assert_eq!(
        super::super::strikes::load().get(&key),
        None,
        "a capacity give-up never strikes"
    );
}

#[test]
fn a_run_that_never_acted_or_failed_frees_its_checkout_and_an_acted_or_canceled_end_keeps_it() {
    let root = tempfile::tempdir().unwrap();
    let state = crate::StatePaths::for_project_root(root.path()).unwrap();
    let checkout = root.path().join("checkout");
    std::fs::create_dir_all(&checkout).unwrap();
    for (run_state, acted, kept) in [
        ("done", false, false),
        ("failed", true, false),
        ("failed", false, false),
        ("canceled", false, true),
        ("done", true, true),
        ("canceled", true, true),
    ] {
        let run: crate::store::workflow::RunRecord = serde_json::from_value(serde_json::json!({
            "version":crate::store::workflow::VERSION, "id":crate::ids::WorkflowRunId::new(), "workflow":"resident",
            "origin":"inline", "task":"resident", "spec_dir":"/spec", "spec_digest":"abc",
            "scope":{"kind":"worktree","path":checkout}, "project_root":root.path(),
            "created_at":"2026-01-01T00:00:00Z", "state":run_state, "node":"agent",
            "entered_at":"2026-01-01T00:00:00Z", "visits":{}, "visit_grant":0,
            "deadline_fired":false, "next_seq":2, "terminal_at":"2026-01-01T00:01:00Z",
            "loop_report":"owed"
        }))
        .unwrap();
        super::super::launch_ledger_store::record_workflow(
            &state,
            "resident",
            &checkout,
            super::super::launch_ledger::WorkflowLaunchRecord {
                at: jiff::Timestamp::now(),
                run: Some(run.id.clone()),
                scope: crate::store::workflow::ScopeKind::Worktree,
                leader: None,
                destination: None,
            },
            false,
        )
        .unwrap();
        let ending = match run_state {
            "done" => RunEnding::Done,
            "failed" => RunEnding::Failed,
            _ => RunEnding::Canceled,
        };
        report_run(&run, &ended(ending, None, acted)).unwrap();
        let memory = super::super::launch_ledger::load_workflows(&state).unwrap();
        assert_eq!(
            memory
                .tasks
                .get("resident")
                .is_some_and(|launches| launches.contains_key(&checkout)),
            kept,
            "{run_state} acted={acted}"
        );
    }
}
