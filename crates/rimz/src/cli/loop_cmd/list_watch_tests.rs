use super::*;
use jiff::{SignedDuration, tz::TimeZone};
use unicode_width::UnicodeWidthStr;

fn now() -> Timestamp {
    Timestamp::from_second(100_000).unwrap()
}

fn row(name: &str) -> TaskRow {
    let mut row = TaskRow::rowless(name.into(), None, None);
    row.section = Section::Routine;
    row.kind = TaskKind::Routine;
    row.state = TaskState::Live;
    row.running = None;
    row
}

fn timed(name: &str, entry: TaskEntry, arming: Option<Arming>, last: Option<Timestamp>) -> TaskRow {
    let timing = schedule::TaskTiming::evaluate(
        &schedule::parse_trigger(name, &entry),
        TaskSource::Config,
        last,
        arming.as_ref(),
        &now().to_zoned(TimeZone::UTC),
    );
    let mut row = row(name);
    row.state = TaskState::observe(&timing, false, true);
    row.head = trigger_text(&entry, &timing, row.state, now()).0;
    row.next_at = timing.next_timestamp();
    row.timing = Some(timing);
    row
}

fn clock(name: &str, seconds: i64) -> TaskRow {
    timed(
        name,
        TaskEntry {
            every: Some("1h".into()),
            ..TaskEntry::default()
        },
        None,
        Some(now() + SignedDuration::from_secs(seconds - 3600)),
    )
}

fn history(row: &mut TaskRow, age: i64, ok: bool) {
    let result = if ok {
        LoopRunResult::Completed
    } else {
        LoopRunResult::Failed
    };
    let mut record = LoopRunRecord::new(&row.name, result, LoopRunMode::Manual, 0);
    record.at = now() - SignedDuration::from_secs(age);
    row.last = Some(LastRun {
        at: record.at,
        result,
        ok,
        streak: 4,
        record,
    });
}

fn running(name: &str) -> TaskRow {
    TaskRow::rowless(
        name.into(),
        Some(RunLockInfo {
            pid: 42,
            started_at: now() - SignedDuration::from_mins(3),
        }),
        None,
    )
}

fn attention_row(name: &str, attention: Attention) -> TaskRow {
    let mut row = row(name);
    row.section = Section::NeedsYou;
    row.attention = Some(attention);
    row.reason = "reason requiring your attention".into();
    row
}

fn retired(name: &str, hours: i64) -> Retired {
    Retired {
        name: name.into(),
        at: now() - SignedDuration::from_hours(hours),
        result: LoopRunResult::Completed,
        checkout: None,
        worktree_here: false,
    }
}

fn room(tasks: Vec<TaskRow>) -> Room {
    Room {
        root: "/repo".into(),
        open: true,
        here: true,
        spend_today_usd: 0.0,
        tasks,
        retired: Vec::new(),
    }
}

fn frame(room: &Room, cols: usize, rows: usize, hold: bool) -> String {
    let mut out = anstream::StripStream::new(Vec::new());
    write(&mut out, room, now(), cols, rows, hold).unwrap();
    let rendered = String::from_utf8(out.into_inner()).unwrap();
    assert!(rendered.lines().count() <= rows.max(1), "{rendered}");
    assert!(
        rendered.lines().all(|line| line.width() <= cols),
        "{rendered}"
    );
    rendered
}

