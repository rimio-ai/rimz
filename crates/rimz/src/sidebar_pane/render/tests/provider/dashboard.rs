use super::*;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

#[test]
fn dashboard_mode_is_selected_once_from_display_and_pet_settings() {
    let mut snapshot = snapshot_with(Vec::new());
    snapshot.providers = two_provider_panels();
    snapshot.theme.display.provider_tabs = crate::config::ProviderTabsMode::Never;
    assert_eq!(dashboard_mode(&snapshot), DashboardMode::Stacked);

    snapshot.theme.display.provider_tabs = crate::config::ProviderTabsMode::Always;
    assert_eq!(dashboard_mode(&snapshot), DashboardMode::Tabbed);

    snapshot.theme.pets.enabled = true;
    assert_eq!(dashboard_mode(&snapshot), DashboardMode::Pet);
}

fn copilot_panel() -> crate::store::snapshot::SidebarProviderPanel {
    let definition = crate::agents::spec_by_kind("copilot").expect("copilot definition");
    let emblem = crate::agents::emblem_for("copilot");
    let mut panel = provider_panel(
        "copilot",
        definition.display_name,
        definition.brand.color,
        false,
        false,
        None,
    );
    panel.art = emblem.lines;
    panel.art_tints = emblem.tints;
    panel
}

#[test]
fn copilot_crest_rides_tabbed_header_without_growing_the_block() {
    let theme = Theme::fixed(false);
    let copilot = [copilot_panel()];
    let claude = [provider_panel("claude", "Claude", 173, false, false, None)];
    let copilot_lines = Dashboard::tabbed(&theme, &copilot).lines();
    let claude_lines = Dashboard::tabbed(&theme, &claude).lines();
    let texts = line_texts(&copilot_lines);
    let header = texts
        .iter()
        .find(|line| line.contains("v2.1.158"))
        .expect("tabbed provider header");

    assert!(header.contains(" ╭─╮╭─╮"), "{header}");
    assert_eq!(copilot_lines.len(), claude_lines.len());
}

#[test]
fn copilot_crest_uses_wide_stacked_spacer() {
    let theme = Theme::fixed(false);
    let texts = Dashboard::stacked(&theme, &[copilot_panel()]).texts();
    let crest = texts
        .iter()
        .position(|line| line.contains("╭─╮╭─╮"))
        .expect("copilot crest");

    assert!(
        texts[crest].starts_with(" ╭─╮╭─╮")
            && texts[crest + 1].starts_with(" ╰─╯╰─╯")
            && texts[crest + 2].starts_with(" █ ▘▝ █")
            && texts[crest + 3].starts_with("  ▔▔▔▔"),
        "{}",
        texts.join("\n")
    );
}

#[test]
fn theme_supplied_narrow_art_centers_as_one_emblem() {
    let theme = Theme::fixed(false);
    let mut panel = copilot_panel();
    panel.art = vec!["xxx".to_owned(), " x ".to_owned()];
    panel.art_tints.clear();

    let texts = Dashboard::stacked(&theme, &[panel]).texts();

    assert!(texts.iter().any(|line| line.starts_with("   xxx   ")));
    assert!(texts.iter().any(|line| line.starts_with("    x    ")));
}

#[test]
fn copilot_art_spans_use_catalog_tints_over_the_brand_tone() {
    let theme = Theme::fixed(false);
    let lines = Dashboard::stacked(&theme, &[copilot_panel()]).lines();
    let art_spans = lines
        .iter()
        .flat_map(|line| &line.spans)
        .collect::<Vec<_>>();
    let tint = |part: &str| {
        art_spans
            .iter()
            .find(|span| span.content.contains(part))
            .unwrap_or_else(|| panic!("{part} span"))
            .style
            .fg
    };

    assert_eq!(tint("╭─╮╭─╮"), Some(Color::Indexed(33)), "goggle");
    assert_eq!(tint("▘▝"), Some(Color::Indexed(84)), "eyes");
    assert_eq!(tint("█"), Some(Color::Indexed(140)), "head");
}

