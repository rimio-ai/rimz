use super::*;
use crate::ids::LinkTier;

fn search_ui(query: &str) -> UiState {
    UiState {
        selected_index: Some(0),
        make_up_filter: BodyLens {
            query: Some(query.to_owned()),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn bottom(snapshot: &SidebarSnapshot, ui: &UiState, theme: &Theme, cells: usize) -> RenderedBlock {
    build_bottom_chrome(
        snapshot,
        None,
        theme,
        cells,
        ui,
        &ui.visible_roster(snapshot),
    )
}

#[test]
fn search_line_typing_color() {
    assert_search_style(true, false);
}

#[test]
fn search_line_typing_no_color() {
    assert_search_style(true, true);
}

#[test]
fn search_line_committed_color() {
    assert_search_style(false, false);
}

#[test]
fn search_line_committed_no_color() {
    assert_search_style(false, true);
}

fn assert_search_style(typing: bool, no_color: bool) {
    let snapshot = snapshot_with(Vec::new());
    let theme = Theme::fixed(no_color);
    let mut ui = search_ui("auth");
    ui.search_draft = typing.then(|| "auth".to_owned());
    let block = bottom(&snapshot, &ui, &theme, 36);
    let line = &block.lines[0];
    assert!(line.to_string().starts_with(" / auth"), "{line}");
    assert!(line.to_string().trim_end().ends_with("0/0"));
    assert_eq!(line.width(), 36);
    let band = typing.then(|| theme.selection_band()).flatten();
    if typing {
        assert!(line.spans.iter().all(|span| span.style.bg == band));
    } else {
        assert_eq!(line.spans.first().unwrap().style.bg, None);
        assert_eq!(line.spans.last().unwrap().style.bg, None);
        assert!(!line.spans.iter().any(
            |span| span.content == " " && span.style.add_modifier.contains(Modifier::REVERSED)
        ));
    }
    assert_eq!(line.spans.first().unwrap().content, " ");
    assert_eq!(line.spans.last().unwrap().content, " ");
    if typing {
        assert!(line.spans.iter().any(
            |span| span.content == " " && span.style.add_modifier.contains(Modifier::REVERSED)
        ));
        let query = line
            .spans
            .iter()
            .find(|span| span.content == "auth")
            .unwrap();
        let mut style = theme.body();
        if let Some(bg) = band {
            style = style.bg(bg);
        }
        assert_eq!(query.style, style);
    } else {
        let chip = line
            .spans
            .iter()
            .find(|span| span.content == "/ auth")
            .unwrap();
        assert_eq!(
            chip.style,
            theme.picked_chip(theme.body_tone(), Modifier::empty())
        );
    }
    ui.theme_cache = Some((snapshot.theme.clone(), Rc::new(theme)));
    assert_snapshot(
        &format!(
            "search_line_{}_{}",
            if typing { "typing" } else { "committed" },
            if no_color { "no_color" } else { "color" }
        ),
        snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, 36, 12),
    );
}

#[test]
fn search_line_resolves_nerd_font_glyph() {
    let snapshot = snapshot_with(Vec::new());
    let theme = Theme::fixed_for_theme(
        false,
        &crate::config::ThemeConfig {
            style: Some(crate::config::ThemeStyle::Modern),
            ..Default::default()
        },
    );
    let block = bottom(&snapshot, &search_ui("auth"), &theme, 36);
    assert!(
        block.lines[0].to_string().starts_with(" \u{f002} auth"),
        "{}",
        block.lines[0]
    );
}

#[test]
fn search_line_count_uses_all_rows_and_live_projection() {
    let mut agents: Vec<_> = (0..9)
        .map(|i| {
            let mut agent = agent(
                &format!("auth-{i}"),
                "codex",
                if i == 8 {
                    AgentStatus::Running
                } else {
                    AgentStatus::Idle
                },
                Some("/repo/main"),
                Some("group-label"),
                None,
            );
            agent.role = Some(format!("auth-{i}"));
            agent
        })
        .collect();
    agents.push(agent(
        "billing",
        "codex",
        AgentStatus::Idle,
        Some("/repo/payments"),
        Some("payments"),
        None,
    ));
    let mut snapshot = snapshot_with(agents);
    let process = snapshot_with(Vec::new())
        .with_live_panes(vec![pane("%shell", "auth-shell", "/repo/main")], None);
    let group = snapshot
        .worktree_groups
        .iter_mut()
        .find(|g| g.rows.len() == 9)
        .unwrap();
    group.label = "group-label".to_owned();
    group.rows.push(process.worktree_groups[0].rows[0].clone());
    let theme = Theme::fixed(true);
    for (query, filter, matches) in [
        ("auth-8", None, 1),
        ("auth-shell", None, 1),
        ("auth-", None, 10),
        ("group-label", None, 10),
        ("auth-", Some(BodyFilter::Status(AgentStatus::Idle)), 8),
    ] {
        let mut ui = search_ui("not-the-draft");
        ui.search_draft = Some(query.to_owned());
        ui.make_up_filter.filter = filter;
        assert_eq!(ui.visible_roster(&snapshot).len(), matches);
        let block = bottom(&snapshot, &ui, &theme, 40);
        assert!(
            block.lines[0]
                .to_string()
                .trim_end()
                .ends_with(&format!("{matches}/11")),
            "{}",
            block.lines[0]
        );
    }
}

#[test]
fn search_line_empty_draft_has_cursor_without_count() {
    let snapshot = snapshot_with(Vec::new());
    let theme = Theme::fixed(false);
    let mut ui = search_ui("old-query");
    ui.search_draft = Some(String::new());
    let block = bottom(&snapshot, &ui, &theme, 36);
    let line = &block.lines[0];
    assert!(line.to_string().starts_with(" / "), "{line}");
    assert!(!line.to_string().contains("0/0"));
    assert!(!line.to_string().contains("old-query"));
    assert!(line.spans.iter().any(|span| span.content == " " && span.style.add_modifier.contains(Modifier::REVERSED)));
}

#[test]
fn search_line_drops_count_before_clipping_and_preserves_cursor() {
    let snapshot = snapshot_with(Vec::new());
    let theme = Theme::fixed(false);
    for typing in [false, true] {
        let mut ui = search_ui("auth");
        ui.search_draft = typing.then(|| "auth".to_owned());
        let threshold = if typing { 13 } else { 12 };
        for cells in [threshold - 1, threshold, threshold + 1] {
            let block = bottom(&snapshot, &ui, &theme, cells);
            let line = &block.lines[0];
            assert_eq!(line.width(), cells);
            assert!(line.to_string().starts_with(" / auth"), "{line}");
            assert_eq!(
                line.to_string().contains("0/0"),
                cells >= threshold,
                "count boundary at {cells}: {line}"
            );
        }
        let block = bottom(&snapshot, &ui, &theme, 6);
        let line = &block.lines[0];
        assert_eq!(line.width(), 6);
        assert!(line.to_string().starts_with(" / a"), "{line}");
        assert!(!line.to_string().contains("auth"));
        if typing {
            assert!(
                line.spans.iter().any(|span| span.content == " "
                    && span.style.add_modifier.contains(Modifier::REVERSED))
            );
        }
        for query in ["auth", "界界"] {
            ui.make_up_filter.query = Some(query.to_owned());
            ui.search_draft = typing.then(|| query.to_owned());
            for cells in 1..=15 {
                let block = bottom(&snapshot, &ui, &theme, cells);
                let line = &block.lines[0];
                assert_eq!(line.width(), cells, "one row at {cells}: {line}");
                if typing && cells >= 3 {
                    assert!(line.spans.iter().any(|span| span.content == " "
                        && span.style.add_modifier.contains(Modifier::REVERSED)));
                }
            }
        }
    }
}

#[test]
fn search_line_reuses_dashboard_separator_and_leaves_folded_footer() {
    let mut snapshot = tabbed_provider_snapshot();
    let theme = Theme::fixed(false);
    for pets in [false, true] {
        snapshot.theme.pets.enabled = pets;
        let mut ui = search_ui("not-a-match");
        ui.pet = pets.then(|| cell_pet(8, 12, "ready"));
        let mut baseline_ui = ui.clone();
        baseline_ui.make_up_filter.query = None;
        let block = bottom(&snapshot, &ui, &theme, 54);
        assert!(block.lines[0].to_string().contains("/ not-a-match"));
        assert!(
            block.lines[1]
                .to_string()
                .contains(theme.glyph(crate::config::GlyphRole::ChromeHairline))
        );
        assert!(!block.lines.last().unwrap().to_string().contains('/'));
        let composed = compose_lines(&snapshot, None, &ui, &theme, 54, 30);
        assert_eq!(
            composed.bottom_height,
            compose_lines(&snapshot, None, &baseline_ui, &theme, 54, 30).bottom_height
        );
        assert!(composed.lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.contains("no match") && span.style == theme.muted())
        }));
        if pets {
            assert_snapshot(
                "search_line_zero_match_folded_dashboard",
                snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, 54, 30),
            );
        }
    }
}

