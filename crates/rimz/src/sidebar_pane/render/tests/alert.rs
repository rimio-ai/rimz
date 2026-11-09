use super::*;

fn drainer_snapshot() -> SidebarSnapshot {
    use crate::store::snapshot::{HookDrainerOutage, HookDrainerOutageAgent};

    let mut coder = agent("session", "claude", AgentStatus::Running, None, None, None);
    coder.name = Some("generated-name".into());
    coder.role = Some("coder".into());
    let mut snapshot = snapshot_with(vec![coder]);
    snapshot.providers = vec![provider_panel(
        "claude",
        "Claude",
        173,
        true,
        true,
        Some((25, 40)),
    )];
    snapshot.value_tally = Some(bottom_tally().into());
    snapshot.hook_drainer_outage = Some(HookDrainerOutage {
        since: fixed_now() - Duration::from_secs(40),
        agents: vec![HookDrainerOutageAgent {
            agent_kind: crate::ids::AgentKind::new_unchecked("claude"),
            agent_name: Some("generated-name".into()),
        }],
    });
    snapshot
}

#[test]
fn render_hook_drainer_alert_names_the_card_and_keeps_dashboard_and_footer() {
    let snapshot = drainer_snapshot();
    let screen = snapshot_to_screen(&snapshot, 80, 24);
    assert!(
        screen.contains("hook drainer unreachable for 40s · coder's card may be stale"),
        "{screen}"
    );
    assert!(
        screen.contains("Claude") && screen.contains("? for help"),
        "{screen}"
    );
    assert_snapshot("hook_drainer_active", screen);
}

#[test]
fn render_hook_drainer_alert_counts_other_cards() {
    use crate::store::snapshot::HookDrainerOutageAgent;

    let mut snapshot = drainer_snapshot();
    snapshot
        .hook_drainer_outage
        .as_mut()
        .unwrap()
        .agents
        .extend([
            HookDrainerOutageAgent {
                agent_kind: crate::ids::AgentKind::new_unchecked("codex"),
                agent_name: Some("reviewer".into()),
            },
            HookDrainerOutageAgent {
                agent_kind: crate::ids::AgentKind::new_unchecked("claude"),
                agent_name: None,
            },
        ]);
    let screen = snapshot_to_screen(&snapshot, 80, 24);
    assert!(
        screen.contains("coder and 2 other cards may be stale"),
        "{screen}"
    );
    assert!(
        screen.contains("Claude") && screen.contains("? for help"),
        "{screen}"
    );
    assert_snapshot("hook_drainer_multiple", screen);
}

#[test]
fn render_hook_drainer_alert_lingers_with_the_observed_gap() {
    let mut snapshot = drainer_snapshot();
    snapshot.hook_drainer_outage = None;
    let view = ViewState {
        hook_drainer_alert: Some(Alert {
            reason: "hook drainer unreachable".into(),
            since: fixed_now() - Duration::from_secs(160),
            recovered_at: Some(fixed_now() - Duration::from_secs(120)),
        }),
        ..selected_ui()
    };
    let screen =
        snapshot_to_screen_with_alert_and_ui(&snapshot, None, &view, &UiState::default(), 80, 24);
    assert!(
        screen.contains("last alert 2m ago: hook drainer unreachable for 40s · Esc dismiss"),
        "{screen}"
    );
    assert!(
        screen.contains("Claude") && screen.contains("? for help"),
        "{screen}"
    );
    assert_snapshot("hook_drainer_recovered", screen);
}

#[test]
fn render_hook_drainer_alert_uses_name_and_provider_fallbacks_without_a_matching_card() {
    let mut snapshot = drainer_snapshot();
    let outage = snapshot.hook_drainer_outage.as_mut().unwrap();
    outage.agents[0].agent_kind = crate::ids::AgentKind::new_unchecked("codex");
    for (name, expected) in [
        (
            Some("generated-name".to_owned()),
            "@generated-name's card may be stale",
        ),
        (None, "Codex's card may be stale"),
    ] {
        snapshot.hook_drainer_outage.as_mut().unwrap().agents[0].agent_name = name;
        let screen = snapshot_to_screen(&snapshot, 110, 24);
        assert!(screen.contains(expected), "{screen}");
    }
}