#[test]
fn copilot_art_stays_out_of_narrow_headers() {
    let theme = Theme::fixed(false);
    let rendered = Dashboard::tabbed(&theme, &[copilot_panel()])
        .width(33)
        .text();

    assert!(!rendered.contains("╭─╮╭─╮"), "{rendered}");
    assert!(!rendered.contains("╰─╯╰─╯"), "{rendered}");
}

/// The pinned per-provider dashboard, tabbed: the tab rail names every account
/// (the active tab a brand-filled chip set into the top hairline), and only the
/// active provider's block paints — here the selection-derived Claude tab, a metered block (the
/// de-named header with plan and version indented to the stats column, the
/// `⇅ rc` flag pinned top-right; the brand emblem; the `◎` session count
/// leading today's stats; 5h/7d "mana" bars draining toward their resets). The
/// other account stays a dim label resting in the rail, its block off screen.
#[test]
fn render_provider_dashboard_pins_panel_with_bars_and_rc_flag() {
    let rendered = snapshot_to_screen(&tabbed_provider_snapshot(), 54, 34);

    // The tab rail names both accounts set into the line; the active chip is a
    // styled span, so text snapshots only pin the semantic labels.
    assert!(
        rendered.contains("Claude") && rendered.contains("Codex"),
        "both tabs rest in the rail:\n{rendered}"
    );
    // The metered Claude block: the rail names the account, so the header
    // drops the name and reads plan-first with the `⇅ rc` remote-control flag
    // pinned to the top-right corner, then today's stats and 5h/7d budget bars
    // paint beside the emblem.
    assert!(rendered.contains("Claude Max · v2.1.158"), "{rendered}");
    assert!(
        !rendered.contains("Claude v2.1.158"),
        "the tabbed header repeats no product name:\n{rendered}"
    );
    assert!(
        rendered.contains("⇅ rc"),
        "rc flag pinned right:\n{rendered}"
    );
    assert!(rendered.contains('◎'), "the session count:\n{rendered}");
    assert!(rendered.contains("5h"), "{rendered}");
    assert!(rendered.contains("7d"), "{rendered}");
    assert!(rendered.contains('▰'), "a draining mana bar:\n{rendered}");
    assert!(rendered.contains('↻'), "a reset countdown:\n{rendered}");
    // The inactive Codex block stays off screen — only its tab label shows.
    assert!(
        !rendered.contains("Codex v0.135.0"),
        "the inactive tab paints no block:\n{rendered}"
    );
    assert!(!rendered.contains('∞'), "no unmetered bar:\n{rendered}");
    assert_snapshot("provider_dashboard", rendered);
}

#[test]
fn render_provider_dashboard_marks_a_down_rc_host_in_alarm_color() {
    let theme = Theme::fixed(false);
    let mut panel = provider_panel("claude", "Claude", 173, false, true, None);
    panel.remote_control = crate::store::snapshot::RemoteControlBadge::Down;

    let lines = Dashboard::stacked(&theme, &[panel]).lines();
    let flag = lines
        .iter()
        .flat_map(|line| &line.spans)
        .find(|span| span.content.contains("⇅ rc"))
        .expect("remote-control flag");

    assert_eq!(flag.style, theme.alarm(Modifier::BOLD));
}

/// Regression (`fix(budget): keep daily caps config-armed and quiet`): a cap
/// that is merely close stays out of the headline, and only a parked account
/// spells the cap out beside the day's spend.
#[test]
fn daily_cap_details_surface_only_while_parked() {
    let theme = Theme::default();
    for (spend_usd, parked, expected) in [(9.5, false, "$3.50"), (10.25, true, "$10.25 of $10/day")]
    {
        let mut panels = two_provider_panels();
        panels[0].day_budget = Some(crate::store::snapshot::DailyBudgetView {
            cap_usd: 10.0,
            spend_usd,
            parked,
        });
        let rendered = Dashboard::stacked(&theme, &panels[..1]).text();

        assert!(rendered.contains(expected), "{expected}: {rendered}");
        if !parked {
            assert!(
                !rendered.contains(" of "),
                "healthy cap stays quiet: {rendered}"
            );
        }
    }
}

