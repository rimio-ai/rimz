use super::*;
use unicode_width::UnicodeWidthStr;

#[test]
fn shared_watch_state_preserves_strike_disabled_reason() {
    let now = Timestamp::from_second(1_000).unwrap();
    let mut row = TaskRow::rowless("struck".into(), None, None);
    row.state = TaskState::Off;
    row.attention = Some(Attention::Strikes);
    row.reason = "disabled after 3 strikes".into();
    assert_eq!(row.state_text(now), "disabled after 3 strikes");
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
        }],
        caller_root: Some("/repo".into()),
        now,
        warnings: Vec::new(),
    };
    let mut out = Vec::new();
    text::write(&mut out, &model, &[&model.rooms[0]], true, None).unwrap();
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
