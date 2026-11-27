use super::*;

/// The tab rail holds to one screen row: a tab that would overflow the panel
/// width is dropped whole — label and hit together — so the hit map stays in
/// lockstep with the frame however many kinds register or however narrow the
/// pane, and the rail still fills with `─`.
#[test]
fn tab_rail_drops_whole_tabs_that_overflow_the_width() {
    let theme = Theme::fixed(false);
    let panels = vec![
        provider_panel("claude", "Claude", 173, true, false, Some((25, 40))),
        provider_panel("codex", "Codex", 33, false, false, None),
        provider_panel("pi", "Pi", 28, false, false, None),
    ];
    // Stub (2) + `─ Claude ─` (10) + gap (2) + `─ Codex ─` (9) = 23 fits in
    // 28; `─ Pi ─` would land at 31, so it drops whole and `─` fills the
    // tail.
    let dashboard = Dashboard::tabbed(&theme, &panels)
        .active("claude")
        .width(28);
    let tab_line = rail_text(&dashboard.lines());
    let hits = dashboard.hits();
    assert_eq!(tab_line, "─── Claude ──── Codex ──────");
    assert_eq!(
        hits.iter().map(provider_tab_kind).collect::<Vec<_>>(),
        vec!["claude", "codex"],
        "the dropped tab carries no hit"
    );
    assert!(tab_line.chars().count() <= 28, "the rail never wraps");

    // The fill stops at the first tab that overflows: below Antigravity's drop
    // width (2 + 10 + 2 + 15 = 29) the shorter `─ Pi ─` behind it would fit,
    // yet it never paints.
    let panels = vec![
        provider_panel("claude", "Claude", 173, true, false, Some((25, 40))),
        provider_panel("antigravity", "Antigravity", 33, false, false, None),
        provider_panel("pi", "Pi", 28, false, false, None),
    ];
    for (width, painted) in [
        (28, &["claude"][..]),
        (29, &["claude", "antigravity"]),
        (30, &["claude", "antigravity"]),
    ] {
        let dashboard = Dashboard::tabbed(&theme, &panels)
            .lead("claude")
            .width(width);
        let tab_line = rail_text(&dashboard.lines());
        assert_eq!(
            dashboard
                .hits()
                .iter()
                .map(provider_tab_kind)
                .collect::<Vec<_>>(),
            painted,
            "width {width}: {tab_line}"
        );
        assert!(!tab_line.contains(" Pi "), "width {width}: {tab_line}");
    }
}

/// The user's four accounts in rank order, for the focus-first rail tests.
fn ranked_account_panels() -> Vec<SidebarProviderPanel> {
    vec![
        provider_panel("claude", "Claude · r", 173, true, false, Some((25, 40))),
        provider_panel("codex", "Codex · t2", 33, false, false, None),
        provider_panel("antigravity", "Antigravity · r", 75, false, false, None),
        provider_panel("opencode", "Open Code", 28, false, false, None),
    ]
}

/// Without a pick, the focused provider leads the rail and is its open tab,
/// even when it ranks last; the rest follow in rank order and the fill stops at
/// the first tab that overflows, so a lower-ranked tab never shows while a
/// higher-ranked one hides. A lead that names no panel leaves the rank order.
#[test]
fn tab_rail_leads_with_the_focused_provider() {
    let theme = Theme::fixed(true);
    let panels = ranked_account_panels();
    for (lead, painted, open) in [
        ("claude", ["claude", "codex"], "Claude · r"),
        ("antigravity", ["antigravity", "claude"], "Antigravity · r"),
        ("gemini", ["claude", "codex"], "Claude · r"),
    ] {
        let dashboard = Dashboard::tabbed(&theme, &panels).lead(lead).width(51);
        let rail = rail_text(&dashboard.lines());
        assert_eq!(
            dashboard
                .hits()
                .iter()
                .map(provider_tab_kind)
                .collect::<Vec<_>>(),
            painted,
            "lead {lead}: {rail}"
        );
        assert!(
            rail.contains(&format!("┤ {open} ├")),
            "lead {lead} opens {open}: {rail}"
        );
    }
}

