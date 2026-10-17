use super::*;

#[test]
fn parked_workflows_keep_their_age_at_wide_and_l2_widths() {
    let mut value = serde_json::to_value(snapshot_with(Vec::new())).unwrap();
    value["parked_workflows"] = serde_json::json!([
        {"run":"wf_0190b8172c0070008000000000000001","workflow":"babysit-pr","node":"triage","reason":"asked","text":"Which compatibility fix should ship before the release?","checkout":"/repo/fix","since":(fixed_now()-Duration::from_secs(300)).to_string()},
        {"run":"wf_0190b8172c0070008000000000000002","workflow":"review","node":"sweep","reason":"cap","text":null,"checkout":"/repo/review","since":(fixed_now()-Duration::from_secs(120)).to_string()}
    ]);
    let mut snapshot: SidebarSnapshot = serde_json::from_value(value).unwrap();
    snapshot.now = fixed_now();
    for (width, name) in [(80, "parked_workflows_wide"), (46, "parked_workflows_l2")] {
        let screen = snapshot_to_screen(&snapshot, width, 18);
        assert!(
            screen.contains("babysit-pr triage asked")
                && screen.contains("review sweep cap")
                && screen.contains("5m")
                && screen.contains("2m"),
            "parked runs and ages must survive at width {width}: {screen}"
        );
        assert_snapshot(name, screen);
    }
}

#[test]
fn parked_workflows_cap_at_three_lines_then_count_the_rest() {
    let mut value = serde_json::to_value(snapshot_with(Vec::new())).unwrap();
    value["parked_workflows"] = (1..=5)
        .map(|n| {
            serde_json::json!({"run":format!("wf_0190b8172c007000800000000000000{n}"),"workflow":format!("flow{n}"),"node":"gate","reason":"user","text":null,"checkout":"/repo","since":(fixed_now()-Duration::from_secs(60 * (6 - n))).to_string()})
        })
        .collect();
    let mut snapshot: SidebarSnapshot = serde_json::from_value(value).unwrap();
    snapshot.now = fixed_now();
    let screen = snapshot_to_screen(&snapshot, 80, 18);
    for shown in ["flow1 gate", "flow2 gate", "flow3 gate", "+2 parked"] {
        assert!(screen.contains(shown), "{shown} missing: {screen}");
    }
    for hidden in ["flow4", "flow5"] {
        assert!(!screen.contains(hidden), "{hidden} shown: {screen}");
    }
}

#[test]
fn render_active_alert_shows_banner_below_snapshot() {
    let snapshot = snapshot_with(Vec::new());
    let alert = Alert {
        reason: "snapshot failed: store not found".to_owned(),
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
    let snapshot = snapshot_with(Vec::new());
    let alert = Alert {
        reason: "snapshot failed: store not found".to_owned(),
        since: fixed_now() - Duration::from_secs(20),
        recovered_at: Some(fixed_now() - Duration::from_secs(8)),
    };
    let rendered = snapshot_to_screen_with_alert(&snapshot, Some(&alert), 80, 18);

    assert!(rendered.contains("last alert"), "{rendered}");
    assert!(rendered.contains("x dismiss"), "{rendered}");
}
#[test]
fn render_no_alert_omits_banner() {
    let snapshot = snapshot_with(Vec::new());
    let rendered = snapshot_to_screen_with_alert(&snapshot, None, 80, 18);
    assert!(
        !rendered.contains("Sidebar degraded"),
        "no alert must not render the banner:\n{rendered}"
    );
}

#[test]
fn render_truth_degraded_notice_keeps_room_chrome() {
    let mut snapshot = snapshot_with(Vec::new());
    snapshot.truth_degraded = Some(crate::store::snapshot::TruthNotice {
        carried: 2,
        since_ms: (fixed_now() - Duration::from_secs(8)).as_millisecond() as u64,
        pane_ids: Vec::new(),
    });
    let rendered = snapshot_to_screen_with_alert(&snapshot, None, 80, 18);

    assert!(rendered.contains("pane source degraded"), "{rendered}");
    assert!(rendered.contains("2 carried panes"), "{rendered}");
    assert!(rendered.contains("? for help"), "{rendered}");
}

#[test]
fn bottom_chrome_active_alert_suppresses_dashboard_store_and_footer() {
    let mut snapshot = snapshot_with(Vec::new());
    snapshot.providers = vec![provider_panel(
        "claude",
        "Claude",
        173,
        true,
        true,
        Some((25, 40)),
    )];
    snapshot.value_tally = Some(bottom_tally().into());
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

#[test]
fn active_alert_suppresses_truth_degraded_notice() {
    let mut snapshot = snapshot_with(Vec::new());
    snapshot.truth_degraded = Some(crate::store::snapshot::TruthNotice {
        carried: 1,
        since_ms: snapshot.now.as_millisecond() as u64,
        pane_ids: Vec::new(),
    });
    let alert = Alert::active("snapshot failed", snapshot.now);

    let (lines, _) = bottom_chrome_texts(&snapshot, Some(&alert));
    let text = lines.join("\n");

    assert!(text.contains("Sidebar degraded"), "{text}");
    assert!(!text.contains("pane source degraded"), "{text}");
}

#[test]
fn render_gate_hold_notice_keeps_room_chrome() {
    let snapshot = snapshot_with(Vec::new());
    let ui = UiState {
        selected_index: Some(0),
        gate_notice: Some(GateNotice {
            rule: crate::diag::record::GateRule::EmptyStampedFrame,
        }),
        ..UiState::default()
    };

    let (lines, _) = bottom_chrome_texts_with_ui(&snapshot, None, &ui);
    let text = lines.join("\n");

    assert!(text.contains("pane updates held"), "{text}");
    assert!(text.contains("empty pane frame"), "{text}");
    assert!(text.contains("? for help"), "{text}");
}

#[test]
fn active_alert_suppresses_gate_hold_notice() {
    let snapshot = snapshot_with(Vec::new());
    let ui = UiState {
        selected_index: Some(0),
        gate_notice: Some(GateNotice {
            rule: crate::diag::record::GateRule::AgentDemotedToProcess,
        }),
        ..UiState::default()
    };
    let alert = Alert::active("snapshot failed", snapshot.now);

    let (lines, _) = bottom_chrome_texts_with_ui(&snapshot, Some(&alert), &ui);
    let text = lines.join("\n");

    assert!(text.contains("Sidebar degraded"), "{text}");
    assert!(!text.contains("pane updates held"), "{text}");
}