#[test]
fn ledgerless_provider_renders_placeholder_stats_row() {
    let theme = Theme::default();
    let mut antigravity = provider_panel("antigravity", "Antigravity", 33, true, false, None);
    antigravity.active_sessions = 1;
    antigravity.spending = None;
    let rendered = Dashboard::stacked(&theme, &[antigravity]).text();

    assert!(rendered.contains("◎ 1"), "{rendered}");
    for placeholder in ["◇ –", "↘ –", "↗ –", "◌ –", "$ –"] {
        assert!(rendered.contains(placeholder), "{placeholder}: {rendered}");
    }
    assert!(!rendered.contains("$0.00"), "{rendered}");

    let accounted = [provider_panel("claude", "Claude", 173, true, false, None)];
    let rendered = Dashboard::stacked(&theme, &accounted).text();

    assert!(rendered.contains("◎ 12"), "{rendered}");
    assert!(rendered.contains("◇ 498k"), "{rendered}");
    assert!(rendered.contains("$3.50"), "{rendered}");
}

#[test]
fn render_provider_dashboard_pins_empty_state_template() {
    let theme = Theme::fixed(false);
    let mut claude = provider_panel("claude", "Claude", 173, true, false, None);
    claude.active_sessions = 0;
    claude.spending = None;
    claude.window_placeholders = vec!["5h".to_owned(), "7d".to_owned()];
    let rendered = Dashboard::stacked(&theme, &[claude]).text();

    assert!(rendered.contains("Claude v2.1.158 · Claude Max"));
    assert!(rendered.contains("◎ 0  ◇ – ↘ – ↗ – ◌ –"));
    assert!(rendered.contains("$ –"));
    assert!(rendered.contains("5h"));
    assert!(rendered.contains("7d"));
    assert!(!rendered.contains('↻'));
    assert_snapshot("provider_dashboard_empty_state", rendered);
}
/// The dashboard paints the Codex block whichever way the active tab is
/// derived: a manual pick (`←`/`→` or a click on the label) swaps the chip onto
/// `Codex` — fill alone, no glyph moves in the rail — and with no manual pick
/// the focus follows the selected pane's provider. Either way the unmetered
/// block (the `∞` icon at the front, a full `▰` bar, no countdown) paints
/// where Claude's was and the Claude block stays off screen.
#[test]
fn render_provider_dashboard_codex_tab_paints_however_derived() {
    // A manual tab pick over a selected Claude row swaps to the Codex block.
    let snapshot = tabbed_provider_snapshot();
    let ui = UiState {
        dashboard_tab: Some(DashboardTab {
            kind: "codex".to_owned(),
            derived_at_start: Some("claude".to_owned()),
        }),
        ..Default::default()
    };
    let rendered = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, 54, 34);

    assert!(rendered.contains("ChatGPT Pro · v0.135.0"), "{rendered}");
    assert!(rendered.contains('∞'), "infinity at the front:\n{rendered}");
    assert!(rendered.contains('▰'), "the full ∞ bar:\n{rendered}");
    assert!(!rendered.contains('▱'), "no empty track:\n{rendered}");
    assert!(
        !rendered.contains("Claude Max"),
        "the unpicked block stays off screen:\n{rendered}"
    );
    assert_snapshot("provider_dashboard_codex_tab", rendered);

    // With no manual pick, the focus follows a selected Codex row to the same
    // block, however the panels are ordered.
    let codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("add tests"),
    );
    let mut snapshot = snapshot_with(vec![codex]);
    snapshot.providers = two_provider_panels();
    snapshot.theme.display.provider_tabs = crate::config::ProviderTabsMode::Always;
    let rendered = snapshot_to_screen(&snapshot, 54, 34);

    assert!(rendered.contains("ChatGPT Pro · v0.135.0"), "{rendered}");
    assert!(
        !rendered.contains("Claude Max"),
        "the other block stays off screen:\n{rendered}"
    );
}

