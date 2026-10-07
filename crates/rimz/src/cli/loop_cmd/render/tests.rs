use super::super::run_report::{render_record_detail, write_failure_pointer};
use super::*;

fn show_view(each_worktree: bool) -> ShowView {
    let now = Timestamp::from_second(100).unwrap();
    let entry = TaskEntry {
        root: "/repo".into(),
        agent: Some("sweeper".into()),
        check: Some("true".into()),
        when: Some(vec!["pr=open && ci=passed".into()]),
        stay: each_worktree,
        each_worktree,
        takeover: each_worktree,
        subscribe: ["ci.failed", "pr.merged"]
            .map(|signal| rimz::config::TeamSignalBinding {
                signal: signal.into(),
                matches: BTreeMap::new(),
                prompt: None,
            })
            .into(),
        ..TaskEntry::default()
    };
    let timing = schedule::TaskTiming::evaluate(
        schedule::TaskShape::compile("sweep", &entry).trigger(),
        TaskSource::Config,
        None,
        None,
        &now.to_zoned(jiff::tz::TimeZone::UTC),
    );
    let mut failed = record(20, LoopRunResult::Errored);
    failed.error = Some("launch failed".into());
    ShowView {
        name: "sweep".into(),
        entry,
        source: TaskSource::Config,
        timing,
        now_zoned: now.to_zoned(jiff::tz::TimeZone::UTC),
        records: vec![failed],
        launches: BTreeMap::from([
            (
                PathBuf::from("/repo/older"),
                schedule::launch_ledger::LaunchRecord {
                    at: Timestamp::from_second(10).unwrap(),
                    leader: "old-leader".into(),
                },
            ),
            (
                PathBuf::from("/repo/newer"),
                schedule::launch_ledger::LaunchRecord {
                    at: Timestamp::from_second(90).unwrap(),
                    leader: "new-leader".into(),
                },
            ),
        ]),
        in_flight: None,
        condition: "condition: waiting\n  pr=open   ✓ open\n  ci=passed   ✗ unknown\n".into(),
        subscriptions: vec![SubscriptionView {
            name: "wake".into(),
            checkout: "/repo/newer".into(),
            target: None,
            signal: Some("ci.failed".into()),
            last: "never fired".into(),
            last_style: ui::palette::muted(),
        }],
        room_is_open: false,
        strike_count: 0,
        live_leader: false,
        you: false,
        config: MachineConfig::default().into(),
        throttle: ShowThrottle::Compact(Some(
            "cpu 14%/18% · io 26%/22% · memory 0%/0% (avg10/avg60)".into(),
        )),
        show_agent_runs: true,
    }
}

fn show_text(view: &ShowView) -> String {
    let mut out = Vec::new();
    render_show(&mut out, view, 10).unwrap();
    anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string()
}

#[test]
fn show_fanout_verdict_precedes_launches_and_has_no_subscriptions_section() {
    let view = show_view(true);
    let text = show_text(&view);
    assert!(
        text.lines()
            .nth(1)
            .is_some_and(|line| line.starts_with("  ✗ failing")),
        "{text}"
    );
    assert!(
        text.lines().next().unwrap().ends_with(" · each worktree"),
        "{text}"
    );
    let sections = [
        "LAUNCHES",
        "condition: evaluated per owned worktree · 2 launched",
        "LAST RUN",
        "AGENT RUNS",
        "RECENT RUNS",
        "  action:",
    ];
    let positions = sections.map(|section| text.find(section).expect(section));
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{text}");
    assert!(!text.contains("SUBSCRIPTIONS"), "{text}");
    assert!(text.contains("  newer  @new-leader  10s ago"), "{text}");
    assert!(
        text.find("  newer").unwrap() < text.find("  older").unwrap(),
        "{text}"
    );
    assert!(
        !text.contains("/repo/newer") && !text.contains("1970-"),
        "{text}"
    );
}

#[test]
fn show_single_checkout_condition_follows_verdict_and_keeps_subscriptions() {
    let mut view = show_view(false);
    view.launches.clear();
    let text = show_text(&view);
    assert!(
        text.lines()
            .nth(1)
            .is_some_and(|line| line.starts_with("  ✗ failing")),
        "{text}"
    );
    assert_eq!(text.lines().nth(2), Some("condition: waiting"), "{text}");
    assert!(
        text.find("RECENT RUNS").unwrap() < text.find("SUBSCRIPTIONS").unwrap(),
        "{text}"
    );
    assert!(
        text.find("SUBSCRIPTIONS").unwrap() < text.find("  action:").unwrap(),
        "{text}"
    );
}

#[test]
fn show_resident_facts_name_action_and_wakes_without_timeout() {
    let mut view = show_view(true);
    view.entry.timeout = Some("1h".into());
    view.entry.account = Some("work".parse().unwrap());
    let text = show_text(&view);
    assert!(
        text.contains("check, then start sweeper · account work · stays · takes over the checkout"),
        "{text}"
    );
    assert!(
        text.contains("leader on ci.failed, pr.merged · 1 armed"),
        "{text}"
    );
    assert!(
        !text.contains("timeout:") && !text.contains("  task:"),
        "{text}"
    );
}

#[test]
fn show_wakes_with_no_subscriptions_and_no_live_leader() {
    let mut view = show_view(true);
    view.subscriptions.clear();
    let text = show_text(&view);
    assert!(
        text.contains("leader on ci.failed, pr.merged · 0 armed, no live leader"),
        "{text}"
    );
}

#[test]
fn show_wakes_with_a_live_leader_and_no_subscriptions() {
    let mut view = show_view(true);
    view.subscriptions.clear();
    view.live_leader = true;
    let text = show_text(&view);
    assert!(
        text.contains("leader on ci.failed, pr.merged · 0 armed"),
        "{text}"
    );
    assert!(!text.contains("no live leader"), "{text}");
}

#[test]
fn show_check_only_action_and_timeout_for_nonresident_agents() {
    let view = show_view(false);
    assert!(show_text(&view).contains("timeout:"));
    let mut view = view;
    view.entry.agent = None;
    view.entry.when = None;
    view.entry.subscribe.clear();
    view.entry.every = Some("1h".into());
    let text = show_text(&view);
    assert!(
        text.lines()
            .any(|line| line.trim_start().starts_with("action:")
                && line.ends_with("run check · true")),
        "{text}"
    );
    assert!(
        !text.contains("  check:") && !text.contains("timeout:"),
        "{text}"
    );
}

#[test]
fn show_no_limits_uses_compact_throttle_facts() {
    let text = show_text(&show_view(true));
    assert!(text.contains("throttle: no limits set"), "{text}");
    assert!(
        text.contains("load:     cpu 14%/18% · io 26%/22% · memory 0%/0% (avg10/avg60)"),
        "{text}"
    );
    assert!(!text.contains("THROTTLE"), "{text}");
}

