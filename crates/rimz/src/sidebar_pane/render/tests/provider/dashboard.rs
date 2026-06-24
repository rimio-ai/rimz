use super::*;

/// The pinned per-provider dashboard, tabbed: the tab rail names every account
/// (the active tab a brand-filled chip set into the top hairline), and only the
/// active provider's block paints — here the selection-derived Claude tab, a metered block (the
/// de-named header with plan and version indented to the stats column, the
/// `⇅ rc` flag pinned top-right; the brand emblem; the `◎` session count
/// leading today's stats; 5h/7d "mana" bars draining toward their resets). The
/// other account stays a dim label resting in the rail, its block off screen.
#[test]
fn render_provider_dashboard_pins_panel_with_bars_and_rc_flag() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    claude.context = Some(claude_context(fixed_now()));
    let mut snapshot = snapshot_with(Vec::new(), vec![claude]);
    snapshot.providers = two_provider_panels();
    snapshot.theme.display.provider_tabs = crate::config::ProviderTabsMode::Always;
    let rendered = snapshot_to_screen(&snapshot, 54, 34);

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
/// The dashboard paints the Codex block whichever way the active tab is
/// derived: a manual pick (`←`/`→` or a click on the label) swaps the chip onto
/// `Codex` — fill alone, no glyph moves in the rail — and with no manual pick
/// the focus follows the selected pane's provider. Either way the unmetered
/// block (the `∞` icon at the front, an empty `▱` track, no countdown) paints
/// where Claude's was and the Claude block stays off screen.
#[test]
fn render_provider_dashboard_codex_tab_paints_however_derived() {
    // A manual tab pick over a selected Claude row swaps to the Codex block.
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    claude.context = Some(claude_context(fixed_now()));
    let mut snapshot = snapshot_with(Vec::new(), vec![claude]);
    snapshot.providers = two_provider_panels();
    snapshot.theme.display.provider_tabs = crate::config::ProviderTabsMode::Always;
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
    assert!(rendered.contains('▱'), "the empty ∞ track:\n{rendered}");
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
    let mut snapshot = snapshot_with(Vec::new(), vec![codex]);
    snapshot.providers = two_provider_panels();
    snapshot.theme.display.provider_tabs = crate::config::ProviderTabsMode::Always;
    let rendered = snapshot_to_screen(&snapshot, 54, 34);

    assert!(rendered.contains("ChatGPT Pro · v0.135.0"), "{rendered}");
    assert!(
        !rendered.contains("Claude Max"),
        "the other block stays off screen:\n{rendered}"
    );
}

#[test]
fn render_pets_dashboard_body_uses_pet_view() {
    let theme = Theme::fixed(false);
    let pet = crate::sidebar_pane::pets::PetView {
        grid: Some(vec![vec![crate::sidebar_pane::pets::PetCell {
            ch: '▀',
            fg: Color::Rgb(200, 20, 20),
            bg: Color::Rgb(20, 20, 200),
        }]]),
        caption: Some("all caught up".to_owned()),
        loading: false,
        action: crate::sidebar_pane::pets::PetAction::Idle,
        active_track: "idle",
    };

    let (lines, hits) = dashboard_panel_lines(
        &theme,
        &[],
        None,
        true,
        None,
        Some(&pet),
        true,
        24,
        &crate::config::BudgetBarConfig::default(),
        fixed_now(),
    );
    let rendered = line_texts(&lines).join("\n");

    assert!(!rendered.contains("Pets"), "{rendered}");
    assert!(rendered.contains('▀'), "sprite cells render:\n{rendered}");
    assert!(rendered.contains("all caught up"), "{rendered}");
    assert!(hits.is_empty());
}

