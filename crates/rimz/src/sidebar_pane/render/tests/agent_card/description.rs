use super::*;
use crate::sidebar_pane::render::theme::Component;
use ratatui::text::Span;

#[test]
fn sleeping_card_describes_its_wake_using_the_snapshot_clock() {
    let mut sleeper = agent(
        "claude-1",
        "claude",
        AgentStatus::Success,
        Some("/repo/main"),
        Some("main"),
        Some("finished work"),
    );
    sleeper.pending_wakes.push(crate::agents::PendingWake {
        name: "wake-test".to_owned(),
        trigger: crate::agents::PendingWakeTrigger::Timer {
            due: fixed_now() + jiff::SignedDuration::from_mins(12),
        },
        armed_at: Some(fixed_now()),
    });
    let mut snapshot = snapshot_with(vec![sleeper]);
    let screen = snapshot_to_screen(&snapshot, 44, 20);
    assert!(screen.contains(Theme::fixed(false).glyph(GlyphRole::StatusSleeping)));
    assert!(screen.contains("wake in 12m"));
    assert!(screen.contains("⧖ waits (1)"));
    assert!(!screen.contains("finished work"));
    assert_snapshot("sleeping_card", screen);

    let theme = Theme::fixed(false);
    let lines = group_lines(&snapshot, &theme, 0);
    let style = span_for(&lines, "wake in 12m").style;
    assert_eq!(style.fg, theme.body().fg);
    assert!(style.add_modifier.contains(Modifier::ITALIC));

    snapshot.now += jiff::SignedDuration::from_mins(12);
    assert!(snapshot_to_screen(&snapshot, 44, 20).contains("wake due"));

    snapshot.worktree_groups[0].rows[0]
        .as_agent_mut()
        .unwrap()
        .turn_error_label = Some("api error".to_owned());
    let screen = snapshot_to_screen(&snapshot, 44, 20);
    assert!(screen.contains("api error"));
    assert!(!screen.contains("wake due"));

    for status in [
        AgentStatus::Running,
        AgentStatus::Waiting,
        AgentStatus::Failed,
    ] {
        let card = snapshot.worktree_groups[0].rows[0].as_agent_mut().unwrap();
        card.turn_error_label = None;
        card.status = status;
        let screen = snapshot_to_screen(&snapshot, 44, 20);
        assert!(screen.contains("finished work"), "{status:?}: {screen}");
        assert!(!screen.contains("wake due"), "{status:?}: {screen}");
        assert!(screen.contains("⧖ waits (1)"), "{status:?}: {screen}");
    }
}

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
        crate::agents::PendingWake {
            name: "timer".to_owned(),
            trigger: crate::agents::PendingWakeTrigger::Timer {
                due: fixed_now() + jiff::SignedDuration::from_mins(12),
            },
            armed_at: Some(fixed_now()),
        },
        crate::agents::PendingWake {
            name: "command".to_owned(),
            trigger: crate::agents::PendingWakeTrigger::Command {
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
    child.usage.total_tokens = Some(12_400);
    let mut snapshot = snapshot_with(vec![parent, child]);
    let theme = Theme::fixed(false);
    let screen = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: usize::MAX,
            ..Default::default()
        },
        54,
        23,
    );
    let rows: Vec<_> = screen.lines().collect();
    let stats = rows
        .iter()
        .position(|line| line.contains("⧉ subagents (1)"))
        .unwrap();
    assert!(rows[stats + 1].contains("⧖ waits (2)"));
    assert_snapshot("pending_wakes_line", screen);

    let lines = group_lines(&snapshot, &theme, 0);
    assert_eq!(
        span_for(&lines, "  ⧖").style.fg,
        Some(theme.component(Component::WakeHeader))
    );
    assert_eq!(span_for(&lines, " waits (2)").style.fg, theme.body().fg);
    let rows = line_texts(&lines);
    let stats = rows
        .iter()
        .position(|line| line.contains("⧉ subagents (1)"))
        .unwrap();
    assert!(rows[stats + 1].contains("inspect the renderer"));
    assert!(rows[stats + 2].contains("12k"));
    assert!(rows[stats + 3].contains("⧖ waits (2)"));

    snapshot.worktree_groups[0].rows[0]
        .as_agent_mut()
        .unwrap()
        .pending_wakes
        .clear();
    assert!(!snapshot_to_screen(&snapshot, 54, 23).contains("waits ("));
}