fn mock(needs_you: bool) -> Room {
    let mut tasks = vec![
        running("trunk-install"),
        clock("accounts-steer", 42),
        clock("dependabot-repair", 720),
        clock("build-janitor", 3060),
        row("sweep"),
        row("triage"),
    ];
    for (row, age) in tasks.iter_mut().zip([1080, 14, 2820, 480, 240, 60]) {
        history(row, age, true);
    }
    let mut listener = timed(
        "trunk-ci-fix",
        TaskEntry {
            signal: Some("ci.failed".into()),
            matches: Some(BTreeMap::from([("branch".into(), "main".into())])),
            ..TaskEntry::default()
        },
        None,
        None,
    );
    listener.heard = Some(Heard {
        signal: "ci.passed".into(),
        at: now() - SignedDuration::from_hours(2),
    });
    tasks.push(listener);
    for i in 0..19 {
        let mut wake = clock(&format!("wake-{i}"), 1);
        wake.kind = TaskKind::WakeUp;
        wake.section = Section::WakeUps;
        tasks.push(wake);
    }
    if needs_you {
        let failed = &mut tasks[3];
        history(failed, 120, false);
        failed.exit = Some("101".into());
        failed.section = Section::NeedsYou;
        failed.attention = Some(Attention::Failing);
        failed.reason = "✗ failing · 1/3 strikes · exit 101".into();
        let wake = tasks.last_mut().unwrap();
        wake.name = "wait-brave-frame".into();
        wake.section = Section::NeedsYou;
        wake.attention = Some(Attention::Lost);
        wake.reason = "watcher gone · will not wake @implementer".into();
        wake.timing = Some(schedule::TaskTiming::evaluate(
            &schedule::parse_trigger(
                "wait-brave-frame",
                &TaskEntry {
                    watch: Some(WatchSpec::Command("cargo test".into())),
                    ..TaskEntry::default()
                },
            ),
            TaskSource::Config,
            None,
            None,
            &now().to_zoned(TimeZone::UTC),
        ));
        wake.head = "on command exit".into();
    }
    let mut room = room(tasks);
    room.retired = (0..6)
        .map(|i| retired(&format!("older-{i}"), 7 + i))
        .collect();
    room.retired.push(retired("shared-view-step2", 6));
    room
}

#[test]
fn healthy_frame_at_64_columns() {
    assert_eq!(
        frame(&mock(false), 64, 38, true),
        concat!(
            "loop · ▸ 1 running · next: accounts-steer in 42s\n\n",
            "  task               next           last run\n",
            "  trunk-install      ▸ running 3m   ✓ 18m ago\n",
            "  accounts-steer     42s            ✓ 14s ago\n",
            "  dependabot-repair  12m            ✓ 47m ago\n",
            "  build-janitor      51m            ✓ 8m ago\n",
            "  sweep              —              ✓ 4m ago\n",
            "  triage             —              ✓ 1m ago\n",
            "  trunk-ci-fix       on ci.failed   heard ci.passed 2h ago\n",
            "                     · branch=main\n\n",
            "  done      7 one-offs · newest shared-view-step2 6h ago\n",
            "  wake-ups  19 wake agents · all healthy · rimz loop list --all\n",
        )
    );
}

#[test]
fn healthy_frame_at_45_columns_keeps_words_and_name_floor_before_clipping_history() {
    let output = frame(&mock(false), 45, 25, true);
    assert_eq!(
        output.lines().nth(2),
        Some("  task         next         last run"),
        "{output}"
    );
    assert!(
        output.contains("  trunk-ci-f…  on           heard ci.passed …\n"),
        "{output}"
    );
    for word in ["ci.failed", "·", "branch=main"] {
        assert!(
            output.lines().any(|line| line.trim() == word),
            "missing whole word {word}:\n{output}"
        );
    }
}

#[test]
fn needs_you_frame_at_64_columns() {
    assert_eq!(
        frame(&mock(true), 64, 38, true),
        concat!(
            "loop · ✗ 2 need you · ▸ 1 running · next: accounts-steer in 42s\n\n",
            "  task               next             last run\n",
            "  build-janitor      51m              ✗ failed 2m ago · exit 101\n",
            "  wait-brave-frame   on command exit  never fired\n",
            "                     ✗ watcher gone · will not wake @implementer\n",
            "  trunk-install      ▸ running 3m     ✓ 18m ago\n",
            "  accounts-steer     42s              ✓ 14s ago\n",
            "  dependabot-repair  12m              ✓ 47m ago\n",
            "  sweep              —                ✓ 4m ago\n",
            "  triage             —                ✓ 1m ago\n",
            "  trunk-ci-fix       on ci.failed     heard ci.passed 2h ago\n",
            "                     · branch=main\n\n",
            "  done      7 one-offs · newest shared-view-step2 6h ago\n",
            "  wake-ups  18 wake agents · all healthy · rimz loop list --all\n",
        )
    );
}

