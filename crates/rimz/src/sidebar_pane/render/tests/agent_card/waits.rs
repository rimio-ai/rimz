use super::*;
use crate::agents::{BackgroundShell, PendingWait, PendingWaitTrigger};
use crate::config::AnimationRole;
use crate::sidebar_pane::render::labels::{activity_age_style, elapsed_glyph, role_glyph};
use crate::sidebar_pane::render::theme::Component;

/// A running shell job's lead: the working animation at the test phase.
fn shell_lead(theme: &Theme, text: &str) -> String {
    format!("{} {text}", role_glyph(theme, AnimationRole::Working, 0))
}

#[test]
fn pending_waits_line_counts_armed_waits() {
    let mut parent = agent(
        "claude-1",
        "claude",
        AgentStatus::Success,
        Some("/repo/main"),
        Some("main"),
        Some("finished work"),
    );
    parent.pending_waits = vec![
        PendingWait {
            name: "timer".to_owned(),
            trigger: PendingWaitTrigger::Timer {
                due: fixed_now() + Duration::from_secs(720),
                delay: None,
            },
            armed_at: Some(fixed_now()),
        },
        PendingWait {
            name: "command".to_owned(),
            trigger: PendingWaitTrigger::Command {
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
        (GlyphRole::CardWaits, Component::WaitHeader),
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
            .any(|line| line.contains("◷ in") || line.contains("make check"))
    );
    assert_snapshot(
        "pending_waits_line",
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
    assert!(expanded[stats + 3].contains("◷ in 12m"));
    assert!(expanded[stats + 4].contains(&shell_lead(&theme, "make check")));
    assert_eq!(
        expanded.len(),
        stats + 5,
        "a command wait's command is line 1, with no second line"
    );

    for width in [24, 30, 36, 45, 46] {
        let narrow = group_lines_at_width(&snapshot, &theme, 0, width);
        let narrow_text = line_texts(&narrow);
        let counts = narrow_text[stats]
            .strip_suffix("$0.42▐")
            .unwrap()
            .trim_end();
        let expected = if width < 46 {
            "▌  ⧉ 1 · ⧖ 2"
        } else {
            "▌  ⧉ subagents (1) · ⧖ waits (2)"
        };
        assert_eq!(counts, expected, "pane width {width}");
        assert_eq!(narrow[stats].width(), width);
        let collapsed = line_texts(&group_lines_at_width(&snapshot, &theme, usize::MAX, width));
        assert_eq!(
            narrow_text[stats]
                .chars()
                .skip(1)
                .take(width - 2)
                .collect::<String>(),
            collapsed[stats]
                .chars()
                .skip(1)
                .take(width - 2)
                .collect::<String>(),
        );
    }

    snapshot.worktree_groups[0].rows[0]
        .as_agent_mut()
        .unwrap()
        .pending_waits
        .push(PendingWait {
            name: "signal".to_owned(),
            trigger: PendingWaitTrigger::Signal {
                selector: "pr.merged".to_owned(),
                deadline: None,
            },
            armed_at: Some(fixed_now()),
        });
    let with_signal = group_lines(&snapshot, &theme, 0);
    let with_signal_text = line_texts(&with_signal);
    assert!(with_signal_text[stats].contains("⧖ waits (3)"));
    let style = theme.styled(Component::WaitHeader, Modifier::empty());
    for (offset, glyph) in [
        (3, theme.glyph(GlyphRole::CardWaitTimer).to_owned()),
        (4, role_glyph(&theme, AnimationRole::Working, 0)),
        (5, theme.glyph(GlyphRole::CardWaitSignal).to_owned()),
    ] {
        let lead = with_signal[stats + offset]
            .spans
            .iter()
            .find(|span| span.content == glyph)
            .unwrap();
        assert_eq!(lead.style.fg, style.fg);
        assert_eq!(lead.style.add_modifier, style.add_modifier);
    }
    assert!(with_signal_text[stats + 5].contains("⌁ pr.merged"));
    assert!(
        with_signal_text[stats + 3..=stats + 5]
            .iter()
            .all(|line| !line.contains(theme.glyph(GlyphRole::CardWaits)))
    );

    snapshot.worktree_groups[0].rows[0]
        .as_agent_mut()
        .unwrap()
        .pending_waits
        .clear();
    let cleared = line_texts(&group_lines(&snapshot, &theme, 0));
    assert!(cleared[stats].contains("⧉ subagents (1)"));
    assert!(!cleared.iter().any(|line| line.contains("⧖")));
    let narrow = line_texts(&group_lines_at_width(&snapshot, &theme, 0, 36));
    assert_eq!(
        narrow[stats].strip_suffix("$0.42▐").unwrap().trim_end(),
        "▌  ⧉ 1"
    );
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
    parent.pending_waits = vec![
        PendingWait {
            name: "timer".to_owned(),
            trigger: PendingWaitTrigger::Timer {
                due: fixed_now() + Duration::from_secs(720),
                delay: Some("30m".to_owned()),
            },
            armed_at: Some(fixed_now() - Duration::from_secs(1080)),
        },
        PendingWait {
            name: "pid".to_owned(),
            trigger: PendingWaitTrigger::Pid { pid: 16776 },
            armed_at: Some(fixed_now() - Duration::from_secs(180)),
        },
        PendingWait {
            name: "command".to_owned(),
            trigger: PendingWaitTrigger::Command {
                command: "/usr/bin/cargo xtask gate --name foo_test".to_owned(),
            },
            armed_at: Some(fixed_now() - Duration::from_secs(240)),
        },
        PendingWait {
            name: "signal".to_owned(),
            trigger: PendingWaitTrigger::Signal {
                selector: "pr.merged".to_owned(),
                deadline: Some(fixed_now() + Duration::from_secs(7200)),
            },
            armed_at: Some(fixed_now() - Duration::from_secs(3600)),
        },
    ];
    let mut snapshot = snapshot_with(vec![parent]);
    let theme = Theme::fixed(false);
    let lines = group_lines(&snapshot, &theme, 0);
    let rows = line_texts(&lines);
    let start = rows
        .iter()
        .position(|line| line.contains("◷ in 12m"))
        .unwrap();
    for (offset, label, seconds, elapsed) in [
        (0, "◷ in 12m".to_owned(), 1080, "18m"),
        (1, shell_lead(&theme, "pid 16776"), 180, "3m"),
        (
            2,
            shell_lead(&theme, "cargo xtask gate --name foo_test"),
            240,
            "4m",
        ),
        (3, "⌁ pr.merged · 2h left".to_owned(), 3600, "1h"),
    ] {
        assert!(
            rows[start + offset].contains(&label),
            "{}",
            rows[start + offset]
        );
        assert!(
            rows[start + offset]
                .ends_with(&format!("{} {elapsed:>3}▐", elapsed_glyph(&theme, seconds)))
        );
        assert_eq!(lines[start + offset].width(), 54);
        let clock = format!("{} {elapsed:>3}", elapsed_glyph(&theme, seconds));
        assert_eq!(
            lines[start + offset]
                .spans
                .iter()
                .find(|span| span.content == clock)
                .unwrap()
                .style
                .fg,
            theme.muted().fg
        );
    }
    assert_eq!(rows.len(), start + 4, "no wait entry takes a second line");
    assert!(!rows.iter().any(|line| line.contains("kill -0")));
    assert!(!rows[start + 2].contains("/usr/bin"));
    assert_snapshot("wait_entries", snapshot_to_screen(&snapshot, 54, 23));
    let cost_rolls = CostRolls::default();
    let lead_style = theme.styled(Component::WaitHeader, Modifier::empty());
    let mut frames = Vec::new();
    for phase in [0, 7] {
        let ctx = test_row_ctx(&snapshot, &theme, 54, 0, phase, &cost_rolls);
        let block = worktree_group_block(&ctx, &snapshot.worktree_groups[0], false, None);
        for offset in [1, 2] {
            let lead = &block.lines[start + offset].spans[2];
            assert_eq!(
                lead.content,
                role_glyph(&theme, AnimationRole::Working, phase)
            );
            assert_eq!(lead.style.fg, lead_style.fg);
            assert_eq!(lead.style.add_modifier, lead_style.add_modifier);
        }
        frames.push(block.lines[start..=start + 3].to_vec());
    }
    assert_eq!(frames[0][0], frames[1][0], "a timer lead is static");
    assert_eq!(frames[0][3], frames[1][3], "a signal lead is static");
    assert_ne!(frames[0][1], frames[1][1], "a pid wait animates");
    assert_ne!(frames[0][2], frames[1][2], "a command wait animates");

    let narrow = group_lines_at_width(&snapshot, &theme, 0, 24);
    let narrow_text = line_texts(&narrow);
    assert!(narrow_text[start + 3].contains("⌁ pr.merged"));
    assert!(narrow_text[start + 3].ends_with("1h▐"));
    assert!(narrow_text[start + 2].contains(&shell_lead(&theme, "cargo")));
    assert!(!narrow_text[start + 2].contains("foo_test"));
    assert!(
        narrow[start..=start + 3]
            .iter()
            .all(|line| line.width() == 24)
    );

    snapshot.now += Duration::from_secs(720);
    let advanced = line_texts(&group_lines(&snapshot, &theme, 0));
    assert!(advanced[start].contains("◷ due"));
    assert!(advanced[start].ends_with("30m▐"));
    assert!(advanced[start + 1].ends_with("15m▐"));
    assert!(advanced[start + 3].contains("⌁ pr.merged · 108m left"));
    assert!(advanced[start + 3].ends_with("1h▐"));
    assert!(advanced[start + 2].ends_with("16m▐"));
    snapshot.worktree_groups[0].rows[0]
        .as_agent_mut()
        .unwrap()
        .pending_waits[0]
        .trigger = PendingWaitTrigger::Timer {
        due: snapshot.now,
        delay: None,
    };
    assert!(line_texts(&group_lines(&snapshot, &theme, 0))[start].contains("◷ due"));
    snapshot.now += Duration::from_secs(6480);
    let expired = line_texts(&group_lines(&snapshot, &theme, 0));
    assert!(expired[start + 3].contains("⌁ pr.merged · 0m left"));
    for (command, detail) in [
        ("env A=b /usr/bin/cargo build", "env A=b cargo build"),
        ("sh -c '/usr/bin/cargo build'", "sh -c 'cargo build'"),
    ] {
        snapshot.worktree_groups[0].rows[0]
            .as_agent_mut()
            .unwrap()
            .pending_waits[2]
            .trigger = PendingWaitTrigger::Command {
            command: command.to_owned(),
        };
        let wrapped = line_texts(&group_lines(&snapshot, &theme, 0));
        assert!(wrapped[start + 2].contains(&shell_lead(&theme, detail)));
    }
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
    parent.pending_waits.push(PendingWait {
        name: "signal".to_owned(),
        trigger: PendingWaitTrigger::Signal {
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
    assert_eq!(rows[stats + 1].trim_matches(['▌', '▐', ' ']), "⌁ pr.merged");
    let narrow = line_texts(&group_lines_at_width(
        &snapshot,
        &Theme::fixed(false),
        0,
        36,
    ));
    assert_eq!(narrow[stats].trim_matches(['▌', '▐', ' ']), "⧖ 1");
}

#[test]
fn long_wait_clocks_stay_muted_while_subagent_clocks_heat() {
    let mut parent = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("working"),
    );
    let started = fixed_now() - Duration::from_secs(7200);
    parent.pending_waits.push(PendingWait {
        name: "timer".to_owned(),
        trigger: PendingWaitTrigger::Timer {
            due: fixed_now() + Duration::from_secs(7200),
            delay: None,
        },
        armed_at: Some(started),
    });
    let mut child = agent(
        "child-1",
        "claude",
        AgentStatus::Running,
        None,
        None,
        Some("Explore"),
    );
    child.parent_agent_id = Some("claude-1".into());
    child.subagent_started_at = Some(started);
    let snapshot = snapshot_with(vec![parent, child]);
    let theme = Theme::fixed(false);
    let lines = group_lines(&snapshot, &theme, 0);
    let clock = format!("{}  2h", elapsed_glyph(&theme, 7200));
    let tones: Vec<_> = lines
        .iter()
        .flat_map(|line| &line.spans)
        .filter(|span| span.content == clock)
        .map(|span| span.style.fg)
        .collect();
    assert_eq!(
        tones,
        vec![activity_age_style(&theme, 7200).fg, theme.muted().fg]
    );
    assert_ne!(tones[0], tones[1]);
}

#[test]
fn background_shells_join_the_shell_jobs_and_the_count() {
    let mut parent = agent(
        "claude-1",
        "claude",
        AgentStatus::Success,
        Some("/repo/main"),
        Some("main"),
        Some("finished work"),
    );
    parent.pending_waits = vec![
        PendingWait {
            name: "timer".to_owned(),
            trigger: PendingWaitTrigger::Timer {
                due: fixed_now() + Duration::from_secs(720),
                delay: None,
            },
            armed_at: Some(fixed_now()),
        },
        PendingWait {
            name: "command".to_owned(),
            trigger: PendingWaitTrigger::Command {
                command: "make check".to_owned(),
            },
            armed_at: Some(fixed_now()),
        },
        PendingWait {
            name: "signal".to_owned(),
            trigger: PendingWaitTrigger::Signal {
                selector: "pr.merged".to_owned(),
                deadline: None,
            },
            armed_at: Some(fixed_now()),
        },
    ];
    parent.background_shells = vec![
        BackgroundShell {
            id: "b1".to_owned(),
            command: Some("/usr/bin/cargo test --workspace".to_owned()),
            description: Some("Run the test suite".to_owned()),
            started_at: fixed_now() - Duration::from_secs(300),
        },
        BackgroundShell {
            id: "b2".to_owned(),
            command: Some("cat <<'EOF' > notes\nfoo\nEOF".to_owned()),
            description: Some("  ".to_owned()),
            started_at: fixed_now() - Duration::from_secs(120),
        },
        BackgroundShell {
            id: "b3".to_owned(),
            command: None,
            description: None,
            started_at: fixed_now() - Duration::from_secs(60),
        },
    ];
    let snapshot = snapshot_with(vec![parent]);
    let theme = Theme::fixed(false);

    let collapsed = line_texts(&group_lines(&snapshot, &theme, usize::MAX));
    assert!(collapsed.iter().any(|line| line.contains("⧖ waits (6)")));
    assert!(!collapsed.iter().any(|line| line.contains("make check")));

    let lines = group_lines(&snapshot, &theme, 0);
    let rows = line_texts(&lines);
    let timer = rows
        .iter()
        .position(|line| line.contains("◷ in 12m"))
        .unwrap();
    assert!(rows[timer + 1].contains(&shell_lead(&theme, "make check")));
    assert!(rows[timer + 2].contains(&shell_lead(&theme, "Run the test suite")));
    assert!(rows[timer + 2].ends_with(&format!("{}  5m▐", elapsed_glyph(&theme, 300))));
    assert!(rows[timer + 3].contains("      cargo test --workspace"));
    assert!(!rows[timer + 3].contains(elapsed_glyph(&theme, 300).as_str()));
    let detail = lines[timer + 3]
        .spans
        .iter()
        .find(|span| span.content == "cargo test --workspace")
        .unwrap();
    assert_eq!(detail.style.fg, theme.muted().fg);
    assert!(rows[timer + 4].contains(&shell_lead(&theme, "cat <<'EOF' > notes foo EOF")));
    assert!(!rows[timer + 4].contains(char::is_control));
    assert!(rows[timer + 4].ends_with(&format!("{}  2m▐", elapsed_glyph(&theme, 120))));
    assert!(rows[timer + 5].contains(&shell_lead(&theme, "background job")));
    assert!(rows[timer + 5].ends_with(&format!("{}  1m▐", elapsed_glyph(&theme, 60))));
    assert!(rows[timer + 6].contains("⌁ pr.merged"));
    assert_eq!(
        rows.len(),
        timer + 7,
        "only the described shell takes a second line"
    );
    assert!(!rows.iter().any(|line| line.contains("bg")));
    let lead = &lines[timer + 2].spans[2];
    assert_eq!(lead.content, role_glyph(&theme, AnimationRole::Working, 0));
    assert_eq!(lead.style.fg, Some(theme.component(Component::WaitHeader)));
    assert!(lead.style.add_modifier.is_empty());
    assert_snapshot(
        "background_shell_entries",
        snapshot_to_screen(&snapshot, 54, 26),
    );

    let narrow = line_texts(&group_lines_at_width(&snapshot, &theme, 0, 36));
    assert!(
        narrow
            .iter()
            .any(|line| line.trim_matches(['▌', '▐', ' ']) == "⧖ 6")
    );

    let mut shells_only = snapshot.clone();
    shells_only.worktree_groups[0].rows[0]
        .as_agent_mut()
        .unwrap()
        .pending_waits
        .clear();
    let rows = line_texts(&group_lines(&shells_only, &theme, 0));
    let stats = rows
        .iter()
        .position(|line| line.contains("⧖ waits (3)"))
        .unwrap();
    assert!(rows[stats + 1].contains(&shell_lead(&theme, "Run the test suite")));
}
