use super::*;
use unicode_width::UnicodeWidthStr;

#[test]
fn held_starts_have_their_own_table_without_moving_the_task() {
    let now = Timestamp::from_second(100_000).unwrap();
    let mut task = TaskRow::rowless("triage".into(), None, None);
    task.section = Section::Routine;
    task.running = None;
    task.state = TaskState::Live;
    task.worktree = Some("fold-rule".into());
    task.worktree_here = true;
    task.held = vec![Held {
        pid: 42,
        checkout: "/repo".into(),
        since_ms: 99_760_000,
        kinds: Vec::new(),
        gives_up_ms: None,
        reason: Some("queued: 1 start ahead".into()),
        hold: Some(rimz::ids::HoldKind::Queue),
        position: 2,
    }];
    let room = Room {
        root: "/repo".into(),
        open: true,
        here: true,
        spend_today_usd: 0.0,
        tasks: vec![task],
        retired: Vec::new(),
    };
    let mut model = ListModel {
        rooms: vec![room],
        caller_root: None,
        now,
        warnings: Vec::new(),
    };
    for (bounded, running) in [(false, false), (true, false), (false, true)] {
        let task = &mut model.rooms[0].tasks[0];
        task.held[0].gives_up_ms = bounded.then_some(101_000_000);
        if running {
            task.running = Some(None);
            task.state = TaskState::Running;
        }
        let mut output = Vec::new();
        text::write(
            &mut output,
            &model,
            &[&model.rooms[0]],
            false,
            None,
            None,
            None,
        )
        .unwrap();
        let output =
            anstream::adapter::strip_str(std::str::from_utf8(&output).unwrap()).to_string();
        assert!(
            output.contains("HELD  starts waiting for capacity, retrying on their own"),
            "{output}"
        );
        assert!(
            output.contains("queued: 1 start ahead") && output.contains("4m"),
            "{output}"
        );
        assert_eq!(
            output.matches("fold-rule (here)").count(),
            2,
            "HELD names the task's worktree as ROUTINE does: {output}"
        );
        assert!(
            output.matches("○ 1 held").count() == 1 && output.contains("ROUTINE"),
            "the count shows once, in NEXT or beside a running state: {output}"
        );
        assert!(!output.contains("NEEDS YOU"), "{output}");
        assert_eq!(output.contains("GIVES UP"), bounded, "{output}");
        assert_eq!(
            serde_json::to_value(&model.rooms[0].tasks[0]).unwrap()["section"],
            "routine"
        );
    }
}

#[test]
fn needs_a_fix_tickets_use_the_oldest_reason_for_attention() {
    let now = Timestamp::from_second(100_000).unwrap();
    let mut row = TaskRow::rowless("triage".into(), None, None);
    let ticket = |hold, since_ms, reason: &str| Held {
        pid: 42,
        checkout: "/repo".into(),
        since_ms,
        kinds: Vec::new(),
        gives_up_ms: None,
        reason: Some(reason.into()),
        hold: Some(hold),
        position: 1,
    };
    row.held = vec![
        ticket(rimz::ids::HoldKind::Queue, 1, "queued: 1 start ahead"),
        ticket(rimz::ids::HoldKind::Disk, 3, "later disk"),
        ticket(
            rimz::ids::HoldKind::Store,
            2,
            "cannot count active agents: broken",
        ),
    ];
    let (kind, reason) = attention(&row, 0, now);
    assert!(
        kind == Some(Attention::Held),
        "a needs-a-fix start must raise attention"
    );
    assert_eq!(reason, "cannot count active agents: broken");
    row.held.truncate(1);
    let (kind, reason) = attention(&row, 0, now);
    assert!(kind.is_none() && reason.is_empty());
}

#[test]
fn capacity_json_retains_the_typed_hold_and_omits_unknown() {
    let mut record: launch_ledger::CapacityRecord = serde_json::from_value(serde_json::json!({
        "since": "2026-01-01T00:00:00Z", "first": "2026-01-01T00:00:00Z",
        "last": "2026-01-01T00:00:00Z", "count": 1, "kinds": [],
        "reason": "free disk low", "hold": "disk", "gave_up": false
    }))
    .unwrap();
    assert_eq!(
        serde_json::to_value(CapacityWaitView::from(&record)).unwrap()["hold"],
        "disk"
    );
    record.hold = None;
    assert!(
        serde_json::to_value(CapacityWaitView::from(&record))
            .unwrap()
            .get("hold")
            .is_none()
    );
}

