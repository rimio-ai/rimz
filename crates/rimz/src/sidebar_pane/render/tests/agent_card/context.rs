use super::*;
use crate::sidebar_pane::pixel::meter::MeterRaster;
use crate::sidebar_pane::render::theme::Component;
use ratatui::text::Span;

#[test]
fn droid_waiting_card_renders_native_ask_and_last_call_context_fill() {
    let mut droid = agent(
        "droid-1",
        "droid",
        AgentStatus::Waiting,
        Some("/repo/main"),
        Some("main"),
        Some("ask me a question"),
    );
    let mut context = claude_context(fixed_now());
    context.source = "droid".to_owned();
    context.session_name = None;
    context.model_id = Some("deepseek-v4-pro".to_owned());
    context.model_display_name = Some("DeepSeek V4 Pro".to_owned());
    context.effort = Some("high".to_owned());
    context.tokens = Some(AgentTokenUsage {
        context_window_size: Some(1_000_000),
        used_percentage: None,
        remaining_percentage: None,
        current_context_tokens: None,
        current_usage: Some(AgentCurrentUsage {
            input_tokens: Some(1_166),
            output_tokens: Some(162),
            cache_creation_input_tokens: None,
            cache_read_input_tokens: Some(17_920),
        }),
        session_usage: None,
    });
    context.rate_limits = None;
    droid.context = Some(context);

    let rendered = snapshot_to_screen(&snapshot_with(vec![droid]), 58, 17);

    assert!(
        rendered.contains("? droid"),
        "the native ask raises the waiting glyph:\n{rendered}"
    );
    assert!(
        rendered.contains("1.9%"),
        "the last call fills the context gauge:\n{rendered}"
    );
    assert!(
        rendered.contains("▤ 19k · ◌ 17k ↘ 1k ↗ 162"),
        "the gauge and composition share Droid's last-call numerator:\n{rendered}"
    );
}

#[test]
fn droid_card_renders_resolved_custom_model_session_usage_and_plain_cost() {
    let mut droid = agent(
        "droid-1",
        "droid",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("build telemetry"),
    );
    droid.model = Some("custom:DeepSeek-V4-Pro-0".to_owned());
    let mut context = claude_context(fixed_now());
    context.source = "droid".to_owned();
    context.session_name = None;
    context.model_id = Some("custom:DeepSeek-V4-Pro-0".to_owned());
    context.model_display_name = Some("DeepSeek V4 Pro".to_owned());
    context.effort = Some("high".to_owned());
    context.cost = Some(AgentCost {
        total_cost_usd: Some(0.42),
        coverage: crate::agents::CostCoverage::CurrentUsage,
        ..Default::default()
    });
    context.tokens = Some(AgentTokenUsage {
        context_window_size: Some(200_000),
        session_usage: Some(AgentSessionUsage {
            input_tokens: Some(12_000),
            output_tokens: Some(4_000),
            cache_creation_input_tokens: Some(3_000),
            cache_read_input_tokens: Some(30_000),
            thinking_tokens: Some(1_000),
        }),
        ..Default::default()
    });
    context.rate_limits = None;
    droid.context = Some(context);

    let rendered = snapshot_to_screen(&snapshot_with(vec![droid.clone()]), 58, 17);

    assert!(
        rendered.contains("DeepSeek V4 Pro · high · 200k"),
        "the resolved identity and configured capacity render together:\n{rendered}"
    );
    assert!(
        rendered.contains("◇ 20k ↘ 15k ↗ 5k ◌ 30k · 67%"),
        "session-lifetime categories use the shared token grammar:\n{rendered}"
    );
    assert!(
        rendered.contains("$0.42") && !rendered.contains('≈'),
        "current-usage costs render as plain dollars:\n{rendered}"
    );
    droid
        .context
        .as_mut()
        .and_then(|context| context.cost.as_mut())
        .unwrap()
        .coverage = crate::agents::CostCoverage::Session;
    let session = snapshot_to_screen(&snapshot_with(vec![droid]), 58, 17);
    assert!(
        session.contains("$0.42") && !session.contains('≈'),
        "session costs render as plain dollars:\n{session}"
    );
    assert!(
        !rendered.contains("custom:DeepSeek-V4-Pro-0")
            && !rendered.contains('▣')
            && rendered.contains('▢')
            && rendered.contains("0%")
            && !rendered.contains('▤')
            && rendered.contains('◇'),
        "a cumulative-only Droid reading keeps the placeholder meter without claiming context fill:\n{rendered}"
    );
}