/// The `↻` credit header is Codex-only and needs credits to report: a non-Codex
/// account, a zero count, and an absent record all leave the header off.
#[test]
fn codex_reset_credit_header_shows_only_when_actionable() {
    let theme = Theme::fixed(false);
    let credits = |count| {
        Some(crate::ResetCredits {
            count,
            soonest_expiry: Some(fixed_now() + Duration::from_secs(36 * 3_600)),
            expiries: Vec::new(),
        })
    };
    let mut codex = provider_panel("codex", "Codex", 33, false, false, None);
    codex.reset_credits = credits(3);
    let mut non_codex = provider_panel("claude", "Claude", 173, false, false, None);
    non_codex.reset_credits = credits(3);
    let mut zero = provider_panel("codex", "Codex", 33, false, false, None);
    zero.reset_credits = credits(0);
    let absent = provider_panel("codex", "Codex", 33, false, false, None);

    assert!(
        Dashboard::stacked(&theme, &[codex]).text().contains("↻ 3"),
        "a Codex account with credits shows the header"
    );
    for (panel, why) in [
        (non_codex, "a non-Codex account"),
        (zero, "a zero credit count"),
        (absent, "an absent credit record"),
    ] {
        let rendered = Dashboard::stacked(&theme, &[panel]).text();
        assert!(
            !rendered.contains('↻'),
            "{why} hides the header:\n{rendered}"
        );
    }
}

#[test]
fn codex_reset_marker_blinks_only_while_a_window_is_spent() {
    let theme = Theme::fixed(false);
    let marker_style = |panel: &crate::store::snapshot::SidebarProviderPanel, phase| {
        Dashboard::stacked(&theme, std::slice::from_ref(panel))
            .phase(phase)
            .lines()
            .into_iter()
            .flat_map(|line| line.spans)
            .find(|span| span.content.as_ref() == "↻")
            .map(|span| span.style)
            .expect("reset marker")
    };
    let styles = |panel: &crate::store::snapshot::SidebarProviderPanel| {
        (0..32)
            .map(|phase| marker_style(panel, phase))
            .collect::<Vec<_>>()
    };

    let mut spent = provider_panel("codex", "Codex", 33, true, false, Some((100, 20)));
    spent.reset_credits = Some(crate::ResetCredits {
        count: 2,
        soonest_expiry: Some(fixed_now() + Duration::from_secs(36 * 3_600)),
        expiries: Vec::new(),
    });
    assert!(
        styles(&spent).windows(2).any(|pair| pair[0] != pair[1]),
        "spent-window marker changes style across animation phases"
    );

    let mut unspent = spent.clone();
    unspent.windows[0].used_percentage = Some(99);
    let mut undated = spent;
    undated.windows[0].resets_at = None;
    for (panel, why) in [
        (unspent, "an unspent window"),
        (undated, "an undated window"),
    ] {
        let steady = styles(&panel);
        assert!(
            steady.iter().all(|style| *style == steady[0]),
            "{why} holds the marker steady"
        );
    }
}

#[test]
fn reset_expiry_heat_amount_matches_expiry_boundaries() {
    fn assert_amount(hours: f64, expected: f32) {
        let actual = reset_expiry_heat_amount(hours).expect("heat amount");
        assert!(
            (actual - expected).abs() < 0.001,
            "{hours}h => {actual}, expected {expected}"
        );
    }

    assert_amount(0.0, 1.0);
    assert_amount(24.0, 1.0);
    assert_amount(48.0, 2.0 / 3.0);
    assert_amount(72.0, 1.0 / 3.0);
    assert_amount(167.999, 0.0);
    assert_eq!(reset_expiry_heat_amount(168.0), None);
    assert_amount(-1.0, 1.0);
}

/// The pets body carries the sprite and its caption, and claims no click
/// targets. Under `NO_COLOR` the sprite drops out entirely while the caption —
/// the part that carries meaning without color — stays.
#[test]
fn pets_dashboard_body_renders_sprite_and_caption() {
    for (no_color, sprite_visible) in [(false, true), (true, false)] {
        let theme = Theme::fixed(no_color);
        let pet = cell_pet(1, 1, "all caught up");
        let dashboard = Dashboard::pets(&theme, &[]).pet(&pet).width(24);
        let rendered = dashboard.text();

        assert_eq!(
            rendered.contains('▀'),
            sprite_visible,
            "no_color={no_color} sprite:\n{rendered}"
        );
        assert!(rendered.contains("all caught up"), "{rendered}");
        assert!(!rendered.contains("Pets"), "{rendered}");
        assert!(dashboard.hits().is_empty());
    }
}

