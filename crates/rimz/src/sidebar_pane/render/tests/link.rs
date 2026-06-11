use super::*;
use crate::remote::link::LinkTier;
use ratatui::style::Color;

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
    assert!(text.contains("? for help"));
}

#[test]
fn footer_prints_meaningful_loss_and_drops_help_on_collision() {
    let snapshot = with_link(
        LinkTier::Bad,
        crate::SidebarLinkFreshness::Fresh,
        Some(612),
        18,
    );

    assert_eq!(footer_text(&snapshot, 17), "⇄ remote 612ms");
    assert_eq!(footer_text(&snapshot, 20), "⇄ remote 612ms 18%");
}

#[test]
fn stale_link_badge_is_unknown() {
    let snapshot = with_link(
        LinkTier::Good,
        crate::SidebarLinkFreshness::Stale,
        Some(42),
        0,
    );

    assert_eq!(footer_text(&snapshot, 20), "⇄ remote ?");
    let spans = footer_spans(&snapshot, 20);
    let badge = spans
        .iter()
        .find(|span| span.content.contains("remote"))
        .unwrap();
    assert_eq!(badge.style.fg, Some(Color::Indexed(242)));
}

#[test]
fn warming_link_badge_uses_remote_ellipsis() {
    let snapshot = with_link(LinkTier::Good, crate::SidebarLinkFreshness::Fresh, None, 0);

    assert!(footer_text(&snapshot, 40).starts_with("⇄ remote …"));
}

#[test]
fn footer_pins_triage_hint_right_when_attention_needs_it() {
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

    assert_eq!(text.find("? for help"), Some(15));
    assert_eq!(text.find("␣ next ?!"), Some(31));
}

#[test]
fn footer_drops_right_hint_before_centered_help() {
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

    assert_eq!(text.find("? for help"), Some(7));
    assert!(!text.contains("␣ next ?!"));
}

#[test]
fn remote_footer_keeps_all_zones_when_they_fit() {
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
    assert!(text.contains("? for help"));
    assert!(text.ends_with("␣ next ?!"));
}
