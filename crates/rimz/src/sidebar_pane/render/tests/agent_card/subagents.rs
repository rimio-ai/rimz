use super::*;
use crate::agents::{PendingWait, PendingWaitTrigger};
use crate::sidebar_pane::render::labels::{activity_age_style, elapsed_glyph};

fn focused_child_snapshot() -> SidebarSnapshot {
    let parent = agent(
        "parent",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        None,
        Some("delegate the scan"),
    );
    let mut snapshot = snapshot_with(vec![parent]);
    let child = PaneId::from_parts(MuxName::Zellij, "terminal_child");
    let card = snapshot.worktree_groups[0].rows[0].as_agent_mut().unwrap();
    card.sub_agent_count = 2;
    card.sub_agents = [
        ("explorer", Some(child.clone()), "inspect the click seam"),
        ("reviewer", None, "review the parent card"),
    ]
    .into_iter()
    .map(|(name, pane, description)| {
        serde_json::from_value(serde_json::json!({
            "id": name, "name": name, "status": "running", "last_activity": snapshot.now,
            "pane": pane, "description": description, "model": "Haiku", "phase": "reasoning",
        }))
        .unwrap()
    })
    .collect();
    snapshot.focused_pane = Some(child);
    snapshot
}

#[test]
fn selected_parent_marks_both_focused_child_entry_lines() {
    let snapshot = focused_child_snapshot();
    for color in [false, true] {
        let theme = Theme::fixed(color);
        let lines = group_lines(&snapshot, &theme, 0);
        let marked = lines
            .iter()
            .filter(|line| line.to_string().contains("  ▌ "))
            .collect::<Vec<_>>();
        assert_eq!(marked.len(), 2);
        for line in marked {
            let marker = line
                .spans
                .iter()
                .skip(1)
                .find(|span| {
                    span.content == theme.glyph(crate::config::GlyphRole::ChromeSpineCardLeft)
                })
                .unwrap();
            assert_eq!(marker.style.fg, theme.selection().fg);
        }
    }
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: Some(0),
            ..Default::default()
        },
        54,
        24,
    );
    assert!(
        rendered
            .lines()
            .any(|line| line.starts_with("▌  ▌ ⠁ explorer"))
    );
    assert!(
        rendered
            .lines()
            .any(|line| line.starts_with("▌  ▌   Haiku"))
    );
    assert!(
        rendered
            .lines()
            .any(|line| line.starts_with("▌    ⠁ reviewer"))
    );
    assert_snapshot("focused_child_selected", rendered);
}

#[test]
fn unselected_parent_does_not_mark_the_focused_child_entry() {
    let mut snapshot = focused_child_snapshot();
    snapshot.theme.display.card_density = crate::config::CardDensityMode::Expanded;
    let rendered =
        snapshot_to_screen_with_alert_and_ui(&snapshot, None, &UiState::default(), 54, 24);
    assert!(rendered.lines().any(|line| line.contains("explorer")));
    assert!(!rendered.contains("  ▌ "));
    assert_snapshot("focused_child_unselected", rendered);
}

#[test]
fn child_clock_uses_muted_runtime_until_quiet_and_the_configured_stall_scale() {
    let parent = agent(
        "root",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        None,
        None,
    );
    let mut child = agent(
        "child",
        "claude",
        AgentStatus::Running,
        None,
        None,
        Some("Explore"),
    );
    child.parent_agent_id = Some("root".into());
    child.subagent_started_at = Some(fixed_now() - Duration::from_secs(2 * 3_600));
    child.model = Some("Haiku".to_owned());
    for (status, quiet, ceiling, expected) in [
        (AgentStatus::Running, 299, 1_800, "   2h"),
        (AgentStatus::Running, 480, 1_800, "◑  8m"),
        (AgentStatus::Running, 960, 1_800, "◕ 16m"),
        (AgentStatus::Running, 960, 900, "◉ 16m"),
        (AgentStatus::Running, 2_400, 1_800, "◉ 40m"),
        (AgentStatus::Paused, 480, 1_800, "   1h"),
        (AgentStatus::Waiting, 480, 1_800, "   1h"),
        (AgentStatus::Success, 480, 1_800, "   1h"),
    ] {
        child.status = status;
        child.last_activity = fixed_now() - Duration::from_secs(quiet);
        let mut snapshot = snapshot_with(vec![parent.clone(), child.clone()]);
        snapshot.attention.stalled_after_secs = std::num::NonZeroU32::new(ceiling).unwrap();
        let theme = Theme::fixed(false);
        let lines = group_lines(&snapshot, &theme, 0);
        let metadata = lines
            .iter()
            .find(|line| line.spans.iter().any(|span| span.content == "Haiku"))
            .unwrap();
        let slot = metadata
            .spans
            .iter()
            .rev()
            .find(|span| span.content != "▐")
            .unwrap();
        assert_eq!(
            slot.content, expected,
            "status={status:?}, quiet={quiet}, ceiling={ceiling}"
        );
        let style = if status == AgentStatus::Running && quiet >= 300 {
            activity_age_style(&theme, quiet as i64, i64::from(ceiling))
        } else {
            theme.muted()
        };
        assert_eq!(slot.style.fg, style.fg);
        if quiet > u64::from(ceiling) && status == AgentStatus::Running {
            assert_eq!(slot.style.fg, theme.alarm(Modifier::empty()).fg);
        }
    }
}

