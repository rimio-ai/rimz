use super::*;
use std::io::Write as _;

fn record(task: &str, second: i64, result: LoopRunResult) -> LoopRunRecord {
    LoopRunRecord {
        checkout: None,
        task: task.to_owned(),
        root: None,
        at: Timestamp::from_second(second).expect("timestamp"),
        result,
        mode: None,
        duration_ms: None,
        throttle_wait_ms: None,
        error: None,
        check: None,
        watch: None,
        signal: None,
        condition: None,
        message_id: None,
        run_id: None,
        transcript_path: None,
        last_message: None,
        target: None,
        cost_usd: None,
        input_tokens: None,
        output_tokens: None,
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
        (Some(0), true, Some(RunPolarity::Good)),
        (Some(1), false, None),
        (None, true, None),
    ] {
        let mut row = record("task", 1, CheckSkipped);
        row.check = Some(CheckRecord {
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
            since: Some(failed.at)
        })
    );
    assert_eq!(
        task.heard,
        Some(HeardSignal {
            signal: "ci.passed".parse().unwrap(),
            at: heard.at
        })
    );
    assert_eq!(task.last.result, LoopRunResult::SignalSkipped);
    assert_eq!(task.runs, 6);
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
            failed,
            neutral.clone(),
            error.clone(),
            neutral
        ]),
        Some(ActingRun {
            record: error,
            polarity: RunPolarity::Failure,
            streak: 2,
            since: Some(good.at)
        })
    );
    assert_eq!(acting_run(&[]), None);
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
            root,
            None,
            since
        ));
        let mut row = record(task, second, LoopRunResult::Expired);
        row.root = Some(recorded_root.to_path_buf());
        row.mode = mode;
        append_to(dir.path(), &row);
        assert_eq!(
            has_scheduled_row_since(dir.path(), "task", root, None, since),
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
    assert_eq!(wait.last.result, LoopRunResult::TargetGone);
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
    assert_eq!(wait.last.result, LoopRunResult::Completed);
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
    assert_eq!(wait.last.result, LoopRunResult::Failed);
}

#[test]
fn new_fields_round_trip_and_task_records_filter() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut with_detail = record("wait", 10, LoopRunResult::Failed);
    with_detail.mode = Some(LoopRunMode::Manual);
    with_detail.duration_ms = Some(123);
    with_detail.check = Some(CheckRecord {
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
    assert!(!serde_json::to_string(&record).unwrap().contains("throttle"));
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