/// Regression (`fix(pets): render pixel placeholders in ratatui buffer`): the
/// kitty pixel path paints placeholder clusters whose diacritics distinguish
/// row from column, and never falls back to cell-art glyphs.
#[test]
fn render_provider_dashboard_pixel_pet_renders_placeholder_clusters() {
    let theme = Theme::fixed(false);
    let providers = two_provider_panels();
    let pet = pixel_pet();
    let rendered = Dashboard::pets(&theme, &providers)
        .active("claude")
        .pet(&pet)
        .width(66)
        .text();

    for (row, col, why) in [
        (0, 0, "pixel placeholder cells render"),
        (0, 1, "placeholder columns carry distinct diacritics"),
        (1, 0, "placeholder rows carry distinct diacritics"),
    ] {
        assert!(
            rendered.contains(&crate::sidebar_pane::pixel::placeholder_cluster(row, col)),
            "{why}:\n{rendered}"
        );
    }
    assert!(
        !rendered.contains('▀'),
        "cell-art glyphs stay off pixel path:\n{rendered}"
    );
}

#[test]
fn render_provider_dashboard_pixel_pet_buffer_cells_carry_image_id_color() {
    let mut snapshot = tabbed_provider_snapshot();
    snapshot.theme.pets.enabled = true;
    let ui = UiState {
        pet: Some(pixel_pet()),
        ..Default::default()
    };

    let backend = TestBackend::new(54, 34);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut ui = fixed_theme_ui(&snapshot, &ui);
    draw_to_terminal_with_ui(&mut terminal, &snapshot, None, &mut ui).unwrap();
    let buffer = terminal.backend().buffer();
    let first = crate::sidebar_pane::pixel::placeholder_cluster(0, 0);
    let (x, y) = (0..buffer.area.height)
        .flat_map(|y| (0..buffer.area.width).map(move |x| (x, y)))
        .find(|(x, y)| buffer[(*x, *y)].symbol() == first)
        .expect("first placeholder cell");

    assert_eq!(buffer[(x, y)].fg, Color::Rgb(0x12, 0x34, 0x56));
    assert_eq!(
        buffer[(x + 1, y)].symbol(),
        crate::sidebar_pane::pixel::placeholder_cluster(0, 1)
    );
    assert_eq!(
        buffer[(x, y + 1)].symbol(),
        crate::sidebar_pane::pixel::placeholder_cluster(1, 0)
    );
    assert_eq!(buffer[(x + 1, y)].fg, Color::Rgb(0x12, 0x34, 0x56));
}

#[test]
fn render_provider_dashboard_pixel_pet_keeps_total_spacer_row() {
    let theme = Theme::fixed(false);
    let providers = two_provider_panels();
    let pet = pixel_pet();
    let texts = Dashboard::pets(&theme, &providers)
        .active("claude")
        .pet(&pet)
        .width(66)
        .texts();
    let total_index = texts
        .iter()
        .position(|line| line.contains("Total:"))
        .expect("total delimiter");
    let above_total = texts
        .get(total_index.saturating_sub(1))
        .expect("row above total");
    let provider_above_total = above_total.chars().take(54).collect::<String>();

    assert!(
        provider_above_total.trim().is_empty(),
        "pixel mode keeps the spacer above Total:\n{}",
        texts.join("\n")
    );
}