#[test]
fn launched_counts_select_the_tasks_own_ledger() {
    use launch_ledger::{LaunchRecord, WorkflowLaunchRecord, WorkflowLedger};
    let at = Timestamp::from_second(100).unwrap();
    let residents = BTreeMap::from([(
        "review".into(),
        BTreeMap::from([(
            PathBuf::from("/repo/stale"),
            LaunchRecord {
                at,
                leader: "fox".into(),
                destination: None,
            },
        )]),
    )]);
    let mut workflows = WorkflowLedger::default();
    workflows.tasks.insert(
        "review".into(),
        ["/repo/one", "/repo/two"]
            .map(|path| {
                (
                    PathBuf::from(path),
                    WorkflowLaunchRecord {
                        at,
                        run: rimz::ids::WorkflowRunId::new(),
                        scope: rimz::store::workflow::ScopeKind::Worktree,
                    },
                )
            })
            .into(),
    );
    let mut entry = TaskEntry {
        workflow: Some("review".into()),
        when: Some(vec!["pr=open".into()]),
        each_worktree: true,
        ..TaskEntry::default()
    };
    assert_eq!(launched_count(&entry, "review", &residents, &workflows), 2);
    entry.workflow = None;
    entry.stay = true;
    assert_eq!(launched_count(&entry, "review", &residents, &workflows), 1);
    entry.stay = false;
    assert_eq!(launched_count(&entry, "review", &residents, &workflows), 0);
    entry.workflow = Some("review".into());
    entry.when = None;
    entry.every = Some("1h".into());
    assert_eq!(launched_count(&entry, "review", &residents, &workflows), 0);
}

#[test]
fn workflow_run_json_includes_only_nonempty_detail() {
    let mut row = WorkflowRunRow {
        id: rimz::ids::WorkflowRunId::new(),
        address: Some("review#repo".into()),
        checkout: "/repo".into(),
        state: "parked",
        node: "gate".into(),
        since: Timestamp::from_second(100).unwrap(),
        reason: Some("user"),
        text: Some("review the probe".into()),
        parked_at: None,
        detail: "user: review the probe".into(),
    };
    assert_eq!(
        serde_json::to_value(&row).unwrap()["detail"],
        "user: review the probe"
    );
    row.detail.clear();
    assert!(serde_json::to_value(&row).unwrap().get("detail").is_none());
}

#[test]
fn done_keeps_running_rows_above_the_finished_cut() {
    let now = Timestamp::from_second(1_000).unwrap();
    let mut tasks = Vec::new();
    for i in 0..7 {
        let mut row = TaskRow::rowless(format!("running-{i}"), None, None);
        row.section = Section::Done;
        tasks.push(row);
    }
    let room = Room {
        root: "/repo".into(),
        open: true,
        here: true,
        spend_today_usd: 0.0,
        tasks,
        retired: (0..6)
            .map(|i| Retired {
                name: format!("finished-{i}"),
                at: now,
                result: LoopRunResult::Completed,
                checkout: None,
                worktree_here: false,
            })
            .collect(),
    };
    let model = ListModel {
        rooms: vec![room],
        caller_root: Some("/repo".into()),
        now,
        warnings: Vec::new(),
    };
    let mut out = Vec::new();
    text::write(
        &mut out,
        &model,
        &[&model.rooms[0]],
        false,
        None,
        None,
        None,
    )
    .unwrap();
    let rendered = String::from_utf8(out).unwrap();
    for i in 0..7 {
        assert!(rendered.contains(&format!("running-{i}")), "{rendered}");
    }
    assert_eq!(rendered.matches("finished-").count(), 5, "{rendered}");
    assert!(rendered.contains("(and 1 older · --all)"), "{rendered}");
}

#[test]
fn expanded_wake_ups_drop_the_unfold_hint() {
    let now = Timestamp::from_second(1_000).unwrap();
    let mut row = TaskRow::rowless("wake".into(), None, None);
    row.kind = TaskKind::WakeUp;
    row.section = Section::WakeUps;
    let model = ListModel {
        rooms: vec![Room {
            root: "/repo".into(),
            open: true,
            here: true,
            spend_today_usd: 0.0,
            tasks: vec![row],
            retired: Vec::new(),
        }],
        caller_root: Some("/repo".into()),
        now,
        warnings: Vec::new(),
    };
    for all in [false, true] {
        let mut out = Vec::new();
        text::write(&mut out, &model, &[&model.rooms[0]], all, None, None, None).unwrap();
        let rendered = String::from_utf8(out).unwrap();
        assert_eq!(
            rendered.contains("rimz loop list --all"),
            !all,
            "{rendered}"
        );
    }
}

