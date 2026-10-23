use super::*;

fn fast_agent(id: &str, status: AgentStatus) -> AgentState {
    let mut state = agent(
        id,
        "codex",
        status,
        Some("/repo/main"),
        Some("main"),
        Some("Ship the badge"),
    );
    state.model = Some("gpt-6.1-sol".to_owned());
    state.effort = Some("xhigh".to_owned());
    state.context = Some(AgentContext {
        fast: Some(true),
        cost: Some(AgentCost {
            total_cost_usd: Some(1.0),
            ..Default::default()
        }),
        ..Default::default()
    });
    state
}

fn identity_text(snapshot: &SidebarSnapshot, theme: &Theme, width: usize) -> String {
    group_lines_at_width(snapshot, theme, usize::MAX, width)
        .iter()
        .find(|line| line.to_string().contains("$1.00"))
        .expect("the card's identity carries its cost")
        .to_string()
}

#[test]
fn fast_card_bolt_follows_the_model_across_tier_edges() {
    let theme = Theme::fixed(false);
    for width in [29, 30, 31, 43, 44, 45] {
        for fast in [true, false] {
            for model in [Some("gpt-6.1-sol"), None] {
                let mut state = fast_agent("fast", AgentStatus::Running);
                state.context.as_mut().unwrap().fast = Some(fast);
                state.model = model.map(str::to_owned);
                let snapshot = snapshot_with(vec![state]);
                let text = identity_text(&snapshot, &theme, width + 2);
                let capabilities = width >= 30 && model.is_some();
                assert_eq!(text.contains('ϟ'), fast && capabilities, "{width}: {text}");
                assert_eq!(
                    text.contains("xhigh"),
                    width >= 44 && model.is_some(),
                    "{width}: {text}"
                );
                if fast && capabilities {
                    assert!(text.contains("GPT 6.1 Sol ϟ"), "{text}");
                    assert!(!text.contains("Sol · ϟ"), "{text}");
                    if let Some(reasoning) = text.find("xhigh") {
                        assert!(text.find('ϟ').unwrap() < reasoning, "{text}");
                    }
                }
                assert!(text.trim_end().ends_with("$1.00"), "{text}");
                assert_eq!(super::super::super::layout::text_width(&text), width + 2);
            }
        }
    }
}

#[test]
fn fast_card_two_cell_override_survives_model_clipping_and_keeps_cost_aligned() {
    let handle = "abcdefghijklmnopqr";
    let mut state = fast_agent("fast", AgentStatus::Running);
    state.model = Some("a-model-with-an-unusually-long-name".to_owned());
    state.name = Some(handle.to_owned());
    state.name_explicit = true;
    let mut plain_state = state.clone();
    plain_state.context.as_mut().unwrap().fast = Some(false);
    let mut snapshot = snapshot_with(vec![state]);
    snapshot.theme = toml::from_str("[glyphs.unicode.card]\nfast = '++'").unwrap();
    let mut plain = snapshot_with(vec![plain_state]);
    plain.theme = snapshot.theme.clone();
    let theme = Theme::fixed_for_theme(false, &snapshot.theme);
    for width in [29, 30, 31, 32, 33, 34, 43, 44, 45] {
        let [plain, fast] = [&plain, &snapshot].map(|snapshot| {
            group_lines_at_width(snapshot, &theme, usize::MAX, width + 2)
                .into_iter()
                .find(|line| line.to_string().contains("$1.00"))
                .unwrap()
        });
        let handle_index = plain
            .spans
            .iter()
            .position(|span| span.content == handle)
            .expect("the full handle fits beside the cost");
        assert_eq!(
            fast.spans[..=handle_index],
            plain.spans[..=handle_index],
            "lead and handle stay unchanged at {width}"
        );
        let text = fast.to_string();
        if width < 33 {
            assert_eq!(fast, plain, "no room for a model cell and bolt at {width}");
        } else {
            assert!(text.contains("… ++"), "{width}: {text}");
        }
        assert!(text.trim_end().ends_with("$1.00"), "{text}");
        assert_eq!(super::super::super::layout::text_width(&text), width + 2);
    }
}

fn fast_frame(width: u16, nerd_font: bool) -> String {
    let mut snapshot = snapshot_with(vec![fast_agent("fast", AgentStatus::Idle)]);
    if nerd_font {
        snapshot.theme.style = Some(crate::config::ThemeStyle::Modern);
    }
    let screen = snapshot_to_screen(&snapshot, width, 15);
    assert!(
        screen.contains(if nerd_font { '\u{f0e7}' } else { 'ϟ' }),
        "{screen}"
    );
    screen
}

#[test]
fn resting_fast_card_l2_frame() {
    assert_snapshot("resting_fast_card_l2", fast_frame(54, false));
}

#[test]
fn resting_fast_card_l1_frame() {
    assert_snapshot("resting_fast_card_l1", fast_frame(32, false));
}

#[test]
fn resting_fast_card_nerd_font_frame() {
    assert_snapshot("resting_fast_card_nerd_font", fast_frame(54, true));
}

fn bolt_style(snapshot: &SidebarSnapshot, theme: &Theme, phase: u64) -> Style {
    let cost_rolls = CostRolls::default();
    let ctx = test_row_ctx(snapshot, theme, 54, 0, phase, &cost_rolls);
    let block = worktree_group_block(&ctx, &snapshot.worktree_groups[0], false, None);
    block
        .lines
        .iter()
        .flat_map(|line| &line.spans)
        .find(|span| span.content.trim() == theme.glyph(GlyphRole::CardFast))
        .expect("fast mode has its own bolt span")
        .style
}

