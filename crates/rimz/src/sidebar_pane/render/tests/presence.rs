use super::*;
use crate::ids::LinkTier;
use ratatui::style::Color;

#[test]
fn search_footer_styles_clipping_and_presence_drop_boundary() {
    use ratatui::style::Modifier;
    let snapshot = with_presence(Some(crate::store::snapshot::SidebarPresence::Detached));
    for typing in [false, true] {
        for no_color in [false, true] {
            let theme = Theme::fixed(no_color);
            let ui = UiState {
                search_draft: typing.then(|| "auth".to_owned()),
                make_up_filter: BodyLens {
                    query: Some("auth".to_owned()),
                    ..Default::default()
                },
                ..Default::default()
            };
            let threshold = if typing { 26 } else { 25 };
            for width in [threshold - 1, threshold, threshold + 1] {
                let block =
                    super::super::compose::build_bottom_chrome(&snapshot, None, &theme, width, &ui);
                let line = block.lines.last().unwrap();
                let text = line.to_string();
                assert!(text.contains("/auth"), "search segment missing: {text}");
                assert!(text.ends_with("? for help"));
                assert_eq!(
                    text.contains("away"),
                    width >= threshold,
                    "presence boundary at {width}"
                );
                let segment = line
                    .spans
                    .iter()
                    .find(|span| span.content.starts_with("/auth"))
                    .unwrap();
                assert_eq!(
                    segment.style,
                    if typing {
                        theme.body()
                    } else {
                        theme.picked_chip(theme.body_tone(), Modifier::empty())
                    }
                );
                if typing {
                    assert!(line.spans.iter().any(|span| span.content == " "
                        && span.style.add_modifier.contains(Modifier::REVERSED)));
                }
            }
            let narrow =
                super::super::compose::build_bottom_chrome(&snapshot, None, &theme, 14, &ui);
            let text = narrow.lines.last().unwrap().to_string();
            assert!(
                text.contains("/a"),
                "query must clip rather than vanish: {text}"
            );
            assert!(!text.contains("/auth"));
            assert!(text.ends_with("? for help"));
            assert_snapshot(
                &format!(
                    "search_footer_{}_{}",
                    if typing { "typing" } else { "committed" },
                    if no_color { "no_color" } else { "color" }
                ),
                snapshot_to_screen_with_alert_and_ui(
                    &snapshot,
                    None,
                    &UiState {
                        theme_cache: Some((snapshot.theme.clone(), Rc::new(theme))),
                        ..ui
                    },
                    36,
                    12,
                ),
            );
        }
    }
}

#[test]
fn search_zero_match_and_dashboard_folded_footer() {
    let mut snapshot = tabbed_provider_snapshot();
    snapshot.theme.pets.enabled = true;
    let mut ui = UiState {
        make_up_filter: BodyLens {
            query: Some("not-a-match".to_owned()),
            ..Default::default()
        },
        pet: Some(cell_pet(8, 12, "ready")),
        ..Default::default()
    };
    let theme = Theme::fixed(false);
    let bottom = super::super::compose::build_bottom_chrome(&snapshot, None, &theme, 52, &ui);
    assert!(
        bottom
            .lines
            .iter()
            .any(|line| line.to_string().contains("/not-a-match")),
        "folded footer must carry search"
    );
    assert!(
        bottom.lines[bottom.lines.len() - 2]
            .to_string()
            .contains("W: $0.00")
    );
    let composed = compose_lines(&snapshot, None, &ui, &theme, 54, 30);
    let line = composed
        .lines
        .iter()
        .find(|line| line.to_string().contains("no match"))
        .expect("empty search body must explain itself");
    assert!(
        line.spans
            .iter()
            .any(|span| span.content.contains("no match") && span.style == theme.muted())
    );
    assert_snapshot(
        "search_zero_match_folded_footer",
        snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, 54, 30),
    );
    ui.search_draft = Some("not-a-match".to_owned());
    let typing = super::super::compose::build_bottom_chrome(&snapshot, None, &theme, 52, &ui);
    let footer = typing.lines.last().unwrap();
    assert!(footer.to_string().contains("/not-a-match"));
    assert!(footer.spans.iter().any(|span| {
        span.content == " "
            && span
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::REVERSED)
    }));
}

