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
fn task_timing_maps_to_watch_labels() {
    let now = Timestamp::from_second(10_000).unwrap();
    let manual = Arming {
        enabled: false,
        at: Some(now),
        pause_until: None,
        strikes: None,
    };
    let strikes = Arming {
        enabled: false,
        at: Some(now),
        pause_until: None,
        strikes: Some(3),
    };
    let timed = Arming {
        enabled: true,
        at: Some(now),
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
}

#[test]
fn running_watch_row_retains_next_fire_through_pause_overlay() {
    let now = Timestamp::from_second(10_000).unwrap();
    let pause = Arming {
        enabled: true,
        at: None,
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
