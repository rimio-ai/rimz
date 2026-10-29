use super::*;
use crate::agents::{
    ATTENTION_AGE_CEILING_SECS, BackgroundShell, PendingWait, PendingWaitTrigger, TurnPhase,
};
use crate::config::AnimationRole;
use crate::sidebar_pane::render::labels::{elapsed_glyph, role_glyph};
use crate::sidebar_pane::render::theme::Component;

#[test]
fn resting_command_wait_uses_the_delegating_head() {
    for phase in [TurnPhase::Idle, TurnPhase::Parked] {
        let mut parent = agent(
            "parent",
            "claude",
            AgentStatus::Success,
            Some("/repo/main"),
            Some("main"),
            Some("finished work"),
        );
        parent.phase = phase;
        parent.pending_waits.push(PendingWait {
            class: crate::agents::PendingWaitClass::Working,
            name: "command".into(),
            trigger: PendingWaitTrigger::Command {
                command: "cargo test".into(),
            },
            armed_at: Some(fixed_now()),
        });
        let snapshot = snapshot_with(vec![parent]);
        let theme = Theme::fixed(false);
        let screen = line_texts(&group_lines(&snapshot, &theme, 0)).join("\n");
        assert!(screen.contains("wakes after cargo test"), "{screen}");
        assert!(
            screen.contains(&format!(
                "{} claude",
                role_glyph(&theme, AnimationRole::Delegating, 0)
            )),
            "{phase:?}: {screen}"
        );
    }
}

fn command_wait_snapshot(unread: bool) -> SidebarSnapshot {
    let mut parent = agent(
        "parent",
        "claude",
        AgentStatus::Success,
        Some("/repo/main"),
        Some("main"),
        Some("finished work"),
    );
    parent.prompt = Some("finish the work".into());
    parent.pending_waits.push(PendingWait {
        class: crate::agents::PendingWaitClass::Working,
        name: "command".into(),
        trigger: PendingWaitTrigger::Command {
            command: "cargo test".into(),
        },
        armed_at: Some(fixed_now()),
    });
    let mut snapshot = snapshot_with(vec![parent]);
    snapshot.rows_mut().next().expect("the parent row").unread = unread;
    snapshot
}

fn command_wait_identity(
    snapshot: &SidebarSnapshot,
    theme: &Theme,
    selected_index: usize,
    phase: u64,
    lead: bool,
) -> Line<'static> {
    let cost_rolls = CostRolls::default();
    let mut ctx = test_row_ctx(snapshot, theme, 54, selected_index, phase, &cost_rolls);
    ctx.lead_unread = lead.then_some("parent");
    worktree_group_block(&ctx, &snapshot.worktree_groups[0], false, None)
        .lines
        .into_iter()
        .find(|line| line.to_string().contains("claude"))
        .expect("the command-wait card has an identity line")
}

#[test]
fn unread_waiting_head_shares_the_non_lead_card_crest() {
    let theme = truecolor_sidebar_theme();
    let unread = command_wait_identity(&command_wait_snapshot(true), &theme, usize::MAX, 0, false);
    assert_eq!(
        unread.spans[1].content,
        role_glyph(&theme, AnimationRole::Delegating, 0)
    );
    assert_eq!(unread.spans[3].content, "claude");
    assert_eq!(unread.spans[1].style, unread.spans[3].style);

    let selected = command_wait_identity(&command_wait_snapshot(false), &theme, 0, 0, false);
    assert_ne!(unread.spans[1].style.fg, selected.spans[1].style.fg);
    assert_eq!(
        selected.spans[1].style.fg,
        theme.animations.natural_color(AnimationRole::Delegating)
    );
}