#[test]
fn show_configured_limits_keep_full_throttle_section() {
    let mut view = show_view(true);
    view.throttle = ShowThrottle::Full(vec![(
        "cpu pressure",
        "avg10 2% · avg60 3% (limit 25%)".into(),
    )]);
    let text = show_text(&view);
    assert!(text.contains("THROTTLE\n"), "{text}");
    assert!(!text.contains("no limits set"), "{text}");
}

#[test]
fn true_condition_without_hold_renders_due() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("blackboard.md"), "Stage: Done\n").unwrap();
    let entry = TaskEntry {
        root: root.path().to_owned(),
        when: Some(vec!["team.stage=Done".to_owned()]),
        check: Some("true".to_owned()),
        ..TaskEntry::default()
    };
    let parsed = schedule::parse_trigger("ready", &entry);
    let mut out = Vec::new();
    super::super::condition::write_receipt(&mut out, &entry, parsed.as_ref().unwrap()).unwrap();
    assert!(String::from_utf8(out).unwrap().contains("now: due\n"));
    let now = Timestamp::now();
    let timing = schedule::TaskTiming::evaluate(
        &parsed,
        schedule::catalog::TaskSource::Config,
        Some(now),
        None,
        &now.to_zoned(jiff::tz::TimeZone::UTC),
    );
    let timing = super::super::condition::observe("ready", &entry, timing, now);
    let mut out = Vec::new();
    super::super::condition::write_show(&mut out, &entry, &timing).unwrap();
    assert!(
        String::from_utf8(out)
            .unwrap()
            .starts_with("condition: due\n")
    );
}