#[test]
fn line_one_prefers_session_name_over_task() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    claude.context = Some(claude_context(fixed_now()));
    let snapshot = snapshot_with(vec![claude]);
    let rendered = snapshot_to_screen(&snapshot, 44, 15);

    assert!(rendered.contains("store refactor"));
    assert!(!rendered.contains("db migrate"));
}
/// An unnamed session whose turn has ended (the activity-bound `task` cleared)
/// keeps its latest prompt on line two instead of falling to an em dash, until
/// a real session name exists.
#[test]
fn line_two_falls_back_to_the_latest_prompt_when_unnamed() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        None, // idle cleared the task; no session name (no context)
    );
    claude.prompt = Some("wire the relay".to_owned());
    let snapshot = snapshot_with(vec![claude]);
    let rendered = snapshot_to_screen(&snapshot, 44, 15);

    assert!(rendered.contains("wire the relay"));
    assert!(
        !rendered.contains('—'),
        "the prompt stands in for the em dash"
    );
}

#[test]
fn line_two_uses_launch_description_before_task_and_prompt() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    claude.description = Some("port auth".to_owned());
    claude.prompt = Some("wire the relay".to_owned());
    let rendered = snapshot_to_screen(&snapshot_with(vec![claude]), 44, 15);

    assert!(rendered.contains("port auth"));
    assert!(!rendered.contains("db migrate"));
    assert!(!rendered.contains("wire the relay"));
}

#[test]
fn line_two_rich_context_replaces_launch_description() {
    let mut preview = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    preview.description = Some("port auth".to_owned());
    let mut preview_context = claude_context(fixed_now());
    preview_context.session_preview = Some("thread preview".to_owned());
    preview_context.session_name = Some("thread name".to_owned());
    preview.context = Some(preview_context);
    let rendered = snapshot_to_screen(&snapshot_with(vec![preview]), 44, 15);

    assert!(rendered.contains("thread name"));
    assert!(!rendered.contains("thread preview"));
    assert!(!rendered.contains("port auth"));

    let mut named = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    named.description = Some("port auth".to_owned());
    let mut named_context = claude_context(fixed_now());
    named_context.session_preview = Some("thread preview".to_owned());
    named_context.session_name = None;
    named.context = Some(named_context);
    let rendered = snapshot_to_screen(&snapshot_with(vec![named]), 44, 15);

    assert!(rendered.contains("thread preview"));
    assert!(!rendered.contains("port auth"));
}

#[test]
fn pre_prompt_rich_context_cannot_reshape_a_fresh_card() {
    let mut kimi = agent(
        "kimi-1",
        "kimi",
        AgentStatus::Idle,
        Some("/repo/main"),
        Some("main"),
        None,
    );
    let mut context = codex_context(fixed_now());
    context.source = "kimi".to_owned();
    context.session_preview = Some("New Session".to_owned());
    context.session_name = Some("Automatic title".to_owned());
    context.model_id = None;
    context.model_display_name = None;
    context.effort = None;
    context.rate_limits = None;
    kimi.context = Some(context);
    let snapshot = snapshot_with(vec![kimi]);
    let theme = Theme::fixed(true);

    let unselected = line_texts(&group_lines(&snapshot, &theme, usize::MAX))
        .into_iter()
        .skip(1)
        .collect::<Vec<_>>();
    assert_eq!(unselected.len(), 1, "{}", unselected.join("\n"));
    assert!(!unselected.iter().any(|line| line.contains("New Session")));

    let selected = line_texts(&group_lines(&snapshot, &theme, 0))
        .into_iter()
        .skip(1)
        .collect::<Vec<_>>();
    assert_eq!(selected.len(), 3, "{}", selected.join("\n"));
    assert!(selected[1].contains(".  "), "{selected:?}");
    assert!(
        selected[2].contains('▢') && selected[2].contains("0%"),
        "{selected:?}"
    );
    assert!(!selected.iter().any(|line| line.contains("New Session")));
}

