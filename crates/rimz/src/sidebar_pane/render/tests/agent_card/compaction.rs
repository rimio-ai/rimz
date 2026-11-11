use super::*;
use crate::config::ContextMeterScale;
use crate::sidebar_pane::render::theme::Component;
use crate::store::snapshot::CacheClock;

fn compaction_snapshot() -> SidebarSnapshot {
    let mut state = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("index docs"),
    );
    state.model = Some("Opus".into());
    state.effort = Some("high".into());
    state.record = Some(Box::new(crate::agents::LaunchRecord {
        auto_compact: Some(372_000),
        ..Default::default()
    }));
    state.usage.context_window = Some(1_000_000);
    state.usage.context_pct = Some(28);
    state.usage.total_tokens = Some(279_000);
    state.usage.cache_read_input_tokens = Some(200_000);
    state.usage.cache_write_input_tokens = Some(60_000);
    state.usage.fresh_input_tokens = Some(19_000);
    state.context = Some(AgentContext {
        tokens: Some(AgentTokenUsage {
            context_window_size: Some(1_000_000),
            current_context_tokens: Some(279_000),
            used_percentage: Some(28),
            ..Default::default()
        }),
        ..Default::default()
    });
    snapshot_with(vec![state])
}

fn opencode_compaction_snapshot() -> SidebarSnapshot {
    let mut snapshot = compaction_snapshot();
    let row = snapshot.rows_mut().next().unwrap();
    row.name = "opencode".into();
    let card = row.as_agent_mut().unwrap();
    card.model = Some("HY4".into());
    card.handle = Some("opencode-docsmith".into());
    card.fast = true;
    card.tier_moved = true;
    card.auto_compact = None;
    card.usage.context_window = Some(983_010);
    card.usage.context_capacity = Some(1_048_576);
    card.context = Some(AgentContext {
        cost: Some(AgentCost {
            total_cost_usd: Some(1.2),
            ..Default::default()
        }),
        ..Default::default()
    });
    snapshot
}

#[test]
fn opencode_capacity_yields_at_the_exact_label_boundary() {
    for set in ["unicode", "nerd_font"] {
        let mut snapshot = opencode_compaction_snapshot();
        snapshot.theme.glyphs.set = Some(set.into());
        let theme = Theme::fixed_for_theme(false, &snapshot.theme);
        for width in [53, 54, 55, 58] {
            let lines = group_lines_at_width(&snapshot, &theme, usize::MAX, width);
            let identity = lines
                .iter()
                .find(|line| line.to_string().contains("opencode-docsmith"))
                .unwrap();
            let text = identity.to_string();
            assert!(text.contains("835k"), "{set}, {width}: {text}");
            assert_eq!(
                text.contains("835k/1M"),
                width >= 54,
                "{set}, {width}: {text}"
            );
            assert!(text.trim_end().ends_with("$1.20"), "{text}");
        }
        assert_snapshot(
            &format!("opencode_compaction_capacity_{set}"),
            snapshot_to_screen(&snapshot, 58, 17),
        );
    }
}

#[test]
fn window_scale_labels_and_tints_the_published_opencode_capacity() {
    let mut snapshot = opencode_compaction_snapshot();
    snapshot.theme.display.context_meter.scale = ContextMeterScale::Window;
    let theme = Theme::fixed_for_theme(false, &snapshot.theme);
    let lines = group_lines_at_width(&snapshot, &theme, usize::MAX, 58);
    let identity = lines
        .iter()
        .find(|line| line.to_string().contains("opencode-docsmith"))
        .unwrap();
    assert_eq!(
        identity
            .spans
            .iter()
            .find(|span| span.content == "1M")
            .map(|span| span.style),
        Some(crate::sidebar_pane::render::labels::window_style(
            &theme, 1_048_576
        ))
    );
}

