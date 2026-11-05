use super::*;
use crate::ids::LinkTier;
use ratatui::style::{Color, Modifier};

fn footer_text(snapshot: &SidebarSnapshot, width: usize) -> String {
    crate::sidebar_pane::render::chrome::footer_lines(
        snapshot,
        &crate::sidebar_pane::render::theme::Theme::fixed(true),
        width,
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
    )[0]
    .spans
    .clone()
}

fn with_link(
    tier: LinkTier,
    freshness: crate::store::snapshot::SidebarLinkFreshness,
    rtt_ms: Option<u32>,
    miss_pct: u16,
) -> SidebarSnapshot {
    let mut snapshot = snapshot_with(Vec::new());
    snapshot.link = Some(crate::store::snapshot::SidebarLinkHealth {
        rtt_ms,
        miss_pct,
        tier,
        freshness,
        sampled_at_ms: 1_700_000_000_000,
    });
    snapshot
}

#[test]
fn footer_left_pins_fresh_link_badge_and_keeps_help_when_it_fits() {
    let snapshot = with_link(
        LinkTier::Degraded,
        crate::store::snapshot::SidebarLinkFreshness::Fresh,
        Some(230),
        4,
    );

    let text = footer_text(&snapshot, 40);

    assert!(text.starts_with("⇄ 230ms"));
    assert!(!text.contains('%'));
    assert!(text.ends_with("? for help"));
}

#[test]
fn bad_link_figure_survives_with_short_help_when_full_help_would_collide() {
    let snapshot = with_link(
        LinkTier::Bad,
        crate::store::snapshot::SidebarLinkFreshness::Fresh,
        Some(612),
        18,
    );

    assert_eq!(
        [17, 20].map(|width| footer_text(&snapshot, width)),
        ["⇄ 612ms 18%     ?", "⇄ 612ms 18%        ?"]
    );
}

#[test]
fn stale_link_badge_names_stale_in_muted_tone() {
    let snapshot = with_link(
        LinkTier::Good,
        crate::store::snapshot::SidebarLinkFreshness::Stale,
        Some(42),
        0,
    );

    let text = footer_text(&snapshot, 24);
    assert!(text.starts_with("⇄ stale"));
    assert!(text.ends_with("? for help"));
    let spans = footer_spans(&snapshot, 24);
    let badge = spans
        .iter()
        .find(|span| span.content.starts_with('⇄'))
        .unwrap();
    assert_eq!(badge.style.fg, Some(Color::Indexed(102)));
}

#[test]
fn warming_link_badge_uses_ellipsis_then_bare_anchor() {
    let snapshot = with_link(
        LinkTier::Good,
        crate::store::snapshot::SidebarLinkFreshness::Fresh,
        None,
        0,
    );

    assert_eq!(
        [4, 5, 6, 40].map(|width| footer_text(&snapshot, width)),
        [
            "⇄  ?".to_owned(),
            "⇄   ?".to_owned(),
            "⇄ …  ?".to_owned(),
            format!("⇄ …{}? for help", " ".repeat(27))
        ]
    );
}

#[test]
fn fresh_link_badge_uses_full_health_ramp() {
    let theme = Theme::fixed(false);

    let healthy = with_link(
        LinkTier::Good,
        crate::store::snapshot::SidebarLinkFreshness::Fresh,
        Some(80),
        0,
    );
    let healthy_badge = footer_spans(&healthy, 40)
        .into_iter()
        .find(|span| span.content.starts_with('⇄'))
        .unwrap();
    assert_eq!(healthy_badge.style.fg, Some(theme.heat_tone(0.0)));
    assert!(!healthy_badge.style.add_modifier.contains(Modifier::BOLD));

    let bad = with_link(
        LinkTier::Bad,
        crate::store::snapshot::SidebarLinkFreshness::Fresh,
        Some(450),
        0,
    );
    let bad_badge = footer_spans(&bad, 40)
        .into_iter()
        .find(|span| span.content.starts_with('⇄'))
        .unwrap();
    assert_eq!(bad_badge.style.fg, Some(theme.heat_tone(1.0)));
    assert!(bad_badge.style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn footer_pins_help_right_when_attention_needs_it() {
    let snapshot = snapshot_with(vec![agent(
        "claude-1",
        "claude",
        AgentStatus::Waiting,
        Some("/repo/main"),
        Some("main"),
        Some("approve deploy"),
    )]);

    let text = footer_text(&snapshot, 40);

    assert_eq!(text.find("? for help"), Some(30));
    assert!(!text.contains("next"));
}

#[test]
fn footer_keeps_help_right_without_attention_hint() {
    let snapshot = snapshot_with(vec![agent(
        "claude-1",
        "claude",
        AgentStatus::Waiting,
        Some("/repo/main"),
        Some("main"),
        Some("approve deploy"),
    )]);

    let text = footer_text(&snapshot, 24);

    assert_eq!(text.find("? for help"), Some(14));
    assert!(!text.contains("next"));
}

#[test]
fn remote_footer_keeps_left_badge_and_right_help_when_they_fit() {
    let mut snapshot = with_link(
        LinkTier::Degraded,
        crate::store::snapshot::SidebarLinkFreshness::Fresh,
        Some(210),
        0,
    );
    snapshot.worktree_groups = snapshot_with(vec![agent(
        "claude-1",
        "claude",
        AgentStatus::Waiting,
        Some("/repo/main"),
        Some("main"),
        Some("approve deploy"),
    )])
    .worktree_groups;

    let text = footer_text(&snapshot, 44);

    assert!(text.starts_with("⇄ 210ms"));
    assert!(text.ends_with("? for help"));
    assert!(!text.contains("next"));
}

#[test]
fn footer_shortens_help_at_narrow_width() {
    let snapshot = snapshot_with(Vec::new());

    assert_eq!(footer_text(&snapshot, 4), "   ?");
}