#[test]
fn line_two_rejects_skill_blocks_at_renderer_backstop() {
    let codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some(
            "<skill name=\"merge\" Location=\"/home/u/.agents/skills/merge/SKILL.md\">body</skill>",
        ),
    );
    let rendered = snapshot_to_screen(&snapshot_with(vec![codex]), 44, 15);

    assert!(!rendered.contains("<skill"));
    assert!(
        rendered.contains("—"),
        "the rejected control block falls through to the empty description:\n{rendered}"
    );
}

#[test]
fn line_two_control_characters_collapse_before_framing() {
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    let mut context = codex_context(fixed_now());
    context.session_preview = Some("ship\nwide\tlabel\rnow\u{0007}".to_owned());
    codex.context = Some(context);
    let rendered = snapshot_to_screen(&snapshot_with(vec![codex]), 44, 15);
    let line = rendered
        .lines()
        .find(|line| line.contains("ship wide label now"))
        .unwrap_or_else(|| panic!("single-line description rendered:\n{rendered}"));

    assert_eq!(
        line.chars().nth(43),
        Some('▐'),
        "the selected card's right rail stays in the final column:\n{rendered}"
    );
    for leaked in ['\n', '\r', '\t', '\u{0007}'] {
        assert!(
            !line.contains(leaked),
            "description line contains no control character {leaked:?}: {line:?}"
        );
    }
}

fn rendered_group_lines_with(
    snapshot: &SidebarSnapshot,
    theme: &Theme,
    phase: u64,
) -> Vec<Line<'static>> {
    let cost_rolls = CostRolls::default();
    let ctx = test_row_ctx(snapshot, theme, 54, 0, phase, &cost_rolls);
    worktree_group_block(&ctx, &snapshot.worktree_groups[0], false, None).lines
}

/// A theme config pinning the unread effect to `blink`, so a test reads one
/// whole-word definition span and the 2-pole weight toggle rather than the
/// default per-character shimmer.
fn blink_theme() -> crate::config::ThemeConfig {
    let mut theme = crate::config::ThemeConfig::default();
    theme.animations.unread = Some(crate::config::UnreadEffect::Blink);
    theme
}

fn rendered_group_lines_blink_no_color(
    snapshot: &SidebarSnapshot,
    phase: u64,
) -> Vec<Line<'static>> {
    rendered_group_lines_with(
        snapshot,
        &Theme::fixed_for_theme(true, &blink_theme()),
        phase,
    )
}

fn span_for<'a>(lines: &'a [Line<'static>], text: &str) -> &'a Span<'static> {
    lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.as_ref() == text)
        .unwrap_or_else(|| panic!("span {text:?} present"))
}

#[test]
fn parked_background_marker_falls_back_to_unicode() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("done"),
    );
    claude.phase = crate::agents::TurnPhase::Parked;
    let snapshot = snapshot_with(vec![claude]);
    let theme = Theme::fixed_for_theme(
        true,
        &crate::config::ThemeConfig {
            glyphs: crate::config::ThemeGlyphsConfig {
                set: Some("nerd_font".to_owned()),
                ..crate::config::ThemeGlyphsConfig::default()
            },
            ..crate::config::ThemeConfig::default()
        },
    );
    let rendered = rendered_group_lines_with(&snapshot, &theme, 0)
        .into_iter()
        .map(|line| {
            line.spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");

    // `card.parked_bg` sits outside the curated Nerd Font overlay, so the parked
    // definition keeps its Unicode ellipsis even while the set is active. The
    // stage-fixed placeholder meter uses the overlay's empty-context tile.
    assert!(rendered.contains("\u{f11d9}"), "{rendered}");
    assert!(rendered.contains("⋯ bg"), "{rendered}");
}

#[test]
fn parked_projection_renders_success_with_background_marker() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("done"),
    );
    claude.phase = crate::agents::TurnPhase::Parked;
    let rendered = snapshot_to_screen(&snapshot_with(vec![claude]), 44, 15);

    assert!(rendered.contains('✓'), "{rendered}");
    assert!(rendered.contains("⋯ bg"), "{rendered}");
}

