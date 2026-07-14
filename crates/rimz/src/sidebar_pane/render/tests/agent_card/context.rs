use super::*;
use crate::sidebar_pane::pixel::meter::MeterRaster;
use crate::sidebar_pane::render::theme::Component;
use ratatui::text::Span;

#[test]
fn agent_context_line_renders_compaction_markers() {
    for (count, expected) in [
        (0, None),
        (1, Some("▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k · ↻ 1")),
        (2, Some("▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k · ↻ 2")),
    ] {
        let mut claude = agent(
            "claude-1",
            "claude",
            AgentStatus::Running,
            Some("/repo/main"),
            Some("main"),
            Some("db migrate"),
        );
        claude.context = Some(claude_context(fixed_now()));
        claude.compaction_count = count;
        let snapshot = snapshot_with(vec![claude]);
        let rendered = snapshot_to_screen(&snapshot, 56, 17);

        if let Some(expected) = expected {
            assert!(
                rendered.contains(expected),
                "compaction {count} trails the context composition:\n{rendered}"
            );
        } else {
            assert!(
                !rendered.contains('↻'),
                "an uncompacted session shows no compaction marker:\n{rendered}"
            );
        }
    }
}
#[test]
fn render_agent_card_context_line_pins_age_not_resource_stats() {
    // Resource stats are process-row vocabulary: even when the agent's
    // stamped pane carries a full `/proc` sample, none of it reaches the
    // card — the context line keeps the age clock as its one right pin.
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    claude.context = Some(claude_context(fixed_now()));
    claude.last_activity = fixed_now() - Duration::from_secs(7 * 60);
    let stamped = pane("%1", "claude", "/repo/main");
    claude.pane = Some(stamped.clone());
    let live = stamped;
    let snapshot = snapshot_with(vec![claude]).with_live_panes(vec![live], None);
    let row = &snapshot.worktree_groups[0].rows[0];
    assert!(row.as_process().is_none());

    let rendered = snapshot_to_screen(&snapshot, 56, 17);

    assert!(
        rendered.contains("▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k"),
        "the token breakdown keeps the line's left side:\n{rendered}"
    );
    assert!(
        !rendered.contains("C 11%"),
        "the pane's resource stats stay off the card:\n{rendered}"
    );
    assert!(
        rendered.contains("◔ 7m"),
        "the age clock keeps the right pin once a five-minute gap opens:\n{rendered}"
    );
    assert_snapshot("agent_card_context_age", rendered);
}
#[test]
fn codex_line_two_walks_the_descriptor_precedence_ladder() {
    // Codex's line-two descriptor follows a precedence ladder: thread preview >
    // thread name > task. A present preview wins over both name and task; with
    // the preview absent the name still beats the task fall-through.
    let codex_with = |session_name: &str, session_preview: Option<&str>| {
        let mut codex = agent(
            "codex-1",
            "codex",
            AgentStatus::Running,
            Some("/repo/main"),
            Some("main"),
            Some("db migrate"),
        );
        let mut context = codex_context(fixed_now());
        context.session_name = Some(session_name.to_owned());
        context.session_preview = session_preview.map(str::to_owned);
        codex.context = Some(context);
        let snapshot = snapshot_with(vec![codex]);
        snapshot_to_screen(&snapshot, 44, 15)
    };

    // Preview present: it wins over the thread name and the task.
    let rendered = codex_with("TUI prototype", Some("Create a TUI"));
    assert!(rendered.contains("Create a TUI"));
    assert!(!rendered.contains("TUI prototype"));
    assert!(!rendered.contains("db migrate"));

    // Preview absent: the thread name beats the task fall-through.
    let rendered = codex_with("TUI prototype", None);
    assert!(rendered.contains("TUI prototype"));
    assert!(!rendered.contains("db migrate"));
}
#[test]
fn selected_agent_without_context_keeps_bare_token_total() {
    // An agent with no context sidecar yet (a Codex session before its first
    // app-server refresh, or any agent that publishes none) degrades to the
    // bare ▤ rollup total standing in for the filled window — no cost, no
    // usage windows.
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("add tests"),
    );
    codex.model = Some("GPT-5.5".to_owned());
    codex.total_tokens = Some(5_000);
    assert!(codex.context.is_none());
    let snapshot = snapshot_with(vec![codex]);
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: 0,
            help_visible: false,
            animation_phase: 0,
            line_map: Vec::new(),
            ..Default::default()
        },
        44,
        17,
    );

    let token_line = rendered
        .lines()
        .find(|line| line.contains("▤ 5k"))
        .unwrap_or_else(|| panic!("bare token total rendered:\n{rendered}"));
    // No split fields, so no composition columns trail the bare total.
    for marker in ['◌', '◍', '↘', '↗'] {
        assert!(
            !token_line.contains(marker),
            "a splitless row keeps the bare total alone:\n{rendered}"
        );
    }
    assert!(!token_line.contains('↻'));
    assert!(!token_line.contains('$'));
}
#[test]
fn codex_card_renders_the_per_call_composition() {
    // A Codex row carries the per-call split from its rollout's
    // `last_token_usage` on the lifecycle rail — no rich context blob. The
    // context line legends it: `▤` the window numerator (cache reads + fresh
    // input, exactly what the `▣` percent scales — not the call total, which
    // includes output), then `◌`/`↘`/`↗`. No `◍` column: the protocol reports
    // no per-call cache-write, so it drops rather than reading a false zero.
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("add tests"),
    );
    codex.model = Some("GPT-5.5".to_owned());
    codex.context_pct = Some(50);
    codex.context_window = Some(258_400);
    codex.total_tokens = Some(130_000);
    codex.cache_read_input_tokens = Some(120_000);
    codex.fresh_input_tokens = Some(9_200);
    codex.output_tokens = Some(800);
    assert!(codex.context.is_none());
    let snapshot = snapshot_with(vec![codex]);
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: 0,
            help_visible: false,
            animation_phase: 0,
            line_map: Vec::new(),
            ..Default::default()
        },
        44,
        17,
    );

    assert!(
        rendered.contains("▤ 129k · ◌ 120k ↘ 9k ↗ 800"),
        "the context line legends the split:\n{rendered}"
    );
    assert!(
        !rendered.contains('◍'),
        "no per-call cache-write exists, so the column drops:\n{rendered}"
    );
    assert_snapshot("codex_card_context_composition", rendered);
}

