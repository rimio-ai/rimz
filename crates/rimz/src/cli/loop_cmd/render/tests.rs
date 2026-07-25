use super::*;

fn dashboard_row(name: &str, state: RowState, failed: bool) -> WatchRow {
    let now = Timestamp::from_second(100).unwrap();
    WatchRow {
        name: name.to_owned(),
        glyph: if failed { "✗" } else { "✓" },
        glyph_style: if failed {
            ui::palette::alarm()
        } else {
            ui::palette::good()
        },
        state,
        failed,
        next_ts: match state {
            RowState::Due => Some(Timestamp::from_second(90).unwrap()),
            RowState::Upcoming(next) => Some(next),
            _ => None,
        },
        next_text: match state {
            RowState::Running => "running now".to_owned(),
            RowState::Due => "due".to_owned(),
            RowState::Held => "paused".to_owned(),
            RowState::Blocked => "blocked · trust".to_owned(),
            RowState::Upcoming(next) => ui::until_label(next, now),
            RowState::NeverRun => "—".to_owned(),
        },
        last_text: "LAST-COLUMN".to_owned(),
        status_text: "STATUS-COLUMN".to_owned(),
    }
}

fn dashboard(group: &WatchGroup, cols: usize, rows: usize) -> String {
    dashboards(std::slice::from_ref(group), cols, rows)
}

fn dashboards(groups: &[WatchGroup], cols: usize, rows: usize) -> String {
    let mut out = anstream::StripStream::new(Vec::new());
    render_dashboard(
        &mut out,
        groups,
        cols,
        rows,
        Timestamp::from_second(100).unwrap(),
        false,
    )
    .unwrap();
    String::from_utf8(out.into_inner()).unwrap()
}

fn dashboard_group(root: &str, names: &[&str]) -> WatchGroup {
    WatchGroup {
        root: PathBuf::from(root),
        room_is_open: true,
        rows: names
            .iter()
            .map(|name| dashboard_row(name, RowState::NeverRun, false))
            .collect(),
    }
}

fn assert_dashboard_bounds(rendered: &str, cols: usize, rows: usize) {
    assert!(rendered.lines().count() <= rows, "{rendered}");
    assert!(
        rendered.lines().all(|line| line.width() <= cols),
        "{rendered}"
    );
}

#[test]
fn held_watch_band_hides_the_quit_key() {
    let group = WatchGroup {
        root: PathBuf::from("/repo"),
        room_is_open: true,
        rows: vec![dashboard_row("task", RowState::NeverRun, false)],
    };
    let render = |hold| {
        let mut out = anstream::StripStream::new(Vec::new());
        write_watch_band(
            &mut out,
            std::slice::from_ref(&group),
            100,
            Timestamp::from_second(100).unwrap(),
            hold,
        )
        .unwrap();
        String::from_utf8(out.into_inner()).unwrap()
    };

    assert!(render(false).contains("q quit"));
    assert!(!render(true).contains("q quit"));
}