#[test]
fn child_clock_shapes_survive_nerd_font_and_no_color() {
    let parent = agent(
        "root",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        None,
        None,
    );
    let mut child = agent(
        "child",
        "claude",
        AgentStatus::Running,
        None,
        None,
        Some("Explore"),
    );
    child.parent_agent_id = Some("root".into());
    child.subagent_started_at = Some(fixed_now() - Duration::from_secs(2 * 3_600));
    child.model = Some("Haiku".to_owned());
    for (modern, no_color, name) in [
        (true, false, "child_clocks_nerd_font"),
        (true, true, "child_clocks_nerd_font_no_color"),
        (false, true, "child_clocks_no_color"),
    ] {
        let mut frame = Vec::new();
        for quiet in [0, 480, 2_400] {
            child.last_activity = fixed_now() - Duration::from_secs(quiet);
            let mut snapshot = snapshot_with(vec![parent.clone(), child.clone()]);
            snapshot.theme.style = modern.then_some(crate::config::ThemeStyle::Modern);
            let theme = Theme::fixed_for_theme(no_color, &snapshot.theme);
            let lines = group_lines(&snapshot, &theme, 0);
            let text = line_texts(&lines).join("\n");
            let expected = if quiet == 0 {
                "   2h".to_owned()
            } else {
                format!(
                    "{} {:>3}",
                    elapsed_glyph(&theme, quiet as i64, 1_800),
                    if quiet == 480 { "8m" } else { "40m" }
                )
            };
            assert!(text.contains(&format!("{expected}▐")), "{text}");
            frame.push(text);
        }
        assert_snapshot(name, frame.join("\n\n"));
    }
}

#[test]
fn paused_child_parks_parent_head_and_pet() {
    let parent = agent(
        "parent",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("delegate"),
    );
    let mut child = agent(
        "child",
        "claude",
        AgentStatus::Running,
        None,
        None,
        Some("review"),
    );
    child.parent_agent_id = Some("parent".into());
    child.launch_depth = Some(1);
    child.name = Some("child".to_owned());
    child.last_activity = fixed_now() - Duration::from_secs(1);
    let mut context = crate::agents::AgentContext::new("claude", fixed_now());
    context.turn_error = Some(crate::agents::AgentTurnError {
        class: crate::agents::TurnErrorClass::PausedRateLimit,
        at: fixed_now(),
        label: Some("usage limit reached".to_owned()),
    });
    child.context = Some(context);
    let snapshot = snapshot_with(vec![parent, child]);
    let ui = UiState {
        selected_index: Some(0),
        ..Default::default()
    };
    assert_eq!(
        selected_pet_action(&snapshot, &ui),
        crate::sidebar_pane::pets::PetAction::Waiting
    );
    assert_ne!(
        animation_cadence_for_test(&snapshot),
        AnimationCadence::Fast
    );
    let bytes = snapshot_to_bytes_with_alert_and_ui(&snapshot, None, &ui, 54, 28);
    let mut parser = vt100::Parser::new(28, 54, 0);
    parser.process(&bytes);
    let screen = parser.screen().contents();
    let parent_line = screen.lines().find(|line| line.contains("claude")).unwrap();
    assert!(parent_line.contains("⏸"), "{screen}");
    assert!(!parent_line.contains("⢄"), "{screen}");
    let (line_index, line) = screen
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains("@child: usage limit reached"))
        .unwrap();
    let col = line.chars().position(|ch| ch == '@').unwrap();
    assert!(
        parser
            .screen()
            .cell(line_index as u16, col as u16)
            .unwrap()
            .italic()
    );
}