#[test]
fn codex_card_fills_bar_from_rich_context_usage_without_reported_percentage() {
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("add tests"),
    );
    codex.model = Some("GPT-5.5".to_owned());
    // This matches Codex app-server context: the sidecar carries current usage
    // and its window, while `used_percentage` is absent. A stale rollout scalar
    // of 0 must not make the bar look empty when the token line is filled.
    codex.context_pct = Some(0);
    let mut context = codex_context(fixed_now());
    context.tokens = Some(AgentTokenUsage {
        context_window_size: Some(258_400),
        used_percentage: None,
        remaining_percentage: None,
        current_usage: Some(AgentCurrentUsage {
            input_tokens: Some(6_700),
            output_tokens: Some(825),
            cache_creation_input_tokens: None,
            cache_read_input_tokens: Some(56_900),
        }),
    });
    codex.context = Some(context);
    let snapshot = snapshot_with(vec![codex]);
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: 0,
            help_visible: false,
            animation_phase: 0,
            line_map: Vec::new(),
            ..Default::default()
        },
        44,
        17,
    );

    let meter = rendered
        .lines()
        .find(|line| line.contains("24.6%"))
        .unwrap_or_else(|| panic!("the context meter shows the precise fill:\n{rendered}"));
    assert!(
        meter.contains('━'),
        "the precise 24.6% fill overrides the stale integer 0% scalar:\n{meter}"
    );
    assert!(
        rendered.contains("▤ 63k · ◌ 56k ↘ 6k ↗ 825"),
        "the token line and bar read from the same rich usage:\n{rendered}"
    );
}

