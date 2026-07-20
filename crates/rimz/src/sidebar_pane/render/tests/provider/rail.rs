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
}

/// The active account keeps a chip even when its low-ranked position would put
/// it beyond the ordinary greedy prefix; hits describe exactly those survivors.
#[test]
fn tab_rail_reserves_the_active_tab_before_fitting_the_ranked_rest() {
    let theme = Theme::fixed(false);
    let panels = vec![
        provider_panel("claude", "Claude", 173, true, false, Some((25, 40))),
        provider_panel("codex", "Codex", 33, false, false, None),
        provider_panel("pi", "Pi", 28, false, false, None),
    ];
    let dashboard = Dashboard::tabbed(&theme, &panels).active("pi").width(20);
    let tab_line = rail_text(&dashboard.lines());
    let hits = dashboard.hits();
    assert_eq!(tab_line, "─── Claude ──── Pi ─");
    assert_eq!(
        hits.iter()
            .map(|hit| { (provider_tab_kind(hit), hit.columns.start, hit.columns.end,) })
            .collect::<Vec<_>>(),
        vec![("claude", 2, 12), ("pi", 14, 20)]
    );
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
}