#[test]
fn here_marks_only_rows_that_name_a_worktree() {
    let mut bound = TaskRow::rowless("bound".into(), None, None);
    bound.worktree = Some("card-ux".into());
    let mut unbound = TaskRow::rowless("unbound".into(), None, None);
    unbound.worktree_here = true;
    let mut tasks = vec![bound, unbound];
    for row in &mut tasks {
        row.kind = TaskKind::Routine;
        row.section = Section::Routine;
    }
    let model = ListModel {
        rooms: vec![Room {
            root: "/repo".into(),
            open: true,
            here: true,
            spend_today_usd: 0.0,
            tasks,
            retired: Vec::new(),
        }],
        caller_root: Some("/repo".into()),
        now: Timestamp::from_second(1_000).unwrap(),
        warnings: Vec::new(),
    };
    let mut out = Vec::new();
    text::write(
        &mut out,
        &model,
        &[&model.rooms[0]],
        false,
        None,
        None,
        None,
    )
    .unwrap();
    let rendered = String::from_utf8(out).unwrap();
    assert!(rendered.contains("card-ux"), "{rendered}");
    assert!(!rendered.contains("(here)"), "{rendered}");
}

#[test]
fn shared_watch_state_keeps_strike_disabled_reason_out_of_next() {
    let now = Timestamp::from_second(1_000).unwrap();
    let mut row = TaskRow::rowless("struck".into(), None, None);
    row.state = TaskState::Off;
    row.attention = Some(Attention::Strikes);
    row.reason = "disabled after 3 strikes".into();
    row.timing = Some(schedule::TaskTiming::evaluate(
        &schedule::parse_trigger(
            "struck",
            &TaskEntry {
                every: Some("1h".into()),
                ..TaskEntry::default()
            },
        ),
        TaskSource::Config,
        None,
        Some(&Arming {
            enabled: false,
            at: None,
            pause_until: None,
            strikes: Some(3),
        }),
        &now.to_zoned(jiff::tz::TimeZone::UTC),
    ));
    assert_eq!(row.state_text(now), "off");
}

#[test]
fn needs_you_lists_parked_runs_under_a_stronger_attention() {
    let now = Timestamp::from_second(1_000).unwrap();
    let mut row = TaskRow::rowless("babysit".into(), None, None);
    row.section = Section::NeedsYou;
    row.attention = Some(Attention::Strikes);
    row.reason = "disabled after 3 strikes".into();
    let run = || WorkflowRunRow {
        id: rimz::ids::WorkflowRunId::new(),
        address: Some("live#probe".into()),
        checkout: "/repo-worktrees/probe".into(),
        state: "parked",
        node: "gate".into(),
        since: now,
        reason: Some("user"),
        text: Some("review the probe 界 ".repeat(8)),
        parked_at: None,
        detail: String::new(),
    };
    row.workflow_runs = vec![run()];
    row.head = "every 5m".into();
    row.continuation = "each worktree".into();
    let mut parked = TaskRow::rowless("parked".into(), None, None);
    parked.head = "every 5m".into();
    parked.attach_workflow_runs(vec![run()], now);
    let model = ListModel {
        rooms: vec![Room {
            root: "/repo".into(),
            open: true,
            here: true,
            spend_today_usd: 0.0,
            tasks: vec![row, parked],
            retired: Vec::new(),
        }],
        caller_root: Some("/repo".into()),
        now,
        warnings: Vec::new(),
    };
    let mut out = Vec::new();
    text::write(
        &mut out,
        &model,
        &[&model.rooms[0]],
        false,
        None,
        None,
        None,
    )
    .unwrap();
    let rendered = String::from_utf8(out).unwrap();
    assert!(
        rendered.contains("  live#probe gate user: review the probe"),
        "{rendered}"
    );
    let mut out = Vec::new();
    text::write(
        &mut out,
        &model,
        &[&model.rooms[0]],
        false,
        Some(60),
        None,
        None,
    )
    .unwrap();
    let rendered = String::from_utf8(out).unwrap();
    let rendered = anstream::adapter::strip_str(&rendered).to_string();
    assert!(
        rendered.lines().all(|line| line.width() <= 60),
        "{rendered}"
    );
    let summary = rendered
        .lines()
        .find(|line| line.contains("1 run(s) parked:"))
        .unwrap();
    assert!(summary.ends_with("… 0s"), "{rendered}");
}

