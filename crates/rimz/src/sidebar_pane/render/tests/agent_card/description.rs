use super::*;
use ratatui::text::Span;

fn waiting_asker() -> AgentState {
    let mut asker = agent(
        "claude-ask",
        "claude",
        AgentStatus::Waiting,
        Some("/repo/main"),
        Some("main"),
        Some("pricing cache"),
    );
    asker.context = Some(claude_context(fixed_now()));
    asker.turn_started_at = Some(fixed_now() - Duration::from_secs(120));
    asker.waiting_since = Some(fixed_now() - Duration::from_secs(60));
    asker.last_activity = asker.waiting_since.unwrap();
    asker.open_ask = Some(crate::agents::OpenAsk {
        id: "ask_0123456789abcdef".parse().unwrap(),
        kind: crate::agents::AskKind::Question,
        detail: Some("Allow the pricing cache write?".to_owned()),
        native_key: None,
        since: asker.last_activity,
    });
    asker
}

#[test]
fn waiting_question_wraps_naturally_without_its_preamble() {
    for width in [32, 46, 48] {
        let mut asker = waiting_asker();
        asker.open_ask.as_mut().unwrap().detail = Some(
            "The cache is ready. Should I keep the current pricing cache for this run? More details follow.".into(),
        );
        let snapshot = snapshot_with(vec![asker]);
        let theme = Theme::fixed(false);
        let text = line_texts(&group_lines_at_width(&snapshot, &theme, 0, width));
        let shown = text
            .iter()
            .map(|line| line.trim_matches(['▌', '▐', ' ']))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            shown.contains("Should I keep the current pricing cache for this run?"),
            "{text:?}"
        );
        let question = text
            .iter()
            .position(|line| line.contains("Should I"))
            .unwrap();
        assert!(text[question + 1].contains('?'), "{text:?}");
        assert!(text[question + 2].contains('%'), "{text:?}");
        assert!(
            !shown.contains("ready") && !shown.contains("details") && !shown.contains('…'),
            "{text:?}"
        );
        assert_snapshot(
            &format!("waiting_question_{width}"),
            snapshot_to_screen(&snapshot, width as u16, 22),
        );
    }
}

#[test]
fn waiting_description_marks_omitted_lines_and_wide_words() {
    for (name, detail) in [
        (
            "prose",
            "Bash: allow the pricing cache write and refresh every downstream consumer before continuing the build with the latest complete dataset",
        ),
        (
            "word",
            "漢字漢字漢字漢字漢字漢字漢字漢字漢字漢字漢字漢字漢字漢字漢字漢字漢字漢字 done",
        ),
    ] {
        let mut asker = waiting_asker();
        asker.open_ask.as_mut().unwrap().detail = Some(detail.into());
        let snapshot = snapshot_with(vec![asker]);
        let theme = Theme::fixed(false);
        let text = line_texts(&group_lines_at_width(&snapshot, &theme, 0, 32));
        let description = text
            .iter()
            .position(|line| line.contains(if name == "prose" { "Bash:" } else { "漢字" }))
            .unwrap();
        assert!(
            text[description + 1]
                .trim_end_matches(['▐', ' '])
                .ends_with('…'),
            "{text:?}"
        );
        assert_snapshot(
            &format!("waiting_truncated_{name}"),
            snapshot_to_screen(&snapshot, 32, 22),
        );
    }
}

#[test]
fn waiting_card_description_prefers_open_ask_to_session_name() {
    let snapshot = snapshot_with(vec![waiting_asker()]);
    assert_eq!(
        snapshot.rows().next().unwrap().status(),
        Some(AgentStatus::Waiting)
    );
    let theme = Theme::fixed(false);
    let lines = group_lines(&snapshot, &theme, 0);
    let text = line_texts(&lines);
    let identity = text
        .iter()
        .position(|line| line.contains("? claude"))
        .unwrap();
    assert!(text[identity + 1].contains("Allow the pricing cache write?"));
    assert!(!text.iter().any(|line| line.contains("store refactor")));
    assert!(
        !span_for(&lines, "Allow the pricing cache write?")
            .style
            .add_modifier
            .contains(Modifier::ITALIC)
    );

    let mut multiline = waiting_asker();
    multiline.open_ask.as_mut().unwrap().detail = Some("Which approach?\n1. merge".to_owned());
    let snapshot = snapshot_with(vec![multiline]);
    let text = line_texts(&group_lines(&snapshot, &theme, 0));
    assert!(
        text.iter().any(|line| line.contains("Which approach?")),
        "{text:?}"
    );
    assert!(
        !text.iter().any(|line| line.contains("1. merge")),
        "{text:?}"
    );
    let question = text
        .iter()
        .position(|line| line.contains("Which approach?"))
        .unwrap();
    assert!(text[question + 1].contains('%'), "{text:?}");
    assert_snapshot(
        "waiting_short_question",
        snapshot_to_screen(&snapshot, 32, 22),
    );
}

#[test]
fn stale_waiting_card_description_returns_to_session_name() {
    let mut asker = waiting_asker();
    asker.last_activity = fixed_now() - Duration::from_secs(30);
    assert!(!asker.is_awaiting_input());
    let snapshot = snapshot_with(vec![asker]);
    let rendered = snapshot_to_screen(&snapshot, 54, 17);
    assert!(rendered.contains("store refactor"), "{rendered}");
    assert!(
        !rendered.contains("Allow the pricing cache write?"),
        "{rendered}"
    );
}