#[test]
fn render_pets_dashboard_body_drops_sprite_under_no_color() {
    let theme = Theme::fixed(true);
    let pet = crate::sidebar_pane::pets::PetView {
        grid: Some(vec![vec![crate::sidebar_pane::pets::PetCell {
            ch: '▀',
            fg: Color::Rgb(200, 20, 20),
            bg: Color::Rgb(20, 20, 200),
        }]]),
        caption: Some("someone needs you".to_owned()),
        loading: false,
        action: crate::sidebar_pane::pets::PetAction::Ask,
        active_track: "ask",
    };

    let (lines, _) = dashboard_panel_lines(
        &theme,
        &[],
        None,
        true,
        None,
        Some(&pet),
        true,
        24,
        &crate::config::BudgetBarConfig::default(),
        fixed_now(),
    );
    let rendered = line_texts(&lines).join("\n");

    assert!(!rendered.contains('▀'), "sprite is omitted:\n{rendered}");
    assert!(rendered.contains("someone needs you"), "{rendered}");
}

#[test]
fn render_provider_dashboard_balances_totals_beside_pet() {
    let theme = Theme::fixed(false);
    let providers = two_provider_panels();
    let cell = crate::sidebar_pane::pets::PetCell {
        ch: '▀',
        fg: Color::Rgb(200, 20, 20),
        bg: Color::Rgb(20, 20, 200),
    };
    let pet = crate::sidebar_pane::pets::PetView {
        grid: Some((0..8).map(|_| vec![cell.clone(), cell.clone()]).collect()),
        caption: Some("all caught up".to_owned()),
        loading: false,
        action: crate::sidebar_pane::pets::PetAction::Idle,
        active_track: "idle",
    };
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
    let active = "claude".to_owned();

    let (lines, hits) = dashboard_panel_lines(
        &theme,
        &providers,
        Some(&active),
        true,
        Some(&fleet),
        Some(&pet),
        true,
        52,
        &crate::config::BudgetBarConfig::default(),
        fixed_now(),
    );
    let texts = line_texts(&lines);
    let rendered = texts.join("\n");

    assert!(!rendered.contains("Pets"), "{rendered}");
    assert_eq!(
        rendered.chars().filter(|ch| *ch == '▀').count(),
        16,
        "one height-matched pet grid is zipped onto the active block:\n{rendered}"
    );
    assert_eq!(
        texts
            .iter()
            .skip(2)
            .filter(|line| line.contains('▀'))
            .count(),
        8,
        "the pet body keeps its height while the bottom row is blank:\n{rendered}"
    );
    assert_eq!(
        hits.iter().map(|hit| hit.kind.as_str()).collect::<Vec<_>>(),
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
    let provider_usd = total_usd.trim_end();
    assert!(
        !total_usd.contains('▀'),
        "normal layout leaves an empty pet row below the sprite:\n{rendered}"
    );
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

#[test]
fn render_provider_dashboard_pet_caption_leaves_inner_gap() {
    let theme = Theme::fixed(false);
    let providers = two_provider_panels();
    let cell = crate::sidebar_pane::pets::PetCell {
        ch: '▀',
        fg: Color::Rgb(200, 20, 20),
        bg: Color::Rgb(20, 20, 200),
    };
    let pet = crate::sidebar_pane::pets::PetView {
        grid: Some((0..3).map(|_| vec![cell.clone(); 12]).collect()),
        caption: Some("ready".to_owned()),
        loading: false,
        action: crate::sidebar_pane::pets::PetAction::Idle,
        active_track: "idle",
    };
    let active = "claude".to_owned();

    let (lines, _) = dashboard_panel_lines(
        &theme,
        &providers,
        Some(&active),
        true,
        None,
        Some(&pet),
        true,
        66,
        &crate::config::BudgetBarConfig::default(),
        fixed_now(),
    );
    let texts = line_texts(&lines);
    let caption = texts.get(1).expect("caption line");

    assert!(
        caption.ends_with("    ready   "),
        "caption is right-aligned with three trailing cells:\n{caption:?}"
    );
}

#[test]
fn render_provider_dashboard_pet_caption_uses_full_width() {
    let theme = Theme::fixed(false);
    let providers = two_provider_panels();
    let cell = crate::sidebar_pane::pets::PetCell {
        ch: '▀',
        fg: Color::Rgb(200, 20, 20),
        bg: Color::Rgb(20, 20, 200),
    };
    let pet = crate::sidebar_pane::pets::PetView {
        grid: Some((0..3).map(|_| vec![cell.clone(); 12]).collect()),
        caption: Some("rough patch - take a look".to_owned()),
        loading: false,
        action: crate::sidebar_pane::pets::PetAction::Failed,
        active_track: "failed",
    };
    let active = "claude".to_owned();

    let (lines, _) = dashboard_panel_lines(
        &theme,
        &providers,
        Some(&active),
        true,
        None,
        Some(&pet),
        true,
        66,
        &crate::config::BudgetBarConfig::default(),
        fixed_now(),
    );
    let texts = line_texts(&lines);
    let caption = texts.get(1).expect("caption line");

    assert!(
        caption.contains("rough patch - take a look"),
        "caption uses the full dashboard row:\n{caption:?}"
    );
    assert!(
        caption.ends_with("take a look   "),
        "caption stays right-aligned with three trailing cells:\n{caption:?}"
    );
}

#[test]
fn render_provider_dashboard_without_pet_uses_main_stats_body() {
    let theme = Theme::fixed(false);
    let mut providers = two_provider_panels();
    providers[0].plan = Some("Claude Max Enterprise".to_owned());
    let active = "claude".to_owned();

    let (lines, _) = dashboard_panel_lines(
        &theme,
        &providers,
        Some(&active),
        true,
        None,
        None,
        false,
        52,
        &crate::config::BudgetBarConfig::default(),
        fixed_now(),
    );
    let rendered = line_texts(&lines).join("\n");

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
    let active = "claude".to_owned();

    let (lines, _) = dashboard_panel_lines(
        &theme,
        &providers,
        Some(&active),
        true,
        None,
        None,
        true,
        38,
        &crate::config::BudgetBarConfig::default(),
        fixed_now(),
    );
    let rendered = line_texts(&lines).join("\n");

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
    let frame = compose_lines(
        &snapshot,
        None,
        &UiState {
            scroll_offset: 6,
            manual_scroll: Some(ManualScroll {
                selection_at_start: None,
            }),
            ..Default::default()
        },
        54,
        23,
    );
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
    assert!(
        !frame.tab_hits.is_empty() && frame.tab_hits.iter().all(|hit| hit.line == rail),
        "dashboard tab hits stay on the rail row after the separator ({} hits):\n{rendered}",
        frame.tab_hits.len()
    );
}
/// In `auto` mode, two providers stay stacked: both account blocks paint at
/// once, separated by a blank row, with no tab rail or tab hit surface.
#[test]
fn render_provider_dashboard_auto_stacks_two_provider_blocks() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    claude.context = Some(claude_context(fixed_now()));
    let mut snapshot = snapshot_with(Vec::new(), vec![claude]);
    snapshot.providers = two_provider_panels();
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
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    claude.context = Some(claude_context(fixed_now()));
    let mut snapshot = snapshot_with(Vec::new(), vec![claude]);
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
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    claude.context = Some(claude_context(fixed_now()));
    let mut snapshot = snapshot_with(Vec::new(), vec![claude]);
    snapshot.providers = two_provider_panels();
    snapshot.providers[0].version = None;
    snapshot.providers[0].plan = None;
    snapshot.theme.display.provider_tabs = crate::config::ProviderTabsMode::Always;
    let rendered = snapshot_to_screen(&snapshot, 54, 34);

    assert!(
        rendered.contains("v?"),
        "the tabbed active header carries a version placeholder:\n{rendered}"
    );
    assert!(
        !rendered.contains("Claude Max ·"),
        "unknown plan stays absent:\n{rendered}"
    );

    let mut snapshot = snapshot_with(Vec::new(), Vec::new());
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