#[test]
fn needs_you_prints_each_of_two_parked_summaries_once_with_park_age() {
    let now = Timestamp::from_second(1_000).unwrap();
    let records = ["first", "second"].map(|lane| {
        serde_json::from_value(serde_json::json!({
            "version":1, "id":rimz::ids::WorkflowRunId::new(), "workflow":"review",
            "spec_dir":"/spec", "spec_digest":"digest", "project_root":"/repo",
            "scope":{"kind":"worktree", "path":format!("/repo/{lane}")},
            "created_at":now, "entered_at":Timestamp::from_second(100).unwrap(),
            "state":"parked", "node":"gate", "task":"babysit",
            "park":{
                "reason":"user", "text":"review this 界 ".repeat(8),
                "at":Timestamp::from_second(880).unwrap(),
            },
            "visits":{}, "visit_grant":0, "deadline_fired":false, "next_seq":1,
        }))
        .unwrap()
    });
    let mut row = TaskRow::rowless("babysit".into(), None, None);
    row.attach_workflow_runs(
        WorkflowRunRow::of_task(
            &records,
            "babysit",
            Path::new("/repo"),
            &BTreeMap::new(),
            now,
        ),
        now,
    );
    let room = Room {
        root: "/repo".into(),
        open: true,
        here: true,
        spend_today_usd: 0.0,
        tasks: vec![row],
        retired: Vec::new(),
    };
    let model = ListModel {
        rooms: vec![room],
        caller_root: Some("/repo".into()),
        now,
        warnings: Vec::new(),
    };
    let mut out = Vec::new();
    text::write(
        &mut out,
        &model,
        &[&model.rooms[0]],
        false,
        None,
        None,
        None,
    )
    .unwrap();
    let text = String::from_utf8(out).unwrap();
    for lane in ["first", "second"] {
        let summary = format!(
            "review#{lane} gate user: {} 2m",
            "review this 界 ".repeat(8).trim_end()
        );
        assert_eq!(text.matches(&summary).count(), 1, "{text}");
    }
    let original = serde_json::to_value(&model.rooms).unwrap();
    let mut out = Vec::new();
    text::write(
        &mut out,
        &model,
        &[&model.rooms[0]],
        false,
        Some(60),
        None,
        None,
    )
    .unwrap();
    let text = String::from_utf8(out).unwrap();
    let text = anstream::adapter::strip_str(&text).to_string();
    assert!(text.lines().all(|line| line.width() <= 60), "{text}");
    let summaries = text
        .lines()
        .filter(|line| line.contains("2m"))
        .collect::<Vec<_>>();
    assert_eq!(summaries.len(), 2, "{text}");
    assert!(summaries.iter().all(|line| line.contains("… 2m")), "{text}");
    assert_eq!(serde_json::to_value(&model.rooms).unwrap(), original);
}

#[test]
fn row_colors_follow_state_not_name_or_trigger_prefixes() {
    let now = Timestamp::from_second(1_000).unwrap();
    let tasks = [
        ("office-sync", LoopRunResult::Completed),
        ("✓sync", LoopRunResult::Failed),
    ]
    .map(|(name, result)| {
        let record = LoopRunRecord::new(name, result, run_log::LoopRunMode::Manual, 0);
        let mut row = TaskRow::rowless(name.into(), None, None);
        row.state = TaskState::Live;
        row.kind = TaskKind::Routine;
        row.section = Section::Routine;
        row.running = None;
        row.head = "off branch=main".into();
        row.last = Some(LastRun {
            at: now,
            result,
            ok: result == LoopRunResult::Completed,
            streak: 1,
            record,
        });
        row
    });
    let model = ListModel {
        rooms: vec![Room {
            root: "/repo".into(),
            open: true,
            here: true,
            spend_today_usd: 0.0,
            tasks: tasks.into(),
            retired: Vec::new(),
        }],
        caller_root: Some("/repo".into()),
        now,
        warnings: Vec::new(),
    };
    let mut out = Vec::new();
    text::write(&mut out, &model, &[&model.rooms[0]], true, None, None, None).unwrap();
    let rendered = String::from_utf8(out).unwrap();
    for name in ["office-sync", "✓sync", "off branch=main"] {
        assert!(
            rendered.contains(&ui::paint(ui::palette::body(), name)),
            "{rendered:?}"
        );
    }
    for (last, role) in [
        ("✓ 0s ago", ui::status::StateRole::Success),
        ("✗ failed 0s ago", ui::status::StateRole::Failed),
    ] {
        assert!(
            rendered.contains(&ui::paint(ui::status::role(role), last)),
            "{rendered:?}"
        );
    }
}