#[test]
fn session_cache_hit_percent_uses_health_tone_and_hides_without_input_data() {
    let theme = Theme::fixed(false);
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("warm cache"),
    );
    let mut context = codex_context(fixed_now());
    context.tokens = Some(AgentTokenUsage {
        current_context_tokens: Some(10_000),
        session_usage: Some(AgentSessionUsage {
            input_tokens: Some(10),
            cache_read_input_tokens: Some(90),
            ..Default::default()
        }),
        ..Default::default()
    });
    codex.context = Some(context);

    let snapshot = snapshot_with(vec![codex.clone()]);
    let cost_rolls = CostRolls::default();
    let row_ctx = test_row_ctx(&snapshot, &theme, 52, 0, 0, &cost_rolls);
    let lines = worktree_group_block(&row_ctx, &snapshot.worktree_groups[0], false, None).lines;
    let percent = lines
        .iter()
        .flat_map(|line| &line.spans)
        .find(|span| span.content == "90%")
        .expect("cache hit percentage span");
    assert_eq!(percent.style.fg, theme.good(Modifier::empty()).fg);

    codex
        .context
        .as_mut()
        .and_then(|context| context.tokens.as_mut())
        .unwrap()
        .session_usage = None;
    let rendered = snapshot_to_screen(&snapshot_with(vec![codex]), 52, 17);
    assert!(!rendered.contains("· 90%"), "{rendered}");
}

#[test]
fn unresolved_droid_selector_is_friendly_without_claiming_capacity_or_cost() {
    let mut droid = agent(
        "droid-1",
        "droid",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("build telemetry"),
    );
    droid.model = Some("custom:DeepSeek-V4-Pro-0".to_owned());

    let rendered = snapshot_to_screen(&snapshot_with(vec![droid]), 52, 17);

    assert!(rendered.contains("DeepSeek V4 Pro"), "{rendered}");
    let identity = rendered
        .lines()
        .find(|line| line.contains("droid"))
        .unwrap_or_else(|| panic!("Droid identity line:\n{rendered}"));
    assert!(
        !rendered.contains("custom:")
            && !rendered.contains("-0")
            && !rendered.contains('▣')
            && rendered.contains('▢')
            && rendered.contains("0%")
            && rendered.contains("▤ 0")
            && !identity.contains('$')
            && !identity.contains("200k"),
        "presentation-only fallback keeps zeroed placeholders without claiming capacity or dollars:\n{rendered}"
    );
}

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
    // Codex's line-two definition follows a precedence ladder: thread name >
    // thread preview > task.
    let codex_with = |session_name: Option<&str>, session_preview: Option<&str>| {
        let mut codex = agent(
            "codex-1",
            "codex",
            AgentStatus::Running,
            Some("/repo/main"),
            Some("main"),
            Some("db migrate"),
        );
        let mut context = codex_context(fixed_now());
        context.session_name = session_name.map(str::to_owned);
        context.session_preview = session_preview.map(str::to_owned);
        codex.context = Some(context);
        let snapshot = snapshot_with(vec![codex]);
        snapshot_to_screen(&snapshot, 44, 15)
    };

    // The generated name wins over the preview and task.
    let rendered = codex_with(Some("TUI prototype"), Some("Create a TUI"));
    assert!(rendered.contains("TUI prototype"));
    assert!(!rendered.contains("Create a TUI"));
    assert!(!rendered.contains("db migrate"));

    // Without a name, the preview still beats the task fall-through.
    let rendered = codex_with(None, Some("Create a TUI"));
    assert!(rendered.contains("Create a TUI"));
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
    codex.usage.total_tokens = Some(5_000);
    assert!(codex.context.is_none());
    let snapshot = snapshot_with(vec![codex]);
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: 0,
            help_visible: false,
            animation_phase: 0,
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
    // includes output), then `◌`/`↘`/`↗`. No `◍` column: this pre-0.145 shape
    // omits per-call cache-write, so it drops rather than reading a false zero.
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("add tests"),
    );
    codex.model = Some("GPT-5.5".to_owned());
    codex.usage.context_pct = Some(50);
    codex.usage.context_window = Some(258_400);
    codex.usage.total_tokens = Some(130_000);
    codex.usage.cache_read_input_tokens = Some(120_000);
    codex.usage.fresh_input_tokens = Some(9_200);
    codex.usage.output_tokens = Some(800);
    assert!(codex.context.is_none());
    let snapshot = snapshot_with(vec![codex]);
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: 0,
            help_visible: false,
            animation_phase: 0,
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
        "an unreported per-call cache-write drops the column:\n{rendered}"
    );
    assert_snapshot("codex_card_context_composition", rendered);
}