#[test]
fn copilot_token_only_context_shows_model_and_composition_without_a_fake_gauge() {
    let mut copilot = agent(
        "copilot-1",
        "copilot",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("review telemetry"),
    );
    copilot.model = Some("auto".to_owned());
    copilot.context_pct = Some(0);
    let mut context = codex_context(fixed_now());
    context.source = "copilot".to_owned();
    context.model_id = Some("gpt-5-mini".to_owned());
    context.model_display_name = None;
    context.effort = None;
    context.agent_version = None;
    context.rate_limits = None;
    context.tokens = Some(AgentTokenUsage {
        current_usage: Some(AgentCurrentUsage {
            input_tokens: Some(10),
            output_tokens: Some(3),
            cache_creation_input_tokens: None,
            cache_read_input_tokens: Some(80),
        }),
        ..AgentTokenUsage::default()
    });
    copilot.context = Some(context);
    let snapshot = snapshot_with(vec![copilot.clone()]);
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: 0,
            ..Default::default()
        },
        48,
        17,
    );

    assert!(
        rendered.contains("GPT 5 Mini"),
        "resolved model wins:\n{rendered}"
    );
    assert!(
        !rendered.contains("Auto"),
        "lifecycle fallback stays hidden:\n{rendered}"
    );
    assert!(
        rendered.contains("▤ 90 · ◌ 80 ↘ 10 ↗ 3"),
        "the latest call remains visible:\n{rendered}"
    );
    assert!(
        !rendered
            .lines()
            .any(|line| line.contains("▌  ▣") || line.contains("▌  ▢")),
        "no denominator means no gauge:\n{rendered}"
    );
    assert!(
        rendered
            .lines()
            .filter(|line| line.contains('▌'))
            .all(|line| !line.contains('$')),
        "the Copilot card carries no dollars:\n{rendered}"
    );

    copilot.status = AgentStatus::Idle;
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot_with(vec![copilot.clone()]),
        None,
        &UiState {
            selected_index: 0,
            ..Default::default()
        },
        48,
        17,
    );
    assert!(rendered.contains("GPT 5 Mini"));
    assert!(rendered.contains("▤ 90 · ◌ 80 ↘ 10 ↗ 3"));
    assert!(
        !rendered
            .lines()
            .any(|line| line.contains("▌  ▣") || line.contains("▌  ▢")),
        "selected idle token-only cards still have no gauge:\n{rendered}",
    );

    let selected = agent(
        "other",
        "claude",
        AgentStatus::Idle,
        Some("/repo/other"),
        Some("other"),
        Some("selected"),
    );
    let mut compact = snapshot_with(vec![copilot, selected]);
    compact.theme.display.card_density = crate::config::CardDensityMode::Compact;
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &compact,
        None,
        &UiState {
            selected_index: 1,
            ..Default::default()
        },
        48,
        17,
    );
    assert!(rendered.contains("GPT 5 Mini"));
    assert!(
        !rendered.contains("▤ 90"),
        "unselected idle compact cards retain compact density:\n{rendered}"
    );
}

#[test]
fn truecolor_context_bar_collects_pixel_spec_and_other_themes_fall_back() {
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("add tests"),
    );
    codex.context_pct = Some(50);
    codex.context_window = Some(258_400);
    codex.total_tokens = Some(130_000);
    codex.cache_read_input_tokens = Some(120_000);
    codex.fresh_input_tokens = Some(9_200);
    let snapshot = snapshot_with(vec![codex]);

    let render = |theme: &Theme, pixels: &mut MeterPixels| {
        let mut lines = Vec::new();
        let mut map = Vec::new();
        let mut more_hits = Vec::new();
        let mut row_index = 0;
        worktree_group_lines_with_meter(
            theme,
            &snapshot.worktree_groups[0],
            &snapshot.providers,
            snapshot.now,
            44,
            &snapshot.theme.display.context_meter,
            snapshot.theme.display.card_density,
            None,
            false,
            None,
            &mut row_index,
            0,
            0,
            &CostRolls::default(),
            lead_unread(&snapshot.worktree_groups).map(|(id, _)| id),
            Some(pixels),
            &mut lines,
            &mut map,
            &mut more_hits,
        );
        lines
    };

    let mut pixels = MeterPixels::new(0x120000);
    pixels.begin_frame();
    let lines = render(&truecolor_sidebar_theme(), &mut pixels);
    pixels.observe_visible(&lines);
    let first_id = pixels
        .visible_rasters()
        .map(|(image_id, _)| image_id)
        .next()
        .expect("one visible meter raster");
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.contains('\u{10eeee}'))
    }));

    pixels.begin_frame();
    let earlier_id = pixels
        .intern(MeterRaster::new(3, 0.25, [9, 8, 7], Vec::new(), [4, 5, 6]))
        .expect("an earlier visible meter id");
    let mut repeated = vec![Line::from(Span::styled(
        crate::sidebar_pane::pixel::placeholder_cluster(0, 0),
        Style::default().fg(crate::sidebar_pane::pixel::image_id_color(earlier_id)),
    ))];
    repeated.extend(render(&truecolor_sidebar_theme(), &mut pixels));
    pixels.observe_visible(&repeated);
    assert_eq!(
        pixels
            .visible_rasters()
            .map(|(image_id, _)| image_id)
            .collect::<std::collections::BTreeSet<_>>(),
        std::collections::BTreeSet::from([first_id, earlier_id]),
        "the same raster keeps its id when scrolling changes visible draw order"
    );

    for theme in [Theme::fixed(false), Theme::fixed(true)] {
        let mut pixels = MeterPixels::new(0x120000);
        pixels.begin_frame();
        let lines = render(&theme, &mut pixels);
        pixels.observe_visible(&lines);
        assert!(pixels.visible_rasters().next().is_none());
        assert!(lines.iter().any(|line| line.to_string().contains('━')));
        assert!(
            !lines
                .iter()
                .any(|line| line.to_string().contains('\u{10eeee}'))
        );
    }
}