#[test]
fn lead_unread_waiting_head_shimmers_with_the_name() {
    let theme = truecolor_sidebar_theme();
    let snapshot = command_wait_snapshot(true);
    let start = command_wait_identity(&snapshot, &theme, usize::MAX, 0, true);
    let later = command_wait_identity(&snapshot, &theme, usize::MAX, 12, true);
    assert_eq!(
        start.spans[1].content,
        role_glyph(&theme, AnimationRole::Delegating, 0)
    );
    assert_eq!(
        later.spans[1].content,
        role_glyph(&theme, AnimationRole::Delegating, 12)
    );
    assert_ne!(
        start.spans[1].style.fg,
        theme.animations.natural_color(AnimationRole::Delegating)
    );
    assert_ne!(start.spans[1].style, later.spans[1].style);
    assert_eq!(start.spans[3].content, "c");
    assert_ne!(start.spans[3].style, later.spans[3].style);
}

#[test]
fn calm_waiting_head_shares_the_name_and_working_head_soft_tone() {
    let theme = truecolor_sidebar_theme();
    let snapshot = command_wait_snapshot(false);
    let calm = command_wait_identity(&snapshot, &theme, usize::MAX, 0, false);
    assert_eq!(
        calm.spans[1].content,
        role_glyph(&theme, AnimationRole::Delegating, 0)
    );
    assert_eq!(calm.spans[1].style, calm.spans[3].style);
    assert_eq!(
        calm.spans[1].style,
        theme.body_brand(theme.provider_brand_tone("claude"))
    );
    let selected = command_wait_identity(&snapshot, &theme, 0, 0, false);
    assert_ne!(calm.spans[1].style.fg, selected.spans[1].style.fg);

    let acting = snapshot_with(vec![agent(
        "parent",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("working"),
    )]);
    let working = command_wait_identity(&acting, &theme, usize::MAX, 0, false);
    assert_eq!(
        working.spans[1].content,
        role_glyph(&theme, AnimationRole::Working, 0)
    );
    assert_eq!(calm.spans[1].style, working.spans[1].style);
}

#[test]
fn compacting_head_takes_the_card_soft_tone_and_unread_crest() {
    let theme = truecolor_sidebar_theme();
    let mut snapshot = command_wait_snapshot(false);
    let row = snapshot.rows_mut().next().expect("the parent row");
    row.as_agent_mut().expect("an agent row").compacting = true;
    let calm = command_wait_identity(&snapshot, &theme, usize::MAX, 0, false);
    assert_eq!(
        calm.spans[1].content,
        role_glyph(&theme, AnimationRole::Compacting, 0)
    );
    let meta = theme
        .animations
        .natural_color(AnimationRole::Compacting)
        .expect("the compacting head has a meta tone");
    assert_eq!(calm.spans[1].style, theme.body_brand(meta));
    let selected = command_wait_identity(&snapshot, &theme, 0, 0, false);
    assert_eq!(selected.spans[1].style.fg, Some(meta));

    snapshot.rows_mut().next().expect("the parent row").unread = true;
    let unread = command_wait_identity(&snapshot, &theme, usize::MAX, 0, false);
    assert_eq!(
        unread.spans[1].content,
        role_glyph(&theme, AnimationRole::Compacting, 0)
    );
    assert!(unread.spans[1].style.add_modifier.contains(Modifier::BOLD));
    assert_ne!(unread.spans[1].style.fg, Some(meta));
    assert_ne!(unread.spans[1].style.fg, calm.spans[1].style.fg);
}

#[test]
fn resting_background_shell_uses_the_delegating_head() {
    for phase in [TurnPhase::Idle, TurnPhase::Parked] {
        let mut parent = agent(
            "parent",
            "claude",
            AgentStatus::Success,
            Some("/repo/main"),
            Some("main"),
            Some("finished work"),
        );
        parent.phase = phase;
        parent.background_shells.push(BackgroundShell {
            id: "shell".into(),
            command: Some("cargo test".into()),
            description: None,
            started_at: fixed_now(),
        });
        let snapshot = snapshot_with(vec![parent]);
        let theme = Theme::fixed(false);
        let screen = line_texts(&group_lines(&snapshot, &theme, 0)).join("\n");
        assert!(
            screen.contains(&format!(
                "{} claude",
                role_glyph(&theme, AnimationRole::Delegating, 0)
            )),
            "{phase:?}: {screen}"
        );
    }
}