#[test]
fn band_priority_and_hold() {
    let mut room = room(vec![
        attention_row("bad", Attention::Failing),
        running("run"),
        clock("clock", 42),
        row("off"),
    ]);
    room.tasks[3].state = TaskState::Off;
    let full = "loop · ✗ 1 needs you · ▸ 1 running · next: clock in 42s · ○ 1 held · q quit";
    for expected in [
        full,
        full.rsplit_once(" · ").unwrap().0,
        "loop · ✗ 1 needs you · ▸ 1 running · next: clock in 42s",
        "loop · ✗ 1 needs you · ▸ 1 running",
        "loop · ✗ 1 needs you",
        "loop",
    ] {
        let output = frame(&room, expected.width(), 24, false);
        assert_eq!(output.lines().next(), Some(expected));
        assert!(!output.lines().next().unwrap().contains("ok"));
        assert!(!output.lines().next().unwrap().contains("tasks"));
    }
    let output = frame(&room, 100, 24, true);
    assert_eq!(
        output.lines().next(),
        Some(full.rsplit_once(" · ").unwrap().0)
    );
}

#[test]
fn throttle_held_running_row_counts_as_needs_you() {
    let mut held = running("held");
    held.section = Section::NeedsYou;
    held.attention = Some(Attention::Held);
    let output = frame(&room(vec![held]), 100, 24, true);
    assert_eq!(output.lines().next(), Some("loop · ✗ 1 needs you"));
}

#[test]
fn order_is_attention_running_clocks_rest_then_held() {
    let mut held = row("aaa-off");
    held.state = TaskState::Off;
    let room = room(vec![
        row("bbb-rest"),
        clock("zzz-soon", 1),
        clock("aaa-later", 2),
        held,
        running("zzz-run"),
        attention_row("zzz-bad", Attention::Failing),
        clock("zzz-due-later", -1),
        clock("zzz-due-earlier", -2),
        clock("aaa-soon", 1),
        row("aaa-rest"),
        running("aaa-run"),
        attention_row("aaa-bad", Attention::Failing),
    ]);
    let output = frame(&room, 100, 30, true);
    let names = output
        .lines()
        .skip(3)
        .map(|line| line.split_whitespace().next().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "aaa-bad",
            "zzz-bad",
            "aaa-run",
            "zzz-run",
            "zzz-due-earlier",
            "zzz-due-later",
            "aaa-soon",
            "zzz-soon",
            "aaa-later",
            "aaa-rest",
            "bbb-rest",
            "aaa-off"
        ]
    );
}

#[test]
fn done_fold_shares_retired_and_launched_history_not_running_rows() {
    let mut launched = row("launched");
    launched.section = Section::Done;
    launched.launch = Some(launch_ledger::LaunchRecord {
        at: now() - SignedDuration::from_hours(1),
        leader: "dont-show-me".into(),
        destination: None,
    });
    history(&mut launched, 7200, true);
    let mut active = running("active-resident");
    active.launch = Some(launch_ledger::LaunchRecord {
        at: now(),
        leader: "dont-show-me".into(),
        destination: None,
    });
    let mut room = room(vec![launched, active, running("rowless")]);
    room.retired.push(retired("retired", 2));
    let output = frame(&room, 100, 24, true);
    assert!(
        output.contains("  done      2 one-offs · newest launched 1h ago\n"),
        "{output}"
    );
    assert!(
        output.contains("active-resident") && output.contains("rowless"),
        "{output}"
    );
    assert!(!output.contains("dont-show-me"), "{output}");
    room.retired.push(retired("newest-retired", 0));
    assert!(frame(&room, 100, 24, true).contains("3 one-offs · newest newest-retired 0s ago"));
}