#[test]
fn unread_descriptor_grows_bold_without_dimming() {
    // A single actionable row leads, so it carries the 2-pole blink the lead row
    // keeps under `blink`; a non-lead unread row would settle to a steady crest.
    let agent = agent(
        "claude-1",
        "claude",
        AgentStatus::Failed,
        Some("/repo/main"),
        Some("main"),
        Some("done"),
    );
    let mut unread = snapshot_with(vec![agent.clone()]);
    unread.worktree_groups[0].rows[0].unread = true;
    // Under NO_COLOR the blink unread definition shares the lead glyph/name
    // 2-pole toggle through a grow-only weight: plain on the off-pole, bold on
    // the on-pole, never dim.
    let unread_mods: Vec<_> = (0..32)
        .map(|phase| {
            span_for(&rendered_group_lines_blink_no_color(&unread, phase), "done")
                .style
                .add_modifier
        })
        .collect();
    assert!(unread_mods.iter().any(|m| m.contains(Modifier::BOLD)));
    assert!(unread_mods.iter().any(|m| m.is_empty()));
    assert!(unread_mods.iter().all(|m| !m.contains(Modifier::DIM)));

    // A read definition never blinks — its weight is the same at every phase.
    let read = snapshot_with(vec![agent]);
    for phase in 0..32 {
        let modifier = span_for(&rendered_group_lines_blink_no_color(&read, phase), "done")
            .style
            .add_modifier;
        assert!(!modifier.contains(Modifier::BOLD));
    }
}

#[test]
fn unread_descriptor_holds_bold_while_colored_pulse_brightens() {
    let mut theme_config = blink_theme();
    theme_config.mode = crate::config::ThemeMode::Truecolor;
    let theme = Theme::fixed_for_theme(false, &theme_config);
    // The lead unread row — the one that needs an answer — carries the continuous
    // pulse; a `failed` row is actionable, so a single one leads.
    let agent = agent(
        "claude-1",
        "claude",
        AgentStatus::Failed,
        Some("/repo/main"),
        Some("main"),
        Some("done"),
    );
    let mut snapshot = snapshot_with(vec![agent]);
    snapshot.worktree_groups[0].rows[0].unread = true;

    let styles: Vec<_> = (0..32)
        .map(|phase| span_for(&rendered_group_lines_with(&snapshot, &theme, phase), "done").style)
        .collect();
    assert!(
        styles
            .iter()
            .all(|style| style.add_modifier == Modifier::BOLD),
        "colored unread descriptors hold bold through the whole pulse"
    );
    assert!(
        styles.iter().any(|style| style.fg != styles[0].fg),
        "the colored pulse changes lightness phase to phase"
    );
    assert!(
        styles
            .iter()
            .all(|style| !style.add_modifier.contains(Modifier::DIM)),
        "the grow-only colored pulse never dims"
    );
}

#[test]
fn unread_turn_error_label_pulses_and_stays_italic() {
    let agent = agent(
        "claude-1",
        "claude",
        AgentStatus::Failed,
        Some("/repo/main"),
        Some("main"),
        Some("done"),
    );
    let mut snapshot = snapshot_with(vec![agent]);
    let row = &mut snapshot.worktree_groups[0].rows[0];
    row.unread = true;
    row.as_agent_mut().unwrap().turn_error_label = Some("api error".to_owned());

    let mods: Vec<_> = (0..32)
        .map(|phase| {
            span_for(
                &rendered_group_lines_blink_no_color(&snapshot, phase),
                "api error",
            )
            .style
            .add_modifier
        })
        .collect();
    assert!(
        mods.iter().any(|m| m.contains(Modifier::BOLD)),
        "the unread error label blinks bold throughout the colored pulse"
    );
    assert!(
        mods.iter().all(|m| m.contains(Modifier::ITALIC)),
        "the error-label branch keeps the soft italic style throughout"
    );
}