#[test]
fn render_provider_dashboard_balances_totals_beside_pet() {
    let theme = Theme::fixed(false);
    let providers = two_provider_panels();
    let pet = cell_pet(
        usize::from(crate::sidebar_pane::pets::DASHBOARD_CELL_PET.rows),
        2,
        "all caught up",
    );
    let fleet = crate::SpendTally {
        week: crate::SpendWindow {
            usd: 44.20,
            tokens: 4_200_000,
            input: 3_100_000,
            output: 1_100_000,
            cache_read: 9_900_000,
            sessions: 44,
            ..Default::default()
        },
        month: crate::SpendWindow {
            usd: 101.99,
            tokens: 9_100_000,
            input: 6_100_000,
            output: 3_000_000,
            cache_read: 20_000_000,
            sessions: 101,
            ..Default::default()
        },
        year: crate::SpendWindow {
            usd: 101.99,
            tokens: 9_100_000,
            ..Default::default()
        },
        ..Default::default()
    };

    let dashboard = Dashboard::pets(&theme, &providers)
        .active("claude")
        .pet(&pet)
        .fleet(&fleet)
        .width(52);
    let texts = dashboard.texts();
    let rendered = texts.join("\n");

    assert!(!rendered.contains("Pets"), "{rendered}");
    assert_eq!(
        rendered.chars().filter(|ch| *ch == '▀').count(),
        18,
        "one height-matched pet grid is zipped onto the active block:\n{rendered}"
    );
    assert_eq!(
        texts
            .iter()
            .skip(2)
            .filter(|line| line.contains('▀'))
            .count(),
        9,
        "the pet body keeps its fixed art height:\n{rendered}"
    );
    let last_pet_body_row = texts
        .iter()
        .rposition(|line| line.contains('▀'))
        .expect("pet body row");
    assert_eq!(
        last_pet_body_row + 1,
        texts.len() - 1,
        "cell art leaves one trailing blank row below it:\n{rendered}"
    );
    assert!(
        !texts.last().expect("bottom row").contains('▀'),
        "bottom row is pet breathing room:\n{rendered}"
    );
    assert_eq!(
        dashboard
            .hits()
            .iter()
            .map(provider_tab_kind)
            .collect::<Vec<_>>(),
        vec!["claude", "codex"]
    );
    assert!(
        !rendered.contains("T:"),
        "pet dashboard starts from the main provider stats layout:\n{rendered}"
    );
    let sessions_index = texts
        .iter()
        .position(|line| line.contains('◎') && line.contains("12"))
        .expect("today sessions row");
    let stats = &texts[sessions_index];
    assert!(stats.contains(" ▐▛███▜▌"), "{stats}");
    assert!(stats.contains("◇ 498k"), "{stats}");
    assert!(stats.contains("$3.50"), "{stats}");
    assert!(rendered.contains("Total:"), "{rendered}");
    assert!(rendered.contains("W: ◎"), "{rendered}");
    assert!(rendered.contains("M: ◎"), "{rendered}");
    let total_index = texts
        .iter()
        .position(|line| line.contains("Total:"))
        .expect("total delimiter");
    let above_total = texts
        .get(total_index.saturating_sub(1))
        .expect("row above total");
    let provider_above_total = above_total.split('▀').next().unwrap_or(above_total).trim();
    assert!(
        provider_above_total.is_empty(),
        "normal layout has a blank provider row above Total:\n{rendered}"
    );
    let week_tokens = texts
        .iter()
        .find(|line| line.contains("W: ◎"))
        .expect("week token row");
    let week_pet_col = week_tokens.find('▀').expect("pet column on week row");
    assert!(
        week_tokens.trim_start().starts_with("W: ◎  44"),
        "history session group starts left:\n{rendered}"
    );
    assert!(
        week_tokens[..week_pet_col.saturating_sub(1)]
            .trim_end()
            .ends_with("9.9M"),
        "history token rows end at the pet gap:\n{rendered}"
    );
    let total_usd = texts
        .iter()
        .find(|line| line.contains("W: $"))
        .expect("total USD row");
    assert!(
        total_usd.contains('▀'),
        "bottom-aligned cell-art pet reaches the final total row:\n{rendered}"
    );
    let total_pet_col = total_usd.find('▀').expect("pet column on total USD row");
    let provider_usd = total_usd[..total_pet_col.saturating_sub(1)].trim_end();
    assert!(
        provider_usd.trim_start().starts_with("W: $44.20"),
        "week USD starts left:\n{rendered}"
    );
    assert!(
        provider_usd.ends_with("M: $101.99"),
        "month USD is pinned right:\n{rendered}"
    );
    assert!(
        !total_usd.contains('·'),
        "normal total USD row uses spacing, not a dot:\n{rendered}"
    );
    let header = texts
        .get(sessions_index.saturating_sub(1))
        .expect("provider header");
    assert!(
        !header.contains("▐▛███▜▌"),
        "normal layout starts art one row below the header:\n{rendered}"
    );
    let spend = stats.find("$3.50").expect("headline spend");
    let pet_col = stats.find('▀').expect("pet column");
    assert!(spend < pet_col, "$ stays left of the pet column: {stats}");
}

