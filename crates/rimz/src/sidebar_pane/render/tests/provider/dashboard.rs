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
    snapshot.sidebar.provider_tabs = crate::config::ProviderTabsMode::Always;
    let rendered = snapshot_to_screen(&snapshot, 54, 34);

    // The tab rail names both accounts set into the line; the active chip is a
    // styled span, so text snapshots only pin the semantic labels.
    assert!(
        rendered.contains("Claude") && rendered.contains("Codex"),
        "both tabs rest in the rail:\n{rendered}"
    );
    // The metered Claude block: the rail names the account, so the header
    // drops the name and reads plan-first with the `⇅ rc` remote-control flag
    // pinned to the top-right corner, the stats line leads with today's `◎`
    // session count, then the 5h/7d budget bars drain.
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
    snapshot.sidebar.provider_tabs = crate::config::ProviderTabsMode::Always;
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
    snapshot.sidebar.provider_tabs = crate::config::ProviderTabsMode::Always;
    let rendered = snapshot_to_screen(&snapshot, 54, 34);

    assert!(rendered.contains("ChatGPT Pro · v0.135.0"), "{rendered}");
    assert!(
        !rendered.contains("Claude Max"),
        "the other block stays off screen:\n{rendered}"
    );
}
#[test]
fn render_scroll_keeps_gap_above_provider_dashboard() {
    let mut snapshot = overflowing_fleet();
    snapshot.providers = two_provider_panels();
    snapshot.sidebar.provider_tabs = crate::config::ProviderTabsMode::Always;
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
        20,
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
    snapshot.sidebar.provider_tabs = crate::config::ProviderTabsMode::Always;
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