#[test]
fn search_line_without_dashboard_reserves_one_row_and_help_stays_above() {
    let mut snapshot = snapshot_with(Vec::new());
    let theme = Theme::fixed(true);
    let ui = search_ui("auth");
    let empty_baseline = compose_lines(&snapshot, None, &selected_ui(), &theme, 42, 24);
    assert_eq!(
        compose_lines(&snapshot, None, &ui, &theme, 42, 24).bottom_height,
        empty_baseline.bottom_height + 1
    );
    snapshot.value_tally = Some(bottom_tally());
    let mut ui = search_ui("auth");
    let block = bottom(&snapshot, &ui, &theme, 42);
    assert!(block.lines[0].to_string().contains("/ auth"));
    assert!(
        block.lines[1]
            .to_string()
            .contains(theme.glyph(crate::config::GlyphRole::ChromeHairline))
    );
    let baseline = compose_lines(&snapshot, None, &selected_ui(), &theme, 42, 24);
    ui.help_visible = true;
    let composed = compose_lines(&snapshot, None, &ui, &theme, 42, 24);
    assert_eq!(composed.bottom_height, baseline.bottom_height + 1);
    let screen = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, 42, 24);
    let rows: Vec<_> = screen.lines().collect();
    let search_row = rows
        .iter()
        .position(|line| line.contains("/ auth"))
        .unwrap();
    assert_eq!(search_row, 24 - composed.bottom_height);
    assert!(
        rows[..search_row]
            .iter()
            .any(|line| line.contains("search")),
        "help overlay must end above the search line: {screen}"
    );
}