#[test]
fn paused_child_headline_stays_in_live_band() {
    let parent = agent(
        "parent",
        "claude",
        AgentStatus::Idle,
        Some("/repo/main"),
        Some("main"),
        Some("delegate"),
    );
    let mut paused = agent("paused", "claude", AgentStatus::Running, None, None, None);
    paused.parent_agent_id = Some("parent".into());
    paused.launch_depth = Some(1);
    paused.profile = Some("review".to_owned());
    paused.registered_at = Some(fixed_now() - Duration::from_secs(100));
    paused.last_activity = fixed_now() - Duration::from_secs(10);
    paused.description = Some("hidden task".to_owned());
    let mut context = crate::agents::AgentContext::new("claude", fixed_now());
    context.turn_error = Some(crate::agents::AgentTurnError {
        class: crate::agents::TurnErrorClass::PausedRateLimit,
        at: fixed_now(),
        label: Some("usage limit\nreached".to_owned()),
    });
    paused.context = Some(context);
    let mut running = paused.clone();
    running.agent_id = "running".into();
    running.profile = Some("build".to_owned());
    running.registered_at = Some(fixed_now() - Duration::from_secs(50));
    running.context = None;
    let snapshot = snapshot_with(vec![parent, paused, running]);
    let ui = UiState {
        selected_index: Some(0),
        ..Default::default()
    };
    for (width, name) in [
        (54, "paused_child_entry"),
        (28, "paused_child_entry_narrow"),
    ] {
        let bytes = snapshot_to_bytes_with_alert_and_ui(&snapshot, None, &ui, width, 28);
        let mut parser = vt100::Parser::new(28, width, 0);
        parser.process(&bytes);
        let screen = parser.screen().contents();
        let (line_index, line) = screen
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains("review ·"))
            .unwrap();
        assert!(line.contains("⏸"), "{screen}");
        assert!(line.contains("usage"), "{screen}");
        let col = line.chars().position(|ch| ch == 'u').unwrap();
        assert!(
            parser
                .screen()
                .cell(line_index as u16, col as u16)
                .unwrap()
                .italic()
        );
        assert!(screen.find("review").unwrap() < screen.find("build").unwrap());
        if width == 54 {
            assert!(line.contains("usage limit reached"));
        } else {
            assert!(!line.contains("usage limit reached"));
        }
        assert_snapshot(name, screen);
    }
}