#[test]
fn pi_card_renders_cache_write_in_the_per_call_composition() {
    let mut pi = agent(
        "pi-1",
        "pi",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("add tests"),
    );
    pi.context_pct = Some(54);
    pi.context_window = Some(258_400);
    pi.total_tokens = Some(140_000);
    pi.cache_read_input_tokens = Some(120_000);
    pi.cache_write_input_tokens = Some(10_000);
    pi.fresh_input_tokens = Some(9_200);
    pi.output_tokens = Some(800);
    let snapshot = snapshot_with(vec![pi]);
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: 0,
            help_visible: false,
            animation_phase: 0,
            line_map: Vec::new(),
            ..Default::default()
        },
        48,
        17,
    );

    assert!(
        rendered.contains("▤ 139k · ◌ 120k ◍ 10k ↘ 9k ↗ 800"),
        "the context line legends the four-way split:\n{rendered}"
    );
}

/// The context bar reads left to right like the context line, segment order
/// driven by the row-level split. Style-level, since text goldens cannot see
/// the segment colors. Two inputs prove the ladder: a Codex two-bucket fill
/// (cache-read then fresh-input — no cache-write segment exists to paint, so the
/// context line stays the bar's legend by construction), and a Claude
/// three-bucket fill where the cache-write accent slots between the cache-read
/// health run and the fresh-input tail.
#[test]
fn calm_context_bar_orders_segments_left_to_right() {
    let theme = Theme::fixed(false);
    let bar_styles_for = |agent: crate::agents::AgentState| {
        let snapshot = snapshot_with(vec![agent]);
        let mut lines = Vec::new();
        let mut map = Vec::new();
        let mut more_hits = Vec::new();
        let mut row_index = 0;
        worktree_group_lines(
            &theme,
            &snapshot.worktree_groups[0],
            &snapshot.providers,
            snapshot.now,
            44,
            &snapshot.theme.display.context_meter,
            snapshot.theme.display.card_density,
            None,
            false,
            None,
            &mut row_index,
            0,
            0,
            &CostRolls::default(),
            lead_unread(&snapshot.worktree_groups).map(|(id, _)| id),
            &mut lines,
            &mut map,
            &mut more_hits,
        );
        // Identify segments by foreground: the selected card's band lays a bg
        // behind every span, orthogonal to which composition accent the segment
        // paints.
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .filter(|span| span.content.contains('━'))
            .map(|span| span.style.fg)
            .collect::<Vec<_>>()
    };
    let input = theme
        .style(theme.component(Component::Input), Modifier::empty())
        .fg;
    let cache_write = theme
        .style(theme.component(Component::CacheWrite), Modifier::empty())
        .fg;

    // Two-bucket Codex fill from the row-level split: cache-read run then the
    // fresh-input accent, with no cache-write segment to paint.
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("add tests"),
    );
    codex.context_pct = Some(50);
    codex.context_window = Some(258_400);
    codex.cache_read_input_tokens = Some(120_000);
    codex.fresh_input_tokens = Some(9_200);
    codex.output_tokens = Some(800);
    let bar_styles = bar_styles_for(codex);
    assert!(
        bar_styles.contains(&input),
        "the fresh-input accent colors the bar tail"
    );
    assert!(
        !bar_styles.contains(&cache_write),
        "no cache-write segment exists to paint"
    );
    let input_at = bar_styles
        .iter()
        .position(|style| *style == input)
        .expect("the fresh-input accent");
    assert!(
        input_at >= 1 && bar_styles[..input_at].iter().all(|style| *style != input),
        "the cache-read health run leads the bar before fresh input: {bar_styles:?}"
    );

    // Three-bucket Claude fill: the cache-write accent slots between the
    // cache-read run and the fresh-input tail.
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("index docs"),
    );
    let mut context = claude_context(fixed_now());
    context.tokens = Some(AgentTokenUsage {
        context_window_size: Some(100_000),
        used_percentage: Some(30),
        remaining_percentage: Some(70),
        current_usage: Some(AgentCurrentUsage {
            input_tokens: Some(10_000),
            output_tokens: Some(0),
            cache_creation_input_tokens: Some(10_000),
            cache_read_input_tokens: Some(10_000),
        }),
    });
    claude.context = Some(context);
    let bar_styles = bar_styles_for(claude);
    let write_at = bar_styles
        .iter()
        .position(|style| *style == cache_write)
        .expect("the cache-write accent");
    let input_at = bar_styles
        .iter()
        .position(|style| *style == input)
        .expect("the fresh-input accent");
    assert!(
        write_at < input_at,
        "cache-write precedes fresh input along the bar: {bar_styles:?}"
    );
    assert!(
        write_at >= 1
            && bar_styles[..write_at]
                .iter()
                .all(|style| *style != cache_write && *style != input),
        "the cache-read health run leads the bar before the accents: {bar_styles:?}"
    );
}
/// The card's age cluster pairs a clock-fill glyph (the face fills with the
/// idle span) with a continuous tone: the dim resting weight while a resume
/// would still hit cache, then a warn-caution-alarm ramp to the hour — the cost
/// warning that resuming will likely re-read the whole context uncached.
#[test]
fn context_line_age_tone_slides_with_the_clock_age() {
    let theme = Theme::fixed(false);
    let age_style = |idle_secs: u64, clock: char| {
        let mut codex = agent(
            "codex-1",
            "codex",
            AgentStatus::Idle,
            Some("/repo/main"),
            Some("main"),
            Some("add tests"),
        );
        codex.context_pct = Some(21);
        codex.total_tokens = Some(5_000);
        codex.last_activity = fixed_now() - Duration::from_secs(idle_secs);
        let snapshot = snapshot_with(vec![codex]);
        group_lines(&snapshot, &theme, usize::MAX)
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content.contains(clock))
            .map(|span| span.style)
            .unwrap_or_else(|| panic!("the context line carries the {clock} age"))
    };
    let heat = |age_secs: i64| {
        theme.style(
            theme.warm_heat_tone(age_heat_amount_for_test(age_secs)),
            Modifier::empty(),
        )
    };
    assert_eq!(
        age_style(7 * 60, '◔'),
        theme.muted(),
        "warm cache rests at the dim weight once the clock shows past five minutes"
    );
    assert_eq!(
        age_style(25 * 60, '◑'),
        heat(25 * 60),
        "warm ramp tone with the half-full face"
    );
    assert_eq!(
        age_style(40 * 60, '◕'),
        heat(40 * 60),
        "mid-ramp tone past the half hour"
    );
    assert_eq!(
        age_style(2 * 60 * 60, '◉'),
        theme.alarm(Modifier::empty()),
        "red once a resume would pay for the context again"
    );
}
#[test]
fn codex_app_server_context_links_to_rich_card() {
    // Codex's split enrichment rides the same `AgentContext` field as Claude's
    // statusline, so it lights up the rich card with no renderer change: the
    // official display name, actual configured effort, and both usage windows in
    // the selected detail block. Token usage and cost are absent in this
    // fixture, so the gauge and detail fall back to the rollout scalars.
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("add tests"),
    );
    // Rollout scalars are the coarse fallback the app-server context upgrades.
    codex.model = Some("gpt-5.5-codex".to_owned());
    codex.context_pct = Some(21);
    codex.total_tokens = Some(48_000);
    codex.context = Some(codex_context(fixed_now()));
    let snapshot = snapshot_with(vec![codex]);
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: 0,
            help_visible: false,
            animation_phase: 0,
            line_map: Vec::new(),
            ..Default::default()
        },
        54,
        17,
    );

    // The app-server display name supersedes the raw catalog id (its hyphen
    // traded for a space, matching `Opus 4.8`), and locally sourced effort
    // surfaces — neither was on the rollout-only row.
    assert!(rendered.contains("GPT 5.5 Codex"));
    assert!(!rendered.contains("gpt-5.5-codex"));
    assert!(rendered.contains("xhigh"));
    // The 5h/7d windows are account-scoped now: they leave the row for the
    // provider dashboard, so no reset mark rides a row.
    assert!(!rendered.contains('↻'));
    assert!(!rendered.contains("5h"));
    assert!(!rendered.contains("7d"));
    // No read-only token usage or cost: the bare rollout total (`▤ 48k`,
    // integer form) stands in for the context line, and no cost pins to the
    // row.
    let token_line = rendered
        .lines()
        .find(|line| line.contains("▤ 48k"))
        .unwrap_or_else(|| panic!("bare token total rendered:\n{rendered}"));
    assert!(!token_line.contains('↗'));
    assert!(!token_line.contains('$'));
}