#[test]
fn wake_fold_excludes_attention_and_running_wakes() {
    let mut quiet = clock("folded", 1);
    quiet.kind = TaskKind::WakeUp;
    quiet.section = Section::WakeUps;
    let mut active = running("active-wake");
    active.kind = TaskKind::WakeUp;
    active.section = Section::WakeUps;
    let mut lost = attention_row("lost-wake", Attention::Lost);
    lost.kind = TaskKind::WakeUp;
    let output = frame(&room(vec![quiet, active, lost]), 100, 24, true);
    assert!(
        output.contains("  wake-ups  1 wake agents · all healthy · rimz loop list --all\n"),
        "{output}"
    );
    assert!(
        output.contains("active-wake") && output.contains("lost-wake"),
        "{output}"
    );
    assert!(!output.contains("folded"), "{output}");
}

#[test]
fn folds_alone_have_no_header_or_next() {
    let mut wake = clock("folded-wake", 1);
    wake.kind = TaskKind::WakeUp;
    wake.section = Section::WakeUps;
    let mut done = clock("folded-done", 0);
    done.section = Section::Done;
    done.kind = TaskKind::OneOff;
    history(&mut done, 3600, true);
    assert_eq!(
        frame(&room(vec![wake, done]), 100, 24, true),
        "loop\n\n  done      1 one-off · newest folded-done 1h ago\n  wake-ups  1 wake agents · all healthy · rimz loop list --all\n"
    );
}

#[test]
fn empty_room() {
    assert_eq!(
        frame(&room(Vec::new()), 64, 24, true),
        "loop\n\nno loop tasks; add one with `rimz loop add …`\n"
    );
}

macro_rules! reason_test {
    ($name:ident, $attention:ident) => {
        #[test]
        fn $name() {
            let output = frame(
                &room(vec![attention_row("task", Attention::$attention)]),
                64,
                24,
                true,
            );
            assert!(
                output
                    .lines()
                    .any(|line| line.trim_start() == "✗ reason requiring your attention"),
                "{output}"
            );
            let reason = output
                .lines()
                .find(|line| line.contains("✗ reason"))
                .unwrap();
            let header = output
                .lines()
                .find(|line| line.contains("last run"))
                .unwrap();
            assert_eq!(reason.find('✗'), header.find("next"));
            let main = output
                .lines()
                .find(|line| line.trim_start().starts_with("task ") && !line.contains("last run"))
                .unwrap();
            assert!(!main.contains("reason"), "{output}");
        }
    };
}
reason_test!(checkout_gone_reason, CheckoutGone);
reason_test!(strikes_reason, Strikes);
reason_test!(blocked_reason, Blocked);
reason_test!(invalid_reason, Invalid);
reason_test!(parked_reason, Parked);
reason_test!(lost_reason, Lost);

#[test]
fn failing_and_held_have_no_reason_line() {
    let output = frame(
        &room(vec![
            attention_row("fail", Attention::Failing),
            attention_row("held", Attention::Held),
        ]),
        64,
        24,
        true,
    );
    assert_eq!(output.lines().count(), 5);
    assert!(!output.contains("reason"), "{output}");
}

#[test]
fn last_run_is_history_even_when_running_or_disabled() {
    let mut good = running("good");
    history(&mut good, 120, true);
    let mut bad = row("bad");
    bad.state = TaskState::Off;
    history(&mut bad, 60, false);
    bad.exit = Some("101".into());
    let mut heard = row("heard");
    heard.heard = Some(Heard {
        signal: "ci.passed".into(),
        at: now() - SignedDuration::from_hours(2),
    });
    let output = frame(
        &room(vec![good, bad, heard, running("first"), row("never")]),
        100,
        24,
        true,
    );
    for (name, last) in [
        ("good", "✓ 2m ago"),
        ("bad", "✗ failed 1m ago · exit 101"),
        ("heard", "heard ci.passed 2h ago"),
        ("first", "—"),
        ("never", "never fired"),
    ] {
        assert!(
            output
                .lines()
                .any(|line| line.trim_start().starts_with(name) && line.ends_with(last)),
            "missing {name}: {last}\n{output}"
        );
    }
    assert!(
        !output.contains("in a row") && !output.contains("never fired ·"),
        "{output}"
    );
}