#[test]
fn delegation_bands_keep_live_children_and_fold_older_ones() {
    let mut parent = agent(
        "parent",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("delegate"),
    );
    parent.user_turn_started_at = Some(fixed_now() - Duration::from_secs(3_600));
    let mut agents = vec![parent];
    // Spawn order task-0..task-8. task-1 and task-4 still run; the finished
    // children landed out of spawn order, six inside the 15m window (the cap
    // keeps five) and one outside it.
    let landed_secs_ago = [300, 0, 60, 780, 0, 600, 1_200, 180, 420];
    for (index, ago) in landed_secs_ago.into_iter().enumerate() {
        let running = matches!(index, 1 | 4);
        let mut child = agent(
            &format!("child-{index}"),
            "claude",
            if running {
                AgentStatus::Running
            } else {
                AgentStatus::Success
            },
            None,
            None,
            Some(&format!("task-{index}")),
        );
        child.parent_agent_id = Some("parent".into());
        child.registered_at = Some(fixed_now() - Duration::from_secs(1_300 - index as u64));
        child.last_activity = fixed_now() - Duration::from_secs(if running { 5 } else { ago });
        child.last_seen = child.last_activity;
        agents.push(child);
    }
    let mut snapshot = snapshot_with(agents);
    // A pending wait sits between the recent band and the folded tail, so the
    // history rows must land after it.
    snapshot.worktree_groups[0].rows[0]
        .as_agent_mut()
        .unwrap()
        .pending_waits
        .push(PendingWait {
            name: "deploy".to_owned(),
            trigger: PendingWaitTrigger::Signal {
                selector: "deploy.done".to_owned(),
            },
            armed_at: Some(fixed_now()),
        });
    let row = &snapshot.worktree_groups[0].rows[0];
    let turn = row.as_agent().unwrap().user_turn_started_at;
    let entries = |rendered: &str| {
        rendered
            .lines()
            .filter_map(|line| {
                line.split_whitespace()
                    .find(|word| word.starts_with("task-"))
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>()
    };
    let mut screens = Vec::new();
    for (selected_index, open, history, name) in [
        (0, None, false, "delegation_bands_selected"),
        (0, None, true, "delegation_bands_history"),
        (usize::MAX, None, false, "delegation_bands_unselected"),
        (0, Some(false), false, "delegation_bands_override_closed"),
    ] {
        let mut ui = UiState {
            selected_index: Some(selected_index),
            ..Default::default()
        };
        if let Some(open) = open {
            ui.delegation_overrides.insert(row.id.clone(), open);
        }
        if history {
            ui.delegation_history.insert(row.id.clone(), turn);
        }
        let rendered = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, 54, 44);
        assert!(rendered.contains("subagents (9)"), "{rendered}");
        assert_snapshot(name, rendered.clone());
        screens.push(rendered);
    }
    let [selected, history, unselected, closed] = screens.as_slice() else {
        unreachable!("four scenarios");
    };
    assert_eq!(
        entries(selected),
        [
            "task-1", "task-4", "task-2", "task-7", "task-0", "task-8", "task-5"
        ]
    );
    assert!(selected.contains("+2 older"), "{selected}");
    let tail = selected
        .lines()
        .position(|line| line.contains("+2 older"))
        .expect("folded tail");
    assert!(
        selected
            .lines()
            .take(tail)
            .any(|line| line.contains("deploy"))
    );
    assert!(
        history.lines().take(tail).eq(selected.lines().take(tail)),
        "history must only append below the tail:\n{selected}\n{history}"
    );
    assert_eq!(&entries(history)[7..], ["task-3", "task-6"]);
    assert!(!history.contains("older"), "{history}");
    for screen in [unselected, closed] {
        assert!(entries(screen).is_empty(), "{screen}");
        assert!(!screen.contains("older"), "{screen}");
    }
}

#[test]
fn render_selected_card_keeps_finished_metadata_and_frozen_runtime() {
    // Finished and running children keep metadata and plain runtime, not a landed age.
    let mut parent = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    parent.context = Some(claude_context(fixed_now()));

    let mut child = agent(
        "child-1",
        "claude",
        AgentStatus::Success,
        None,
        None,
        Some("Explore"),
    );
    child.parent_agent_id = Some("claude-1".into());
    child.subagent_description = Some("locate the render seam".to_owned());
    child.subagent_cost_usd = Some(0.42);
    child.subagent_started_at = Some(fixed_now() - Duration::from_secs(90));
    child.last_activity = fixed_now() - Duration::from_secs(30);
    child.last_seen = fixed_now() - Duration::from_secs(30);
    child.usage.fresh_input_tokens = Some(12_400);
    // A bare model id — the renderer prettifies it through `model_label`.
    child.model = Some("claude-opus-4-8".to_owned());
    // Claude reports the child's effort on its `SubagentStop`.
    child.effort = Some("high".to_owned());

    let mut fresh = agent(
        "child-2",
        "claude",
        AgentStatus::Running,
        None,
        None,
        Some("review"),
    );
    fresh.parent_agent_id = Some("claude-1".into());
    // Mid-reasoning: the child's leading cell is the thinking orbit (frame 0
    // of the animation at the test's fixed phase), not the static `⢿`.
    fresh.phase = crate::agents::TurnPhase::Reasoning;
    fresh.subagent_description = Some("audit the trust hash".to_owned());
    fresh.subagent_started_at = Some(fixed_now() - Duration::from_secs(30));
    fresh.usage.run_total_tokens = Some(3_100);
    // A sibling on a different model — the per-child label tells them apart.
    fresh.model = Some("claude-haiku-4-5".to_owned());

    let snapshot = snapshot_with(vec![parent, child, fresh]);
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: Some(0),
            ..Default::default()
        },
        54,
        23,
    );

    assert!(
        rendered.contains("⧉ subagents (2)"),
        "the expanded card lists its children:\n{rendered}"
    );
    assert!(
        rendered.contains("Explore · locate the render seam"),
        "the finished child keeps its type line:\n{rendered}"
    );
    let priced_line = rendered
        .lines()
        .find(|line| line.contains("Explore · locate the render seam"))
        .expect("priced child line");
    assert!(
        priced_line.ends_with(" $0.42▐") && !priced_line.contains('◔'),
        "the exact child cost pins right on line 1, the clock stays off it:\n{rendered}"
    );
    // The running child's leading cell is the thinking orbit (frame 0 at the
    // test's fixed animation phase), the agent-row head vocabulary verbatim.
    assert!(
        rendered.contains("⠁ review · audit the trust hash"),
        "a reasoning child wears the thinking head:\n{rendered}"
    );
    assert!(
        rendered.contains("◇  3k · Haiku 4.5"),
        "the running child carries its token spend and model:\n{rendered}"
    );
    assert!(
        rendered
            .lines()
            .any(|line| line.contains("Haiku 4.5") && line.ends_with("  <1m▐")),
        "the running child reads plain sub-minute runtime:\n{rendered}"
    );
    assert!(
        rendered
            .lines()
            .any(|line| line.contains("▤ 12k · Opus 4.8") && line.contains("· high")),
        "the finished child retains exact metadata:\n{rendered}"
    );
    let finished_line = rendered
        .lines()
        .find(|line| line.contains("▤ 12k"))
        .expect("finished child metadata line");
    assert!(
        finished_line.ends_with("   1m▐"),
        "the finished child's frozen runtime pins right on line 2:\n{rendered}"
    );
    // Both metadata-bearing children render a second line.
    let subagent_metadata_rows = rendered
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            (line.contains("◇") || line.contains("▤"))
                && line.starts_with("▌      ")
                && !trimmed.starts_with("W:")
                && !trimmed.starts_with("M:")
        })
        .count();
    assert_eq!(
        subagent_metadata_rows, 2,
        "both metadata-bearing children carry a token row:\n{rendered}"
    );
    assert_snapshot("subagent_two_line_entry", rendered);
}

