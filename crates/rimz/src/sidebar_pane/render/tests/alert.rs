use super::*;

#[test]
fn render_active_alert_shows_banner_below_snapshot() {
    let snapshot = snapshot_with(Vec::new(), Vec::new());
    let alert = Alert {
        reason: "snapshot failed: ledger not found".to_owned(),
        since: fixed_now() - Duration::from_secs(8),
        recovered_at: None,
    };

    assert_snapshot(
        "degraded_banner",
        snapshot_to_screen_with_alert(&snapshot, Some(&alert), 80, 18),
    );
}
#[test]
fn render_recovered_alert_lingers_with_dismiss_hint() {
    let snapshot = snapshot_with(Vec::new(), Vec::new());
    let alert = Alert {
        reason: "snapshot failed: ledger not found".to_owned(),
        since: fixed_now() - Duration::from_secs(20),
        recovered_at: Some(fixed_now() - Duration::from_secs(8)),
    };
    let rendered = snapshot_to_screen_with_alert(&snapshot, Some(&alert), 80, 18);

    assert!(rendered.contains("last alert"), "{rendered}");
    assert!(rendered.contains("x dismiss"), "{rendered}");
}
#[test]
fn render_no_alert_omits_banner() {
    let snapshot = snapshot_with(Vec::new(), Vec::new());
    let rendered = snapshot_to_screen_with_alert(&snapshot, None, 80, 18);
    assert!(
        !rendered.contains("Sidebar degraded"),
        "no alert must not render the banner:\n{rendered}"
    );
}
#[test]
fn bottom_chrome_active_alert_suppresses_dashboard_ledger_and_footer() {
    let mut snapshot = snapshot_with(Vec::new(), Vec::new());
    snapshot.providers = vec![provider_panel(
        "claude",
        "Claude",
        173,
        true,
        true,
        Some((25, 40)),
    )];
    snapshot.value_tally = Some(bottom_tally());
    let alert = Alert::active("snapshot failed", snapshot.now);

    let (lines, hits) = bottom_chrome_texts(&snapshot, Some(&alert));
    let text = lines.join("\n");

    assert_eq!(lines.len(), 1, "only the active alert remains:\n{text}");
    assert!(text.contains("Sidebar degraded"), "{text}");
    assert!(!text.contains("Claude"), "{text}");
    assert!(!text.contains("W:"), "{text}");
    assert!(!text.contains("? for help"), "{text}");
    assert!(hits.is_empty());
}