#[test]
fn qwen_card_combines_live_gauge_with_correlated_call_split() {
    let mut qwen = agent(
        "qwen-1",
        "qwen",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("inspect adapter"),
    );
    qwen.model = Some("[DeepSeek] deepseek-v4-pro".to_owned());
    qwen.usage.context_pct = Some(4);
    qwen.usage.context_window = Some(1_000_000);
    qwen.usage.total_tokens = Some(38_812);
    qwen.usage.cache_read_input_tokens = Some(38_656);
    qwen.usage.fresh_input_tokens = Some(71);
    qwen.usage.output_tokens = Some(85);
    let mut context = codex_context(fixed_now());
    context.source = "qwen".to_owned();
    context.model_id = None;
    context.model_display_name = Some("DeepSeek V4 Pro".to_owned());
    context.effort = None;
    context.agent_version = Some("0.19.10".to_owned());
    context.rate_limits = None;
    context.cost = Some(AgentCost {
        total_cost_usd: Some(0.0153),
        coverage: crate::agents::CostCoverage::Session,
        ..Default::default()
    });
    context.tokens = Some(AgentTokenUsage {
        context_window_size: Some(1_000_000),
        used_percentage: Some(4),
        remaining_percentage: Some(96),
        current_context_tokens: Some(38_727),
        current_usage: None,
        session_usage: Some(AgentSessionUsage {
            input_tokens: Some(12_000),
            output_tokens: Some(1_500),
            cache_creation_input_tokens: None,
            cache_read_input_tokens: Some(7_000),
            thinking_tokens: Some(500),
        }),
    });
    qwen.context = Some(context);

    let render = |agent| {
        snapshot_to_screen_with_alert_and_ui(
            &snapshot_with(vec![agent]),
            None,
            &UiState {
                selected_index: 0,
                ..Default::default()
            },
            52,
            17,
        )
    };
    let first = render(qwen.clone());
    assert!(first.contains("DeepSeek V4 Pro · 1m"), "{first}");
    assert!(first.contains("3.9%"), "{first}");
    assert!(first.contains("▤ 38k · ◌ 38k ↘ 71 ↗ 85"), "{first}");
    assert!(first.contains("$0.02"), "{first}");
    assert!(!first.contains("◇ 14k"), "{first}");
    assert!(!first.contains("[DeepSeek]"), "{first}");
    assert!(!first.contains("↘ 38k"), "{first}");
    assert!(!first.contains('◍'), "{first}");

    qwen.usage.context_pct = None;
    let tokens = qwen
        .context
        .as_mut()
        .and_then(|context| context.tokens.as_mut())
        .unwrap();
    tokens.used_percentage = None;
    tokens.remaining_percentage = None;
    tokens.current_context_tokens = Some(40_000);
    let second = render(qwen);
    assert!(second.contains("4.0%"), "{second}");
    assert!(second.contains("▤ 40k"), "{second}");
    assert!(!second.contains("◇ 14k"), "{second}");
    assert!(!second.contains("◌ 38k"), "{second}");
    assert!(!second.contains("↘ 71"), "{second}");
    assert_ne!(first, second);
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
    codex.usage.context_pct = Some(0);
    let mut context = codex_context(fixed_now());
    context.tokens = Some(AgentTokenUsage {
        context_window_size: Some(258_400),
        used_percentage: None,
        remaining_percentage: None,
        current_context_tokens: None,
        current_usage: Some(AgentCurrentUsage {
            input_tokens: Some(6_700),
            output_tokens: Some(825),
            cache_creation_input_tokens: None,
            cache_read_input_tokens: Some(56_900),
        }),
        session_usage: None,
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
fn context_meter_can_restore_linear_fill_geometry() {
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("use the large window"),
    );
    codex.usage.context_pct = Some(40);
    codex.usage.context_window = Some(1_000_000);
    codex.usage.total_tokens = Some(400_000);
    let snapshot = snapshot_with(vec![codex]);
    let theme = Theme::fixed(false);
    let meter_geometry = |snapshot: &SidebarSnapshot| {
        let meter = group_lines(snapshot, &theme, usize::MAX)
            .into_iter()
            .find(|line| line.to_string().contains("40%"))
            .unwrap_or_else(|| panic!("context meter renders"))
            .to_string();
        let bar_cells = meter
            .chars()
            .filter(|glyph| matches!(glyph, '━' | '╸' | '─'))
            .count();
        let ink_halves = meter.chars().fold(0, |halves, glyph| {
            halves
                + match glyph {
                    '━' => 2,
                    '╸' => 1,
                    _ => 0,
                }
        });
        (bar_cells, ink_halves)
    };

    let (_, scaled_halves) = meter_geometry(&snapshot);
    let mut linear = snapshot;
    linear.theme.display.context_meter.log_scale = false;
    let (bar_cells, linear_halves) = meter_geometry(&linear);

    assert_eq!(
        linear_halves,
        (0.4 * bar_cells as f64 * 2.0).round() as usize,
        "disabling log scale restores the raw 40% fill"
    );
    assert!(
        scaled_halves > linear_halves,
        "the default log curve gives the working range more room"
    );
}

#[test]
fn copilot_token_only_context_shows_composition_with_placeholder_gauge() {
    let mut copilot = agent(
        "copilot-1",
        "copilot",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("review telemetry"),
    );
    copilot.model = Some("auto".to_owned());
    copilot.usage.context_pct = Some(0);
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
        rendered
            .lines()
            .any(|line| line.contains("▌  ▢") && line.contains("0%")),
        "no denominator keeps the stage-fixed placeholder gauge:\n{rendered}"
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
        rendered
            .lines()
            .any(|line| line.contains("▌  ▢") && line.contains("0%")),
        "selected idle token-only cards keep the placeholder gauge:\n{rendered}",
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
        19,
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
    codex.usage.context_pct = Some(50);
    codex.usage.context_window = Some(258_400);
    codex.usage.total_tokens = Some(130_000);
    codex.usage.cache_read_input_tokens = Some(120_000);
    codex.usage.fresh_input_tokens = Some(9_200);
    let snapshot = snapshot_with(vec![codex]);

    let render = |theme: &Theme, pixels: &mut MeterPixels| {
        let cost_rolls = CostRolls::default();
        let ctx = test_row_ctx(&snapshot, theme, 44, 0, 0, &cost_rolls);
        worktree_group_block(&ctx, &snapshot.worktree_groups[0], false, Some(pixels)).lines
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
    pi.usage.context_pct = Some(54);
    pi.usage.context_window = Some(258_400);
    pi.usage.total_tokens = Some(140_000);
    pi.usage.cache_read_input_tokens = Some(120_000);
    pi.usage.cache_write_input_tokens = Some(10_000);
    pi.usage.fresh_input_tokens = Some(9_200);
    pi.usage.output_tokens = Some(800);
    let snapshot = snapshot_with(vec![pi]);
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: 0,
            help_visible: false,
            animation_phase: 0,
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
/// the segment colors. Cache-write-only and cache-write-plus-input cases prove
/// that an absent cache-read run contributes no health tone. Codex and Claude
/// cases with cache reads prove that common two- and three-bucket compositions
/// retain their existing order and colors.
#[test]
fn calm_context_bar_orders_segments_left_to_right() {
    let theme = Theme::fixed(false);
    let bar_styles_for = |agent: crate::agents::AgentState| {
        let snapshot = snapshot_with(vec![agent]);
        let cost_rolls = CostRolls::default();
        let ctx = test_row_ctx(&snapshot, &theme, 44, 0, 0, &cost_rolls);
        let lines = worktree_group_block(&ctx, &snapshot.worktree_groups[0], false, None).lines;
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
    let health = theme.style(theme.heat_tone(0.0), Modifier::empty()).fg;

    let context_agent = |name: &str, percent: u8, reads: u64, writes: u64, input: u64| {
        let mut agent = agent(
            name,
            "claude",
            AgentStatus::Running,
            Some("/repo/main"),
            Some("main"),
            Some("index docs"),
        );
        let mut context = claude_context(fixed_now());
        context.tokens = Some(AgentTokenUsage {
            context_window_size: Some(100_000),
            used_percentage: Some(percent),
            remaining_percentage: Some(100 - percent),
            current_context_tokens: None,
            current_usage: Some(AgentCurrentUsage {
                input_tokens: Some(input),
                output_tokens: Some(0),
                cache_creation_input_tokens: Some(writes),
                cache_read_input_tokens: Some(reads),
            }),
            session_usage: None,
        });
        agent.context = Some(context);
        agent
    };

    let write_only = bar_styles_for(context_agent("claude-write", 26, 0, 26_000, 0));
    assert!(
        write_only.contains(&cache_write),
        "a cache-write-only fill uses the cache-write tone: {write_only:?}"
    );
    assert!(
        write_only.iter().all(|style| *style != health),
        "a cache-write-only fill does not inherit the absent cache-read health tone: {write_only:?}"
    );

    let write_and_input = bar_styles_for(context_agent("claude-mixed", 26, 0, 20_000, 6_000));
    let write_at = write_and_input
        .iter()
        .position(|style| *style == cache_write)
        .expect("the cache-write lead");
    let input_at = write_and_input
        .iter()
        .position(|style| *style == input)
        .expect("the fresh-input accent");
    assert!(
        write_at < input_at,
        "cache-write leads fresh input when cache-read is absent: {write_and_input:?}"
    );
    assert!(
        write_and_input.iter().all(|style| *style != health),
        "a mixed fill without cache reads has no health-colored run: {write_and_input:?}"
    );

    let hot_health = theme
        .style(theme.heat_tone(2.0 / 3.0), Modifier::empty())
        .fg;
    let flat_read = theme
        .style(theme.component(Component::CacheRead), Modifier::empty())
        .fg;
    assert_ne!(
        hot_health, flat_read,
        "the fixture must distinguish health from the flat cache-read tone"
    );
    let hot_reads = bar_styles_for(context_agent("claude-hot", 80, 80_000, 0, 0));
    assert!(
        hot_reads.iter().all(|style| *style == hot_health),
        "an amber cache-read run carries the source-seeded health tone: {hot_reads:?}"
    );

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
    codex.usage.context_pct = Some(50);
    codex.usage.context_window = Some(258_400);
    codex.usage.cache_read_input_tokens = Some(120_000);
    codex.usage.fresh_input_tokens = Some(9_200);
    codex.usage.output_tokens = Some(800);
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
        current_context_tokens: None,
        current_usage: Some(AgentCurrentUsage {
            input_tokens: Some(10_000),
            output_tokens: Some(0),
            cache_creation_input_tokens: Some(10_000),
            cache_read_input_tokens: Some(10_000),
        }),
        session_usage: None,
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
/// warning that resuming will likely re-read the whole context uncached. The
/// ramp reads off the clock alone, so a finished card carries the same warning.
#[test]
fn context_line_age_tone_slides_with_the_clock_age() {
    let theme = Theme::fixed(false);
    let age_style = |status, idle_secs: u64, clock: char| {
        let mut codex = agent(
            "codex-1",
            "codex",
            status,
            Some("/repo/main"),
            Some("main"),
            Some("add tests"),
        );
        codex.usage.context_pct = Some(21);
        codex.usage.total_tokens = Some(5_000);
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
        age_style(AgentStatus::Idle, 7 * 60, '◔'),
        theme.muted(),
        "warm cache rests at the dim weight once the clock shows past five minutes"
    );
    assert_eq!(
        age_style(AgentStatus::Idle, 25 * 60, '◑'),
        heat(25 * 60),
        "warm ramp tone with the half-full face"
    );
    assert_eq!(
        age_style(AgentStatus::Idle, 40 * 60, '◕'),
        heat(40 * 60),
        "mid-ramp tone past the half hour"
    );
    assert_eq!(
        age_style(AgentStatus::Idle, 10 * 60 * 60, '◉'),
        theme.alarm(Modifier::empty()),
        "red once a resume would pay for the context again"
    );
    assert_eq!(
        age_style(AgentStatus::Success, 10 * 60 * 60, '◉'),
        theme.alarm(Modifier::empty()),
        "a finished-success context heats on the same ramp — prompting it again \
         re-reads the whole context uncached"
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
    codex.usage.context_pct = Some(21);
    codex.usage.total_tokens = Some(48_000);
    codex.context = Some(codex_context(fixed_now()));
    let snapshot = snapshot_with(vec![codex]);
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: 0,
            help_visible: false,
            animation_phase: 0,
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