#[test]
fn launched_subagent_renders_profile_cost_and_parent_rollup() {
    let mut parent = agent(
        "claude-root",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("delegate"),
    );
    let mut parent_context = claude_context(fixed_now());
    parent_context.cost = Some(crate::agents::AgentCost {
        total_cost_usd: Some(0.10),
        ..crate::agents::AgentCost::default()
    });
    parent.context = Some(parent_context);
    let mut child = agent(
        "codex-child",
        "codex",
        AgentStatus::Success,
        None,
        None,
        Some("map sidebar"),
    );
    child.name = Some("helper".to_owned());
    child.parent_agent_id = Some(parent.agent_id.clone());
    child.parent_agent_kind = Some(parent.kind.clone());
    child.launch_depth = Some(1);
    child.profile = Some("explorer".to_owned());
    child.description = Some("map sidebar".to_owned());
    child.usage.fresh_input_tokens = Some(12_000);
    let mut child_context = crate::agents::AgentContext::new("codex", fixed_now());
    child_context.cost = Some(crate::agents::AgentCost {
        total_cost_usd: Some(0.42),
        ..crate::agents::AgentCost::default()
    });
    child_context.tokens = Some(crate::agents::AgentTokenUsage {
        session_usage: Some(crate::agents::AgentSessionUsage {
            input_tokens: Some(30_000),
            output_tokens: Some(5_000),
            cache_creation_input_tokens: Some(2_000),
            cache_read_input_tokens: Some(200_000),
            ..crate::agents::AgentSessionUsage::default()
        }),
        ..crate::agents::AgentTokenUsage::default()
    });
    child.context = Some(child_context);

    let snapshot = snapshot_with(vec![parent, child]);
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: Some(0),
            ..Default::default()
        },
        60,
        20,
    );

    assert!(
        rendered.contains("explorer · map sidebar"),
        "the child line names its profile:\n{rendered}"
    );
    assert!(
        !rendered.contains("helper"),
        "the child card must not render its petname:\n{rendered}"
    );
    assert!(
        rendered
            .lines()
            .any(|line| line.contains("explorer · map sidebar") && line.contains("$0.42")),
        "the child cost pins on its line:\n{rendered}"
    );
    assert!(
        rendered
            .lines()
            .any(|line| line.contains("⧉ subagents (1)") && line.contains("$0.42")),
        "the lifetime stats line carries child cost:\n{rendered}"
    );
    assert!(
        rendered.contains("▤ 12k"),
        "the child metadata prefers window occupancy over session tokens:\n{rendered}"
    );
    assert!(
        rendered.lines().any(|line| line.contains("$0.52")),
        "the parent cost includes delegated spend:\n{rendered}"
    );
}

