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

fn with_presence(presence: Option<crate::SidebarPresence>) -> SidebarSnapshot {
    let mut snapshot = snapshot_with(Vec::new());
    snapshot.presence = presence;
    snapshot
}

#[test]
fn idle_presence_badge_renders_muted_elapsed_time() {
    let snapshot = with_presence(Some(crate::SidebarPresence::Idle {
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
    let snapshot = with_presence(Some(crate::SidebarPresence::Idle { idle_ms: 17_000 }));

    let text = footer_text(&snapshot, 32);

    assert!(text.starts_with("zᶻ idle"));
    assert!(!text.contains('·'));
    assert!(text.ends_with("? for help"));
}

#[test]
fn idle_presence_badge_floors_elapsed_time_to_minutes() {
    let snapshot = with_presence(Some(crate::SidebarPresence::Idle { idle_ms: 90_000 }));

    let text = footer_text(&snapshot, 32);

    assert!(text.starts_with("zᶻ idle · 1m"));
    assert!(text.ends_with("? for help"));
}

#[test]
fn detached_presence_badge_renders_away() {
    let snapshot = with_presence(Some(crate::SidebarPresence::Detached));

    let text = footer_text(&snapshot, 28);

    assert!(text.starts_with("zᶻ away"));
    assert!(text.ends_with("? for help"));
}

#[test]
fn active_and_unknown_presence_render_no_badge() {
    let active = with_presence(Some(crate::SidebarPresence::Active));
    let unknown = with_presence(None);

    assert_eq!(footer_text(&active, 20), "          ? for help");
    assert_eq!(footer_text(&unknown, 20), "          ? for help");
}

#[test]
fn presence_badge_precedes_remote_link_when_both_fit() {
    let mut snapshot = with_presence(Some(crate::SidebarPresence::Detached));
    snapshot.link = Some(crate::SidebarLinkHealth {
        rtt_ms: Some(42),
        miss_pct: 0,
        tier: LinkTier::Good,
        freshness: crate::SidebarLinkFreshness::Fresh,
        sampled_at_ms: 1_700_000_000_000,
    });

    let text = footer_text(&snapshot, 44);

    assert!(text.starts_with("zᶻ away  ⇄ remote 42ms"));
    assert!(text.ends_with("? for help"));
}

#[test]
fn presence_badge_drops_remote_link_when_footer_is_narrow() {
    let mut snapshot = with_presence(Some(crate::SidebarPresence::Detached));
    snapshot.link = Some(crate::SidebarLinkHealth {
        rtt_ms: Some(42),
        miss_pct: 0,
        tier: LinkTier::Good,
        freshness: crate::SidebarLinkFreshness::Fresh,
        sampled_at_ms: 1_700_000_000_000,
    });

    let text = footer_text(&snapshot, 24);

    assert!(text.starts_with("zᶻ away"));
    assert!(!text.contains("remote"));
    assert!(text.ends_with("? for help"));
}

#[test]
fn link_badge_does_not_replace_presence_when_only_link_fits() {
    let mut snapshot = with_presence(Some(crate::SidebarPresence::Idle {
        idle_ms: 17 * 60_000,
    }));
    snapshot.link = Some(crate::SidebarLinkHealth {
        rtt_ms: None,
        miss_pct: 0,
        tier: LinkTier::Good,
        freshness: crate::SidebarLinkFreshness::Stale,
        sampled_at_ms: 1_700_000_000_000,
    });

    let text = footer_text(&snapshot, 22);

    assert_eq!(text, "            ? for help");
    assert!(!text.contains("remote"));
}