#[test]
fn render_hook_drainer_alert_coexists_with_lingering_but_not_active_refresh_alerts() {
    let snapshot = drainer_snapshot();
    let mut refresh = Alert {
        reason: "snapshot failed".into(),
        since: fixed_now() - Duration::from_secs(20),
        recovered_at: Some(fixed_now() - Duration::from_secs(8)),
    };
    let screen = snapshot_to_screen_with_alert(&snapshot, Some(&refresh), 110, 24);
    let drainer_row = screen
        .lines()
        .position(|line| line.contains("hook drainer unreachable"));
    assert!(
        drainer_row.is_some(),
        "the drainer line must coexist with a recovered refresh alert: {screen}"
    );
    let refresh_row = screen
        .lines()
        .position(|line| line.contains("last alert 8s ago: snapshot failed"))
        .unwrap();
    assert_eq!(drainer_row.unwrap() + 1, refresh_row);
    refresh.recovered_at = None;
    let screen = snapshot_to_screen_with_alert(&snapshot, Some(&refresh), 110, 24);
    assert!(screen.contains("Sidebar degraded"), "{screen}");
    assert!(!screen.contains("hook drainer unreachable"), "{screen}");
}

#[test]
fn throttle_notices_keep_the_cause_count_and_fix_in_bottom_chrome() {
    let mut value = serde_json::to_value(snapshot_with(Vec::new())).unwrap();
    value["throttle_notices"] = serde_json::json!([
        {"kind": "check_failed", "reason": "capacity check failing: claude exit 1", "count": 4},
        {"kind": "disk", "reason": "free disk 1G < 2G at /repo", "count": 1},
        {"kind": "store", "reason": "cannot count open agents: broken record", "count": 2}
    ]);
    let mut snapshot: SidebarSnapshot = serde_json::from_value(value.clone()).unwrap();
    snapshot.now = fixed_now();
    let wide = snapshot_to_screen(&snapshot, 110, 24);
    let narrow = snapshot_to_screen(&snapshot, 46, 24);
    value["throttle_notices"] = serde_json::json!([]);
    let mut clear: SidebarSnapshot = serde_json::from_value(value).unwrap();
    clear.now = fixed_now();
    let clear = snapshot_to_screen(&clear, 110, 24);
    assert!(
        wide.contains("⚠ capacity check failing: claude exit 1")
            && wide.contains("· 4 held · rimz doctor")
            && wide.contains("⚠ starts held: free disk 1G < 2G at /repo")
            && wide.contains("⚠ starts held: cannot count open agents: broken record")
            && narrow.contains("· 4 held · rimz doctor")
            && !wide.contains("1 held")
            && !clear.contains("rimz doctor"),
        "notices keep their count and fix while the cause clips:\n{wide}\n{narrow}\n{clear}"
    );
    let lines: Vec<_> = wide.lines().collect();
    let held = lines
        .iter()
        .rposition(|line| line.contains("rimz doctor"))
        .unwrap();
    assert!(lines[held + 1].trim().is_empty(), "{wide}");
    for (name, screen) in [
        ("throttle_notices_wide", wide),
        ("throttle_notices_l2", narrow),
        ("throttle_notices_clear", clear),
    ] {
        assert_snapshot(name, screen);
    }
    let theme = Theme::fixed(false);
    let line =
        super::super::chrome::throttle_notice_line(&theme, &snapshot.throttle_notices[0], 80);
    assert!(
        line.spans
            .iter()
            .filter(|span| !span.content.trim().is_empty())
            .all(|span| span.style.fg == theme.warn(Modifier::empty()).fg)
    );
    snapshot.providers = two_provider_panels();
    let (lines, _) = bottom_chrome_texts(&snapshot, None);
    let notice = lines
        .iter()
        .position(|line| line.contains("rimz doctor"))
        .unwrap();
    assert!(
        lines
            .iter()
            .skip(notice + 1)
            .any(|line| line.contains("? for help")),
        "{lines:?}"
    );
    let alert = Alert::active("unreadable", fixed_now());
    assert!(
        !bottom_chrome_texts(&snapshot, Some(&alert))
            .0
            .iter()
            .any(|line| line.contains("rimz doctor"))
    );
}