/// A hand pick of a tab the fill hides reserves its footprint first; the rail
/// then fills in order, stops at the first tab that overflows, and paints the
/// pick at its rail-order position, after the tabs that fit beside it. On a
/// pane narrower than the pick alone, it still holds a clipped chip.
#[test]
fn tab_rail_reserves_a_hidden_pick_at_its_rail_position() {
    let theme = Theme::fixed(false);
    let panels = vec![
        provider_panel("claude", "Claude", 173, true, false, Some((25, 40))),
        provider_panel("codex", "Codex", 33, false, false, None),
        provider_panel("pi", "Pi", 28, false, false, None),
    ];
    let tabs = |width| {
        let dashboard = Dashboard::tabbed(&theme, &panels)
            .lead("pi")
            .active("codex")
            .width(width);
        let hits = dashboard.hits();
        (
            rail_text(&dashboard.lines()),
            hits.iter()
                .map(|hit| (provider_tab_kind(hit).to_owned(), hit.columns.clone()))
                .collect::<Vec<_>>(),
        )
    };
    // Pi leads: `─ Pi ─` then `─ Claude ─` fill 20 of 22 cells and `─ Codex ─`
    // hides. The pick reserves its 9 cells, Pi fits beside it, Claude no longer.
    assert_eq!(
        tabs(22),
        (
            "─── Pi ──── Codex ────".to_owned(),
            vec![("pi".to_owned(), 2..8), ("codex".to_owned(), 10..19)]
        )
    );
    assert_eq!(tabs(8).1, vec![("codex".to_owned(), 2..8)]);

    // The fill beside the pick stops at the first tab that overflows too: with Codex's 9
    // cells reserved, Antigravity drops below 2 + 9 + 2 + 10 + 2 + 15 = 40, and the shorter
    // `─ Pi ─` behind it never takes its place.
    let panels = vec![
        provider_panel("claude", "Claude", 173, true, false, Some((25, 40))),
        provider_panel("antigravity", "Antigravity", 33, false, false, None),
        provider_panel("pi", "Pi", 28, false, false, None),
        provider_panel("codex", "Codex", 33, false, false, None),
    ];
    for (width, painted) in [
        (39, &["claude", "codex"][..]),
        (40, &["claude", "antigravity", "codex"]),
        (41, &["claude", "antigravity", "codex"]),
    ] {
        let dashboard = Dashboard::tabbed(&theme, &panels)
            .lead("claude")
            .active("codex")
            .width(width);
        assert_eq!(
            dashboard
                .hits()
                .iter()
                .map(provider_tab_kind)
                .collect::<Vec<_>>(),
            painted,
            "width {width}: {}",
            rail_text(&dashboard.lines())
        );
    }
}
/// With color, the pick is fill and weight alone: whichever tab is active,
/// the rail renders glyph-for-glyph identical text — no caps, no swap — and
/// every hit covers the same edge-to-edge footprint, so a click moves color
/// without a single cell of glyph motion.
#[test]
fn tab_rail_keeps_every_glyph_still_across_picks() {
    let theme = Theme::fixed(false);
    let panels = two_provider_panels();
    let claude = Dashboard::tabbed(&theme, &panels)
        .active("claude")
        .width(52);
    let codex = Dashboard::tabbed(&theme, &panels).active("codex").width(52);
    let (claude_lines, codex_lines) = (claude.lines(), codex.lines());
    assert_eq!(
        claude.hits(),
        codex.hits(),
        "the click targets hold still as the pick moves"
    );
    let (claude_rail, codex_rail) = (rail_text(&claude_lines), rail_text(&codex_lines));
    assert_eq!(
        claude_rail, codex_rail,
        "the rail's text never changes with the pick — color carries it"
    );
    assert!(
        !claude_rail.contains('┤'),
        "with color, no caps paint:\n{claude_rail}"
    );

    // A pick of a painted tab behind a low-ranked lead highlights it in place.
    let panels = ranked_account_panels();
    let led = Dashboard::tabbed(&theme, &panels)
        .lead("antigravity")
        .width(51);
    let picked = Dashboard::tabbed(&theme, &panels)
        .lead("antigravity")
        .active("claude")
        .width(51);
    assert_eq!(led.hits(), picked.hits(), "a pick never reorders the rail");
    assert_eq!(rail_text(&led.lines()), rail_text(&picked.lines()));
}
/// Under `NO_COLOR` the chip fill drops, so the `┤ ├` caps return as the
/// pick's shape — painted into the rail cells every tab reserves, so the
/// labels still rest at the same columns whichever tab is picked.
#[test]
fn tab_rail_caps_mark_the_pick_under_no_color() {
    let theme = Theme::fixed(true);
    let panels = two_provider_panels();
    let claude_lines = Dashboard::tabbed(&theme, &panels)
        .active("claude")
        .width(52)
        .lines();
    let codex_lines = Dashboard::tabbed(&theme, &panels)
        .active("codex")
        .width(52)
        .lines();
    let (claude_rail, codex_rail) = (rail_text(&claude_lines), rail_text(&codex_lines));
    assert!(
        claude_rail.contains("┤ Claude ├") && !claude_rail.contains("┤ Codex ├"),
        "the caps notch the active tab alone:\n{claude_rail}"
    );
    assert!(
        codex_rail.contains("┤ Codex ├") && !codex_rail.contains("┤ Claude ├"),
        "the caps follow the pick:\n{codex_rail}"
    );
    for label in ["Claude", "Codex"] {
        assert_eq!(
            claude_rail.find(label),
            codex_rail.find(label),
            "`{label}` rests at one column whichever tab is active:\n{claude_rail}\n{codex_rail}"
        );
    }
}

#[test]
fn pets_enabled_keeps_rail_to_provider_tabs_only() {
    let theme = Theme::fixed(false);
    let panels = two_provider_panels();
    let dashboard = Dashboard::pets(&theme, &panels).active("claude").width(40);
    let rail = rail_text(&dashboard.lines());
    let hits = dashboard.hits();

    assert!(
        !rail.contains("Pets"),
        "pets are not a dashboard tab:\n{rail}"
    );
    assert!(
        rail.contains("Claude") && rail.contains("Codex"),
        "provider tabs still render across the rail:\n{rail}"
    );
    assert_eq!(
        hits.iter().map(provider_tab_kind).collect::<Vec<_>>(),
        vec!["claude", "codex"]
    );

    // Pet mode paints the tabbed rail, lead first.
    let led = |dashboard: Dashboard| rail_text(&dashboard.lead("codex").width(40).lines());
    let pet_rail = led(Dashboard::pets(&theme, &panels));
    assert_eq!(pet_rail, led(Dashboard::tabbed(&theme, &panels)));
    assert!(pet_rail.starts_with("─── Codex "), "{pet_rail}");
}