#[test]
fn fast_bolt_stays_bold_and_still_for_every_resting_status() {
    let theme = Theme::fixed(false);
    for status in [
        AgentStatus::Idle,
        AgentStatus::Success,
        AgentStatus::Sleeping,
        AgentStatus::Paused,
        AgentStatus::Waiting,
        AgentStatus::Failed,
    ] {
        let snapshot = snapshot_with(vec![fast_agent("fast", status)]);
        assert_eq!(snapshot.rows().next().unwrap().status(), Some(status));
        let rest = bolt_style(&snapshot, &theme, 0);
        assert!(rest.add_modifier.contains(Modifier::BOLD), "{status:?}");
        let fast_mode =
            crate::theme::Palette::resolve(&snapshot.theme, crate::config::ColorDepth::Indexed)
                .fast_mode();
        let crate::theme::Tone::Indexed(index) = fast_mode else {
            panic!("indexed palette has indexed tones");
        };
        assert_eq!(rest.fg, Some(Color::Indexed(index)));
        for phase in 1..=64 {
            assert_eq!(
                bolt_style(&snapshot, &theme, phase),
                rest,
                "{status:?}, {phase}"
            );
        }
    }
}

#[test]
fn resting_fast_card_adds_no_animation_cadence() {
    let snapshot = snapshot_with(vec![fast_agent("fast", AgentStatus::Idle)]);
    assert_eq!(
        animation_cadence_for_test(&snapshot),
        AnimationCadence::None
    );
}

#[test]
fn fast_bolt_no_color_keeps_bold_without_a_foreground() {
    let theme = Theme::fixed(true);
    for status in [AgentStatus::Idle, AgentStatus::Running] {
        let snapshot = snapshot_with(vec![fast_agent("fast", status)]);
        for phase in 0..256 {
            let style = bolt_style(&snapshot, &theme, phase);
            assert_eq!(style.fg, None);
            assert!(style.add_modifier.contains(Modifier::BOLD));
        }
    }
}

#[test]
fn running_fast_bolt_crackles_aperiodically_with_an_agent_offset() {
    for mode in [
        crate::config::ThemeMode::Truecolor,
        crate::config::ThemeMode::Indexed,
    ] {
        let config = crate::config::ThemeConfig {
            mode,
            ..Default::default()
        };
        let theme = Theme::fixed_for_theme(false, &config);
        let rest = bolt_style(
            &snapshot_with(vec![fast_agent("fast", AgentStatus::Idle)]),
            &theme,
            0,
        );
        let flashes = ["fast-a", "fast-b"].map(|id| {
            let snapshot = snapshot_with(vec![fast_agent(id, AgentStatus::Running)]);
            let styles = (0..256)
                .map(|phase| bolt_style(&snapshot, &theme, phase))
                .collect::<Vec<_>>();
            let flashes = styles
                .iter()
                .map(|style| *style != rest)
                .collect::<Vec<_>>();
            let count = flashes.iter().filter(|flash| **flash).count();
            assert!(count > 0 && count < flashes.len() / 2, "{mode:?}: {count}");
            for period in 1..=flashes.len() / 2 {
                assert!(
                    (period..flashes.len()).any(|i| flashes[i] != flashes[i - period]),
                    "{id}, {mode:?}: periodic at {period}"
                );
            }
            let flash = styles.iter().find(|style| **style != rest).unwrap();
            if mode == crate::config::ThemeMode::Truecolor {
                let brightness = |color| match color {
                    Some(Color::Rgb(r, g, b)) => u32::from(r) + u32::from(g) + u32::from(b),
                    _ => panic!("truecolor bolt has an RGB tone"),
                };
                assert!(brightness(flash.fg) > brightness(rest.fg));
            } else {
                assert_eq!(flash.fg, rest.fg);
                assert!(flash.add_modifier.contains(Modifier::UNDERLINED));
            }
            flashes
        });
        assert_ne!(flashes[0], flashes[1], "{mode:?}");
    }
}

#[test]
fn fast_bolt_keeps_its_room_before_the_tier_move_mark() {
    let mut state = fast_agent("fast", AgentStatus::Idle);
    state.model = Some("astra".to_owned());
    state.effort = None;
    state.name = Some("abcdefghijklmnop".to_owned());
    state.name_explicit = true;
    state.tier = Some(
        serde_json::from_value(serde_json::json!({
            "tier": "senior", "model": "astra",
            "unbuildable": [{"model": "opus", "error": "enable model invocation"}]
        }))
        .unwrap(),
    );
    let theme = Theme::fixed(false);
    let snapshot = snapshot_with(vec![state]);
    for width in [33, 34, 35, 36, 37] {
        let text = identity_text(&snapshot, &theme, width + 2);
        assert!(text.contains("abcdefghijklmnop · "), "{width}: {text}");
        let model = if width >= 34 { "astra ϟ" } else { "ast… ϟ" };
        assert!(text.contains(model), "{width}: {text}");
        assert_eq!(text.contains("ϟ ↪"), width >= 36, "{width}: {text}");
        assert_eq!(text.contains('↪'), width >= 36, "{width}: {text}");
    }
}