fn record(second: i64, result: LoopRunResult) -> LoopRunRecord {
    LoopRunRecord {
        checkout: None,
        task: "wait".to_owned(),
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

fn check(code: Option<i32>, output: &str) -> CheckRecord {
    CheckRecord {
        output_path: None,
        code,
        timed_out: false,
        output: output.to_owned(),
    }
}

/// An overlap as `prepare_run_lock` records it: a note no two fires share.
fn overlap(second: i64) -> LoopRunRecord {
    let mut row = record(second, LoopRunResult::Overlapped);
    row.mode = Some(LoopRunMode::Scheduled);
    row.duration_ms = Some(0);
    row.error = Some(format!(
        "previous run still active (pid {}, started {second}m ago) — skipped",
        4_000 + second
    ));
    row
}

fn scheduled(second: i64, result: LoopRunResult) -> LoopRunRecord {
    let mut row = record(second, result);
    row.mode = Some(LoopRunMode::Scheduled);
    row
}

fn runs_table(records: &[LoopRunRecord], limit: usize) -> String {
    let mut out = Vec::new();
    write_runs_table(
        &mut out,
        records,
        limit,
        Timestamp::from_second(100).unwrap(),
    )
    .unwrap();
    anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string()
}

fn last_run(records: &[LoopRunRecord]) -> String {
    let mut out = Vec::new();
    write_last_run(
        &mut out,
        "wait",
        &TaskEntry::default(),
        records,
        Timestamp::from_second(100).unwrap(),
        ui::prose::Prose::Raw,
    )
    .unwrap();
    anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string()
}

#[test]
fn throttle_skip_is_a_refused_fire_with_a_visible_reason_and_a_waited_row_says_held() {
    let mut skipped = record(0, LoopRunResult::ThrottleSkipped);
    skipped.error = Some("cpu pressure 41% >= 25%; held 30m".into());
    assert!(is_refused_fire(&skipped));

    let mut waited = record(0, LoopRunResult::Launched);
    waited.throttle_wait_ms = Some(192_000);
    assert_eq!(record_note(&waited).as_deref(), Some("held 3m"));
    waited.target = Some("@fixer".into());
    assert_eq!(record_note(&waited).as_deref(), Some("held 3m · @fixer"));
    waited.checkout = Some(PathBuf::from("/repo/lane"));
    assert_eq!(
        record_note(&waited).as_deref(),
        Some("/repo/lane held 3m · @fixer")
    );
}

#[test]
fn checkout_without_a_note_has_no_trailing_separator() {
    let mut row = record(0, LoopRunResult::Launched);
    row.checkout = Some(PathBuf::from("/repo/lane"));
    assert_eq!(record_note(&row).as_deref(), Some("/repo/lane"));
    row.target = Some("@fixer".into());
    assert_eq!(record_note(&row).as_deref(), Some("/repo/lane @fixer"));
}

#[test]
fn task_rules_and_check_rows_use_action_specific_verbs() {
    let spawn = TaskEntry {
        agent: Some("codex".to_owned()),
        check: Some("cargo test".to_owned()),
        on: Some(CheckOn::Fail),
        ..TaskEntry::default()
    };
    let spawn_action = schedule::TaskShape::compile("task", &spawn)
        .action()
        .unwrap()
        .clone();
    assert_eq!(
        task_run_rule(&spawn, &spawn_action),
        "check, then start codex on fail"
    );
    assert_eq!(
        check_summary(&spawn, Some(&spawn_action)).as_deref(),
        Some("cargo test (starts codex on fail)")
    );

    let wait = TaskEntry {
        wait: Some(TaskTarget {
            kind: rimz::ids::AgentKind::new_unchecked("claude"),
            session: "sess-planner".into(),
            handle: "@planner".to_owned(),
        }),
        check: Some("cargo test".to_owned()),
        on: Some(CheckOn::Success),
        ..TaskEntry::default()
    };
    let wait_action = schedule::TaskShape::compile("task", &wait)
        .action()
        .unwrap()
        .clone();
    assert_eq!(
        task_run_rule(&wait, &wait_action),
        "check, then wake @planner on success"
    );
    assert_eq!(
        check_summary(&wait, Some(&wait_action)).as_deref(),
        Some("cargo test (wakes @planner on success)")
    );

    let check = TaskEntry {
        check: Some("cargo test".to_owned()),
        ..TaskEntry::default()
    };
    let check_action = schedule::TaskShape::compile("task", &check)
        .action()
        .unwrap()
        .clone();
    assert_eq!(task_run_rule(&check, &check_action), "check");

    let spawn = TaskEntry {
        agent: Some("claude".to_owned()),
        verify: Some("cargo xtask gate".to_owned()),
        max_attempts: Some(4),
        ..TaskEntry::default()
    };
    let spawn_action = schedule::TaskShape::compile("task", &spawn)
        .action()
        .unwrap()
        .clone();
    assert_eq!(
        task_run_rule(&spawn, &spawn_action),
        "start claude, verify `cargo xtask gate` (up to 4 attempts)"
    );

    let mut wait_only = wait;
    wait_only.check = None;
    let wait_action = schedule::TaskShape::compile("task", &wait_only)
        .action()
        .unwrap()
        .clone();
    assert_eq!(task_run_rule(&wait_only, &wait_action), "wake @planner");
}

#[test]
fn budget_label_names_the_run_and_daily_caps() {
    let entry = TaskEntry {
        budget: Some("$5.00".to_owned()),
        budget_per_day: Some("$20.00".to_owned()),
        ..TaskEntry::default()
    };
    assert_eq!(
        budget_label(&entry).as_deref(),
        Some("$5 per run · $20 per day")
    );
}

#[test]
fn surplus_label_shows_explicit_and_implied_thresholds() {
    let explicit = TaskEntry {
        surplus: Some("1.5x".to_owned()),
        surplus_after: Some("3d".to_owned()),
        ..TaskEntry::default()
    };
    assert_eq!(
        surplus_label(&explicit).as_deref(),
        Some("surplus ≥ 1.5x · after 3d of window")
    );
    let implied = TaskEntry {
        surplus_after: Some("2d".to_owned()),
        ..TaskEntry::default()
    };
    assert_eq!(
        surplus_label(&implied).as_deref(),
        Some("surplus ≥ 1.0x · after 2d of window")
    );
}

#[test]
fn spend_label_renders_today_last_and_cost_window() {
    let now = "2026-06-02T12:00:00Z[UTC]".parse::<jiff::Zoned>().unwrap();
    let entry = TaskEntry {
        budget_per_day: Some("20".to_owned()),
        ..TaskEntry::default()
    };
    let mut records = Vec::new();
    for (second, cost) in [(1, 0.28), (2, 0.42)] {
        let mut run = record(second, LoopRunResult::Completed);
        run.at = now.timestamp() + jiff::SignedDuration::from_secs(second);
        run.cost_usd = Some(cost);
        records.push(run);
    }

    assert_eq!(
        spend_label(&entry, &records, &now, true).as_deref(),
        Some("$0.70 today of $20 · $0.42 last · ø $0.35 over 2 runs")
    );
    assert_eq!(
        spend_label(&entry, &records, &now, false).as_deref(),
        Some("$0.70 today of $20")
    );
    assert_eq!(spend_label(&TaskEntry::default(), &[], &now, true), None);
}

#[test]
fn verdict_names_the_newest_run_and_the_bound_of_its_streak() {
    let now = Timestamp::from_second(50).unwrap();
    let failed = record(10, LoopRunResult::Failed);
    let mut passed_check = record(20, LoopRunResult::CheckSkipped);
    passed_check.check = Some(check(Some(0), "ok"));
    let neutral = record(30, LoopRunResult::Overlapped);
    let mut completed = record(40, LoopRunResult::Completed);
    completed.duration_ms = Some(5_000);

    let (healthy, style) = verdict_line(
        &[failed, passed_check, neutral.clone(), completed.clone()],
        now,
    )
    .unwrap();
    assert_eq!(
        healthy,
        "✓ healthy · last run 10s ago, completed in 5.0s · 2 in a row since a failure 40s ago"
    );
    assert_eq!(style, ui::palette::good());

    let errored = record(40, LoopRunResult::Errored);
    let failed = record(20, LoopRunResult::Failed);
    let (failing, style) = verdict_line(
        &[completed.clone(), failed, neutral.clone(), errored.clone()],
        now,
    )
    .unwrap();
    assert_eq!(
        failing,
        "✗ failing · last run 10s ago, error · 2 in a row since a good run 10s ago"
    );
    assert_eq!(style, ui::palette::alarm());

    let (first, _) = verdict_line(&[completed.clone(), errored], now).unwrap();
    assert_eq!(
        first,
        "✗ failing · last run 10s ago, error · first since a good run 10s ago"
    );
    let (unbounded, _) = verdict_line(&[completed.clone(), completed], now).unwrap();
    assert_eq!(
        unbounded,
        "✓ healthy · last run 10s ago, completed in 5.0s · 2 in a row"
    );
    assert!(verdict_line(&[neutral], now).is_none());
}

#[test]
fn agent_run_predicate_counts_spawn_and_delivery_attempts() {
    let mut spawned = record(1, LoopRunResult::Completed);
    spawned.run_id = Some("run_0123456789abcdef01234567".to_owned());
    assert!(is_agent_run(&spawned));
    assert!(is_agent_run(&record(2, LoopRunResult::Delivered)));
    assert!(is_agent_run(&record(3, LoopRunResult::TargetGone)));
    for result in [
        LoopRunResult::CheckSkipped,
        LoopRunResult::BudgetSkipped,
        LoopRunResult::SurplusSkipped,
        LoopRunResult::AccountSkipped,
        LoopRunResult::Overlapped,
        LoopRunResult::Expired,
    ] {
        assert!(!is_agent_run(&record(4, result)), "{result:?}");
    }
}

#[test]
fn agent_runs_heading_aggregates_all_valid_costs() {
    let now = Timestamp::from_second(50).unwrap();
    let mut costed = record(10, LoopRunResult::Completed);
    costed.run_id = Some("run_0123456789abcdef01234567".to_owned());
    costed.cost_usd = Some(0.25);
    let mut delivered = record(20, LoopRunResult::Delivered);
    delivered.cost_usd = Some(0.75);
    let skipped = record(30, LoopRunResult::CheckSkipped);
    let mut out = Vec::new();
    write_agent_runs(&mut out, &[costed, delivered, skipped.clone()], now).unwrap();
    let out = anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string();
    assert!(
        out.contains("AGENT RUNS — 2 of 3 runs · $1.00 total · ø $0.50"),
        "{out}"
    );

    let mut cost_free = record(10, LoopRunResult::Delivered);
    cost_free.cost_usd = Some(f64::NAN);
    let mut out = Vec::new();
    write_agent_runs(&mut out, &[cost_free], now).unwrap();
    let out = anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string();
    assert!(out.contains("AGENT RUNS — 1 of 1 runs"), "{out}");
    assert!(!out.contains("total"), "{out}");

    let mut out = Vec::new();
    write_agent_runs(&mut out, &[skipped], now).unwrap();
    let out = anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string();
    assert!(out.contains("AGENT RUNS — none in 1 runs"), "{out}");
    assert!(!out.contains("WHEN"), "{out}");
}

#[test]
fn check_failure_line_uses_last_non_empty_failed_check_line() {
    let mut failed = record(10, LoopRunResult::Failed);
    failed.check = Some(CheckRecord {
        output_path: None,
        code: Some(127),
        timed_out: false,
        output: "ignored\n\nmissing command\n".to_owned(),
    });
    assert_eq!(check_failure_line(&failed), Some("missing command"));

    let mut passed = record(11, LoopRunResult::Completed);
    passed.check = Some(CheckRecord {
        output_path: None,
        code: Some(0),
        timed_out: false,
        output: "ok".to_owned(),
    });
    assert_eq!(check_failure_line(&passed), None);
}

#[test]
fn record_note_prefers_error_then_failed_check_output() {
    let mut failed = record(10, LoopRunResult::Failed);
    failed.check = Some(CheckRecord {
        output_path: None,
        code: Some(1),
        timed_out: false,
        output: "first\ncheck failed".to_owned(),
    });
    failed.last_message = Some("last message".to_owned());
    assert_eq!(record_note(&failed), Some("check failed".to_owned()));

    failed.error = Some("outer error\nignored detail".to_owned());
    assert_eq!(record_note(&failed), Some("outer error".to_owned()));
}

#[test]
fn run_status_names_check_skipped_outcomes() {
    let mut skipped = record(10, LoopRunResult::CheckSkipped);
    skipped.check = Some(CheckRecord {
        output_path: None,
        code: Some(0),
        timed_out: false,
        output: "ok".to_owned(),
    });
    let status = run_status(&skipped);
    assert_eq!(status.glyph, "✓");
    assert_eq!(status.label, "check passed");
    assert_eq!(status.style, ui::palette::good());

    skipped.check = Some(CheckRecord {
        output_path: None,
        code: Some(1),
        timed_out: false,
        output: "not yet".to_owned(),
    });
    let status = run_status(&skipped);
    assert_eq!(status.glyph, "○");
    assert_eq!(status.label, "check failed");
    assert_eq!(status.style, ui::palette::muted());

    skipped.check = Some(CheckRecord {
        output_path: None,
        code: None,
        timed_out: true,
        output: "too slow".to_owned(),
    });
    let status = run_status(&skipped);
    assert_eq!(status.glyph, "○");
    assert_eq!(status.label, "check timed out");
    assert_eq!(status.style, ui::palette::warn());

    assert_eq!(
        loop_result_mark(LoopRunResult::SurplusSkipped).style,
        ui::palette::muted()
    );
}

#[test]
fn run_result_marks_and_static_labels_cover_every_variant() {
    let cases = [
        (
            LoopRunResult::Completed,
            "✓",
            ui::palette::good(),
            "completed",
        ),
        (
            LoopRunResult::Delivered,
            "✓",
            ui::palette::good(),
            "delivered",
        ),
        (LoopRunResult::Failed, "✗", ui::palette::alarm(), "failed"),
        (
            LoopRunResult::VerifyFailed,
            "✗",
            ui::palette::alarm(),
            "verify failed",
        ),
        (
            LoopRunResult::TimedOut,
            "✗",
            ui::palette::alarm(),
            "timed out",
        ),
        (
            LoopRunResult::BudgetExceeded,
            "✗",
            ui::palette::alarm(),
            "budget exceeded",
        ),
        (LoopRunResult::Errored, "✗", ui::palette::alarm(), "error"),
        (
            LoopRunResult::StartFailed,
            "✗",
            ui::palette::alarm(),
            "start failed",
        ),
        (LoopRunResult::Expired, "○", ui::palette::warn(), "expired"),
        (
            LoopRunResult::Canceled,
            "○",
            ui::palette::warn(),
            "canceled",
        ),
        (
            LoopRunResult::TargetGone,
            "○",
            ui::palette::warn(),
            "target gone",
        ),
        (
            LoopRunResult::Overlapped,
            "○",
            ui::palette::warn(),
            "overlapped",
        ),
        (
            LoopRunResult::BudgetSkipped,
            "○",
            ui::palette::warn(),
            "budget skipped",
        ),
        (
            LoopRunResult::SurplusSkipped,
            "○",
            ui::palette::muted(),
            "surplus skipped",
        ),
        (
            LoopRunResult::AccountSkipped,
            "○",
            ui::palette::warn(),
            "account skipped",
        ),
        (
            LoopRunResult::ThrottleSkipped,
            "○",
            ui::palette::warn(),
            "throttle skipped",
        ),
        (
            LoopRunResult::TakeoverBlocked,
            "○",
            ui::palette::warn(),
            "takeover blocked",
        ),
        (
            LoopRunResult::CheckSkipped,
            "○",
            ui::palette::muted(),
            "skipped",
        ),
    ];

    for (result, glyph, style, label) in cases {
        let mark = loop_result_mark(result);
        assert_eq!(mark.glyph, glyph, "{result:?}");
        assert_eq!(mark.style, style, "{result:?}");
        assert_eq!(result.label(), label, "{result:?}");
        if result != LoopRunResult::CheckSkipped {
            let status = run_status(&record(10, result));
            assert_eq!(status.glyph, glyph, "{result:?}");
            assert_eq!(status.style, style, "{result:?}");
            assert_eq!(status.label, label, "{result:?}");
        }
    }
}

#[test]
fn team_source_label_requires_an_instance_row() {
    let entry = TaskEntry {
        root: PathBuf::from("/tmp/project"),
        team: Some("forge#feat-x".parse().unwrap()),
        ..TaskEntry::default()
    };
    assert_eq!(
        source_description(TaskSource::Instance, &entry),
        "team forge#feat-x"
    );
    assert_eq!(source_description(TaskSource::Config, &entry), "machine");
    assert_eq!(
        source_description(
            TaskSource::Project {
                state: TrustState::Untrusted
            },
            &entry
        ),
        "project · untrusted"
    );
    assert_eq!(
        source_description(TaskSource::Instance, &TaskEntry::default()),
        "state"
    );
    assert!(source_detail(TaskSource::Instance, &entry).starts_with("team forge#feat-x — "));
}

#[test]
fn source_detail_names_definition_path() {
    let entry = TaskEntry {
        root: PathBuf::from("/repo"),
        ..TaskEntry::default()
    };

    assert_eq!(
        source_detail(TaskSource::Config, &entry),
        format!(
            "machine — {}",
            ui::home_relative(MachineConfig::loop_path().to_string_lossy().as_ref())
        )
    );
    assert_eq!(
        source_detail(
            TaskSource::Project {
                state: TrustState::Untrusted
            },
            &entry,
        ),
        "project · untrusted — /repo/.rimz/config.toml"
    );
    assert_eq!(
        source_detail(TaskSource::Instance, &entry),
        format!(
            "state — {}",
            ui::home_relative(
                StatePaths::for_project_root(&entry.resolved_root())
                    .expect("state paths")
                    .root
                    .join("records/loop-instances.json")
                    .to_string_lossy()
                    .as_ref(),
            )
        )
    );
}

#[test]
fn blocked_project_rendering_names_the_gate_and_fix() {
    assert_eq!(
        blocked_notice(TrustState::Untrusted),
        "project trust is untrusted — review with `rimz trust`, approve with `rimz trust grant`"
    );
}

fn interval_timing(
    blocked: Option<TrustState>,
    last_fire: Option<Timestamp>,
    arming: Option<&Arming>,
    now: Timestamp,
) -> schedule::TaskTiming {
    let entry = TaskEntry {
        agent: Some("claude".to_owned()),
        every: Some("15m".to_owned()),
        ..TaskEntry::default()
    };
    schedule::TaskTiming::evaluate(
        schedule::TaskShape::compile("task", &entry).trigger(),
        blocked.map_or(TaskSource::Config, |state| TaskSource::Project { state }),
        last_fire,
        arming,
        &now.to_zoned(jiff::tz::TimeZone::UTC),
    )
}

#[test]
fn signal_timing_renders_trigger_matches_and_listening_state() {
    let now = Timestamp::from_second(10_000).unwrap();
    let entry = TaskEntry {
        signal: Some("ci.failed".to_owned()),
        matches: Some(BTreeMap::from([(
            "branch".to_owned(),
            "feature".to_owned(),
        )])),
        ..TaskEntry::default()
    };
    let timing = schedule::TaskTiming::evaluate(
        schedule::TaskShape::compile("task", &entry).trigger(),
        TaskSource::Config,
        None,
        None,
        &now.to_zoned(jiff::tz::TimeZone::UTC),
    );

    let mut out = Vec::new();
    write_show_headline(&mut out, "task", &timing, None, now, false).unwrap();
    let show = String::from_utf8(out).unwrap();
    assert!(
        show.contains("on ci.failed [branch=feature] · listening"),
        "{show}"
    );
}

#[test]
fn running_replaces_the_headline_state_and_scales_its_elapsed_time() {
    let now = Timestamp::from_second(100_000).unwrap();
    let holder = |age: i64| RunLockInfo {
        pid: 4_162_080,
        started_at: now - jiff::SignedDuration::from_secs(age),
    };
    for (age, text) in [
        (45, "▸ running 45s"),
        (190 * 60, "▸ running 3h"),
        (-5, "▸ running 0s"),
    ] {
        assert_eq!(running_text(Some(holder(age)), now), text);
    }
    assert_eq!(running_text(None, now), "▸ running");
    let holderless = InFlightRun {
        holder: None,
        run: None,
        held: Vec::new(),
    };
    assert_eq!(running_text_full(&holderless, now), "▸ running");

    let since_ms = u64::try_from((now - jiff::SignedDuration::from_secs(192)).as_millisecond())
        .expect("after the epoch");
    let mut held = Held {
        pid: holder(200).pid,
        checkout: "/repo/.worktrees/a".into(),
        since_ms,
        reason: Some("cpu pressure 41% >= 25%".to_owned()),
        position: 2,
    };
    let waiting = InFlightRun {
        holder: Some(holder(200)),
        run: None,
        held: vec![held.clone()],
    };
    assert_eq!(
        running_text_full(&waiting, now),
        format!(
            "held: cpu pressure 41% >= 25%, 3m · pid {}",
            holder(200).pid
        )
    );
    let json = held_json(&held);
    assert_eq!(json["pid"], holder(200).pid);
    assert_eq!(json["checkout"], "/repo/.worktrees/a");
    assert_eq!(json["reason"], "cpu pressure 41% >= 25%");
    assert_eq!(json["position"], 2);
    assert_eq!(
        json["since"],
        serde_json::json!(now - jiff::SignedDuration::from_secs(192))
    );
    held.reason = None;
    assert_eq!(held_run_text(&held, now), "held: waiting for its turn, 3m");

    let mut readings = Vec::new();
    write_throttle_readings(
        &mut readings,
        &[("cpu pressure", "avg10 2% · avg60 3%".into())],
    )
    .expect("write");
    let readings = String::from_utf8(readings).expect("utf8");
    assert!(readings.starts_with("\nTHROTTLE\n"), "{readings}");
    assert!(readings.contains("cpu pressure"), "{readings}");
    assert!(readings.contains("avg10 2% · avg60 3%"), "{readings}");

    let pause = Arming {
        enabled: true,
        at: None,
        pause_until: Some(now + jiff::SignedDuration::from_secs(300)),
        strikes: None,
    };
    let timing = interval_timing(None, None, Some(&pause), now);
    let in_flight = InFlightRun {
        holder: Some(holder(180)),
        run: None,
        held: Vec::new(),
    };
    let mut out = Vec::new();
    write_show_headline(&mut out, "task", &timing, Some(&in_flight), now, false).unwrap();
    let show = anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string();
    assert!(
        show.ends_with(" · ▸ running 3m · pid 4162080\n") && !show.contains("paused"),
        "{show}"
    );
}

#[test]
fn show_headline_keeps_blocked_before_pause() {
    let now = Timestamp::from_second(10_000).unwrap();
    let pause = Arming {
        enabled: true,
        at: None,
        pause_until: Timestamp::from_second(10_300).ok(),
        strikes: None,
    };
    let timing = interval_timing(Some(TrustState::Untrusted), None, Some(&pause), now);
    let mut out = Vec::new();

    write_show_headline(&mut out, "task", &timing, None, now, false).unwrap();

    let out = anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string();
    assert!(out.contains("next blocked · trust"), "{out}");
    assert!(!out.contains("paused"), "{out}");
    assert!(!out.contains("loop enable"), "{out}");
}

#[test]
fn record_exit_maps_terminal_spawn_results_only_with_run_id() {
    for (result, expected) in [
        (LoopRunResult::Completed, Some("0")),
        (LoopRunResult::Failed, Some("1")),
        (LoopRunResult::VerifyFailed, Some("123")),
        (LoopRunResult::TimedOut, Some("124")),
        (LoopRunResult::BudgetExceeded, Some("125")),
        (LoopRunResult::Canceled, Some("130")),
        (LoopRunResult::BudgetSkipped, None),
        (LoopRunResult::SurplusSkipped, None),
        (LoopRunResult::AccountSkipped, None),
        (LoopRunResult::Delivered, None),
        (LoopRunResult::TargetGone, None),
        (LoopRunResult::CheckSkipped, None),
        (LoopRunResult::SignalSkipped, None),
        (LoopRunResult::Expired, None),
        (LoopRunResult::Errored, None),
        (LoopRunResult::StartFailed, None),
        (LoopRunResult::Overlapped, None),
        (LoopRunResult::TakeoverBlocked, None),
    ] {
        let mut run = record(10, result);
        assert_eq!(record_exit(&run), None, "{result:?} without run_id");

        run.run_id = Some("run_0123456789abcdef01234567".to_owned());
        assert_eq!(record_exit(&run).as_deref(), expected, "{result:?}");
    }
}

#[test]
fn run_status_merges_failed_check_exit() {
    let mut failed = record(10, LoopRunResult::Failed);
    failed.run_id = Some("run_0123456789abcdef01234567".to_owned());
    failed.check = Some(CheckRecord {
        output_path: None,
        code: Some(127),
        timed_out: false,
        output: "missing".to_owned(),
    });

    let status = run_status(&failed);

    assert_eq!(status.glyph, "✗");
    assert_eq!(status.label, "failed (exit 127)");
    assert_eq!(record_exit(&failed).as_deref(), Some("127"));
}

#[test]
fn runs_table_shows_tokens_only_when_present() {
    let now = Timestamp::from_second(30).expect("timestamp");
    let without_tokens = record(10, LoopRunResult::Completed);
    let mut out = Vec::new();
    write_runs_table(&mut out, &[without_tokens], 10, now).unwrap();
    let out = anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string();
    assert!(!out.contains("TOKENS"), "{out}");

    let mut with_tokens = record(20, LoopRunResult::Completed);
    with_tokens.input_tokens = Some(14_000);
    with_tokens.output_tokens = Some(269);
    let without_tokens = record(10, LoopRunResult::Failed);
    let mut out = Vec::new();
    write_runs_table(&mut out, &[without_tokens, with_tokens], 10, now).unwrap();
    let out = anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string();
    assert!(out.contains("TOKENS"), "{out}");
    assert!(out.contains("↘ 14k ↗ 269"), "{out}");
    assert!(
        out.lines()
            .any(|line| line.contains("✗ failed") && line.ends_with('-')),
        "{out}"
    );
}

#[test]
fn collapsed_run_rows_merge_adjacent_matching_render_columns() {
    let mut first = record(10, LoopRunResult::Failed);
    first.mode = Some(LoopRunMode::Scheduled);
    first.duration_ms = Some(10);
    first.check = Some(CheckRecord {
        output_path: None,
        code: Some(1),
        timed_out: false,
        output: "boom".to_owned(),
    });
    let mut second = first.clone();
    second.at = Timestamp::from_second(20).expect("timestamp");
    second.duration_ms = Some(20);
    let mut third = second.clone();
    third.at = Timestamp::from_second(30).expect("timestamp");
    third.check = Some(CheckRecord {
        output_path: None,
        code: Some(1),
        timed_out: false,
        output: "different".to_owned(),
    });
    let records = vec![first, second, third];

    let rows = collapsed_run_rows(&records);

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].count, 2);
    assert_eq!(
        rows[0].latest.at,
        Timestamp::from_second(20).expect("timestamp")
    );
    assert_eq!(rows[0].latest.duration_ms, Some(20));
    assert_eq!(rows[0].key.note.as_deref(), Some("boom"));
    assert_eq!(rows[1].count, 1);
    assert_eq!(rows[1].key.note.as_deref(), Some("different"));
}