#[test]
fn subagent_stats_line_outlives_the_turn() {
    let mut parent = agent(
        "claude-root",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("delegate"),
    );
    parent.turn_started_at = Some(fixed_now() - Duration::from_secs(10));
    parent.user_turn_started_at = parent.turn_started_at;

    let mut child = agent(
        "codex-child",
        "codex",
        AgentStatus::Success,
        None,
        None,
        Some("map sidebar"),
    );
    child.parent_agent_id = Some(parent.agent_id.clone());
    child.parent_agent_kind = Some(parent.kind.clone());
    child.launch_depth = Some(1);
    child.last_activity = fixed_now() - Duration::from_secs(60);
    child.last_seen = child.last_activity;
    child.ended_at = Some(child.last_activity);
    let mut context = crate::agents::AgentContext::new("codex", fixed_now());
    context.cost = Some(crate::agents::AgentCost {
        total_cost_usd: Some(0.42),
        ..crate::agents::AgentCost::default()
    });
    child.context = Some(context);

    let snapshot = snapshot_with(vec![parent, child]);
    let theme = Theme::fixed(false);
    let unselected = line_texts(&group_lines(&snapshot, &theme, usize::MAX));
    assert_eq!(unselected.len(), 6, "{}", unselected.join("\n"));
    assert!(
        unselected[5].contains("⧉ subagents (1)") && unselected[5].contains("$0.42"),
        "{}",
        unselected.join("\n")
    );

    // The child finished before the parent's current turn but landed inside
    // the recent window, so the selected card still lists it with its age.
    let selected = line_texts(&group_lines(&snapshot, &theme, 0));
    assert_eq!(selected.len(), 7, "{}", selected.join("\n"));
    assert!(selected[5].contains("⧉ subagents (1)"));
    assert!(selected[6].contains("map sidebar") && selected[6].contains("1m $0.42"));
    assert!(!selected.iter().any(|line| line.contains("older")));

    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: Some(usize::MAX),
            ..Default::default()
        },
        60,
        20,
    );
    assert_snapshot("subagent_stats_line", rendered);
}

#[test]
fn all_older_children_open_in_one_click_without_a_lone_fold() {
    // Every child landed outside the recent window, so no live or recent row
    // is visible. Opening the section shows the older rows directly rather
    // than a lone `+K older` fold that would cost a second click.
    let mut parent = agent(
        "claude-root",
        "claude",
        AgentStatus::Success,
        Some("/repo/main"),
        Some("main"),
        Some("delegate"),
    );
    parent.user_turn_started_at = Some(fixed_now() - Duration::from_secs(7_200));
    let mut agents = vec![parent];
    for index in 0..3 {
        let mut child = agent(
            &format!("child-{index}"),
            "claude",
            AgentStatus::Success,
            None,
            None,
            Some(&format!("task-{index}")),
        );
        child.parent_agent_id = Some("claude-root".into());
        child.last_activity = fixed_now() - Duration::from_secs(3_600 + index);
        child.last_seen = child.last_activity;
        agents.push(child);
    }
    let snapshot = snapshot_with(agents);
    let mut ui = UiState {
        selected_index: Some(0),
        ..Default::default()
    };
    ui.delegation_overrides
        .insert(snapshot.worktree_groups[0].rows[0].id.clone(), true);
    let text = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, 54, 30);
    assert!(text.contains("subagents (3)"), "{text}");
    assert!(!text.contains("older"), "{text}");
    for index in 0..3 {
        assert!(text.contains(&format!("task-{index}")), "{text}");
    }
}

#[test]
fn engaged_card_without_children_has_no_stats_line() {
    let parent = agent(
        "claude-root",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("delegate"),
    );
    let snapshot = snapshot_with(vec![parent]);
    let rendered = line_texts(&group_lines(&snapshot, &Theme::fixed(false), usize::MAX));

    assert_eq!(rendered.len(), 5, "{}", rendered.join("\n"));
    assert!(!rendered.iter().any(|line| line.contains("⧉ subagents")));
}

#[test]
fn narrow_subagent_line_truncates_description_before_exact_cost() {
    let parent = agent(
        "claude-root",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("delegate"),
    );
    let mut child = agent(
        "claude-child",
        "claude",
        AgentStatus::Success,
        None,
        None,
        Some("Explore"),
    );
    child.parent_agent_id = Some("claude-root".into());
    child.subagent_description =
        Some("trace every caller through the complete rendering pipeline".to_owned());
    child.subagent_cost_usd = Some(0.42);

    let snapshot = snapshot_with(vec![parent, child]);
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: Some(0),
            ..Default::default()
        },
        34,
        23,
    );
    let child_line = rendered
        .lines()
        .find(|line| line.contains("Explore"))
        .expect("child line");

    assert!(child_line.contains("$0.42▐"), "{rendered}");
    assert!(
        !child_line.contains("complete rendering pipeline"),
        "description yields width to the exact cost:\n{rendered}"
    );
}

