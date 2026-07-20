use super::*;

/// The fleet store pinned at the bottom of the dashboard: the static
/// `W:` (week) and `M:` (month) rows, each reading `◎ sessions  ◇ ↘ ↗ ◌
/// $spend` across every provider — precise one-decimal token figures and the
/// bold dollar-green spend, right-aligned into one aligned grid. Today's
/// headline stays in the cockpit's animated `$`, never repeated here.
#[test]
fn render_fleet_store_pins_week_month_rows_under_the_dashboard() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    claude.context = Some(claude_context(fixed_now()));
    let mut snapshot = snapshot_with(vec![claude]);
    snapshot.providers = vec![provider_panel(
        "claude",
        "Claude",
        173,
        true,
        true,
        Some((25, 40)),
    )];
    snapshot.value_tally = Some(crate::SpendTally {
        headline: crate::SpendWindow {
            usd: 40.23,
            tokens: 3_420_000,
            input: 420_000,
            output: 3_000_000,
            cache_write: 120_000,
            cache_read: 6_800_000,
            sessions: 12,
        },
        week: crate::SpendWindow {
            usd: 312.40,
            tokens: 21_900_000,
            input: 3_200_000,
            output: 18_700_000,
            cache_write: 900_000,
            cache_read: 51_000_000,
            sessions: 92,
        },
        month: crate::SpendWindow {
            usd: 1_240.57,
            tokens: 34_900_000,
            input: 6_200_000,
            output: 28_700_000,
            cache_write: 1_900_000,
            cache_read: 121_000_000,
            sessions: 212,
        },
        year: crate::SpendWindow {
            usd: 4_821.90,
            tokens: 50_200_000,
            input: 10_200_000,
            output: 40_000_000,
            cache_write: 3_000_000,
            cache_read: 210_000_000,
            sessions: 980,
        },
    });
    snapshot.workspace_value_tally = Some(crate::SpendTally {
        headline: crate::SpendWindow {
            usd: 8.25,
            tokens: 1_210_000,
            input: 210_000,
            output: 1_000_000,
            cache_write: 80_000,
            cache_read: 2_400_000,
            sessions: 4,
        },
        week: crate::SpendWindow {
            usd: 8.25,
            tokens: 1_210_000,
            input: 210_000,
            output: 1_000_000,
            cache_write: 80_000,
            cache_read: 2_400_000,
            sessions: 4,
        },
        month: crate::SpendWindow {
            usd: 8.25,
            tokens: 1_210_000,
            input: 210_000,
            output: 1_000_000,
            cache_write: 80_000,
            cache_read: 2_400_000,
            sessions: 4,
        },
        year: crate::SpendWindow {
            usd: 8.25,
            tokens: 1_210_000,
            input: 210_000,
            output: 1_000_000,
            cache_write: 80_000,
            cache_read: 2_400_000,
            sessions: 4,
        },
    });
    let rendered = snapshot_to_screen(&snapshot, 60, 34);

    // The `W:` and `M:` rows: each labelled left, the `$` spend pinned right.
    assert!(rendered.contains("W:"), "the week store row:\n{rendered}");
    assert!(rendered.contains("M:"), "the month store row:\n{rendered}");
    assert!(
        rendered.contains("$312.40"),
        "this week's spend:\n{rendered}"
    );
    assert!(
        rendered.contains("$1,240.57"),
        "this month's spend:\n{rendered}"
    );
    // Session counts and the precise (one-decimal) token total.
    assert!(rendered.contains("212"), "month session count:\n{rendered}");
    assert!(
        rendered.contains("34.9M"),
        "month token total, precise form:\n{rendered}"
    );
    // The `year` window is no longer surfaced — the store tops out at month.
    assert!(
        !rendered.contains("$4,821.90"),
        "the year pile is gone from the store:\n{rendered}"
    );
    assert_snapshot("fleet_store", rendered);
}

#[test]
fn render_fleet_store_keeps_zero_rows_for_missing_or_zero_tally() {
    let theme = Theme::fixed(true);
    let missing = line_texts(&fleet_store_lines(&theme, None, 60)).join("\n");

    assert!(missing.contains("W:"), "missing tally week row:\n{missing}");
    assert!(
        missing.contains("M:"),
        "missing tally month row:\n{missing}"
    );
    assert_eq!(
        missing.matches("$0.00").count(),
        2,
        "missing tally renders zero USD:\n{missing}"
    );

    let zero = crate::SpendTally::default();
    let zero_rendered = line_texts(&fleet_store_lines(&theme, Some(&zero), 60)).join("\n");

    assert!(
        zero_rendered.contains("W:") && zero_rendered.contains("M:"),
        "zero tally keeps both rows:\n{zero_rendered}"
    );
    assert_eq!(
        zero_rendered.matches("$0.00").count(),
        2,
        "zero tally renders zero USD:\n{zero_rendered}"
    );

    let panel = provider_panel("claude", "Claude", 173, true, true, Some((25, 40)));
    let active = panel.kind.clone();
    let rendered = Dashboard::pets(&theme, std::slice::from_ref(&panel))
        .active(&active)
        .width(40)
        .text();

    assert!(
        rendered.contains("W: $0.00") && rendered.contains("M: $0.00"),
        "normal total USD row renders zero:\n{rendered}"
    );
}