#[test]
fn failed_card_description_keeps_error_over_stale_ask() {
    let mut asker = waiting_asker();
    asker.status = AgentStatus::Failed;
    asker.context.as_mut().unwrap().turn_error = Some(AgentTurnError {
        class: TurnErrorClass::Failed,
        at: fixed_now() - Duration::from_secs(10),
        label: Some("API Error: Bad request".to_owned()),
    });
    let snapshot = snapshot_with(vec![asker]);
    assert_eq!(
        snapshot.rows().next().unwrap().status(),
        Some(AgentStatus::Failed)
    );
    let rendered = snapshot_to_screen(&snapshot, 54, 17);
    assert!(rendered.contains("API Error: Bad request"), "{rendered}");
    assert!(
        !rendered.contains("Allow the pricing cache write?"),
        "{rendered}"
    );
}

#[test]
fn render_paused_turn_card_quotes_provider_reason_in_italics() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    claude.last_activity = fixed_now() - Duration::from_secs(60);
    let mut context = claude_context(fixed_now());
    context.turn_error = Some(AgentTurnError {
        class: TurnErrorClass::PausedOverloaded,
        at: fixed_now() - Duration::from_secs(10),
        label: Some("API Error: Overloaded".to_owned()),
    });
    claude.context = Some(context);
    let snapshot = snapshot_with(vec![claude]);
    let rendered = snapshot_to_screen(&snapshot, 54, 17);
    assert!(rendered.contains("API Error: Overloaded"), "{rendered}");
    assert!(!rendered.contains("store refactor"), "{rendered}");
    let theme = Theme::fixed(false);
    let lines = group_lines(&snapshot, &theme, 0);
    assert!(
        span_for(&lines, "API Error: Overloaded")
            .style
            .add_modifier
            .contains(Modifier::ITALIC)
    );
    assert_snapshot("paused_turn_card", rendered);
}

#[test]
fn sleeping_card_describes_its_wait_using_the_snapshot_clock() {
    let mut sleeper = agent(
        "claude-1",
        "claude",
        AgentStatus::Success,
        Some("/repo/main"),
        Some("main"),
        Some("finished work"),
    );
    sleeper.pending_waits.push(crate::agents::PendingWait {
        class: crate::agents::PendingWaitClass::Sleeping,
        name: "wait-test".to_owned(),
        trigger: crate::agents::PendingWaitTrigger::Timer {
            due: fixed_now() + jiff::SignedDuration::from_mins(12),
            delay: Some("30m".to_owned()),
        },
        armed_at: Some(fixed_now()),
    });
    let mut snapshot = snapshot_with(vec![sleeper]);
    let screen = snapshot_to_screen(&snapshot, 44, 20);
    assert!(screen.contains(Theme::fixed(false).glyph(GlyphRole::StatusSleeping)));
    assert!(screen.contains("wakes in 12m"));
    assert!(screen.contains("⧖ 1"));
    assert!(!screen.contains("finished work"));
    assert_snapshot("sleeping_card", screen);

    let theme = Theme::fixed(false);
    let lines = group_lines(&snapshot, &theme, 0);
    let style = span_for(&lines, "wakes in 12m").style;
    assert_eq!(style.fg, theme.body().fg);
    assert!(style.add_modifier.contains(Modifier::ITALIC));

    snapshot.now += jiff::SignedDuration::from_mins(12);
    assert!(snapshot_to_screen(&snapshot, 44, 20).contains("wakes now"));

    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0]
    .as_agent_mut()
    .unwrap()
    .pending_waits[0]
        .trigger = crate::agents::PendingWaitTrigger::Signal {
        selector: "pr.merged".to_owned(),
    };
    let lines = group_lines(&snapshot, &theme, 0);
    assert!(
        line_texts(&lines)
            .iter()
            .any(|line| line.contains("wakes on pr.merged"))
    );
    let headline = span_for(&lines, "wakes on pr.merged");
    assert_eq!(headline.style.fg, theme.body().fg);
    assert!(headline.style.add_modifier.contains(Modifier::ITALIC));

    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0]
    .as_agent_mut()
    .unwrap()
    .turn_error_label = Some("api error".to_owned());
    let screen = snapshot_to_screen(&snapshot, 44, 20);
    assert!(screen.contains("api error"));
    assert!(!screen.contains("wakes"));

    for status in [
        AgentStatus::Running,
        AgentStatus::Waiting,
        AgentStatus::Failed,
    ] {
        let card = std::sync::Arc::make_mut(
            &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
        )[0]
        .as_agent_mut()
        .unwrap();
        card.turn_error_label = None;
        card.status = status;
        let screen = snapshot_to_screen(&snapshot, 44, 20);
        assert!(screen.contains("finished work"), "{status:?}: {screen}");
        assert!(!screen.contains("wakes"), "{status:?}: {screen}");
        assert!(screen.contains("⧖ 1"), "{status:?}: {screen}");
    }
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
    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut unread.worktree_groups)[0].rows)
        [0]
    .unread = true;
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
    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0]
    .unread = true;

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
    let row = &mut std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0];
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