/// Regressions (`fix(sidebar): align pet provider dashboard rows`, `fix(pets):
/// widen captions and move config to theme`): the caption is right-aligned with
/// exactly three trailing cells, and a long one runs the whole dashboard row
/// rather than being clipped to the pet column.
#[test]
fn pet_caption_is_right_aligned_with_three_trailing_cells() {
    let theme = Theme::fixed(false);
    let providers = two_provider_panels();
    for (text, ends_with) in [
        ("ready", "    ready   "),
        ("rough patch - take a look", "take a look   "),
    ] {
        let pet = cell_pet(3, 12, text);
        let texts = Dashboard::pets(&theme, &providers)
            .active("claude")
            .pet(&pet)
            .width(66)
            .texts();
        let caption = texts.get(1).expect("caption line");

        assert!(
            caption.contains(text),
            "caption is not clipped:\n{caption:?}"
        );
        assert!(
            caption.ends_with(ends_with),
            "caption is right-aligned with three trailing cells:\n{caption:?}"
        );
    }
}

#[test]
fn render_provider_dashboard_without_pet_uses_main_stats_body() {
    let theme = Theme::fixed(false);
    let mut providers = two_provider_panels();
    providers[0].plan = Some("Claude Max Enterprise".to_owned());
    let rendered = Dashboard::tabbed(&theme, &providers)
        .active("claude")
        .width(52)
        .text();

    let stats = rendered
        .lines()
        .find(|line| line.contains('◎') && line.contains("$3.50"))
        .expect("headline stats row");
    assert!(stats.contains("◇ 498k"), "{stats}");
    assert!(stats.contains("$3.50"), "{stats}");
    assert!(
        !rendered.contains("T:"),
        "no-pets dashboard keeps the main layout:\n{rendered}"
    );
}

#[test]
fn render_provider_dashboard_narrow_hides_io_tokens_and_version() {
    let theme = Theme::fixed(false);
    let mut providers = two_provider_panels();
    providers[0].plan = Some("Claude Max Enterprise".to_owned());
    let rendered = Dashboard::pets(&theme, &providers)
        .active("claude")
        .width(38)
        .text();

    assert!(
        !rendered.contains("v2.1.158"),
        "narrow header drops the version before truncating the plan:\n{rendered}"
    );
    assert!(rendered.contains("Claude Max Enterprise"), "{rendered}");
    assert!(
        !rendered.contains('↘') && !rendered.contains('↗'),
        "narrow token stats hide input/output splits:\n{rendered}"
    );
    assert!(rendered.contains("◇ 498k"), "{rendered}");
    assert!(rendered.contains("◌ 68k"), "{rendered}");
    assert!(rendered.contains("$3.50"), "{rendered}");
}

