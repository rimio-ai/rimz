use super::*;
use crate::remote::link::LinkTier;

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

    assert!(text.starts_with("⇅ 230ms 4%"));
    assert!(text.contains("? for help"));
}

#[test]
fn footer_drops_help_on_collision() {
    let snapshot = with_link(
        LinkTier::Bad,
        crate::SidebarLinkFreshness::Fresh,
        Some(612),
        18,
    );

    assert_eq!(footer_text(&snapshot, 12), "⇅ 612ms 18%");
}

#[test]
fn stale_link_badge_is_unknown() {
    let snapshot = with_link(
        LinkTier::Good,
        crate::SidebarLinkFreshness::Stale,
        Some(42),
        0,
    );

    assert_eq!(footer_text(&snapshot, 10), "⇅ ?");
}