#[test]
fn detail_indices_point_at_the_current_failure_only_when_a_later_run_shadows_it() {
    let mut error = record(10, LoopRunResult::Errored);
    error.error = Some("reading prompt-file\nmissing".to_owned());
    let mut failed = record(20, LoopRunResult::Failed);
    failed.run_id = Some("run_0123456789abcdef01234567".to_owned());
    let mut records = vec![error, failed];
    assert_eq!(detail_indices(&records), (Some(1), None));

    let mut skipped = record(30, LoopRunResult::BudgetSkipped);
    skipped.error = Some("daily budget reached".to_owned());
    records.push(skipped);
    assert_eq!(detail_indices(&records), (Some(2), Some(1)));

    let mut recovered = record(40, LoopRunResult::Completed);
    recovered.check = Some(check(Some(0), "ok"));
    records.push(recovered);
    assert_eq!(detail_indices(&records), (Some(3), None));
}

#[test]
fn detail_indices_skip_a_trailing_overlap() {
    let mut completed = record(10, LoopRunResult::Completed);
    completed.check = Some(check(Some(0), "ok"));
    let records = vec![completed, overlap(20)];

    assert_eq!(detail_indices(&records), (Some(0), None));
    assert!(last_run(&records).contains("LAST RUN — ✓ completed"));
}