#[test]
fn render_scroll_keeps_gap_above_provider_dashboard() {
    let mut snapshot = overflowing_fleet();
    snapshot.providers = two_provider_panels();
    snapshot.theme.display.provider_tabs = crate::config::ProviderTabsMode::Always;
    let theme = Theme::for_sidebar(&snapshot.theme);
    let ui = UiState {
        scroll_offset: 6,
        manual_scroll: Some(ManualScroll {
            selection_at_start: None,
        }),
        ..Default::default()
    };
    let frame = compose_lines(&snapshot, None, &ui, &theme, 54, 23);
    let lines = line_texts(&frame.lines);
    let rendered = lines.join("\n");
    let rail = lines
        .iter()
        .position(|line| line.contains("Claude") && line.contains("Codex"))
        .expect("the tab rail renders");

    assert!(
        lines[rail - 1].trim().is_empty(),
        "a fixed blank separates scrolled cards from the dashboard:\n{rendered}"
    );
    assert!(
        !lines[rail - 2].trim().is_empty(),
        "the separator is not body padding; cards reach it while overflowing:\n{rendered}"
    );
    let provider_hits = frame
        .interactions
        .regions()
        .iter()
        .filter(|hit| matches!(hit.target, HitTarget::ProviderTab(_)))
        .collect::<Vec<_>>();
    assert!(
        !provider_hits.is_empty() && provider_hits.iter().all(|hit| hit.rows.start == rail),
        "dashboard tab hits stay on the rail row after the separator ({} hits):\n{rendered}",
        provider_hits.len()
    );
}
/// In `auto` mode, two providers stay stacked: both account blocks paint at
/// once, separated by a blank row, with no tab rail or tab hit surface.
#[test]
fn render_provider_dashboard_auto_stacks_two_provider_blocks() {
    let mut snapshot = tabbed_provider_snapshot();
    snapshot.theme.display.provider_tabs = crate::config::ProviderTabsMode::Auto;
    let rendered = snapshot_to_screen(&snapshot, 54, 40);

    assert!(
        !rendered.contains("─ Claude ─") && !rendered.contains("─ Codex ─"),
        "stacked mode paints no tab rail:\n{rendered}"
    );
    assert!(
        rendered.contains("Claude v2.1.158 · Claude Max"),
        "{rendered}"
    );
    assert!(
        rendered.contains("Codex v0.135.0 · ChatGPT Pro"),
        "{rendered}"
    );
    assert!(rendered.contains("5h"), "stacked 5h label:\n{rendered}");
    assert!(rendered.contains("7d"), "stacked 7d label:\n{rendered}");
    assert!(
        rendered.contains('∞'),
        "codex block paints too:\n{rendered}"
    );
    let lines: Vec<&str> = rendered.lines().collect();
    let codex_header = lines
        .iter()
        .position(|line| line.contains("Codex v0.135.0"))
        .expect("codex header");
    assert!(
        lines[codex_header - 1].trim().is_empty(),
        "a blank row separates stacked provider blocks:\n{rendered}"
    );
}
/// A dashboard with a single account keeps its block bare — no tab rail, a
/// plain hairline: there is nothing to switch to, so the header line alone
/// names the provider (the one place the de-named tabbed header never applies).
#[test]
fn render_single_provider_dashboard_has_no_tab_rail() {
    let mut snapshot = tabbed_provider_snapshot();
    snapshot.providers = vec![provider_panel(
        "claude",
        "Claude",
        173,
        true,
        true,
        Some((25, 40)),
    )];
    let rendered = snapshot_to_screen(&snapshot, 54, 34);
    assert!(
        !rendered.contains('┤') && !rendered.contains('├'),
        "one account, plain rule, no rail:\n{rendered}"
    );
    assert!(
        rendered.contains("Claude v2.1.158 · Claude Max"),
        "{rendered}"
    );
}

#[test]
fn render_provider_dashboard_shows_version_placeholder_when_unknown() {
    let mut snapshot = tabbed_provider_snapshot();
    snapshot.providers[0].version = None;
    snapshot.providers[0].plan = None;
    let rendered = snapshot_to_screen(&snapshot, 54, 34);

    assert!(
        rendered.contains("v?"),
        "the tabbed active header carries a version placeholder:\n{rendered}"
    );
    assert!(
        !rendered.contains("Claude Max ·"),
        "unknown plan stays absent:\n{rendered}"
    );

    let mut snapshot = snapshot_with(Vec::new());
    snapshot.providers = vec![provider_panel(
        "claude",
        "Claude",
        173,
        true,
        false,
        Some((25, 40)),
    )];
    snapshot.providers[0].version = None;
    snapshot.providers[0].plan = None;
    let rendered = snapshot_to_screen(&snapshot, 54, 34);

    assert!(
        rendered.contains("Claude v?"),
        "the untabbed header carries the version placeholder:\n{rendered}"
    );
    assert!(
        !rendered.contains("Claude Max"),
        "unknown plan stays absent:\n{rendered}"
    );
}
