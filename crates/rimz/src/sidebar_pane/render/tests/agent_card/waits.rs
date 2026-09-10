use super::*;
use crate::agents::{PendingWake, PendingWakeTrigger};
use crate::sidebar_pane::render::labels::elapsed_glyph;
use crate::sidebar_pane::render::theme::Component;

#[test]
fn pending_wakes_line_counts_armed_wakes() {
    let mut parent = agent(
        "claude-1",
        "claude",
        AgentStatus::Success,
        Some("/repo/main"),
        Some("main"),
        Some("finished work"),
    );
    parent.pending_wakes = vec![
        PendingWake {
            name: "timer".to_owned(),
            trigger: PendingWakeTrigger::Timer {
                due: fixed_now() + Duration::from_secs(720),
            },
            armed_at: Some(fixed_now()),
        },
        PendingWake {
            name: "command".to_owned(),
            trigger: PendingWakeTrigger::Command {
                command: "make check".to_owned(),
            },
            armed_at: Some(fixed_now()),
        },
    ];
    let mut child = agent(
        "child-1",
        "claude",
        AgentStatus::Success,
        None,
        None,
        Some("Explore"),
    );
    child.parent_agent_id = Some("claude-1".into());
    child.subagent_description = Some("inspect the renderer".to_owned());
    child.subagent_cost_usd = Some(0.42);
    child.usage.total_tokens = Some(12_400);
    let mut snapshot = snapshot_with(vec![parent, child]);
    let theme = Theme::fixed(false);
    let collapsed = group_lines(&snapshot, &theme, usize::MAX);
    let collapsed_text = line_texts(&collapsed);
    let stats = collapsed_text
        .iter()
        .position(|line| line.contains("⧉ subagents (1) · ⧖ waits (2)"))
        .unwrap();
    let summary = &collapsed[stats];
    for (glyph, component) in [
        (GlyphRole::CardSubagents, Component::SubagentHeader),
        (GlyphRole::CardWaits, Component::WakeHeader),
    ] {
        let span = summary
            .spans
            .iter()
            .find(|span| span.content == theme.glyph(glyph))
            .unwrap();
        assert_eq!(span.style.fg, Some(theme.component(component)));
    }
    for (text, style) in [
        (" subagents (1)", theme.body()),
        (" waits (2)", theme.body()),
        (" · ", theme.muted()),
        ("$0.42", theme.money_style(Modifier::empty())),
    ] {
        let span = summary
            .spans
            .iter()
            .find(|span| span.content == text)
            .unwrap();
        assert_eq!(span.style.fg, style.fg);
    }
    assert!(collapsed_text[stats].trim_end().ends_with("$0.42"));
    assert!(
        !collapsed_text
            .iter()
            .any(|line| line.contains("inspect the renderer"))
    );
    assert!(
        !collapsed_text
            .iter()
            .any(|line| line.contains("⧖ in 12m") || line.contains("make check"))
    );
    assert_snapshot(
        "pending_wakes_line",
        snapshot_to_screen_with_alert_and_ui(
            &snapshot,
            None,
            &UiState {
                selected_index: usize::MAX,
                ..Default::default()
            },
            54,
            23,
        ),
    );

    let expanded = line_texts(&group_lines(&snapshot, &theme, 0));
    for (resting, selected) in collapsed_text[2..=stats].iter().zip(&expanded[2..=stats]) {
        assert_eq!(
            resting.chars().skip(1).take(52).collect::<String>(),
            selected.chars().skip(1).take(52).collect::<String>(),
            "selection only appends delegation entries"
        );
    }
    assert!(expanded[stats].ends_with("$0.42▐"));
    assert!(expanded[stats + 1].contains("inspect the renderer"));
    assert!(expanded[stats + 2].contains("12k"));
    assert!(expanded[stats + 3].contains("⧖ in 12m"));
    assert!(expanded[stats + 4].contains("⧖ make"));
    assert!(expanded[stats + 5].contains("make check"));

    let narrow = group_lines_at_width(&snapshot, &theme, 0, 24);
    let narrow_text = line_texts(&narrow);
    assert!(narrow_text[stats].contains("subagents"));
    assert!(narrow_text[stats].ends_with("$0.42▐"));
    assert_eq!(narrow[stats].width(), 24);

    snapshot.worktree_groups[0].rows[0]
        .as_agent_mut()
        .unwrap()
        .pending_wakes
        .clear();
    let cleared = line_texts(&group_lines(&snapshot, &theme, 0));
    assert!(cleared[stats].contains("⧉ subagents (1)"));
    assert!(!cleared.iter().any(|line| line.contains("⧖")));
}