#[test]
fn metadata_free_finished_subagent_stays_one_line() {
    let parent = agent(
        "copilot-root",
        "copilot",
        AgentStatus::Success,
        Some("/repo/main"),
        Some("main"),
        Some("delegate"),
    );
    let mut child = agent(
        "copilot-child",
        "copilot",
        AgentStatus::Success,
        None,
        None,
        Some("cleanup"),
    );
    child.parent_agent_id = Some("copilot-root".into());
    child.subagent_started_at = Some(fixed_now() - Duration::from_secs(600));
    child.last_activity = fixed_now() - Duration::from_secs(60);
    child.subagent_cost_usd = Some(0.42);

    let snapshot = snapshot_with(vec![parent, child]);
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: Some(0),
            ..Default::default()
        },
        54,
        21,
    );
    let lines = rendered.lines().collect::<Vec<_>>();
    let child_line = lines
        .iter()
        .position(|line| line.contains("✓ cleanup"))
        .unwrap_or_else(|| panic!("finished child missing:\n{rendered}"));
    assert!(
        !lines
            .get(child_line + 1)
            .is_some_and(|line| line.starts_with("▌      ")),
        "metadata-free completion stays one line:\n{rendered}"
    );
    assert!(
        lines[child_line].ends_with("   9m $0.42▐") && !lines[child_line].contains('◔'),
        "with no line 2, frozen runtime pins ahead of cost:\n{rendered}"
    );
}
#[test]
fn subagent_metadata_blank_fills_the_per_card_grid() {
    // Window, total, and unknown figures share one grid; the token-less child's model stays aligned without a bare glyph or orphan seam.
    let mut parent = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    parent.context = Some(claude_context(fixed_now()));

    let mut spender = agent(
        "child-1",
        "claude",
        AgentStatus::Running,
        None,
        None,
        Some("Explore"),
    );
    spender.parent_agent_id = Some("claude-1".into());
    spender.subagent_started_at = Some(fixed_now() - Duration::from_secs(90));
    spender.usage.fresh_input_tokens = Some(12_400);
    spender.model = Some("claude-opus-4-8".to_owned());
    spender.effort = Some("high".to_owned());

    // A sibling before its first `subagentStatusLine` report: no tokens yet,
    // its model already known from its own lifecycle events.
    let mut quiet = agent(
        "child-2",
        "claude",
        AgentStatus::Running,
        None,
        None,
        Some("review"),
    );
    quiet.parent_agent_id = Some("claude-1".into());
    quiet.model = Some("claude-haiku-4-5".to_owned());

    let mut total = spender.clone();
    total.agent_id = "child-3".into();
    total.model = Some("claude-sonnet-4-6".to_owned());
    let mut snapshot = snapshot_with(vec![parent, spender, quiet, total]);
    for child in &mut snapshot.worktree_groups[0].rows[0]
        .as_agent_mut()
        .unwrap()
        .sub_agents
    {
        child.tokens = match child.id.as_str() {
            "child-1" => Some(crate::store::snapshot::SubAgentTokens::Window(12_400)),
            "child-3" => Some(crate::store::snapshot::SubAgentTokens::Total(3_100)),
            _ => None,
        };
    }
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: Some(0),
            ..Default::default()
        },
        54,
        27,
    );

    // Anchor each lookup to the child's own metadata line — the parent's
    // identity line also reads `Opus 4.8`.
    let char_col = |line_needle: &str, col_needle: &str| {
        let line = rendered
            .lines()
            .find(|line| line.contains(line_needle))
            .unwrap_or_else(|| panic!("{line_needle:?} missing:\n{rendered}"));
        line[..line.find(col_needle).unwrap()].chars().count()
    };
    assert_eq!(
        char_col("▤ 12k", "Opus 4.8"),
        char_col("Haiku 4.5", "Haiku 4.5"),
        "the token-less child's model starts under its sibling's:\n{rendered}"
    );
    assert_eq!(char_col("▤ 12k", "▤"), char_col("◇  3k", "◇"));
    assert_eq!(char_col("▤ 12k", "k"), char_col("◇  3k", "k"));
    assert_eq!(
        char_col("▤ 12k", "Opus 4.8"),
        char_col("◇  3k", "Sonnet 4.6")
    );
    assert!(
        !rendered.contains("· Haiku 4.5"),
        "a blank-filled token slot carries no orphan seam:\n{rendered}"
    );
    let quiet_line = rendered
        .lines()
        .find(|line| line.contains("Haiku 4.5"))
        .expect("the token-less child still renders its metadata line");
    assert!(
        !quiet_line.contains('◇'),
        "no bare `◇` over a blank figure:\n{rendered}"
    );
}