#[test]
fn detail_indices_skip_last_run_after_a_clean_launch_even_with_a_trailing_overlap() {
    let mut error = record(10, LoopRunResult::Errored);
    error.error = Some("launch failed".to_owned());
    let mut launched = record(20, LoopRunResult::Launched);
    launched.checkout = Some(PathBuf::from("/repo/lane"));
    launched.target = Some("@fixer".to_owned());
    let mut records = vec![error, launched];

    assert_eq!(detail_indices(&records), (None, None));
    records.push(overlap(30));
    assert!(!last_run(&records).contains("LAST RUN"));
}

#[test]
fn render_record_detail_titles_status_age_and_mode() {
    let mut detail = record(20, LoopRunResult::Errored);
    detail.mode = Some(LoopRunMode::Manual);
    detail.error = Some("outer error\ninner detail".to_owned());
    detail.cost_usd = Some(0.42);
    detail.input_tokens = Some(12_000);
    detail.output_tokens = Some(3_400);
    let entry = TaskEntry {
        root: PathBuf::from("/tmp/rimz-run"),
        ..TaskEntry::default()
    };
    let mut out = Vec::new();

    render_record_detail(
        &mut out,
        &entry,
        &detail,
        "LAST FAILURE",
        "rimz loop logs wait -n 1",
        Timestamp::from_second(30).expect("timestamp"),
        ui::prose::Prose::Raw,
    )
    .unwrap();

    let raw = String::from_utf8(out).unwrap();
    assert!(raw.contains(&ui::paint(ui::palette::muted(), "  error:")));
    let out = anstream::adapter::strip_str(&raw).to_string();
    assert!(out.contains("LAST FAILURE — ✗ error · "));
    assert!(out.contains(" · manual"));
    assert!(out.contains("  error:\n  │ outer error\n  │ inner detail"));
    assert!(out.contains("  cost: $0.42 · ↘ 12k ↗ 3k"));
}

