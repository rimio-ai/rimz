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
        screen.contains("last alert 2m ago: hook drainer unreachable for 40s · x dismiss"),
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
fn capacity_waits_share_one_cockpit_line_with_parked_runs() {
    let mut value = serde_json::to_value(snapshot_with(Vec::new())).unwrap();
    value["parked_workflows"] = serde_json::json!([
        {
            "run": "wf_0190b8172c0070008000000000000001", "workflow": "review",
            "node": "triage", "reason": "asked", "text": null,
            "checkout": "/repo", "since": fixed_now()
        }
    ]);
    value["capacity_wait"] = serde_json::json!({
        "kinds": ["claude", "codex"], "since": fixed_now(),
        "gives_up_at": fixed_now() + Duration::from_secs(3600), "count": 4
    });
    let mut snapshot: SidebarSnapshot = serde_json::from_value(value.clone()).unwrap();
    snapshot.now = fixed_now();
    let phrase = crate::theme::fmt::capacity_wait(
        &[
            crate::ids::AgentKind::new_unchecked("claude"),
            crate::ids::AgentKind::new_unchecked("codex"),
        ],
        fixed_now(),
        Some(fixed_now() + Duration::from_secs(3600)),
        fixed_now(),
    );
    let wide = snapshot_to_screen(&snapshot, 110, 18);
    let narrow = snapshot_to_screen(&snapshot, 46, 18);
    value["capacity_wait"]["gives_up_at"] = serde_json::Value::Null;
    value["capacity_wait"]["count"] = 1.into();
    let mut forever: SidebarSnapshot = serde_json::from_value(value).unwrap();
    forever.now = fixed_now();
    let forever = snapshot_to_screen(&forever, 110, 18);
    assert!(
        wide.contains(&phrase)
            && wide.contains("· 4 waiting")
            && wide.contains("review triage asked")
            && narrow.contains("waiting for capacity")
            && narrow.contains("4 waiting")
            && !forever.contains("gives up")
            && !forever.contains("1 waiting")
            && forever.contains("waiting for capacity (claude, codex) since"),
        "capacity waits must share one inert line, keep the count when narrow, and omit a forever deadline:\n{wide}\n{narrow}\n{forever}"
    );
    assert_eq!(wide.matches("waiting for capacity").count(), 1);
    for (name, screen) in [
        ("capacity_waits_wide", wide),
        ("capacity_waits_l2", narrow),
        ("capacity_waits_forever", forever),
    ] {
        let since = fixed_now()
            .to_zoned(jiff::tz::TimeZone::system())
            .strftime("%H:%M");
        let until = (fixed_now() + Duration::from_secs(3600))
            .to_zoned(jiff::tz::TimeZone::system())
            .strftime("%H:%M");
        assert_snapshot(
            name,
            screen
                .replace(&since.to_string(), "HH:MM")
                .replace(&until.to_string(), "HH:MM"),
        );
    }
}

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