#[test]
fn search_footer_drops_presence_then_link_and_help_lists_search() {
    let mut snapshot = with_presence(Some(crate::store::snapshot::SidebarPresence::Detached));
    snapshot.link = Some(crate::store::snapshot::SidebarLinkHealth {
        rtt_ms: Some(210),
        miss_pct: 0,
        tier: LinkTier::Good,
        freshness: crate::store::snapshot::SidebarLinkFreshness::Fresh,
        sampled_at_ms: 1_700_000_000_000,
    });
    let theme = Theme::fixed(false);
    let ui = UiState {
        make_up_filter: BodyLens {
            query: Some("auth".to_owned()),
            ..Default::default()
        },
        ..Default::default()
    };
    for width in [40, 41, 42] {
        let bottom =
            super::super::compose::build_bottom_chrome(&snapshot, None, &theme, width, &ui);
        let text = bottom.lines.last().unwrap().to_string();
        assert!(text.contains("/auth"));
        assert!(text.contains("remote"));
        assert_eq!(text.contains("away"), width >= 41);
    }
    for width in [31, 32, 33] {
        let bottom =
            super::super::compose::build_bottom_chrome(&snapshot, None, &theme, width, &ui);
        let text = bottom.lines.last().unwrap().to_string();
        assert!(text.contains("/auth"));
        assert!(!text.contains("away"));
        assert_eq!(text.contains("remote"), width >= 32);
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
fn search_folded_footer_uses_its_own_badge_drop_width() {
    let mut snapshot = tabbed_provider_snapshot();
    snapshot.theme.pets.enabled = true;
    snapshot.presence = Some(crate::store::snapshot::SidebarPresence::Detached);
    snapshot.link = Some(crate::store::snapshot::SidebarLinkHealth {
        rtt_ms: Some(210),
        miss_pct: 0,
        tier: LinkTier::Good,
        freshness: crate::store::snapshot::SidebarLinkFreshness::Fresh,
        sampled_at_ms: 1_700_000_000_000,
    });
    let theme = Theme::fixed(false);
    let ui = UiState {
        make_up_filter: BodyLens {
            query: Some("auth".to_owned()),
            ..Default::default()
        },
        pet: Some(cell_pet(8, 12, "ready")),
        ..Default::default()
    };
    for width in [53, 54, 55] {
        let bottom =
            super::super::compose::build_bottom_chrome(&snapshot, None, &theme, width, &ui);
        let text = bottom.lines.last().unwrap().to_string();
        assert!(text.contains("/auth"));
        assert!(
            text.contains("remote 210ms"),
            "link must remain whole: {text}"
        );
        assert_eq!(
            text.contains("away"),
            width >= 54,
            "folded presence boundary at {width}: {text}"
        );
    }
    for width in [44, 45, 46] {
        let bottom =
            super::super::compose::build_bottom_chrome(&snapshot, None, &theme, width, &ui);
        let text = bottom.lines.last().unwrap().to_string();
        assert!(!text.contains("away"));
        assert_eq!(text.contains("remote 210ms"), width >= 45);
    }
}

fn footer_text(snapshot: &SidebarSnapshot, width: usize) -> String {
    crate::sidebar_pane::render::chrome::footer_lines(
        snapshot,
        &crate::sidebar_pane::render::theme::Theme::fixed(true),
        width,
        &UiState::default(),
    )[0]
    .spans
    .iter()
    .map(|span| span.content.as_ref())
    .collect()
}

fn footer_spans(snapshot: &SidebarSnapshot, width: usize) -> Vec<ratatui::text::Span<'static>> {
    crate::sidebar_pane::render::chrome::footer_lines(
        snapshot,
        &crate::sidebar_pane::render::theme::Theme::fixed(false),
        width,
        &UiState::default(),
    )[0]
    .spans
    .clone()
}

fn with_presence(presence: Option<crate::store::snapshot::SidebarPresence>) -> SidebarSnapshot {
    let mut snapshot = snapshot_with(Vec::new());
    snapshot.presence = presence;
    snapshot
}

#[test]
fn idle_presence_badge_renders_muted_elapsed_time() {
    let snapshot = with_presence(Some(crate::store::snapshot::SidebarPresence::Idle {
        idle_ms: 17 * 60_000,
    }));

    let text = footer_text(&snapshot, 32);

    assert!(text.starts_with("zᶻ idle · 17m"));
    assert!(text.ends_with("? for help"));
    let spans = footer_spans(&snapshot, 32);
    let badge = spans
        .iter()
        .find(|span| span.content.contains("idle"))
        .unwrap();
    assert_eq!(badge.style.fg, Some(Color::Indexed(102)));
}

#[test]
fn idle_presence_badge_omits_sub_minute_elapsed_time() {
    let snapshot = with_presence(Some(crate::store::snapshot::SidebarPresence::Idle {
        idle_ms: 17_000,
    }));

    let text = footer_text(&snapshot, 32);

    assert!(text.starts_with("zᶻ idle"));
    assert!(!text.contains('·'));
    assert!(text.ends_with("? for help"));
}

#[test]
fn idle_presence_badge_floors_elapsed_time_to_minutes() {
    let snapshot = with_presence(Some(crate::store::snapshot::SidebarPresence::Idle {
        idle_ms: 90_000,
    }));

    let text = footer_text(&snapshot, 32);

    assert!(text.starts_with("zᶻ idle · 1m"));
    assert!(text.ends_with("? for help"));
}

#[test]
fn detached_presence_badge_renders_away() {
    let snapshot = with_presence(Some(crate::store::snapshot::SidebarPresence::Detached));

    let text = footer_text(&snapshot, 28);

    assert!(text.starts_with("zᶻ away"));
    assert!(text.ends_with("? for help"));
}

#[test]
fn active_and_unknown_presence_render_no_badge() {
    let active = with_presence(Some(crate::store::snapshot::SidebarPresence::Active));
    let unknown = with_presence(None);

    assert_eq!(footer_text(&active, 20), "          ? for help");
    assert_eq!(footer_text(&unknown, 20), "          ? for help");
}

#[test]
fn presence_badge_precedes_remote_link_when_both_fit() {
    let mut snapshot = with_presence(Some(crate::store::snapshot::SidebarPresence::Detached));
    snapshot.link = Some(crate::store::snapshot::SidebarLinkHealth {
        rtt_ms: Some(42),
        miss_pct: 0,
        tier: LinkTier::Good,
        freshness: crate::store::snapshot::SidebarLinkFreshness::Fresh,
        sampled_at_ms: 1_700_000_000_000,
    });

    let text = footer_text(&snapshot, 44);

    assert!(text.starts_with("zᶻ away  ⇄ remote 42ms"));
    assert!(text.ends_with("? for help"));
}

#[test]
fn presence_badge_drops_remote_link_when_footer_is_narrow() {
    let mut snapshot = with_presence(Some(crate::store::snapshot::SidebarPresence::Detached));
    snapshot.link = Some(crate::store::snapshot::SidebarLinkHealth {
        rtt_ms: Some(42),
        miss_pct: 0,
        tier: LinkTier::Good,
        freshness: crate::store::snapshot::SidebarLinkFreshness::Fresh,
        sampled_at_ms: 1_700_000_000_000,
    });

    let text = footer_text(&snapshot, 24);

    assert!(text.starts_with("zᶻ away"));
    assert!(!text.contains("remote"));
    assert!(text.ends_with("? for help"));
}

#[test]
fn link_badge_does_not_replace_presence_when_only_link_fits() {
    let mut snapshot = with_presence(Some(crate::store::snapshot::SidebarPresence::Idle {
        idle_ms: 17 * 60_000,
    }));
    snapshot.link = Some(crate::store::snapshot::SidebarLinkHealth {
        rtt_ms: None,
        miss_pct: 0,
        tier: LinkTier::Good,
        freshness: crate::store::snapshot::SidebarLinkFreshness::Stale,
        sampled_at_ms: 1_700_000_000_000,
    });

    let text = footer_text(&snapshot, 22);

    assert_eq!(text, "            ? for help");
    assert!(!text.contains("remote"));
}