fn record(second: i64, result: LoopRunResult) -> LoopRunRecord {
    LoopRunRecord {
        task: "wake".to_owned(),
        at: Timestamp::from_second(second).expect("timestamp"),
        result,
        mode: None,
        duration_ms: None,
        error: None,
        check: None,
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
fn watch_dashboard_adapts_band_columns_rank_height_and_width() {
    let group = WatchGroup {
        root: PathBuf::from("/a/very/long/project/root/that/must/be/clipped"),
        room_is_open: true,
        rows: vec![
            dashboard_row("never", RowState::NeverRun, false),
            dashboard_row("paused", RowState::Held, false),
            dashboard_row(
                "later",
                RowState::Upcoming(Timestamp::from_second(300).unwrap()),
                false,
            ),
            dashboard_row(
                "sooner",
                RowState::Upcoming(Timestamp::from_second(200).unwrap()),
                false,
            ),
            dashboard_row("due", RowState::Due, false),
            dashboard_row(
                "failed",
                RowState::Upcoming(Timestamp::from_second(150).unwrap()),
                true,
            ),
            dashboard_row("running", RowState::Running, false),
        ],
    };
    let wide = dashboard(&group, 100, 20);
    assert!(
        wide.starts_with("loop · 7 tasks · ▸ 1 running · ✗ 1 failed"),
        "{wide}"
    );
    assert_eq!(wide.lines().count(), 11, "{wide}");
    assert!(
        wide.lines().any(|line| line.contains("task")
            && line.contains("next")
            && line.contains("last run")
            && line.contains("status")),
        "{wide}"
    );
    let body = wide.lines().skip(4).collect::<Vec<_>>().join("\n");
    let positions = [
        "running", "failed", "due", "sooner", "later", "paused", "never",
    ]
    .map(|name| body.find(name).unwrap());
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{wide}");
    let narrow = dashboard(&group, WATCH_NARROW - 1, 20);
    assert!(
        narrow.contains("task") && narrow.contains("next"),
        "{narrow}"
    );
    assert!(!narrow.contains("last run"), "{narrow}");
    let middle = dashboard(&group, WATCH_NARROW, 20);
    assert!(middle.contains("last run"), "{middle}");
    assert!(!middle.contains("status"), "{middle}");
    assert!(dashboard(&group, WATCH_WIDE, 20).contains("status"));
    let sooner = wide.lines().find(|line| line.contains("sooner")).unwrap();
    assert!(sooner.contains("1m"), "{sooner}");
    assert!(!sooner.contains("in 1m"), "{sooner}");
    let short = dashboard(&group, 30, 6);
    assert_eq!(short.lines().count(), 6, "{short}");
    assert!(short.contains("+6 more"), "{short}");
    assert!(short.lines().all(|line| line.width() <= 30), "{short}");
    assert_eq!(dashboard(&group, 14, 2).lines().count(), 1);
}

#[test]
fn watch_dashboard_renders_two_complete_groups_in_order() {
    let groups = [
        dashboard_group("/repo/first", &["z-last", "a-first"]),
        dashboard_group("/repo/second", &["d-last", "b-first"]),
    ];

    let rendered = dashboards(&groups, 80, 10);

    assert_eq!(rendered.lines().count(), 10, "{rendered}");
    let positions = [
        "/repo/first",
        "a-first",
        "z-last",
        "/repo/second",
        "b-first",
        "d-last",
    ]
    .map(|text| rendered.find(text).expect(text));
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "{rendered}"
    );
    assert_eq!(rendered.matches("last run").count(), 2, "{rendered}");
    assert_dashboard_bounds(&rendered, 80, 10);
}

#[test]
fn watch_dashboard_hides_only_the_second_group_when_it_does_not_fit() {
    let groups = [
        dashboard_group("/repo/first", &["one", "two"]),
        dashboard_group("/repo/second", &["three", "four", "five"]),
    ];

    let rendered = dashboards(&groups, 80, 9);

    assert_eq!(rendered.lines().count(), 7, "{rendered}");
    assert!(rendered.contains("/repo/first"), "{rendered}");
    assert!(
        rendered.contains("one") && rendered.contains("two"),
        "{rendered}"
    );
    assert!(!rendered.contains("/repo/second"), "{rendered}");
    assert!(rendered.contains("+3 more"), "{rendered}");
    assert_dashboard_bounds(&rendered, 80, 9);
}

#[test]
fn watch_dashboard_partially_renders_only_the_first_group() {
    let groups = [
        dashboard_group("/repo/first", &["delta", "alpha", "charlie", "bravo"]),
        dashboard_group("/repo/second", &["echo", "foxtrot"]),
    ];

    let rendered = dashboards(&groups, 80, 7);

    assert_eq!(rendered.lines().count(), 7, "{rendered}");
    let alpha = rendered.find("alpha").expect("highest-ranked task");
    let bravo = rendered.find("bravo").expect("second-ranked task");
    assert!(alpha < bravo, "{rendered}");
    assert!(
        !rendered.contains("charlie") && !rendered.contains("delta"),
        "{rendered}"
    );
    assert!(!rendered.contains("/repo/second"), "{rendered}");
    assert!(rendered.contains("+4 more"), "{rendered}");
    assert_dashboard_bounds(&rendered, 80, 7);
}

#[test]
fn watch_dashboard_uses_only_more_line_below_partial_section_minimum() {
    let groups = [
        dashboard_group("/repo/first", &["one", "two"]),
        dashboard_group("/repo/second", &["three"]),
    ];

    let rendered = dashboards(&groups, 12, 4);

    assert_eq!(rendered.lines().count(), 3, "{rendered}");
    assert_eq!(rendered.lines().last(), Some("+3 more"), "{rendered}");
    assert!(!rendered.contains("/repo/"), "{rendered}");
    assert_dashboard_bounds(&rendered, 12, 4);
}

#[test]
fn watch_dashboard_reserves_more_line_when_first_group_exactly_fills_budget() {
    let groups = [
        dashboard_group("/repo/first", &["one", "two"]),
        dashboard_group("/repo/second", &["three"]),
    ];

    let rendered = dashboards(&groups, 80, 6);

    assert_eq!(rendered.lines().count(), 6, "{rendered}");
    assert!(rendered.contains("one"), "{rendered}");
    assert!(
        !rendered.contains("two") && !rendered.contains("three"),
        "{rendered}"
    );
    assert!(rendered.contains("+2 more"), "{rendered}");
    assert_dashboard_bounds(&rendered, 80, 6);
}

#[test]
fn task_rules_and_check_rows_use_action_specific_verbs() {
    let spawn = TaskEntry {
        agent: Some("codex".to_owned()),
        check: Some("cargo test".to_owned()),
        on: Some(CheckOn::Fail),
        ..TaskEntry::default()
    };
    let spawn_action = TaskAction::from_entry("task", &spawn).unwrap();
    assert_eq!(
        task_run_rule(&spawn, &spawn_action),
        "check, then start codex on fail"
    );
    assert_eq!(
        check_summary(&spawn, Some(&spawn_action)).as_deref(),
        Some("cargo test (starts codex on fail)")
    );

    let wake = TaskEntry {
        wake: Some(TaskTarget {
            kind: "claude".to_owned(),
            session: "sess-planner".to_owned(),
            handle: "@planner".to_owned(),
        }),
        check: Some("cargo test".to_owned()),
        on: Some(CheckOn::Success),
        ..TaskEntry::default()
    };
    let wake_action = TaskAction::from_entry("task", &wake).unwrap();
    assert_eq!(
        task_run_rule(&wake, &wake_action),
        "check, then wake @planner on success"
    );
    assert_eq!(
        check_summary(&wake, Some(&wake_action)).as_deref(),
        Some("cargo test (wakes @planner on success)")
    );

    let check = TaskEntry {
        check: Some("cargo test".to_owned()),
        ..TaskEntry::default()
    };
    let check_action = TaskAction::from_entry("task", &check).unwrap();
    assert_eq!(task_run_rule(&check, &check_action), "check");

    let spawn = TaskEntry {
        agent: Some("claude".to_owned()),
        verify: Some("cargo xtask gate".to_owned()),
        max_attempts: Some(4),
        ..TaskEntry::default()
    };
    let spawn_action = TaskAction::from_entry("task", &spawn).unwrap();
    assert_eq!(
        task_run_rule(&spawn, &spawn_action),
        "start claude, verify `cargo xtask gate` (up to 4 attempts)"
    );

    let mut wake_only = wake;
    wake_only.check = None;
    let wake_action = TaskAction::from_entry("task", &wake_only).unwrap();
    assert_eq!(task_run_rule(&wake_only, &wake_action), "wake @planner");
}

#[test]
fn budget_and_cost_labels_cover_capped_plain_and_empty_spend() {
    let entry = TaskEntry {
        budget: Some("$5.00".to_owned()),
        budget_per_day: Some("$20.00".to_owned()),
        ..TaskEntry::default()
    };
    assert_eq!(
        budget_label(&entry).as_deref(),
        Some("$5 per run · $20 per day")
    );
    assert_eq!(list_cost_label(&entry, 3.2).as_deref(), Some("$3.20/$20"));

    let uncapped = TaskEntry::default();
    assert_eq!(list_cost_label(&uncapped, 0.85).as_deref(), Some("$0.85"));
    assert_eq!(list_cost_label(&uncapped, 0.0), None);
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
fn verdict_uses_the_latest_conclusive_streak() {
    let now = Timestamp::from_second(50).unwrap();
    let failed = record(10, LoopRunResult::Failed);
    let mut passed_check = record(20, LoopRunResult::CheckSkipped);
    passed_check.check = Some(CheckRecord {
        code: Some(0),
        timed_out: false,
        output: "ok".to_owned(),
    });
    let neutral = record(30, LoopRunResult::Overlapped);
    let completed = record(40, LoopRunResult::Completed);

    let (healthy, style) = verdict_line(
        &[failed, passed_check, neutral.clone(), completed.clone()],
        now,
    )
    .unwrap();
    assert_eq!(healthy, "✓ healthy · completed ×2 since 30s ago");
    assert_eq!(style, ui::palette::good());

    let errored = record(40, LoopRunResult::Errored);
    let failed = record(20, LoopRunResult::Failed);
    let (failing, style) =
        verdict_line(&[completed, failed, neutral.clone(), errored], now).unwrap();
    assert_eq!(failing, "✗ failing · error ×2 since 30s ago");
    assert_eq!(style, ui::palette::alarm());
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
        code: Some(127),
        timed_out: false,
        output: "ignored\n\nmissing command\n".to_owned(),
    });
    assert_eq!(check_failure_line(&failed), Some("missing command"));

    let mut passed = record(11, LoopRunResult::Completed);
    passed.check = Some(CheckRecord {
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
        code: Some(0),
        timed_out: false,
        output: "ok".to_owned(),
    });
    let status = run_status(&skipped);
    assert_eq!(status.glyph, "✓");
    assert_eq!(status.label, "check passed");
    assert_eq!(status.style, ui::palette::good());

    skipped.check = Some(CheckRecord {
        code: Some(1),
        timed_out: false,
        output: "not yet".to_owned(),
    });
    let status = run_status(&skipped);
    assert_eq!(status.glyph, "○");
    assert_eq!(status.label, "check failed");
    assert_eq!(status.style, ui::palette::muted());

    skipped.check = Some(CheckRecord {
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
                schedule::catalog::instances_path(&state_home())
                    .to_string_lossy()
                    .as_ref(),
            )
        )
    );
}