#[test]
fn render_record_detail_marks_failed_check_output() {
    let mut detail = record(20, LoopRunResult::Failed);
    detail.check = Some(CheckRecord {
        output_path: None,
        code: Some(2),
        timed_out: false,
        output: "first line\nsecond line".to_owned(),
    });
    let entry = TaskEntry {
        root: PathBuf::from("/tmp/rimz-run"),
        ..TaskEntry::default()
    };
    let mut out = Vec::new();

    render_record_detail(
        &mut out,
        &entry,
        &detail,
        "LAST FAILURE",
        "rimz loop logs wait -n 1",
        Timestamp::from_second(30).expect("timestamp"),
        ui::prose::Prose::Raw,
    )
    .unwrap();

    let raw = String::from_utf8(out).unwrap();
    assert!(raw.contains(&ui::paint(ui::palette::alarm(), "first line")));
    let out = anstream::adapter::strip_str(&raw).to_string();
    assert!(out.contains("LAST FAILURE — ✗ failed (exit 2)"));
    assert!(out.contains("  │ first line\n  │ second line"));
}

#[test]
fn last_run_header_carries_duration_exit_and_signal() {
    let mut completed = scheduled(90, LoopRunResult::Completed);
    completed.duration_ms = Some(5_000);
    completed.check = Some(check(Some(0), "ok"));
    completed.signal = Some(rimz::harness::schedule::run_log::SignalRecord {
        name: "trunk.moved".parse().unwrap(),
        payload: Default::default(),
    });

    let out = last_run(&[completed.clone()]);
    assert!(
        out.contains("LAST RUN — ✓ completed · 10s ago · 5.0s · exit 0 · signal trunk.moved\n"),
        "{out}"
    );
    assert!(!out.contains("signal:"), "{out}");

    completed.mode = Some(LoopRunMode::Manual);
    completed.signal = None;
    let out = last_run(&[completed]);
    assert!(
        out.contains("LAST RUN — ✓ completed · 10s ago · 5.0s · exit 0 · manual\n"),
        "{out}"
    );
}