#[test]
fn codex_identity_shows_compaction_only_after_a_reported_window() {
    for window in [None, Some(828_400)] {
        let mut state = agent(
            "codex-1",
            "codex",
            AgentStatus::Running,
            Some("/repo/main"),
            Some("main"),
            Some("index docs"),
        );
        state.model = Some("gpt-6.1-sol".into());
        state.record = Some(Box::new(crate::agents::LaunchRecord {
            auto_compact: Some(372_000),
            ..Default::default()
        }));
        state.context = Some(AgentContext {
            model_id: Some("gpt-6.1-sol".into()),
            model_context_limits: Some(crate::agents::context::ModelContextLimits {
                model: "gpt-6.1-sol".into(),
                max_window: Some(872_000),
                usable_percent: Some(95),
                own_compact_limit: None,
            }),
            tokens: window.map(|window| AgentTokenUsage {
                context_window_size: Some(window),
                ..Default::default()
            }),
            ..Default::default()
        });
        let snapshot = snapshot_with(vec![state]);
        let theme = Theme::fixed_for_theme(false, &snapshot.theme);
        let lines = group_lines_at_width(&snapshot, &theme, 0, 58);
        let identity = lines
            .iter()
            .find(|line| line.to_string().contains("codex"))
            .unwrap();
        let text = identity.to_string();
        assert!(!text.contains("272k"), "{text}");
        assert_eq!(text.contains('/'), window.is_some(), "{text}");
        assert_eq!(text.contains("372k/872k"), window.is_some(), "{text}");
        assert_eq!(text.contains("372k"), window.is_some(), "{text}");
        assert_eq!(
            lines.iter().any(|line| line.to_string().contains('┤')),
            window.is_some()
        );
        if window.is_some() {
            assert_snapshot(
                "codex_compaction_capacity",
                snapshot_to_screen_with_alert_and_ui(
                    &snapshot,
                    None,
                    &selected_ui(),
                    &UiState::default(),
                    58,
                    17,
                ),
            );
        }
    }
}

#[test]
fn codex_cards_show_lowered_compaction_points() {
    codex_compaction_frame(Some(372_000), 258_400, None, "244k/872k", "lowered");
}

#[test]
fn codex_cards_show_provider_owned_compaction_points() {
    codex_compaction_frame(None, 828_400, Some(300_000), "300k/872k", "own");
}

fn codex_compaction_frame(
    count: Option<u64>,
    window: u64,
    own: Option<u64>,
    label: &str,
    name: &str,
) {
    let mut snapshot = compaction_snapshot();
    let row = snapshot.rows_mut().next().unwrap();
    row.name = "codex".into();
    let card = row.as_agent_mut().unwrap();
    card.auto_compact = count;
    card.model = Some("gpt-6.1-sol".into());
    card.usage.total_tokens = Some(100_000);
    card.context = Some(AgentContext {
        model_id: Some("gpt-6.1-sol".into()),
        model_context_limits: Some(crate::agents::context::ModelContextLimits {
            model: "gpt-6.1-sol".into(),
            max_window: Some(872_000),
            usable_percent: Some(95),
            own_compact_limit: own,
        }),
        tokens: Some(AgentTokenUsage {
            context_window_size: Some(window),
            current_context_tokens: Some(100_000),
            ..Default::default()
        }),
        ..Default::default()
    });
    let theme = Theme::fixed_for_theme(false, &snapshot.theme);
    let lines = group_lines_at_width(&snapshot, &theme, 0, 58);
    assert!(
        lines.iter().any(|line| line.to_string().contains(label)),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.to_string().contains('┤')),
        "missing compaction wall"
    );
    assert_snapshot(
        &format!("codex_compaction_{name}"),
        snapshot_to_screen_with_alert_and_ui(
            &snapshot,
            None,
            &selected_ui(),
            &UiState::default(),
            58,
            17,
        ),
    );
}