#[test]
fn trigger_wrap_preserves_modifiers_and_prioritizes_boundaries() {
    let text = "on ci.failed · branch=main · gate docs";
    let lines = wrap_trigger(text, 24);
    assert_eq!(lines, ["on ci.failed", "· branch=main", "· gate docs"]);
    let condition = "when pr=open && team.stage=Done && ci=passed";
    let lines = wrap_trigger(condition, 24);
    assert_eq!(
        lines,
        ["when pr=open", "&& team.stage=Done", "&& ci=passed"]
    );
    let unicode = "on command exit · réviser les changements";
    let lines = wrap_trigger(unicode, 24);
    assert!(lines.iter().all(|line| line.width() <= 24));
    assert_eq!(lines.join(" "), unicode);

    let rows = [
        (
            vec![
                "dependabot-repair".into(),
                "every 1h · next in 5m".into(),
                "run check".into(),
                "✓ 53m ago · 143 in a row".into(),
            ],
            String::new(),
            ui::status::role(ui::status::StateRole::Success),
        ),
        (
            vec![
                "triage".into(),
                condition.into(),
                "start yagni,reflect-notes".into(),
                "✓ 30m ago · 15 worktrees".into(),
            ],
            "for 3m · each worktree".into(),
            ui::status::role(ui::status::StateRole::Success),
        ),
    ];
    let mut terminal = Vec::new();
    text::write_table(
        &mut terminal,
        &["NAME", "TRIGGER", "ACTION", "LAST"],
        &rows,
        1,
        3,
        Some(100),
    )
    .unwrap();
    let terminal = String::from_utf8(terminal).unwrap();
    let terminal = anstream::adapter::strip_str(&terminal).to_string();
    assert!(
        terminal.lines().all(|line| line.width() <= 100),
        "{terminal}"
    );
    let mut piped = Vec::new();
    text::write_table(
        &mut piped,
        &["NAME", "TRIGGER", "ACTION", "LAST"],
        &rows,
        1,
        3,
        None,
    )
    .unwrap();
    let piped = String::from_utf8(piped).unwrap();
    assert_eq!(piped.lines().count(), 4, "{piped}");
    assert!(piped.contains(condition));

    let worktrees = [
        (
            vec![
                "hermetic-fixtures".into(),
                "on ci.failed".into(),
                "@coder".into(),
                "never fired".into(),
                "↳ team recon".into(),
            ],
            String::new(),
            ui::palette::body(),
        ),
        (
            vec![
                "codex-spend".into(),
                "on ci.failed pr.{conflicted,dequeued,merged}".into(),
                "@sweeper".into(),
                "heard ci.passed 49m ago".into(),
                "↳ sweep".into(),
            ],
            String::new(),
            ui::palette::body(),
        ),
        (
            vec![
                "observe-flaps".into(),
                "on command exit".into(),
                "@simple-keystone".into(),
                "watching 12m".into(),
                "wait-trusty-store".into(),
            ],
            String::new(),
            ui::palette::body(),
        ),
    ];
    for width in [100, 107, 120] {
        let mut terminal = Vec::new();
        text::write_table(
            &mut terminal,
            &["WORKTREE", "TRIGGER", "WAKES", "LAST", "NAME"],
            &worktrees,
            1,
            3,
            Some(width),
        )
        .unwrap();
        let terminal = String::from_utf8(terminal).unwrap();
        let terminal = anstream::adapter::strip_str(&terminal).to_string();
        assert!(
            terminal.lines().all(|line| line.width() <= width.max(107)),
            "{terminal}"
        );
    }
}
