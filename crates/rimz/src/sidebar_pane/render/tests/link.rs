use super::*;
use crate::remote::link::LinkTier;
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
    freshness: crate::SidebarLinkFreshness,
    rtt_ms: Option<u32>,
    miss_pct: u16,
) -> SidebarSnapshot {
    let mut snapshot = snapshot_with(Vec::new(), Vec::new());
    snapshot.link = Some(crate::SidebarLinkHealth {
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
        crate::SidebarLinkFreshness::Fresh,
        Some(230),
        4,
    );

    let text = footer_text(&snapshot, 40);

    assert!(text.starts_with("⇄ remote 230ms"));
    assert!(!text.contains('%'));
    assert!(text.ends_with("? for help"));
}

#[test]
fn footer_prints_right_help_when_remote_badge_would_collide() {
    let snapshot = with_link(
        LinkTier::Bad,
        crate::SidebarLinkFreshness::Fresh,
        Some(612),
        18,
    );

    assert_eq!(footer_text(&snapshot, 17), "       ? for help");
    assert_eq!(footer_text(&snapshot, 20), "          ? for help");
}

#[test]
fn stale_link_badge_is_unknown() {
    let snapshot = with_link(
        LinkTier::Good,
        crate::SidebarLinkFreshness::Stale,
        Some(42),
        0,
    );

    let text = footer_text(&snapshot, 24);
    assert!(text.starts_with("⇄ remote ?"));
    assert!(text.ends_with("? for help"));
    let spans = footer_spans(&snapshot, 24);
    let badge = spans
        .iter()
        .find(|span| span.content.contains("remote"))
        .unwrap();
    assert_eq!(badge.style.fg, Some(Color::Indexed(102)));
}

#[test]
fn warming_link_badge_uses_remote_ellipsis() {
    let snapshot = with_link(LinkTier::Good, crate::SidebarLinkFreshness::Fresh, None, 0);

    assert!(footer_text(&snapshot, 40).starts_with("⇄ remote …"));
}

#[test]
fn fresh_link_badge_uses_full_health_ramp() {
    let theme = Theme::fixed(false);

    let healthy = with_link(
        LinkTier::Good,
        crate::SidebarLinkFreshness::Fresh,
        Some(80),
        0,
    );
    let healthy_badge = footer_spans(&healthy, 40)
        .into_iter()
        .find(|span| span.content.contains("remote"))
        .unwrap();
    assert_eq!(healthy_badge.style.fg, Some(theme.heat_tone(0.0)));
    assert!(!healthy_badge.style.add_modifier.contains(Modifier::BOLD));

    let bad = with_link(
        LinkTier::Bad,
        crate::SidebarLinkFreshness::Fresh,
        Some(450),
        0,
    );
    let bad_badge = footer_spans(&bad, 40)
        .into_iter()
        .find(|span| span.content.contains("remote"))
        .unwrap();
    assert_eq!(bad_badge.style.fg, Some(theme.heat_tone(1.0)));
    assert!(bad_badge.style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn footer_pins_help_right_when_attention_needs_it() {
    let snapshot = snapshot_with(
        Vec::new(),
        vec![agent(
            "claude-1",
            "claude",
            AgentStatus::Waiting,
            Some("/repo/main"),
            Some("main"),
            Some("approve deploy"),
        )],
    );

    let text = footer_text(&snapshot, 40);

    assert_eq!(text.find("? for help"), Some(30));
    assert!(!text.contains("next"));
}

#[test]
fn footer_keeps_help_right_without_attention_hint() {
    let snapshot = snapshot_with(
        Vec::new(),
        vec![agent(
            "claude-1",
            "claude",
            AgentStatus::Waiting,
            Some("/repo/main"),
            Some("main"),
            Some("approve deploy"),
        )],
    );

    let text = footer_text(&snapshot, 24);

    assert_eq!(text.find("? for help"), Some(14));
    assert!(!text.contains("next"));
}

#[test]
fn remote_footer_keeps_left_badge_and_right_help_when_they_fit() {
    let mut snapshot = with_link(
        LinkTier::Degraded,
        crate::SidebarLinkFreshness::Fresh,
        Some(210),
        0,
    );
    snapshot.worktree_groups = snapshot_with(
        Vec::new(),
        vec![agent(
            "claude-1",
            "claude",
            AgentStatus::Waiting,
            Some("/repo/main"),
            Some("main"),
            Some("approve deploy"),
        )],
    )
    .worktree_groups;

    let text = footer_text(&snapshot, 44);

    assert!(text.starts_with("⇄ remote 210ms"));
    assert!(text.ends_with("? for help"));
    assert!(!text.contains("next"));
}

#[test]
fn footer_trims_help_to_narrow_width() {
    let snapshot = snapshot_with(Vec::new(), Vec::new());

    assert_eq!(footer_text(&snapshot, 4), "? fo");
}