#[test]
fn blocked_project_rendering_names_the_gate_and_fix() {
    let mut table = ui::Table::new(["NEXT"]);
    table.row([blocked_next_cell(TrustState::Stale)]);
    let mut out = Vec::new();
    table.render(&mut out).unwrap();
    write_blocked_footer(&mut out, 2).unwrap();
    write_disabled_footer(&mut out, 1).unwrap();

    let out = anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string();
    assert!(out.contains("blocked · trust"), "{out}");
    assert!(
        out.contains(
            "2 task(s) blocked by project trust — review with `rimz trust`, approve with `rimz trust grant`"
        ),
        "{out}"
    );
    assert_eq!(
        blocked_notice(TrustState::Untrusted),
        "project trust is untrusted — review with `rimz trust`, approve with `rimz trust grant`"
    );
    assert!(
        out.contains("1 project task(s) disabled — arm with `rimz loop enable <name>`"),
        "{out}"
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
        schedule::TaskShape::compile("task", &entry).schedule(),
        blocked.map_or(TaskSource::Config, |state| TaskSource::Project { state }),
        last_fire,
        arming,
        &now.to_zoned(jiff::tz::TimeZone::UTC),
    )
}

#[test]
fn task_timing_maps_to_existing_list_and_watch_labels() {
    let now = Timestamp::from_second(10_000).unwrap();
    let manual = Arming {
        enabled: false,
        at: now,
        pause_until: None,
        strikes: None,
    };
    let strikes = Arming {
        enabled: false,
        at: now,
        pause_until: None,
        strikes: Some(3),
    };
    let timed = Arming {
        enabled: true,
        at: now,
        pause_until: Timestamp::from_second(10_300).ok(),
        strikes: None,
    };
    let cases = [
        (
            interval_timing(Some(TrustState::Stale), None, None, now),
            RowState::Blocked,
            "blocked · trust",
        ),
        (
            interval_timing(None, None, Some(&manual), now),
            RowState::Held,
            "disabled",
        ),
        (
            interval_timing(None, None, Some(&strikes), now),
            RowState::Held,
            "disabled · 3 strikes",
        ),
        (
            interval_timing(None, None, Some(&timed), now),
            RowState::Held,
            "paused · in 5m",
        ),
        (
            schedule::TaskTiming::evaluate(
                schedule::TaskShape::compile(
                    "task",
                    &TaskEntry {
                        agent: Some("claude".to_owned()),
                        every: Some("15m".to_owned()),
                        ..TaskEntry::default()
                    },
                )
                .schedule(),
                TaskSource::Project {
                    state: TrustState::Trusted,
                },
                None,
                None,
                &now.to_zoned(jiff::tz::TimeZone::UTC),
            ),
            RowState::Held,
            "disabled · enable to arm",
        ),
        (
            interval_timing(None, Timestamp::from_second(8_800).ok(), None, now),
            RowState::Due,
            "due",
        ),
        (
            interval_timing(None, Timestamp::from_second(9_400).ok(), None, now),
            RowState::Upcoming(Timestamp::from_second(10_300).unwrap()),
            "5m",
        ),
        (
            schedule::TaskTiming::evaluate(
                schedule::TaskShape::compile("task", &TaskEntry::default()).schedule(),
                TaskSource::Config,
                Some(now),
                None,
                &now.to_zoned(jiff::tz::TimeZone::UTC),
            ),
            RowState::NeverRun,
            "—",
        ),
        (
            interval_timing(None, None, None, now),
            RowState::NeverRun,
            "—",
        ),
    ];
    for (timing, state, label) in cases {
        assert_eq!(row_state_for_timing(&timing), state);
        assert_eq!(timing_next_text(&timing, now), label);
    }

    let upcoming = interval_timing(None, Timestamp::from_second(9_400).ok(), None, now);
    let mut table = ui::Table::new(["NEXT"]);
    table.row([next_cell(&upcoming, now)]);
    let mut out = Vec::new();
    table.render(&mut out).unwrap();
    assert!(String::from_utf8(out).unwrap().contains("in 5m"));
}