#[test]
fn resting_timer_wait_keeps_the_sleeping_head() {
    let mut parent = agent(
        "parent",
        "claude",
        AgentStatus::Success,
        Some("/repo/main"),
        Some("main"),
        Some("finished work"),
    );
    parent.pending_waits.push(PendingWait {
        class: crate::agents::PendingWaitClass::Sleeping,
        name: "timer".into(),
        trigger: PendingWaitTrigger::Timer {
            due: fixed_now() + Duration::from_secs(300),
            delay: None,
        },
        armed_at: Some(fixed_now()),
    });
    let screen = snapshot_to_screen(&snapshot_with(vec![parent]), 54, 24);
    assert!(screen.contains("☾ claude"), "{screen}");
}

#[test]
fn live_turn_with_background_shell_keeps_its_active_head() {
    for (phase, role) in [
        (TurnPhase::Acting, AnimationRole::Working),
        (TurnPhase::Reasoning, AnimationRole::Thinking),
    ] {
        let mut parent = agent(
            "parent",
            "claude",
            AgentStatus::Running,
            Some("/repo/main"),
            Some("main"),
            Some("working"),
        );
        parent.phase = phase;
        parent.background_shells.push(BackgroundShell {
            id: "shell".into(),
            command: Some("cargo test".into()),
            description: None,
            started_at: fixed_now(),
        });
        let snapshot = snapshot_with(vec![parent]);
        let theme = Theme::fixed(false);
        let screen = line_texts(&group_lines(&snapshot, &theme, 0)).join("\n");
        assert!(
            screen.contains(&format!("{} claude", role_glyph(&theme, role, 0))),
            "{phase:?}: {screen}"
        );
    }
}

#[test]
fn subagent_wait_uses_the_child_row_without_a_wait_count_or_entry() {
    let mut parent = agent(
        "parent",
        "claude",
        AgentStatus::Success,
        Some("/repo/main"),
        Some("main"),
        None,
    );
    parent.pending_waits.push(PendingWait {
        class: crate::agents::PendingWaitClass::Inert,
        name: "calm-fox".into(),
        trigger: PendingWaitTrigger::Subagent {
            active_at: fixed_now(),
            deadline_at: None,
            settled: Some("done".into()),
        },
        armed_at: Some(fixed_now()),
    });
    let mut child = agent(
        "child",
        "claude",
        AgentStatus::Success,
        None,
        None,
        Some("child-result"),
    );
    child.parent_agent_id = Some("parent".into());
    let snapshot = snapshot_with(vec![parent, child]);
    let screen = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &ViewState {
            selected_index: Some(0),
            ..Default::default()
        },
        &UiState::default(),
        64,
        30,
    );
    assert!(!screen.contains("wakes when calm-fox reports"), "{screen}");
    assert_eq!(screen.matches("child-result").count(), 1, "{screen}");
    assert!(!screen.contains('⧖'), "{screen}");
    assert!(!screen.contains("reporting"), "{screen}");
}

#[test]
fn team_wait_draws_a_static_counted_stage_entry_and_working_description() {
    let mut parent = agent(
        "parent",
        "claude",
        AgentStatus::Success,
        Some("/repo/main"),
        Some("main"),
        None,
    );
    parent.pending_waits.push(PendingWait {
        class: crate::agents::PendingWaitClass::Working,
        name: "forge#feat-x".into(),
        trigger: PendingWaitTrigger::Team {
            stage: Some("Review".into()),
        },
        armed_at: Some(fixed_now()),
    });
    let snapshot = snapshot_with(vec![parent]);
    let screen = snapshot_to_screen(&snapshot, 64, 30);
    assert!(
        screen.contains("wakes when forge#feat-x reaches Done"),
        "{screen}"
    );
    assert!(screen.contains("waits (1)"), "{screen}");
    let theme = Theme::fixed(false);
    let lead = theme.glyph(GlyphRole::CardWaitSignal);
    assert!(
        screen.contains(&format!("{lead} team · stage Review")),
        "{screen}"
    );
}