#[test]
fn wait_entries_show_trigger_program_and_command() {
    let mut parent = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("working"),
    );
    parent.pending_wakes = vec![
        PendingWake {
            name: "timer".to_owned(),
            trigger: PendingWakeTrigger::Timer {
                due: fixed_now() + Duration::from_secs(720),
            },
            armed_at: Some(fixed_now() - Duration::from_secs(1080)),
        },
        PendingWake {
            name: "signal".to_owned(),
            trigger: PendingWakeTrigger::Signal {
                selector: "pr.merged".to_owned(),
                deadline: Some(fixed_now() + Duration::from_secs(7200)),
            },
            armed_at: Some(fixed_now() - Duration::from_secs(300)),
        },
        PendingWake {
            name: "command".to_owned(),
            trigger: PendingWakeTrigger::Command {
                command: "/usr/bin/cargo xtask gate --name foo_test".to_owned(),
            },
            armed_at: Some(fixed_now() - Duration::from_secs(240)),
        },
    ];
    let mut snapshot = snapshot_with(vec![parent]);
    let theme = Theme::fixed(false);
    let lines = group_lines(&snapshot, &theme, 0);
    let rows = line_texts(&lines);
    let start = rows
        .iter()
        .position(|line| line.contains("⧖ in 12m"))
        .unwrap();
    for (offset, label, seconds, elapsed) in [
        (0, "⧖ in 12m", 1080, "18m"),
        (1, "⧖ on pr.merged · 2h left", 300, "5m"),
        (2, "⧖ cargo", 240, "4m"),
    ] {
        assert!(rows[start + offset].contains(label));
        assert!(
            rows[start + offset]
                .ends_with(&format!("{} {elapsed:>3}▐", elapsed_glyph(&theme, seconds)))
        );
        assert_eq!(lines[start + offset].width(), 54);
    }
    assert!(rows[start + 3].contains("      cargo xtask gate --name foo_test"));
    assert!(!rows[start + 3].contains("/usr/bin"));
    assert!(!rows[start + 3].contains(elapsed_glyph(&theme, 240).as_str()));
    let detail = lines[start + 3]
        .spans
        .iter()
        .find(|span| span.content == "cargo xtask gate --name foo_test")
        .unwrap();
    assert_eq!(detail.style.fg, theme.muted().fg);
    assert_snapshot("wait_entries", snapshot_to_screen(&snapshot, 54, 23));

    let narrow = group_lines_at_width(&snapshot, &theme, 0, 24);
    let narrow_text = line_texts(&narrow);
    assert!(narrow_text[start + 1].contains("⧖ on pr.merg"));
    assert!(narrow_text[start + 1].ends_with("5m▐"));
    assert!(narrow_text[start + 3].contains("cargo xtask gate"));
    assert!(!narrow_text[start + 3].contains("foo_test"));
    assert!(
        narrow[start..=start + 3]
            .iter()
            .all(|line| line.width() == 24)
    );

    snapshot.now += Duration::from_secs(720);
    let advanced = line_texts(&group_lines(&snapshot, &theme, 0));
    assert!(advanced[start].contains("⧖ due"));
    assert!(advanced[start].ends_with("30m▐"));
    assert!(advanced[start + 1].contains("on pr.merged · 108m left"));
    assert!(advanced[start + 1].ends_with("17m▐"));
    assert!(advanced[start + 2].ends_with("16m▐"));
    snapshot.now += Duration::from_secs(6480);
    let expired = line_texts(&group_lines(&snapshot, &theme, 0));
    assert!(expired[start + 1].contains("on pr.merged · 0m left"));
}

#[test]
fn wait_entry_without_armed_at_has_no_clock() {
    let mut parent = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("working"),
    );
    parent.pending_wakes.push(PendingWake {
        name: "signal".to_owned(),
        trigger: PendingWakeTrigger::Signal {
            selector: "pr.merged".to_owned(),
            deadline: None,
        },
        armed_at: None,
    });
    let snapshot = snapshot_with(vec![parent]);
    let rows = line_texts(&group_lines(&snapshot, &Theme::fixed(false), 0));
    let stats = rows
        .iter()
        .position(|line| line.contains("waits (1)"))
        .unwrap();
    assert_eq!(rows[stats].trim_matches(['▌', '▐', ' ']), "⧖ waits (1)");
    assert_eq!(
        rows[stats + 1].trim_matches(['▌', '▐', ' ']),
        "⧖ on pr.merged"
    );
}