#[test]
fn show_headline_keeps_blocked_before_pause() {
    let now = Timestamp::from_second(10_000).unwrap();
    let pause = Arming {
        enabled: true,
        at: Timestamp::MIN,
        pause_until: Timestamp::from_second(10_300).ok(),
        strikes: None,
    };
    let timing = interval_timing(Some(TrustState::Untrusted), None, Some(&pause), now);
    let mut out = Vec::new();

    write_show_headline(&mut out, "task", &timing, now).unwrap();

    let out = anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string();
    assert!(out.contains("next blocked · trust"), "{out}");
    assert!(!out.contains("paused"), "{out}");
    assert!(!out.contains("loop enable"), "{out}");
}

#[test]
fn running_watch_row_retains_next_fire_through_pause_overlay() {
    let now = Timestamp::from_second(10_000).unwrap();
    let pause = Arming {
        enabled: true,
        at: Timestamp::MIN,
        pause_until: Timestamp::from_second(10_300).ok(),
        strikes: None,
    };
    let timing = interval_timing(None, Timestamp::from_second(9_400).ok(), Some(&pause), now);

    assert_eq!(
        timing.state(),
        schedule::TaskTimingState::Paused(Timestamp::from_second(10_300).unwrap())
    );
    assert_eq!(timing.next_timestamp(), None);
    assert_eq!(watch_next_timestamp(&timing, false), None);
    assert_eq!(
        watch_next_timestamp(&timing, true),
        Timestamp::from_second(10_300).ok()
    );
}