#[test]
fn last_run_cuts_a_passing_check_to_its_tail_and_points_at_the_rest() {
    let seven = "l1\nl2\nl3\nl4\nl5\nl6\nl7\n";
    let mut passed = scheduled(80, LoopRunResult::Completed);
    passed.check = Some(check(Some(0), seven));
    let out = last_run(&[passed.clone(), overlap(90)]);
    assert!(
        out.contains("exit 0\n  │ l3\n  │ l4\n  │ l5\n  │ l6\n  │ l7\n"),
        "{out}"
    );
    assert!(
        out.ends_with("  full output: rimz loop logs wait -n 2\n"),
        "{out}"
    );

    passed.check = Some(check(Some(0), "l1\nl2\nl3\nl4\nl5\n"));
    let out = last_run(&[passed]);
    assert!(out.contains("  │ l1\n  │ l2\n"), "{out}");
    assert!(!out.contains("full output"), "{out}");

    let mut failed = scheduled(80, LoopRunResult::Failed);
    failed.check = Some(check(Some(1), seven));
    let mut gated = scheduled(80, LoopRunResult::CheckSkipped);
    gated.check = Some(check(Some(1), seven));
    for record in [failed, gated] {
        let out = last_run(&[record]);
        assert!(out.contains("  │ l1\n  │ l2\n  │ l3\n"), "{out}");
        assert!(!out.contains("full output"), "{out}");
    }
}

#[test]
fn overlaps_fold_into_the_run_that_follows_them() {
    let mut records = (1..=4).map(overlap).collect::<Vec<_>>();
    let mut failed = scheduled(5, LoopRunResult::Failed);
    failed.error = Some("checkout changed".to_owned());
    records.push(failed);

    let rows = collapsed_run_rows(&records);
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].count, rows[0].skipped), (1, 4));
    let table = runs_table(&records, 10);
    assert!(!table.contains("overlapped"), "{table}");
    assert!(
        table.contains("checkout changed · 4 fires skipped, run already active"),
        "{table}"
    );
    assert!(
        table.contains("RECENT RUNS (newest first · 5 of 5)"),
        "{table}"
    );

    let records = vec![
        scheduled(10, LoopRunResult::Completed),
        overlap(20),
        scheduled(30, LoopRunResult::Completed),
    ];
    let rows = collapsed_run_rows(&records);
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].count, rows[0].skipped), (2, 1));
    let table = runs_table(&records, 10);
    assert!(table.contains("✓ completed ×2"), "{table}");
    assert!(
        table.contains("1 fire skipped, run already active"),
        "{table}"
    );
}

#[test]
fn trailing_overlaps_keep_a_row_and_mark_the_verdict() {
    let mut records = vec![
        scheduled(10, LoopRunResult::Completed),
        overlap(20),
        overlap(30),
    ];
    let table = runs_table(&records, 10);
    assert!(
        table.find("○ overlapped ×2").unwrap() < table.find("✓ completed").unwrap(),
        "{table}"
    );
    assert_eq!(
        stale_clause(&records, false).as_deref(),
        Some("last 2 fires skipped, nothing has run since")
    );
    assert_eq!(
        stale_clause(&records, true).as_deref(),
        Some("2 fires skipped while the active run holds the lock")
    );

    records.push(scheduled(40, LoopRunResult::Completed));
    assert_eq!(
        stale_clause(&records, false).as_deref(),
        Some("2 fires skipped during the last run, nothing has run since")
    );
    assert_eq!(stale_clause(&records, true), None);

    records.push(scheduled(50, LoopRunResult::Completed));
    assert_eq!(stale_clause(&records, false), None);
    assert_eq!(
        stale_clause(&[overlap(10)], false).as_deref(),
        Some("last fire skipped, nothing has run since")
    );

    // A fire refused at an earlier gate ran nothing: it is neither the run a
    // skipped fire waits for nor a skipped fire itself, and it does not end a count.
    let completed = |second| scheduled(second, LoopRunResult::Completed);
    for refused in [
        LoopRunResult::BudgetSkipped,
        LoopRunResult::AccountSkipped,
        LoopRunResult::SurplusSkipped,
        LoopRunResult::SignalSkipped,
    ] {
        let refused = |second| scheduled(second, refused);
        let clause = |records: &[LoopRunRecord], has_active_run| {
            stale_clause(records, has_active_run).unwrap_or_default()
        };
        assert_eq!(
            clause(&[overlap(10), completed(20), refused(30)], false),
            "1 fire skipped during the last run, nothing has run since"
        );
        assert_eq!(clause(&[overlap(10), completed(20), refused(30)], true), "");
        assert_eq!(
            clause(&[completed(10), overlap(20), refused(30)], true),
            "1 fire skipped while the active run holds the lock"
        );
        // An overlap with another refused fire after it is no longer the last fire.
        assert_eq!(
            clause(&[completed(10), overlap(20), refused(30)], false),
            "1 fire skipped, nothing has run since"
        );
        assert_eq!(
            clause(&[overlap(10), refused(20)], false),
            "1 fire skipped, nothing has run since"
        );
        assert_eq!(
            clause(&[completed(10), refused(20), overlap(30)], false),
            "last fire skipped, nothing has run since"
        );
        assert_eq!(
            clause(
                &[completed(10), refused(20), overlap(30), overlap(40)],
                false
            ),
            "last 2 fires skipped, nothing has run since"
        );
        assert_eq!(
            clause(
                &[
                    completed(10),
                    overlap(20),
                    refused(30),
                    overlap(40),
                    completed(50)
                ],
                false
            ),
            "2 fires skipped during the last run, nothing has run since"
        );
        assert_eq!(
            clause(
                &[
                    completed(10),
                    refused(20),
                    overlap(30),
                    refused(40),
                    overlap(50)
                ],
                false
            ),
            "2 fires skipped, nothing has run since"
        );
        assert_eq!(clause(&[completed(10), refused(20)], false), "");
        assert_eq!(clause(&[refused(10)], false), "");
    }
}

#[test]
fn runs_table_is_newest_first_and_limits_from_the_newest() {
    let records = vec![
        scheduled(10, LoopRunResult::Failed),
        scheduled(20, LoopRunResult::Completed),
    ];
    let table = runs_table(&records, 10);
    assert!(
        table.find("✓ completed").unwrap() < table.find("✗ failed").unwrap(),
        "{table}"
    );

    let table = runs_table(&records, 1);
    assert!(table.contains("✓ completed"), "{table}");
    assert!(!table.contains("✗ failed"), "{table}");
    assert!(
        table.contains("RECENT RUNS (newest first · 1 of 2)"),
        "{table}"
    );
}

#[test]
fn runs_table_drops_uniform_mode_and_empty_cost() {
    let mut records = vec![
        scheduled(10, LoopRunResult::Failed),
        scheduled(20, LoopRunResult::Completed),
    ];
    let table = runs_table(&records, 10);
    assert!(
        !table.contains("MODE") && !table.contains("COST"),
        "{table}"
    );

    let mut manual = record(30, LoopRunResult::Failed);
    manual.mode = Some(LoopRunMode::Manual);
    manual.cost_usd = Some(0.25);
    records.push(manual);
    let table = runs_table(&records, 10);
    assert!(table.contains("MODE"), "{table}");
    assert!(table.contains("manual"), "{table}");
    assert!(table.contains("COST"), "{table}");
    assert!(table.contains("$0.25"), "{table}");
}