#[test]
fn running_card_shows_queued_question_without_changing_status() {
    let parent = agent(
        "codex-1",
        "codex",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("working"),
    );
    let mut value = serde_json::to_value(parent).unwrap();
    value["queued_asks"] = serde_json::json!([
        {"id":crate::ids::AskId::new(),"native_key":"first","detail":"Which format?","since":fixed_now()},
        {"id":crate::ids::AskId::new(),"native_key":"second","detail":"How much detail?","since":fixed_now()}
    ]);
    let parent = serde_json::from_value(value).unwrap();
    let snapshot = snapshot_with(vec![parent]);
    let theme = Theme::fixed(false);
    let lines = group_lines(&snapshot, &theme, usize::MAX);
    assert!(
        line_texts(&lines)
            .iter()
            .any(|line| line.contains("Which format?") && line.contains("+1"))
    );
    assert_eq!(snapshot.agents[0].status, AgentStatus::Running);
    assert_snapshot(
        "running_async_question",
        snapshot_to_screen(&snapshot, 54, 24),
    );
}

/// A running shell job's lead: the working animation at the test phase.
fn shell_lead(theme: &Theme, text: &str) -> String {
    format!("{} {text}", role_glyph(theme, AnimationRole::Working, 0))
}

#[test]
fn a_pending_idle_stop_is_one_wait_entry() {
    let mut resting = agent(
        "claude-1",
        "claude",
        AgentStatus::Success,
        Some("/repo/main"),
        Some("main"),
        Some("finished work"),
    );
    let pending = |due_at| crate::agents::PendingIdleStop {
        stop: crate::agents::IdleStop {
            after_secs: 180,
            requested_at: fixed_now(),
            requested_by: None,
        },
        due_at,
    };
    for (due_at, entry) in [
        (None, "stop · after 3m idle "),
        (
            Some(fixed_now() + jiff::SignedDuration::from_secs(120)),
            "stop · after 3m idle, in 2m",
        ),
        (Some(fixed_now()), "stop · after 3m idle, due"),
    ] {
        let mut clocked = resting.clone();
        clocked.idle_stop = Some(pending(due_at));
        let lines = line_texts(&group_lines(
            &snapshot_with(vec![clocked]),
            &Theme::fixed(false),
            0,
        ));
        assert!(lines.iter().any(|line| line.contains(entry)), "{lines:?}");
    }
    resting.idle_stop = Some(pending(None));
    let snapshot = snapshot_with(vec![resting]);
    let theme = Theme::fixed(false);
    let collapsed = line_texts(&group_lines(&snapshot, &theme, usize::MAX));
    assert!(
        !collapsed.iter().any(|line| line.contains("⧖ waits (1)")),
        "{collapsed:?}"
    );
    assert!(!collapsed.iter().any(|line| line.contains("after 3m idle")));
    let expanded = line_texts(&group_lines(&snapshot, &theme, 0));
    assert_eq!(
        expanded
            .iter()
            .filter(|line| line.contains("stop · after 3m idle"))
            .count(),
        1,
        "{expanded:?}"
    );
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
            class: crate::agents::PendingWaitClass::Sleeping,
            name: "timer".to_owned(),
            trigger: PendingWaitTrigger::Timer {
                due: fixed_now() + Duration::from_secs(720),
                delay: None,
            },
            armed_at: Some(fixed_now()),
        },
        PendingWait {
            class: crate::agents::PendingWaitClass::Working,
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
    child.usage.fresh_input_tokens = Some(12_400);
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
            .any(|line| line.contains("◷ timer · in") || line.contains("command · make"))
    );
    assert_snapshot(
        "pending_waits_line",
        snapshot_to_screen_with_alert_and_ui(
            &snapshot,
            None,
            &ViewState {
                selected_index: Some(usize::MAX),
                ..Default::default()
            },
            &UiState::default(),
            54,
            23,
        ),
    );

    let expanded = line_texts(&group_lines(&snapshot, &theme, 0));
    for (resting, selected) in collapsed_text[2..stats].iter().zip(&expanded[2..stats]) {
        assert_eq!(
            resting.chars().skip(1).take(52).collect::<String>(),
            selected.chars().skip(1).take(52).collect::<String>(),
            "selection leaves identity, description, and meter in place"
        );
    }
    assert!(expanded[stats].contains("▤ 0"));
    let selected_stats = stats + 1;
    assert!(expanded[selected_stats].ends_with("$0.42▐"));
    assert!(expanded[selected_stats + 1].contains("inspect the renderer"));
    assert!(expanded[selected_stats + 2].contains("12k"));
    assert!(expanded[selected_stats + 3].contains("◷ timer · in 12m"));
    assert!(expanded[selected_stats + 4].contains(&shell_lead(&theme, "command · make")));
    assert!(expanded[selected_stats + 5].contains("      make check"));
    assert_eq!(
        expanded.len(),
        selected_stats + 6,
        "a command wait always carries its command on line 2"
    );

    for width in [24, 30, 36, 45, 46] {
        let narrow = group_lines_at_width(&snapshot, &theme, 0, width);
        let narrow_text = line_texts(&narrow);
        let counts = narrow_text[selected_stats]
            .strip_suffix("$0.42▐")
            .unwrap()
            .trim_end();
        let expected = if width < 46 {
            "▌  ⧉ 1 · ⧖ 2"
        } else {
            "▌  ⧉ subagents (1) · ⧖ waits (2)"
        };
        assert_eq!(counts, expected, "pane width {width}");
        assert_eq!(narrow[selected_stats].width(), width);
        let collapsed = line_texts(&group_lines_at_width(&snapshot, &theme, usize::MAX, width));
        assert_eq!(
            narrow_text[selected_stats]
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

    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows)
        [0]
    .as_agent_mut()
    .unwrap()
    .pending_waits
    .push(PendingWait {
        class: crate::agents::PendingWaitClass::Sleeping,
        name: "signal".to_owned(),
        trigger: PendingWaitTrigger::Signal {
            selector: "pr.merged".to_owned(),
        },
        armed_at: Some(fixed_now()),
    });
    let stats = selected_stats;
    let with_signal = group_lines(&snapshot, &theme, 0);
    let with_signal_text = line_texts(&with_signal);
    assert!(with_signal_text[stats].contains("⧖ waits (3)"));
    let style = theme.styled(Component::WaitHeader, Modifier::empty());
    for (offset, glyph) in [
        (3, theme.glyph(GlyphRole::CardWaitTimer).to_owned()),
        (4, role_glyph(&theme, AnimationRole::Working, 0)),
        (6, theme.glyph(GlyphRole::CardWaitSignal).to_owned()),
    ] {
        let lead = with_signal[stats + offset]
            .spans
            .iter()
            .find(|span| span.content == glyph)
            .unwrap();
        assert_eq!(lead.style.fg, style.fg);
        assert_eq!(lead.style.add_modifier, style.add_modifier);
    }
    assert!(with_signal_text[stats + 6].contains("⌁ signal · pr.merged"));
    assert!(
        with_signal_text[stats + 3..=stats + 6]
            .iter()
            .all(|line| !line.contains(theme.glyph(GlyphRole::CardWaits)))
    );

    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows)
        [0]
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
        AgentStatus::Success,
        Some("/repo/main"),
        Some("main"),
        Some("working"),
    );
    parent.pending_waits = vec![
        PendingWait {
            class: crate::agents::PendingWaitClass::Sleeping,
            name: "timer".to_owned(),
            trigger: PendingWaitTrigger::Timer {
                due: fixed_now() + Duration::from_secs(720),
                delay: Some("30m".to_owned()),
            },
            armed_at: Some(fixed_now() - Duration::from_secs(1080)),
        },
        PendingWait {
            class: crate::agents::PendingWaitClass::Working,
            name: "pid".to_owned(),
            trigger: PendingWaitTrigger::Pid { pid: 16776 },
            armed_at: Some(fixed_now() - Duration::from_secs(180)),
        },
        PendingWait {
            class: crate::agents::PendingWaitClass::Working,
            name: "command".to_owned(),
            trigger: PendingWaitTrigger::Command {
                command: "/usr/bin/cargo xtask gate --name foo_test".to_owned(),
            },
            armed_at: Some(fixed_now() - Duration::from_secs(240)),
        },
        PendingWait {
            class: crate::agents::PendingWaitClass::Sleeping,
            name: "signal".to_owned(),
            trigger: PendingWaitTrigger::Signal {
                selector: "pr.merged".to_owned(),
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
        .position(|line| line.contains("◷ timer · in 12m"))
        .unwrap();
    for (offset, label, seconds, elapsed) in [
        (0, "◷ timer · in 12m".to_owned(), 1080, "18m"),
        (1, shell_lead(&theme, "pid · 16776"), 180, "3m"),
        (2, shell_lead(&theme, "command · cargo"), 240, "4m"),
        (4, "⌁ signal · pr.merged".to_owned(), 3600, "1h"),
    ] {
        assert!(
            rows[start + offset].contains(&label),
            "{}",
            rows[start + offset]
        );
        assert!(rows[start + offset].ends_with(&format!(
            "{} {elapsed:>3}▐",
            elapsed_glyph(&theme, seconds, ATTENTION_AGE_CEILING_SECS)
        )));
        assert_eq!(lines[start + offset].width(), 54);
        let clock = format!(
            "{} {elapsed:>3}",
            elapsed_glyph(&theme, seconds, ATTENTION_AGE_CEILING_SECS)
        );
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
    assert_eq!(rows.len(), start + 5, "only the command takes two lines");
    assert!(rows[start + 3].contains("      cargo xtask gate --name foo_test"));
    assert!(!rows.iter().any(|line| line.contains("kill -0")));
    assert!(!rows[start + 2].contains("/usr/bin"));
    assert_snapshot("wait_entries", snapshot_to_screen(&snapshot, 54, 25));
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
        frames.push(block.lines[start..=start + 4].to_vec());
    }
    assert_eq!(frames[0][0], frames[1][0], "a timer lead is static");
    assert_eq!(frames[0][4], frames[1][4], "a signal lead is static");
    assert_ne!(frames[0][1], frames[1][1], "a pid wait animates");
    assert_ne!(frames[0][2], frames[1][2], "a command wait animates");

    let narrow = group_lines_at_width(&snapshot, &theme, 0, 24);
    let narrow_text = line_texts(&narrow);
    assert!(narrow_text[start + 4].contains("⌁ signal"));
    assert!(narrow_text[start + 4].ends_with("1h▐"));
    assert!(narrow_text[start + 2].contains(&shell_lead(&theme, "command")));
    assert!(!narrow_text[start + 2].contains("foo_test"));
    assert!(
        narrow[start..=start + 4]
            .iter()
            .all(|line| line.width() == 24)
    );

    snapshot.now += Duration::from_secs(720);
    let advanced = line_texts(&group_lines(&snapshot, &theme, 0));
    assert!(advanced[start].contains("◷ timer · due"));
    assert!(advanced[start].ends_with("30m▐"));
    assert!(advanced[start + 1].ends_with("15m▐"));
    assert!(advanced[start + 4].ends_with("1h▐"));
    assert!(advanced[start + 2].ends_with("16m▐"));
    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0]
    .as_agent_mut()
    .unwrap()
    .pending_waits[0]
        .trigger = PendingWaitTrigger::Timer {
        due: snapshot.now,
        delay: None,
    };
    assert!(line_texts(&group_lines(&snapshot, &theme, 0))[start].contains("◷ timer · due"));
    for (command, detail) in [
        ("env A=b /usr/bin/cargo build", "env A=b cargo build"),
        ("sh -c '/usr/bin/cargo build'", "sh -c 'cargo build'"),
    ] {
        std::sync::Arc::make_mut(
            &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
        )[0]
        .as_agent_mut()
        .unwrap()
        .pending_waits[2]
            .trigger = PendingWaitTrigger::Command {
            command: command.to_owned(),
        };
        let wrapped = line_texts(&group_lines(&snapshot, &theme, 0));
        assert!(wrapped[start + 3].contains(detail));
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
        class: crate::agents::PendingWaitClass::Sleeping,
        name: "signal".to_owned(),
        trigger: PendingWaitTrigger::Signal {
            selector: "pr.merged".to_owned(),
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
        "⌁ signal · pr.merged"
    );
    let narrow = line_texts(&group_lines_at_width(
        &snapshot,
        &Theme::fixed(false),
        0,
        36,
    ));
    assert_eq!(narrow[stats].trim_matches(['▌', '▐', ' ']), "⧖ 1");

    let theme = Theme::fixed_for_theme(
        false,
        &toml::from_str::<crate::config::ThemeConfig>("[glyphs.unicode.value]\nseam = '|'")
            .unwrap(),
    );
    let rows = line_texts(&group_lines(&snapshot, &theme, 0));
    assert!(rows[stats + 1].contains("signal | pr.merged"));
}

#[test]
fn check_and_file_waits_have_type_specific_detail_lines() {
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
            class: crate::agents::PendingWaitClass::Working,
            name: "check".to_owned(),
            trigger: PendingWaitTrigger::Check {
                command: "/usr/bin/test -f ready".to_owned(),
            },
            armed_at: None,
        },
        PendingWait {
            class: crate::agents::PendingWaitClass::Working,
            name: "negated".to_owned(),
            trigger: PendingWaitTrigger::Check {
                command: "! systemctl is-active --quiet abyssal-verify-boros".to_owned(),
            },
            armed_at: None,
        },
        PendingWait {
            class: crate::agents::PendingWaitClass::Sleeping,
            name: "file".to_owned(),
            trigger: PendingWaitTrigger::File {
                path: "app.log".into(),
                grep: None,
            },
            armed_at: None,
        },
        PendingWait {
            class: crate::agents::PendingWaitClass::Sleeping,
            name: "match".to_owned(),
            trigger: PendingWaitTrigger::File {
                path: "app.log".into(),
                grep: Some("ready".to_owned()),
            },
            armed_at: None,
        },
    ];
    let snapshot = snapshot_with(vec![parent]);
    let theme = Theme::fixed(false);
    let rows = line_texts(&group_lines(&snapshot, &theme, 0));
    let start = rows
        .iter()
        .position(|line| line.contains("check · test"))
        .unwrap();
    assert!(rows[start + 1].contains("      test -f ready"));
    assert!(rows[start + 2].contains("check · systemctl"));
    assert!(rows[start + 3].contains("      ! systemctl is-active --quiet abyssal"));
    assert!(rows[start + 4].contains("file · app.log changes"));
    assert!(rows[start + 5].contains("file · app.log matches `ready`"));
    assert_eq!(rows.len(), start + 6);
}