#[test]
fn held_states_sort_last_and_do_not_supply_next() {
    let off = Arming {
        enabled: false,
        at: None,
        pause_until: None,
        strikes: None,
    };
    let paused = Arming {
        enabled: true,
        pause_until: Some(now() + SignedDuration::from_hours(1)),
        ..off
    };
    let entry = TaskEntry {
        every: Some("1h".into()),
        ..TaskEntry::default()
    };
    let mut tasks = vec![
        timed("off", entry.clone(), Some(off), Some(now())),
        timed("paused", entry, Some(paused), Some(now())),
        row("not-enabled"),
        clock("closed", 1),
        clock("live", 42),
    ];
    tasks[2].state = TaskState::NotEnabled;
    tasks[3].state = TaskState::WaitsForRoom;
    let output = frame(&room(tasks), 100, 24, true);
    assert_eq!(
        output.lines().next(),
        Some("loop · next: live in 42s · ○ 4 held")
    );
    let names = output
        .lines()
        .skip(3)
        .map(|line| line.split_whitespace().next().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(names, ["live", "closed", "not-enabled", "off", "paused"]);
}

#[test]
fn running_clock_retains_scheduled_next_through_pause() {
    let mut row = timed(
        "running-clock",
        TaskEntry {
            every: Some("1h".into()),
            ..TaskEntry::default()
        },
        Some(Arming {
            enabled: true,
            at: None,
            pause_until: Some(now() + SignedDuration::from_mins(5)),
            strikes: None,
        }),
        Some(now() - SignedDuration::from_mins(55)),
    );
    row.state = TaskState::Running;
    row.running = Some(None);
    let output = frame(&room(vec![row]), 100, 24, true);
    assert_eq!(
        output.lines().next(),
        Some("loop · ▸ 1 running · next: running-clock in 5m")
    );
}

#[test]
fn blocked_clocks_never_supply_next() {
    let mut tasks = vec![
        clock("gone", 1),
        clock("blocked", 2),
        clock("invalid", 3),
        clock("live", 42),
    ];
    for (row, attention) in tasks.iter_mut().zip([
        Attention::CheckoutGone,
        Attention::Blocked,
        Attention::Invalid,
    ]) {
        row.section = Section::NeedsYou;
        row.attention = Some(attention);
        row.reason = "not ready".into();
    }
    let output = frame(&room(tasks), 100, 24, true);
    assert_eq!(
        output.lines().next(),
        Some("loop · ✗ 3 need you · next: live in 42s")
    );
}

#[test]
fn height_cuts_whole_rows_from_end_and_keeps_folds() {
    let room = mock(true);
    let output = frame(&room, 64, 8, true);
    assert!(output.contains("+7 more"), "{output}");
    assert!(output.contains("  build-janitor"), "{output}");
    assert!(!output.contains("  wait-brave-frame"), "{output}");
    assert!(
        output.contains("  task ") && output.contains("  done ") && output.contains("  wake-ups "),
        "{output}"
    );
    let output = frame(&room, 64, 11, true);
    assert!(
        output.contains("  wait-brave-frame") && output.contains("✗ watcher gone"),
        "{output}"
    );
    assert!(output.contains("+5 more"), "{output}");
}

#[test]
fn narrow_frames_wrap_then_clip_without_exceeding_bounds() {
    let mut room = room(vec![
        row("a-very-long-task-name"),
        attention_row("reason", Attention::Lost),
    ]);
    history(&mut room.tasks[0], 120, false);
    room.tasks[1].reason = "a long reason · with several details && more explanation".into();
    for cols in [1, 2, 8, 16, 32, 44, 64, 100] {
        for rows in [1, 2, 3, 4, 5, 8, 24] {
            let output = frame(&room, cols, rows, true);
            assert!(!output.is_empty());
            if rows <= 2 {
                assert_eq!(output.lines().count(), 1);
            }
        }
    }
    let output = frame(&room, 32, 24, true);
    assert!(output.contains('…'), "{output}");
}