#[test]
fn compaction_wall_and_label_render_across_meter_states_and_glyph_sets() {
    for (set, no_color) in [("unicode", false), ("nerd_font", false), ("unicode", true)] {
        for state in [
            "measured", "flat", "expired", "estimate", "empty", "idle", "full",
        ] {
            let mut snapshot = compaction_snapshot();
            snapshot.theme.glyphs.set = Some(set.into());
            let row = snapshot.rows_mut().next().unwrap();
            let card = row.as_agent_mut().unwrap();
            match state {
                "flat" => {
                    card.usage.cache_read_input_tokens = None;
                    card.usage.cache_write_input_tokens = None;
                    card.usage.fresh_input_tokens = None;
                }
                "expired" => {
                    card.cache = Some(CacheClock {
                        ceiling_secs: 1800,
                        last_request_at: None,
                        warm_until: None,
                        rewarm_cost_usd: None,
                        recorded: false,
                    });
                    row.last_activity = fixed_now() - Duration::from_secs(2400);
                }
                "estimate" => card.compacted_unmeasured = true,
                "empty" => {
                    card.compacted_unmeasured = true;
                    card.usage.context_pct = None;
                    card.usage.total_tokens = None;
                }
                "idle" => card.status = AgentStatus::Idle,
                "full" => {
                    card.usage.total_tokens = Some(400_000);
                    card.context
                        .as_mut()
                        .unwrap()
                        .tokens
                        .as_mut()
                        .unwrap()
                        .current_context_tokens = Some(400_000);
                }
                _ => {}
            }
            let theme = Theme::fixed_for_theme(no_color, &snapshot.theme);
            let lines = group_lines_at_width(&snapshot, &theme, 0, 58);
            let meter = lines.iter().find(|line| line.to_string().contains('┤'));
            assert!(meter.is_some(), "{set}, {state}: missing compaction wall");
            let meter = meter.unwrap();
            let wall = meter.spans.iter().find(|span| span.content == "┤").unwrap();
            assert_eq!(
                wall.style.fg,
                theme.styled(Component::CacheWrite, Modifier::DIM).fg
            );
            assert!(wall.style.add_modifier.contains(Modifier::DIM));
            let text = meter.to_string();
            if state == "empty" {
                assert!(!text.contains('%'), "{text}");
            } else {
                assert!(
                    text.contains(match state {
                        "full" => "100%",
                        _ => "75.0%",
                    }),
                    "{text}"
                );
            }
            let identity = lines
                .iter()
                .find(|line| line.to_string().contains("claude"))
                .unwrap();
            assert_eq!(identity.to_string().contains("372k/1M"), state != "idle");
            if state != "idle" {
                let point = identity
                    .spans
                    .iter()
                    .find(|span| span.content == "372k")
                    .unwrap();
                let window = identity
                    .spans
                    .iter()
                    .find(|span| span.content == "/1M")
                    .unwrap();
                assert_eq!(point.style, wall.style);
                assert_eq!(window.style.fg, theme.muted().fg);
                assert_eq!(window.style.add_modifier, theme.muted().add_modifier);
            }
            if matches!(state, "flat" | "full") {
                continue;
            }
            let ui = UiState {
                theme_cache: Some((snapshot.theme.clone(), Rc::new(theme))),
                ..Default::default()
            };
            assert_snapshot(
                &format!(
                    "compaction_{set}_{}_{state}",
                    if no_color { "plain" } else { "color" }
                ),
                snapshot_to_screen_with_alert_and_ui(&snapshot, None, &selected_ui(), &ui, 58, 17),
            );
        }
    }
}

#[test]
fn compaction_label_yields_whole_tokens_at_exact_width_boundaries() {
    for set in ["unicode", "nerd_font"] {
        let mut snapshot = compaction_snapshot();
        snapshot.theme.glyphs.set = Some(set.into());
        let card = snapshot.rows_mut().next().unwrap().as_agent_mut().unwrap();
        card.handle = Some("claude-docsmith".into());
        card.fast = true;
        card.tier_moved = true;
        card.context = Some(AgentContext {
            cost: Some(AgentCost {
                total_cost_usd: Some(1.2),
                ..Default::default()
            }),
            ..Default::default()
        });
        let theme = Theme::fixed_for_theme(false, &snapshot.theme);
        let mut frames = Vec::new();
        for width in [49, 50, 51, 52, 53, 54] {
            let lines = group_lines_at_width(&snapshot, &theme, usize::MAX, width);
            let identity = lines
                .iter()
                .find(|line| line.to_string().contains("$1.20"))
                .unwrap();
            let text = identity.to_string();
            assert_eq!(
                text.contains("372k/1M"),
                width >= 53,
                "{set}, {width}: {text}"
            );
            assert_eq!(text.contains("372k"), width >= 50, "{set}, {width}: {text}");
            assert!(!text.contains("372") || text.contains("372k"), "{text}");
            assert!(text.contains("claude-docsmith"), "{text}");
            assert!(
                text.contains(&format!(
                    "Opus {} ↪ · high",
                    theme.glyph(GlyphRole::CardFast)
                )),
                "{text}"
            );
            assert!(text.trim_end().ends_with("$1.20"), "{text}");
            frames.push((
                format!("compaction_label_{set}_{width}"),
                snapshot_to_screen(&snapshot, width as u16, 17),
            ));
        }
        for (name, frame) in frames {
            assert_snapshot(&name, frame);
        }
        let rendered = snapshot_to_screen(&snapshot, 31, 17);
        assert!(rendered.contains('┤'), "L0 keeps the wall: {rendered}");
        assert!(!rendered.contains("372k"), "L0 drops the label: {rendered}");
    }
}