#[test]
fn run_status_merges_failed_check_exit() {
    let mut failed = record(10, LoopRunResult::Failed);
    failed.check = Some(CheckRecord {
        code: Some(127),
        timed_out: false,
        output: "missing".to_owned(),
    });

    let status = run_status(&failed);

    assert_eq!(status.glyph, "✗");
    assert_eq!(status.label, "failed (exit 127)");
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
fn detail_indices_include_prior_failure_when_latest_detail_shadows_it() {
    let mut error = record(10, LoopRunResult::Errored);
    error.error = Some("reading prompt-file\nmissing".to_owned());
    let mut failed = record(20, LoopRunResult::Failed);
    failed.run_id = Some("run_0123456789abcdef01234567".to_owned());
    let records = vec![error, failed];

    assert_eq!(detail_indices(&records), (Some(1), Some(0)));
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
        Timestamp::from_second(30).expect("timestamp"),
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
        Timestamp::from_second(30).expect("timestamp"),
    )
    .unwrap();

    let raw = String::from_utf8(out).unwrap();
    assert!(raw.contains(&ui::paint(ui::palette::alarm(), "first line")));
    let out = anstream::adapter::strip_str(&raw).to_string();
    assert!(out.contains("LAST FAILURE — ✗ failed (exit 2)"));
    assert!(out.contains("  │ first line\n  │ second line"));
}

#[test]
fn failure_pointer_links_to_filtered_logs_without_full_forensics() {
    let mut failure = record(20, LoopRunResult::Errored);
    failure.mode = Some(LoopRunMode::Scheduled);
    failure.error = Some("outer error\ninner detail".to_owned());
    let mut out = Vec::new();

    write_failure_pointer(
        &mut out,
        "wake",
        &failure,
        Timestamp::from_second(30).unwrap(),
    )
    .unwrap();

    let out = anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string();
    assert!(out.contains(
        "last failure — ✗ error · 10s ago · scheduled · dig in: rimz loop logs wake --failed"
    ));
    assert!(!out.contains("outer error"));
}
