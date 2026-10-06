use super::*;
use unicode_width::UnicodeWidthStr;

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
        ),
        (
            vec![
                "triage".into(),
                condition.into(),
                "start yagni,reflect-notes".into(),
                "✓ 30m ago · 15 worktrees".into(),
            ],
            "for 3m · each worktree".into(),
        ),
    ];
    let mut terminal = Vec::new();
    text::write_table(
        &mut terminal,
        &["NAME", "TRIGGER", "ACTION", "LAST"],
        &rows,
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
        ),
    ];
    for width in [100, 107, 120] {
        let mut terminal = Vec::new();
        text::write_table(
            &mut terminal,
            &["WORKTREE", "TRIGGER", "WAKES", "LAST", "NAME"],
            &worktrees,
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