#[test]
fn window_scale_renders_the_same_frame_as_an_unconfigured_card() {
    for set in ["unicode", "nerd_font"] {
        let mut snapshot = compaction_snapshot();
        snapshot.theme.glyphs.set = Some(set.into());
        snapshot.theme.display.context_meter.scale = ContextMeterScale::Window;
        let mut fallback = snapshot.clone();
        fallback
            .rows_mut()
            .next()
            .unwrap()
            .as_agent_mut()
            .unwrap()
            .auto_compact = None;
        for width in [31, 32, 45, 46, 58] {
            let rendered = snapshot_to_screen(&snapshot, width, 17);
            assert_eq!(rendered, snapshot_to_screen(&fallback, width, 17));
            assert!(
                !rendered.contains('┤') && !rendered.contains("372k"),
                "{rendered}"
            );
        }
    }
}

#[test]
fn compaction_pixel_meters_keep_the_wall_including_an_empty_compaction() {
    use crate::sidebar_pane::pixel::meter::MeterRaster;

    for state in ["measured", "expired", "estimate", "empty"] {
        let mut snapshot = compaction_snapshot();
        snapshot.theme.mode = crate::config::ThemeMode::Truecolor;
        snapshot.theme.display.context_meter.log_scale = false;
        let row = snapshot.rows_mut().next().unwrap();
        let card = row.as_agent_mut().unwrap();
        card.usage.cache_read_input_tokens = None;
        card.usage.cache_write_input_tokens = None;
        card.usage.fresh_input_tokens = None;
        if matches!(state, "estimate" | "empty") {
            card.compacted_unmeasured = true;
        }
        if state == "empty" {
            card.usage.context_pct = None;
            card.usage.total_tokens = None;
        }
        if state == "expired" {
            card.cache = Some(CacheClock {
                ceiling_secs: 1800,
                last_request_at: None,
                warm_until: None,
                rewarm_cost_usd: None,
                recorded: false,
            });
            row.last_activity = fixed_now() - Duration::from_secs(2400);
        }
        let theme = Theme::fixed_for_theme(false, &snapshot.theme);
        let cost_rolls = CostRolls::default();
        let ctx = test_row_ctx(&snapshot, &theme, 58, 0, 0, &cost_rolls);
        let mut pixels = MeterPixels::new(0x120000);
        let lines =
            worktree_group_block(&ctx, &snapshot.worktree_groups[0], false, Some(&mut pixels))
                .lines;
        pixels.observe_visible(&lines);
        let rasters = pixels.visible_rasters().collect::<Vec<_>>();
        assert_eq!(rasters.len(), 1, "{state} keeps a pixel meter");
        let raster = rasters[0].1;
        let health = theme
            .pixel_rgb(if state == "empty" {
                theme.component(Component::ContextCompacted)
            } else if state == "expired" {
                theme.component(Component::CacheExpired)
            } else {
                lines
                    .iter()
                    .flat_map(|line| &line.spans)
                    .find(|span| {
                        span.content
                            == theme.glyph(if state == "estimate" {
                                GlyphRole::MeterCompacted
                            } else {
                                GlyphRole::MeterContextFull
                            })
                    })
                    .unwrap()
                    .style
                    .fg
                    .unwrap()
            })
            .unwrap();
        let fill = if state == "empty" { 0.0 } else { 0.75 };
        let open = MeterRaster::new(
            raster.width_cells,
            fill,
            health,
            Vec::new(),
            raster.track,
            None,
        );
        assert_ne!(
            *raster, open,
            "{state} has a walled rather than open raster"
        );
        assert_eq!(
            *raster,
            MeterRaster::new(
                raster.width_cells,
                fill,
                health,
                Vec::new(),
                raster.track,
                Some(
                    theme
                        .pixel_rgb(theme.component(Component::CacheWrite))
                        .unwrap()
                ),
            ),
            "{state}"
        );
    }
}

#[test]
fn compaction_wall_role_is_configurable_in_both_glyph_sets() {
    let role = GlyphRole::from_namespaced("meter", "compaction_wall");
    assert!(role.is_some(), "the meter catalog exposes compaction_wall");
    for set in ["unicode", "nerd_font"] {
        let mut snapshot = compaction_snapshot();
        snapshot.theme.glyphs = toml::from_str(&format!(
            "set = '{set}'\n[{set}.meter]\ncompaction_wall = '|'"
        ))
        .unwrap();
        let theme = Theme::fixed_for_theme(false, &snapshot.theme);
        assert_eq!(theme.glyph(role.unwrap()), "|");
        let rendered = snapshot_to_screen(&snapshot, 58, 17);
        assert!(
            rendered.contains('|') && !rendered.contains('┤'),
            "{rendered}"
        );
    }
}