#[test]
fn pets_provider_dashboard_owns_total_rows() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    claude.context = Some(claude_context(fixed_now()));
    let mut snapshot = snapshot_with(vec![claude]);
    snapshot.providers = two_provider_panels();
    snapshot.theme.display.provider_tabs = crate::config::ProviderTabsMode::Always;
    snapshot.theme.pets.enabled = true;
    snapshot.value_tally = Some(crate::SpendTally {
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
    });

    let rendered = snapshot_to_screen(&snapshot, 60, 34);
    let lines = rendered.lines().collect::<Vec<_>>();

    assert!(
        !rendered.contains("T:"),
        "pet dashboard uses stacked main stats, not a T table:\n{rendered}"
    );
    assert!(
        rendered.contains("◎ 12"),
        "provider today sessions:\n{rendered}"
    );
    assert!(
        rendered.contains("$3.50"),
        "active provider today USD:\n{rendered}"
    );
    assert!(rendered.contains("Total:"), "scope delimiter:\n{rendered}");
    assert!(
        rendered.contains("$44.20") && rendered.contains("$101.99"),
        "W/M use fleet totals:\n{rendered}"
    );
    let total_usd = lines
        .iter()
        .find(|line| line.contains("W: $"))
        .expect("total USD row");
    assert!(
        total_usd.trim_start().starts_with("W: $44.20"),
        "week USD starts left:\n{rendered}"
    );
    assert!(
        total_usd.trim_end().ends_with("M: $101.99"),
        "month USD is pinned right:\n{rendered}"
    );
    assert!(
        !total_usd.contains('·'),
        "normal total USD row uses spacing, not a dot:\n{rendered}"
    );
    let total_index = lines
        .iter()
        .position(|line| line.contains("Total:"))
        .expect("total delimiter");
    assert!(
        lines[total_index.saturating_sub(1)].trim().is_empty(),
        "normal layout has a blank row above Total:\n{rendered}"
    );
    assert_eq!(
        lines.iter().filter(|line| line.contains("W: ◎")).count(),
        1,
        "tabbed dashboard does not duplicate the bottom W row:\n{rendered}"
    );
    let week_tokens = lines
        .iter()
        .find(|line| line.contains("W: ◎"))
        .expect("week token row");
    assert!(
        week_tokens.trim_start().starts_with("W: ◎  44"),
        "history session group starts left:\n{rendered}"
    );
    assert!(
        week_tokens.trim_end().ends_with("9.9M"),
        "history token stats are pinned right:\n{rendered}"
    );
    assert_eq!(
        lines.iter().filter(|line| line.contains("M: ◎")).count(),
        1,
        "tabbed dashboard does not duplicate the bottom M row:\n{rendered}"
    );
}

#[test]
fn pets_provider_dashboard_folds_footer_left_of_pet() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    claude.context = Some(claude_context(fixed_now()));
    let mut snapshot = snapshot_with(vec![claude]);
    snapshot.providers = two_provider_panels();
    snapshot.theme.display.provider_tabs = crate::config::ProviderTabsMode::Always;
    snapshot.theme.pets.enabled = true;
    snapshot.link = Some(crate::SidebarLinkHealth {
        rtt_ms: Some(210),
        miss_pct: 0,
        tier: crate::remote::link::LinkTier::Good,
        freshness: crate::SidebarLinkFreshness::Fresh,
        sampled_at_ms: 1_700_000_000_000,
    });
    let cell = crate::sidebar_pane::pets::PetCell {
        ch: '▀',
        fg: Color::Rgb(200, 20, 20),
        bg: Color::Rgb(20, 20, 200),
    };
    let ui = UiState {
        pet: Some(crate::sidebar_pane::pets::PetView {
            body: Some(crate::sidebar_pane::pets::PetBody::Cell(
                (0..usize::from(crate::sidebar_pane::pets::DASHBOARD_CELL_PET.rows))
                    .map(|_| vec![cell.clone(); 12])
                    .collect(),
            )),
            caption: Some("ready".to_owned()),
            frame_interval: None,
        }),
        ..Default::default()
    };

    let rendered = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, 60, 34);
    let lines = rendered.lines().collect::<Vec<_>>();
    let footer_index = lines
        .iter()
        .position(|line| line.contains("⇄ remote 210ms"))
        .expect("folded remote footer");
    let footer = lines[footer_index];

    assert!(
        footer.contains("? for help"),
        "footer keeps help:\n{footer}"
    );
    assert!(
        !footer.contains('▀'),
        "cell-art pet leaves breathing room on the footer row:\n{rendered}"
    );
    assert_eq!(
        footer_index,
        lines.len() - 1,
        "folded footer is the bottom row:\n{rendered}"
    );
    assert!(
        lines[footer_index.saturating_sub(1)].contains("W: $0.00")
            && lines[footer_index.saturating_sub(1)].contains("M: $0.00"),
        "the zero total row sits above the footer:\n{rendered}"
    );
    assert!(
        lines[footer_index.saturating_sub(1)].contains('▀'),
        "cell-art pet ends one row above the footer:\n{rendered}"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.contains("? for help"))
            .count(),
        1,
        "footer is not duplicated below the pet dashboard:\n{rendered}"
    );
}