#[test]
fn agent_runs_are_newest_first() {
    let mut out = Vec::new();
    write_agent_runs(
        &mut out,
        &[
            record(10, LoopRunResult::TargetGone),
            record(20, LoopRunResult::Delivered),
        ],
        Timestamp::from_second(100).unwrap(),
    )
    .unwrap();
    let out = anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string();
    assert!(
        out.find("delivered").unwrap() < out.find("target gone").unwrap(),
        "{out}"
    );
}

#[test]
fn loop_logs_forensics_print_a_signal_payload_under_its_name() {
    use super::super::run_report::{Forensics, write_record_forensics};
    use rimz::harness::schedule::run_log::SignalRecord;

    let render = |payload: serde_json::Map<String, serde_json::Value>| {
        let mut detail = record(20, LoopRunResult::Delivered);
        detail.signal = Some(SignalRecord {
            name: "deploy.finished".parse().expect("signal name"),
            payload,
        });
        let mut out = Vec::new();
        write_record_forensics(
            &mut out,
            None,
            &detail,
            ui::prose::Prose::Raw,
            Forensics::Full,
        )
        .unwrap();
        anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string()
    };

    let payload = serde_json::json!({"detail": {"a": 1}, "env": "prod"});
    let out = render(payload.as_object().expect("object").clone());
    assert!(
        out.contains("  signal: deploy.finished\n  {\"detail\":{\"a\":1},\"env\":\"prod\"}\n"),
        "{out}"
    );

    let out = render(serde_json::Map::new());
    assert!(out.ends_with("  signal: deploy.finished\n"), "{out}");
}

#[test]
fn failure_pointer_links_to_filtered_logs_without_full_forensics() {
    let mut failure = record(20, LoopRunResult::Errored);
    failure.mode = Some(LoopRunMode::Scheduled);
    failure.error = Some("outer error\ninner detail".to_owned());
    let mut out = Vec::new();

    write_failure_pointer(
        &mut out,
        "wait",
        &failure,
        Timestamp::from_second(30).unwrap(),
    )
    .unwrap();

    let out = anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string();
    assert!(out.contains(
        "last failure — ✗ error · 10s ago · scheduled · dig in: rimz loop logs wait --failed"
    ));
    assert!(!out.contains("outer error"));
}

#[test]
fn watch_history_uses_verdict_words_and_output_path() {
    use rimz::harness::schedule::signal::WatchVerdict;

    for (verdict, expected) in [
        (
            WatchVerdict::Exited {
                code: Some(0),
                elapsed_ms: 3_000,
            },
            "exit 0 after 3s",
        ),
        (
            WatchVerdict::Exited {
                code: Some(3),
                elapsed_ms: 3_000,
            },
            "exit 3 after 3s",
        ),
        (
            WatchVerdict::Exited {
                code: None,
                elapsed_ms: 3_000,
            },
            "killed by signal after 3s",
        ),
        (
            WatchVerdict::TimedOut { elapsed_ms: 3_000 },
            "timed out after 3s",
        ),
        (
            WatchVerdict::Lost {
                detail: "watcher process exited without reporting".to_owned(),
                elapsed_ms: 3_000,
            },
            "watcher died after 3s; the command may still be running or may have died with it",
        ),
    ] {
        for result in [
            LoopRunResult::Delivered,
            LoopRunResult::Failed,
            LoopRunResult::TimedOut,
            LoopRunResult::CheckSkipped,
        ] {
            let mut detail = record(20, result);
            detail.watch = Some(verdict.clone());
            detail.duration_ms = Some(0);
            detail.check = Some(CheckRecord {
                code: match verdict {
                    WatchVerdict::Exited { code, .. } => code,
                    _ => None,
                },
                timed_out: matches!(verdict, WatchVerdict::TimedOut { .. }),
                output: "last line".to_owned(),
                output_path: Some("/tmp/wait.log".into()),
            });
            assert_eq!(record_exit(&detail).as_deref(), Some(expected));
            let mut out = Vec::new();
            render_record_detail(
                &mut out,
                &TaskEntry::default(),
                &detail,
                "last run",
                "rimz loop logs wait -n 1",
                Timestamp::from_second(30).unwrap(),
                ui::prose::Prose::Raw,
            )
            .unwrap();
            let raw = String::from_utf8(out).unwrap();
            let out = anstream::adapter::strip_str(&raw).to_string();
            assert_eq!(out.matches(expected).count(), 1, "{out}");
            assert!(
                out.contains("  output: /tmp/wait.log\n  │ last line"),
                "{out}"
            );
            assert!(!out.contains("after 3s in"), "{out}");
            let mut table = Vec::new();
            write_runs_table(
                &mut table,
                &[detail],
                5,
                Timestamp::from_second(30).unwrap(),
            )
            .unwrap();
            let table = String::from_utf8(table).unwrap();
            assert!(table.contains("3.0s"), "{table}");
            assert!(!table.contains("0ms"), "{table}");
        }
    }
}

#[test]
fn a_fan_out_shows_its_running_checkout_and_the_others_waiting_beside_it() {
    let now = Timestamp::from_second(100_000).unwrap();
    let running = RunLockInfo {
        pid: 41,
        started_at: now - jiff::SignedDuration::from_secs(120),
    };
    let since_ms = u64::try_from((now - jiff::SignedDuration::from_secs(30)).as_millisecond())
        .expect("after the epoch");
    let waiting = |pid: u32, checkout: &str, reason: Option<&str>, position| Held {
        pid,
        checkout: checkout.into(),
        since_ms,
        reason: reason.map(str::to_owned),
        position,
    };
    // Checkout a launched and runs; b and c wait in other processes.
    let held = [
        waiting(42, "/repo/.worktrees/b", Some("2 starts ahead"), 3),
        waiting(43, "/repo/.worktrees/c", None, 4),
    ];
    assert_eq!(
        in_flight_text(Some(running), &held, now),
        "▸ running 2m · 2 held: 2 starts ahead"
    );
    let in_flight = InFlightRun {
        holder: Some(running),
        run: None,
        held: held.to_vec(),
    };
    assert_eq!(
        running_text_full(&in_flight, now),
        "▸ running 2m · pid 41 · 2 held: 2 starts ahead"
    );
    let (own, others) = split_held(Some(running), &held);
    assert!(own.is_none());
    assert_eq!(others.map(|held| held.pid).collect::<Vec<_>>(), [42, 43]);

    // The holder is itself held: its own reason, never another checkout's.
    let held = [
        waiting(42, "/repo/.worktrees/b", Some("1 start ahead"), 2),
        waiting(41, "/repo/.worktrees/a", Some("cpu pressure 41% >= 25%"), 3),
    ];
    assert_eq!(
        in_flight_text(Some(running), &held, now),
        "held: cpu pressure 41% >= 25%, 30s · 1 held: 1 start ahead"
    );
    let (own, _) = split_held(Some(running), &held);
    assert_eq!(own.map(|held| held.pid), Some(41));
    // A holder the lock does not name owns no ticket.
    assert_eq!(
        in_flight_text(None, &held, now),
        "▸ running · 2 held: 1 start ahead"
    );
}