#[test]
fn long_wait_clocks_and_child_runtimes_stay_muted() {
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
        class: crate::agents::PendingWaitClass::Sleeping,
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
    let clock = format!(
        "{}  2h",
        elapsed_glyph(&theme, 7200, ATTENTION_AGE_CEILING_SECS)
    );
    let tones: Vec<_> = lines
        .iter()
        .flat_map(|line| &line.spans)
        .filter(|span| span.content == clock)
        .map(|span| span.style.fg)
        .collect();
    assert_eq!(tones, vec![theme.muted().fg]);
    let runtime = lines
        .iter()
        .flat_map(|line| &line.spans)
        .find(|span| span.content == "   2h")
        .expect("a healthy child's runtime has no face");
    assert_eq!(runtime.style.fg, theme.muted().fg);
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
            class: crate::agents::PendingWaitClass::Sleeping,
            name: "timer".to_owned(),
            trigger: PendingWaitTrigger::Timer {
                due: fixed_now() + Duration::from_secs(720),
                delay: None,
            },
            armed_at: Some(fixed_now()),
        },
        PendingWait {
            class: crate::agents::PendingWaitClass::Working,
            name: "command".to_owned(),
            trigger: PendingWaitTrigger::Command {
                command: "make check".to_owned(),
            },
            armed_at: Some(fixed_now()),
        },
        PendingWait {
            class: crate::agents::PendingWaitClass::Sleeping,
            name: "signal".to_owned(),
            trigger: PendingWaitTrigger::Signal {
                selector: "pr.merged".to_owned(),
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
    assert!(
        collapsed
            .iter()
            .any(|line| line.contains("wakes after make check"))
    );
    assert!(!collapsed.iter().any(|line| line.contains("command · make")));

    let lines = group_lines(&snapshot, &theme, 0);
    let rows = line_texts(&lines);
    let timer = rows
        .iter()
        .position(|line| line.contains("◷ timer · in 12m"))
        .unwrap();
    assert!(rows[timer + 1].contains(&shell_lead(&theme, "command · make")));
    assert!(rows[timer + 3].contains(&shell_lead(&theme, "shell · Run the test suite")));
    assert!(rows[timer + 3].ends_with(&format!(
        "{}  5m▐",
        elapsed_glyph(&theme, 300, ATTENTION_AGE_CEILING_SECS)
    )));
    assert!(rows[timer + 4].contains("      cargo test --workspace"));
    assert!(
        !rows[timer + 4].contains(elapsed_glyph(&theme, 300, ATTENTION_AGE_CEILING_SECS).as_str())
    );
    let detail = lines[timer + 4]
        .spans
        .iter()
        .find(|span| span.content == "cargo test --workspace")
        .unwrap();
    assert_eq!(detail.style.fg, theme.muted().fg);
    assert!(rows[timer + 5].contains(&shell_lead(&theme, "shell · cat")));
    assert!(!rows[timer + 5].contains(char::is_control));
    assert!(rows[timer + 5].ends_with(&format!(
        "{}  2m▐",
        elapsed_glyph(&theme, 120, ATTENTION_AGE_CEILING_SECS)
    )));
    assert!(rows[timer + 7].contains(&shell_lead(&theme, "shell")));
    assert!(rows[timer + 7].ends_with(&format!(
        "{}  1m▐",
        elapsed_glyph(&theme, 60, ATTENTION_AGE_CEILING_SECS)
    )));
    assert!(rows[timer + 8].contains("⌁ signal · pr.merged"));
    assert_eq!(
        rows.len(),
        timer + 9,
        "command and shells with commands take a second line"
    );
    assert!(rows[timer + 6].contains("      cat <<'EOF' > notes foo EOF"));
    assert!(!rows[timer + 7].contains("shell ·"));
    assert!(!rows.iter().any(|line| line.contains("bg")));
    let lead = &lines[timer + 3].spans[2];
    assert_eq!(lead.content, role_glyph(&theme, AnimationRole::Working, 0));
    assert_eq!(lead.style.fg, Some(theme.component(Component::WaitHeader)));
    assert!(lead.style.add_modifier.is_empty());
    assert_snapshot(
        "background_shell_entries",
        snapshot_to_screen(&snapshot, 54, 28),
    );

    let narrow = line_texts(&group_lines_at_width(&snapshot, &theme, 0, 36));
    assert!(
        narrow
            .iter()
            .any(|line| line.trim_matches(['▌', '▐', ' ']) == "⧖ 6")
    );

    let mut shells_only = snapshot.clone();
    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut shells_only.worktree_groups)[0].rows,
    )[0]
    .as_agent_mut()
    .unwrap()
    .pending_waits
    .clear();
    let rows = line_texts(&group_lines(&shells_only, &theme, 0));
    let stats = rows
        .iter()
        .position(|line| line.contains("⧖ waits (3)"))
        .unwrap();
    assert!(rows[stats + 1].contains(&shell_lead(&theme, "shell · Run the test suite")));
}