#[test]
fn codex_subagent_renders_nickname_nested_path_and_current_context() {
    let parent = agent(
        "codex-root",
        "codex",
        AgentStatus::Success,
        Some("/repo/main"),
        Some("main"),
        Some("ship hooks"),
    );
    let mut child = agent(
        "codex-child",
        "codex",
        AgentStatus::Running,
        None,
        None,
        Some("research/explore_hooks"),
    );
    child.parent_agent_id = Some("codex-root".into());
    child.name = Some("Atlas".to_owned());
    child.name_explicit = true;
    child.usage.fresh_input_tokens = Some(32_100);
    child.model = Some("gpt-5.5-codex".to_owned());
    child.effort = Some("xhigh".to_owned());

    let snapshot = snapshot_with(vec![parent, child]);
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: Some(0),
            ..Default::default()
        },
        60,
        20,
    );

    assert!(
        rendered.contains("Atlas · research/explore_hooks"),
        "nickname and flat nested path stay distinct:\n{rendered}"
    );
    assert!(
        rendered.contains("▤ 32k"),
        "Codex's current context reading is rendered:\n{rendered}"
    );
}

#[test]
fn subagent_window_glyph_heats_like_the_parent_context() {
    let parent = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    let tokens = [("child-1", 12_400), ("child-2", 400_000)];
    let children = tokens.map(|(id, _)| {
        let mut child = agent(
            id,
            "claude",
            AgentStatus::Running,
            None,
            None,
            Some("Explore"),
        );
        child.parent_agent_id = Some("claude-1".into());
        child
    });
    let mut snapshot = snapshot_with([vec![parent], children.to_vec()].concat());
    for child in &mut snapshot.worktree_groups[0].rows[0]
        .as_agent_mut()
        .unwrap()
        .sub_agents
    {
        let window = tokens
            .iter()
            .find(|(id, _)| *id == child.id)
            .map(|(_, window)| *window);
        child.tokens = window.map(crate::store::snapshot::SubAgentTokens::Window);
    }
    let theme = Theme::fixed(false);
    let lines = group_lines_at_width(&snapshot, &theme, 0, 54);
    let glyph_style = |figure: &str| {
        let line = lines
            .iter()
            .find(|line| line.spans.iter().any(|span| span.content.contains(figure)))
            .unwrap_or_else(|| panic!("{figure} missing"));
        line.spans
            .iter()
            .find(|span| span.content == theme.glyph(GlyphRole::TokensFilled))
            .expect("window glyph")
            .style
            .fg
    };
    assert_eq!(glyph_style("12k"), Some(theme.heat_tone(0.0)));
    assert_eq!(glyph_style("400k"), Some(theme.heat_tone(1.0)));

    for (window, percent) in [(Some(200_000), 95), (None, 0), (Some(0), 0)] {
        for child in &mut snapshot.worktree_groups[0].rows[0]
            .as_agent_mut()
            .unwrap()
            .sub_agents
        {
            child.tokens = Some(crate::store::snapshot::SubAgentTokens::Window(190_000));
            child.context_window = window;
        }
        let lines = group_lines_at_width(&snapshot, &theme, 0, 54);
        let glyph = lines
            .iter()
            .find(|line| line.spans.iter().any(|span| span.content.contains("190k")))
            .expect("child token line")
            .spans
            .iter()
            .find(|span| span.content == theme.glyph(GlyphRole::TokensFilled))
            .expect("child window glyph");
        assert_eq!(
            glyph.style.fg,
            Some(crate::sidebar_pane::render::labels::severity_heat_color(
                &theme,
                crate::agents::ContextSeverity::Calm,
                percent,
                Some(190_000),
                &Default::default(),
            )),
            "reported window {window:?}"
        );
    }
}