#[test]
fn legacy_workflow_parks_do_not_draw_attention_notices() {
    for count in [1, 3] {
        let snapshot = legacy_workflow_snapshot(count);
        for width in [46, 80] {
            let screen = snapshot_to_screen(&snapshot, width, 18);
            assert!(!screen.contains("rimz workflow list"), "{screen}");
            assert!(!screen.contains("runs parked"), "{screen}");
        }
    }
}

fn legacy_workflow_snapshot(count: usize) -> SidebarSnapshot {
    let mut value = serde_json::to_value(snapshot_with(Vec::new())).unwrap();
    value["parked_workflows"] = (1..=count)
        .map(|n| {
            serde_json::json!({
                "run": format!("wf_0190b8172c007000800000000000000{n}"),
                "address": "land#dollar-align",
                "since": fixed_now() - Duration::from_secs(60 * (count + 1 - n) as u64)
            })
        })
        .collect();
    let mut snapshot: SidebarSnapshot = serde_json::from_value(value).unwrap();
    snapshot.now = fixed_now();
    snapshot
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
    assert!(rendered.contains("Esc dismiss"), "{rendered}");
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
    snapshot.stall = serde_json::from_value(serde_json::json!([
        {"resource":"memory", "percent":34, "phase":"active"}
    ]))
    .unwrap();
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

    // The alert is one notice, split into rows at the chrome's 40-cell inner width.
    let words = text.split_whitespace().collect::<Vec<_>>();
    assert_eq!(
        words[1..].join(" "),
        "Sidebar degraded for 0s: snapshot failed",
        "only the active alert remains:\n{text}"
    );
    assert_eq!(words.len(), 7, "only the active alert remains:\n{text}");
    assert!(!text.contains("Claude"), "{text}");
    assert!(!text.contains("W:"), "{text}");
    assert!(!text.contains("? for help"), "{text}");
    assert!(!text.contains("stall"), "{text}");
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
    let ui = ViewState {
        selected_index: Some(0),
        ..Default::default()
    };
    let pane_ui = UiState {
        gate_notice: Some(GateNotice {
            rule: crate::diag::record::GateRule::EmptyStampedFrame,
        }),
        ..UiState::default()
    };

    let (lines, _) = bottom_chrome_texts_with_ui(&snapshot, None, &ui, &pane_ui);
    let text = lines.join("\n");

    assert!(text.contains("pane updates held"), "{text}");
    assert!(text.contains("empty pane frame"), "{text}");
    assert!(text.contains("? for help"), "{text}");
}

#[test]
fn active_alert_suppresses_gate_hold_notice() {
    let snapshot = snapshot_with(Vec::new());
    let ui = ViewState {
        selected_index: Some(0),
        ..Default::default()
    };
    let pane_ui = UiState {
        gate_notice: Some(GateNotice {
            rule: crate::diag::record::GateRule::AgentDemotedToProcess,
        }),
        ..UiState::default()
    };
    let alert = Alert::active("snapshot failed", snapshot.now);

    let (lines, _) = bottom_chrome_texts_with_ui(&snapshot, Some(&alert), &ui, &pane_ui);
    let text = lines.join("\n");

    assert!(text.contains("Sidebar degraded"), "{text}");
    assert!(!text.contains("pane updates held"), "{text}");
}