#[test]
fn search_line_keeps_plain_and_folded_footer_badge_admission() {
    let theme = Theme::fixed(false);
    for folded in [false, true] {
        let mut snapshot = if folded {
            tabbed_provider_snapshot()
        } else {
            snapshot_with(Vec::new())
        };
        snapshot.theme.pets.enabled = folded;
        snapshot.presence = Some(crate::store::snapshot::SidebarPresence::Detached);
        snapshot.link = Some(crate::store::snapshot::SidebarLinkHealth {
            rtt_ms: Some(210),
            miss_pct: 0,
            tier: LinkTier::Good,
            freshness: crate::store::snapshot::SidebarLinkFreshness::Fresh,
            sampled_at_ms: 1_700_000_000_000,
        });
        let mut ui = search_ui("auth");
        ui.pet = folded.then(|| cell_pet(8, 12, "ready"));
        let mut baseline_ui = ui.clone();
        baseline_ui.make_up_filter.query = None;
        let widths: &[usize] = if folded {
            &[44, 45, 46, 53, 54, 55]
        } else {
            &[31, 32, 33, 40, 41, 42]
        };
        for &inner in widths {
            let searched = bottom(&snapshot, &ui, &theme, inner + 2);
            let baseline = bottom(&snapshot, &baseline_ui, &theme, inner + 2);
            let footer = searched.lines.last().unwrap().to_string();
            assert_eq!(
                footer,
                baseline.lines.last().unwrap().to_string(),
                "badges must not give way to search at {inner}"
            );
            assert!(!footer.contains('/'));
        }
    }
    let help = super::super::chrome::help_lines(
        &theme,
        None,
        None,
        &crate::config::SidebarKeys::default(),
        54,
    );
    assert!(help.iter().any(|line| {
        let text = line.to_string();
        let words: Vec<_> = text.split_whitespace().collect();
        words.contains(&"/") && words.contains(&"search")
    }));
}

#[test]
fn search_line_stays_visible_during_active_alert() {
    let snapshot = tabbed_provider_snapshot();
    let theme = Theme::fixed(false);
    let ui = search_ui("auth");
    let alert = Alert {
        reason: "snapshot unavailable".to_owned(),
        since: fixed_now(),
        recovered_at: None,
    };
    let block = build_bottom_chrome(
        &snapshot,
        Some(&alert),
        &theme,
        54,
        &ui,
        &ui.visible_roster(&snapshot),
    );
    assert_eq!(block.lines.len(), 2);
    assert!(block.lines[0].to_string().contains("/ auth"));
    assert!(block.lines[1].to_string().contains("Sidebar degraded"));
    assert!(
        block.interactions.regions().is_empty(),
        "search chrome is inert"
    );
}
