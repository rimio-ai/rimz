//! Which sidebar events provoke a producer fetch, when, and how a burst
//! coalesces into one.

use super::*;

fn parked_rig() -> (Rig, FrameOutput, SidebarSnapshot) {
    let clock = LoopClock::play("2026-01-01T00:00:00Z".parse().unwrap());
    let mut rig = Rig::with_clock(None, BodyLens::default(), None, clock.clone());
    let output = FrameOutput::default();
    rig.terminal = Terminal::with_options(
        PaneBackend::headless(output.clone()),
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 80, 24)),
        },
    )
    .unwrap();
    let mut value = serde_json::to_value(agent_snapshot(&rig.ws)).unwrap();
    value["parked_workflows"] = serde_json::json!([{
        "run":"wf_0190b8172c0070008000000000000001", "address":"review#repo",
        "since":"2026-01-01T00:00:00Z"
    }]);
    let mut snapshot: SidebarSnapshot = serde_json::from_value(value).unwrap();
    snapshot.now = clock.timestamp();
    (rig, output, snapshot)
}

fn parked_screen(rig: &mut Rig, output: &FrameOutput) -> String {
    let until_frame = rig
        .state
        .pane
        .next_frame
        .saturating_duration_since(rig.state.pane.clock.now());
    rig.state.pane.clock.advance(until_frame);
    rig.paint(true);
    let mut screen = vt100::Parser::new(24.try_into().unwrap(), 80.try_into().unwrap(), 0);
    screen.process(&output.0.lock().unwrap());
    screen.screen().contents()
}

#[test]
fn park_notice_dismissal_hides_the_painted_pairs_and_a_later_fold_reraises_it() {
    let (mut rig, output, mut snapshot) = parked_rig();
    rig.fold(snapshot.clone(), SnapshotSource::Produced);
    assert!(parked_screen(&mut rig, &output).contains("rimz workflow list"));
    rig.input(KeyAction::Dismiss);
    assert_eq!(rig.view().ui().dismissed_parks.len(), 1);
    assert!(!parked_screen(&mut rig, &output).contains("rimz workflow list"));
    let mut new_park = snapshot.parked_workflows[0].clone();
    new_park.run = "wf_0190b8172c0070008000000000000002".parse().unwrap();
    snapshot.parked_workflows.push(new_park);
    rig.fold(snapshot.clone(), SnapshotSource::Produced);
    let screen = parked_screen(&mut rig, &output);
    assert!(screen.contains("2 workflow runs parked"), "{screen}");
    let recovered = crate::sidebar_pane::render::Alert {
        reason: "recovered".into(),
        since: snapshot.now,
        recovered_at: Some(snapshot.now),
    };
    rig.view().ctx().health.alert = Some(recovered.clone());
    rig.view().ui().hook_drainer_alert = Some(recovered);
    rig.input(KeyAction::Dismiss);
    assert_eq!(rig.view().ui().dismissed_parks.len(), 2);
    assert!(rig.view().ctx().health.alert.is_none());
    assert!(rig.view().ui().hook_drainer_alert.is_none());
    snapshot.parked_workflows.remove(0);
    snapshot.parked_workflows[0].since += Duration::from_secs(1);
    rig.fold(snapshot, SnapshotSource::Produced);
    assert!(parked_screen(&mut rig, &output).contains("review#repo parked"));
    rig.input(KeyAction::Dismiss);
    assert_eq!(
        rig.view().ui().dismissed_parks.len(),
        1,
        "dismissal replaces the record"
    );
    assert!(!rig.state.alert_active());
}

#[test]
fn dismiss_leaves_an_unpainted_park_undismissed() {
    let (mut rig, output, snapshot) = parked_rig();
    let mut empty = snapshot.clone();
    empty.parked_workflows.clear();
    rig.fold(empty, SnapshotSource::Produced);
    let screen = parked_screen(&mut rig, &output);
    assert!(!screen.contains("rimz workflow list"), "{screen}");
    rig.send(FetchUpdate::Snapshot {
        snapshot: Box::new(snapshot),
        phase: FetchPhase::Final,
        source: SnapshotSource::Produced,
    });
    rig.input(KeyAction::Dismiss);
    assert!(rig.view().ui().dismissed_parks.is_empty());
    let screen = parked_screen(&mut rig, &output);
    assert!(screen.contains("rimz workflow list"), "{screen}");
}

#[test]
fn dismiss_leaves_a_park_clipped_below_the_painted_frame_undismissed() {
    let (mut rig, output, mut snapshot) = parked_rig();
    snapshot.skipped_settings = 1;
    rig.terminal = Terminal::with_options(
        PaneBackend::headless(output.clone()),
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 80, 1)),
        },
    )
    .unwrap();
    rig.fold(snapshot, SnapshotSource::Produced);
    let screen = parked_screen(&mut rig, &output);
    assert!(!screen.contains("rimz workflow list"), "{screen}");
    rig.input(KeyAction::Dismiss);
    assert!(rig.view().ui().dismissed_parks.is_empty());
}

#[test]
fn dismiss_records_only_painted_pairs_when_replay_adds_or_reparks_a_run() {
    for repark in [false, true] {
        let (mut rig, output, mut snapshot) = parked_rig();
        rig.fold(snapshot.clone(), SnapshotSource::Produced);
        assert!(parked_screen(&mut rig, &output).contains("review#repo parked"));
        let painted = snapshot.parked_workflows[0].clone();
        if repark {
            snapshot.parked_workflows[0].since += Duration::from_secs(1);
        } else {
            let mut new_park = painted.clone();
            new_park.run = "wf_0190b8172c0070008000000000000002".parse().unwrap();
            snapshot.parked_workflows.push(new_park);
        }
        rig.send(FetchUpdate::Snapshot {
            snapshot: Box::new(snapshot),
            phase: FetchPhase::Final,
            source: SnapshotSource::Produced,
        });
        rig.input(KeyAction::Dismiss);
        assert_eq!(
            rig.view().ui().dismissed_parks,
            [(painted.run, painted.since)].into_iter().collect(),
            "repark={repark}"
        );
        let screen = parked_screen(&mut rig, &output);
        assert!(screen.contains("rimz workflow list"), "{screen}");
    }
}

#[test]
fn dismiss_leaves_a_park_hidden_by_the_painted_alert_when_replay_recovers_it() {
    let (mut rig, output, snapshot) = parked_rig();
    rig.fold(snapshot.clone(), SnapshotSource::Produced);
    for _ in 0..super::super::super::health::ALERT_AFTER_FAILURES {
        rig.deliver(FetchUpdate::Failed {
            error: "store not found".into(),
        });
    }
    let screen = parked_screen(&mut rig, &output);
    assert!(screen.contains("Sidebar degraded"), "{screen}");
    assert!(!screen.contains("rimz workflow list"), "{screen}");
    rig.send(FetchUpdate::Snapshot {
        snapshot: Box::new(snapshot),
        phase: FetchPhase::Final,
        source: SnapshotSource::Produced,
    });
    rig.input(KeyAction::Dismiss);
    assert!(rig.view().ctx().health.alert.is_none());
    assert!(rig.view().ui().dismissed_parks.is_empty());
    let screen = parked_screen(&mut rig, &output);
    assert!(screen.contains("rimz workflow list"), "{screen}");
    assert!(!screen.contains("Sidebar degraded"), "{screen}");
}

#[test]
fn active_refresh_alert_dismissal_leaves_the_hidden_park_for_a_second_press() {
    let (mut rig, output, snapshot) = parked_rig();
    rig.fold(snapshot, SnapshotSource::Produced);
    for _ in 0..super::super::super::health::ALERT_AFTER_FAILURES {
        rig.deliver(FetchUpdate::Failed {
            error: "store not found".into(),
        });
    }
    let screen = parked_screen(&mut rig, &output);
    assert!(screen.contains("Sidebar degraded"), "{screen}");
    assert!(!screen.contains("rimz workflow list"), "{screen}");
    rig.input(KeyAction::Dismiss);
    assert!(rig.view().ui().dismissed_parks.is_empty());
    let screen = parked_screen(&mut rig, &output);
    assert!(screen.contains("rimz workflow list"), "{screen}");
    assert!(!screen.contains("Sidebar degraded"), "{screen}");
    rig.input(KeyAction::Dismiss);
    assert_eq!(rig.view().ui().dismissed_parks.len(), 1);
    assert!(!parked_screen(&mut rig, &output).contains("rimz workflow list"));
}

#[test]
fn hook_drainer_alert_dismissal_and_search_do_not_change_refresh_health() {
    use crate::store::snapshot::{HookDrainerOutage, HookDrainerOutageAgent};

    let mut rig = Rig::new();
    let mut fetched = agent_snapshot(&rig.ws);
    fetched.hook_drainer_outage = Some(HookDrainerOutage {
        since: fetched.now,
        agents: vec![HookDrainerOutageAgent {
            agent_kind: crate::ids::AgentKind::new_unchecked("claude"),
            agent_name: Some("coder".into()),
        }],
    });
    rig.fold(fetched.clone(), SnapshotSource::Cached);
    assert!(
        rig.view().ui().hook_drainer_alert.is_some(),
        "the outage needs its own alert slot"
    );
    assert!(!rig.state.alert_active());
    rig.input(KeyAction::Dismiss);
    assert!(
        rig.view()
            .ui()
            .hook_drainer_alert
            .as_ref()
            .unwrap()
            .is_active()
    );
    rig.input(KeyAction::Search);
    assert_eq!(rig.view().ui().search_draft.as_deref(), Some(""));
    rig.view().ui().search_draft = Some("cod".into());
    fetched.now += jiff::SignedDuration::from_secs(1);
    rig.fold(fetched.clone(), SnapshotSource::Cached);
    assert_eq!(rig.view().ui().search_draft.as_deref(), Some("cod"));
    rig.view().ui().search_draft = None;
    fetched.hook_drainer_outage = None;
    rig.fold(fetched, SnapshotSource::Cached);
    assert!(
        rig.view()
            .ui()
            .hook_drainer_alert
            .as_ref()
            .unwrap()
            .recovered_at
            .is_some()
    );
    rig.input(KeyAction::Dismiss);
    assert!(rig.view().ui().hook_drainer_alert.is_none());
}

#[test]
fn coalesced_idle_interim_and_final_complete_without_reapplying_content() {
    let mut rig = Rig::new();
    let mut snapshot = agent_snapshot(&rig.ws);
    rig.fold(snapshot.clone(), SnapshotSource::Produced);
    rig.request_if_idle(FetchRequest::default());
    assert!(rig.next_request().is_some());
    rig.request_if_idle(FetchRequest::fresh_panes());
    assert!(rig.next_request().is_none());
    for phase in [FetchPhase::Interim, FetchPhase::Final] {
        snapshot.now += jiff::SignedDuration::from_secs(1);
        rig.send(FetchUpdate::Snapshot {
            snapshot: Box::new(snapshot.clone()),
            phase,
            source: SnapshotSource::Produced,
        });
    }
    rig.state.on_snapshot();
    assert_eq!(rig.view().ctx().current.now, snapshot.now);
    assert_eq!(
        rig.state.pane.content_applications, 1,
        "coalescing must retain clock-only delivery"
    );
    rig.request_if_idle(FetchRequest::fresh_panes());
    assert!(
        rig.next_request().unwrap().is_fresh_panes(),
        "unchanged final clears the latch"
    );
}

#[test]
fn idle_delivery_skips_content_but_changes_reach_the_renderer() {
    let mut rig = Rig::new();
    let mut snapshot = agent_snapshot(&rig.ws);
    rig.fold(snapshot.clone(), SnapshotSource::Produced);
    snapshot.now += jiff::SignedDuration::from_secs(1);
    rig.fold(snapshot.clone(), SnapshotSource::Produced);
    assert_eq!(
        rig.state.pane.content_applications, 1,
        "idle delivery skips content"
    );
    assert_eq!(rig.view().ctx().current.now, snapshot.now);

    snapshot.display_name = "renamed room".into();
    rig.fold(snapshot.clone(), SnapshotSource::Produced);
    assert_eq!(rig.view().ctx().current.display_name, "renamed room");
    assert_eq!(rig.state.pane.content_applications, 2);

    let sample = crate::store::snapshot::PresenceSample {
        human_clients: 1,
        last_input_ms: Some(100),
        client_input_ms: Default::default(),
        client_views: Vec::new(),
        sampled_at_ms: 100,
    };
    snapshot.presence = Some(crate::store::snapshot::SidebarPresence::classify(
        sample.clone(),
        199,
        100,
    ));
    rig.fold(snapshot.clone(), SnapshotSource::Produced);
    snapshot.presence = Some(crate::store::snapshot::SidebarPresence::classify(
        sample, 200, 100,
    ));
    snapshot.now =
        Timestamp::from_millisecond((snapshot.sidebar.afk_after_ms() + 100) as i64).unwrap();
    rig.fold(snapshot.clone(), SnapshotSource::Produced);
    assert_eq!(rig.view().ctx().current.presence, snapshot.presence);
    assert_eq!(rig.state.pane.content_applications, 4);
}

#[test]
fn renderer_local_reconciliation_state_forces_full_content() {
    type Hold = fn(&mut PaneState, &mut ViewState, &mut ViewContext);
    let blockers: [(&str, Hold); 8] = [
        ("should_exit", |state, _, _| state.should_exit = true),
        ("overlay_baseline", |_, _, view| {
            view.overlay_baseline = Some(view.current.clone());
        }),
        ("gate", |_, _, view| view.gate.reject_streak = 1),
        ("failure_streak", |_, _, view| {
            view.health.failure_streak = 1
        }),
        ("freeze", |_, ui, _| {
            ui.freeze = Some(crate::sidebar_pane::render::LayoutFreeze {
                frozen: crate::sidebar_pane::render::FrozenOrder::default(),
                armed_ms: jiff::Timestamp::now().as_millisecond(),
            });
        }),
        ("tab_read_dwell", |_, _, view| {
            view.tab_read_dwell_until = Some(Instant::now() + Duration::from_secs(60));
        }),
        ("self_close", |state, _, _| {
            // The rig's own view has shown a sibling, which would close at once instead.
            state.self_close = Default::default();
            state.self_close.should_close(Some(0), Instant::now());
        }),
        ("focus_repair", |state, _, _| {
            state.pending_focus_repair = Some(PendingFocusRepair {
                pane_id: pane("terminal_9", "tab_0", false).pane_id,
                generation: 1,
                clients: Vec::new(),
                sent_at_ms: crate::utils::time::unix_now_ms(),
            });
        }),
    ];
    let skipped: Vec<_> = blockers
        .into_iter()
        .filter_map(|(name, hold)| {
            let mut rig = Rig::new();
            let mut snapshot = agent_snapshot(&rig.ws);
            rig.fold(snapshot.clone(), SnapshotSource::Produced);
            {
                let pane = &mut rig.state.pane;
                let mut guard = rig.state.views.rig(&pane.config.own_pane, pane.clock.now());
                let (ui, view) = guard.pair();
                hold(pane, ui, view);
            }
            // A view decides clock-only at take, so the first equal fold under the hold is full.
            snapshot.now += jiff::SignedDuration::from_secs(1);
            rig.fold(snapshot.clone(), SnapshotSource::Produced);
            (rig.state.pane.content_applications != 2).then_some(name)
        })
        .collect();
    assert!(
        skipped.is_empty(),
        "held across an equal fold, these must re-admit full content: {skipped:?}"
    );
    let rig = Rig::new();
    assert!(rig.state.admits_clock_only(false));
    assert!(
        !rig.state.admits_clock_only(true),
        "a gate-rejected fold must not admit clock-only delivery"
    );
}

#[test]
fn a_late_consumer_does_not_reinsert_an_overlay_expired_since_ingress() {
    let clock = LoopClock::play(Timestamp::from_second(1_700_000_000).unwrap());
    let mut rig = Rig::with_clock(None, BodyLens::default(), None, clock.clone());
    let mut process = pane("terminal_9", "tab_0", false);
    process.command = Some("sh".to_owned());
    let pane_id = process.pane_id.clone();
    let mut snapshot = snapshot_with_panes(&rig.ws, vec![process]);
    snapshot.panes_produced_at_ms = Some(clock.now_ms() - 1);
    rig.fold(snapshot, SnapshotSource::Produced);
    let envelope = SidebarEventEnvelope::new(
        rig.ws.clone(),
        Some("rimz-test".to_owned()),
        clock.now_ms(),
        SidebarEvent::CommandChanged {
            pane_id: pane_id.clone(),
            command: "vim".to_owned(),
        },
    );
    rig.record_event(&envelope);
    clock.advance(Duration::from_millis(12_001));
    rig.state.events().prune(clock.now_ms());
    rig.state.on_event(&mut rig.terminal, envelope);
    assert_eq!(
        rig.view()
            .ctx()
            .current
            .row_owning_pane(&pane_id)
            .unwrap()
            .pane
            .as_ref()
            .unwrap()
            .command
            .as_deref(),
        Some("sh"),
        "a late pane must not extend the overlay's ingress TTL"
    );
    assert_eq!(*rig.state.events(), EventStore::default());
}

#[test]
fn a_body_filter_save_reaches_origin_and_peer_in_memory_only_with_effects() {
    let mut rig = Rig::new();
    let plane = super::super::super::plane::DataPlane::detached();
    let mut peer = rig.state.pane.config.clone();
    peer.instance_id = crate::SidebarInstanceId::new();
    let (origin_tx, origin_rx) = std::sync::mpsc::sync_channel(64);
    let (peer_tx, peer_rx) = std::sync::mpsc::sync_channel(64);
    let _origin = plane.subscribe(&rig.state.pane.config, origin_tx);
    let _peer = plane.subscribe(&peer, peer_tx);
    rig.state.room_events = Some(plane.room_events(&rig.runtime));
    let filter = BodyLens {
        filter: None,
        query: Some("auth".to_owned()),
    };
    rig.state.persist_body_filter(filter.clone());
    assert_eq!(crate::sidebar::body_filter::load(&rig.runtime), filter);
    for inbox in [&origin_rx, &peer_rx] {
        let wakeup = inbox.try_recv();
        assert!(
            matches!(&wakeup, Ok(Wakeup::Event(envelope))
                if envelope.workspace_id == rig.ws
                    && envelope.session_name.as_deref() == Some("rimz-test")
                    && envelope.event == SidebarEvent::BodyFilterChanged),
            "the save must fan out in memory, including its origin: {wakeup:?}"
        );
        assert!(inbox.try_recv().is_err());
    }

    rig.state.pane.clock = LoopClock::play(Timestamp::now());
    rig.state.persist_body_filter(BodyLens::default());
    assert_eq!(
        crate::sidebar::body_filter::load(&rig.runtime),
        BodyLens::default()
    );
    for inbox in [&origin_rx, &peer_rx] {
        assert!(inbox.try_recv().is_err(), "play clocks disable dispatch");
    }
}

#[test]
fn synthetic_fold_releases_baseline_after_overlay_expires() {
    let mut rig = Rig::new();
    let mut process = pane("terminal_9", "tab_0", false);
    process.command = Some("sh".to_owned());
    let pane_id = process.pane_id.clone();
    let snapshot = snapshot_with_panes(&rig.ws, vec![process]);
    rig.fold(snapshot.clone(), SnapshotSource::Produced);
    assert!(rig.view().ctx().overlay_baseline.is_none());
    rig.send(FetchUpdate::Snapshot {
        snapshot: Box::new(snapshot),
        phase: FetchPhase::Final,
        source: SnapshotSource::Produced,
    });

    rig.event(SidebarEvent::CommandChanged {
        pane_id: pane_id.clone(),
        command: "vim".to_owned(),
    });
    rig.state.on_snapshot();
    // The event's own fold catches the view up to the queued equal content and fuses it, so the
    // take that follows has nothing left to apply.
    assert_eq!(
        rig.state.pane.content_applications, 1,
        "a local event requires queued content to reconcile through fusion"
    );
    assert!(rig.view().ctx().overlay_baseline.is_some());
    assert_eq!(
        rig.view()
            .ctx()
            .current
            .row_owning_pane(&pane_id)
            .unwrap()
            .pane
            .as_ref()
            .unwrap()
            .command
            .as_deref(),
        Some("vim")
    );

    *rig.state.events() = EventStore::default();
    rig.state.fold_fused_now();
    assert!(rig.view().ctx().overlay_baseline.is_none());
    assert_eq!(
        rig.view()
            .ctx()
            .current
            .row_owning_pane(&pane_id)
            .unwrap()
            .pane
            .as_ref()
            .unwrap()
            .command
            .as_deref(),
        Some("sh")
    );
}

pub(super) fn focus_records(rig: &Rig) -> Vec<serde_json::Value> {
    std::fs::read_to_string(crate::diag::focus_trace::log_path(rig._dir.path()))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()["event"].clone())
        .filter(|event| event["kind"] == "fold_decided")
        .collect()
}

pub(super) fn enable_focus_trace(rig: &mut Rig) {
    rig.state.pane.diag = crate::diag::DiagSink::under(
        rig._dir.path().to_owned(),
        rig.ws.clone(),
        "rimz-test",
        Some(rig.state.pane.config.instance_id.clone()),
    );
}

#[test]
fn carried_baseline_seats_while_the_sidebar_has_focus() {
    carried_baseline_fold("own");
}

#[test]
fn carried_baseline_yields_to_real_focus() {
    carried_baseline_fold("other");
}

#[test]
fn carried_baseline_drops_when_its_pane_departed() {
    carried_baseline_fold("departed");
}

#[test]
fn carried_baseline_holds_without_a_frame() {
    carried_baseline_fold("frameless");
}

fn carried_baseline_fold(case: &str) {
    let own = pane("terminal_10", "tab_0", false).pane_id;
    let selected = pane("terminal_9", "tab_0", false).pane_id;
    let other = pane("terminal_11", "tab_0", false).pane_id;
    let mut rig = Rig::with_baseline(
        Some(own.clone()),
        BodyLens::default(),
        Some(selected.clone()),
    );
    assert_eq!(
        rig.view().ui().baseline_pane,
        Some(selected.clone()),
        "the carried identity is seated before any fold"
    );
    let output = FrameOutput::default();
    rig.terminal = Terminal::with_options(
        PaneBackend::headless(output.clone()),
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 54, 24)),
        },
    )
    .unwrap();
    let mut snapshot = agent_snapshot(&rig.ws);
    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0]
    .name = "baseline-card".into();
    let mut second = snapshot.worktree_groups[0].rows[0].clone();
    second.id = "agent-2".into();
    second.name = "other-card".into();
    second.pane.as_mut().unwrap().pane_id = other.clone();
    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows)
        .push(second);
    snapshot.own_view = Some(crate::store::snapshot::SidebarOwnView {
        sibling_count: 2,
        working_pane_ids: vec![selected.clone(), other.clone()],
        own_view_is_daemon: false,
    });
    snapshot.viewed_panes = vec![own.clone()];
    snapshot.focused_pane = Some(if case == "other" { other.clone() } else { own });
    if case == "departed" {
        std::sync::Arc::make_mut(
            &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
        )
        .remove(0);
    }
    if case == "frameless" {
        snapshot.panes_produced_at_ms = None;
    }
    rig.fold(snapshot, SnapshotSource::Produced);
    let expected = match case {
        "departed" => None,
        "other" => Some(other),
        _ => Some(selected),
    };
    assert_eq!(rig.view().ui().baseline_pane, expected);
    assert_eq!(rig.view().ui().selected_pane, expected);
    if case == "frameless" {
        assert_eq!(rig.view().ui().selected_index, None);
        return;
    }
    rig.state.pane.next_frame = Instant::now();
    rig.paint(true);
    let mut parser = vt100::Parser::new(24.try_into().unwrap(), 54.try_into().unwrap(), 0);
    parser.process(&output.0.lock().unwrap());
    let screen = parser.screen().contents();
    assert!(screen.contains("other-card"), "{screen}");
    for (name, is_selected) in [
        ("baseline-card", case == "own"),
        ("other-card", case == "other"),
    ] {
        assert_eq!(
            screen
                .lines()
                .any(|line| line.contains(name) && line.contains('▌')),
            is_selected,
            "{screen}"
        );
    }
}

#[test]
fn pre_tab_fold_seeds_without_selection() {
    let shell = pane("terminal_9", "tab_0", true);
    let own = pane("terminal_10", "tab_1", false);
    let mut rig = Rig::with_own_pane(own.pane_id.clone());
    enable_focus_trace(&mut rig);
    rig.runtime.ensure_dirs().unwrap();
    let mut seed = snapshot_with_panes(&rig.ws, vec![shell.clone()]);
    seed.focused_pane = Some(shell.pane_id.clone());
    let now_ms = crate::utils::time::unix_now_ms();
    let frame =
        crate::sidebar::frame::assemble_frame(vec![shell.clone()], now_ms - 1_000, "rimz-test");
    rig.state.seed(&WorkspaceFold {
        sampled_at: Instant::now(),
        workspace: crate::sidebar::enrich::WorkspaceSnapshot(seed),
        frame: Some(Arc::new(frame)),
    });
    let records = focus_records(&rig);
    assert_eq!(records.len(), 1, "the seed records its paint decision");
    assert_eq!(records[0]["seed"], true);
    assert_eq!(
        records[0]["own_view"], false,
        "seed decision: {}",
        records[0]
    );
    assert_eq!(
        records[0]["snapshot_focused_pane"],
        serde_json::to_value(&shell.pane_id).unwrap()
    );
    assert!(records[0].get("baseline").is_none());
    assert!(records[0].get("selected_after").is_none());
    assert_eq!(
        rig.view().ctx().current.rows().count(),
        1,
        "the cards seeded"
    );
    assert_eq!(rig.view().ui().selected_pane, None);
    assert_eq!(rig.view().ui().selected_index, None);
    assert!(rig.view().ctx().current.own_view.is_none());
}

#[test]
fn pre_tab_seed_and_worker_fold_paint_resting_cards_until_correction() {
    let own = pane("terminal_10", "tab_1", false);
    let shell = pane("terminal_11", "tab_1", true);
    let mut rig = Rig::with_own_pane(own.pane_id.clone());
    enable_focus_trace(&mut rig);
    let output = FrameOutput::default();
    rig.terminal = Terminal::with_options(
        PaneBackend::headless(output.clone()),
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 54, 24)),
        },
    )
    .unwrap();
    rig.runtime.ensure_dirs().unwrap();
    let mut seed = agent_snapshot(&rig.ws);
    seed.theme.display.card_density = crate::config::CardDensityMode::Compact;
    seed.focused_pane = Some(
        seed.worktree_groups[0].rows[0]
            .pane
            .as_ref()
            .unwrap()
            .pane_id
            .clone(),
    );
    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut seed.worktree_groups)[0].rows)[0]
        .as_agent_mut()
        .unwrap()
        .status = crate::agents::AgentStatus::Idle;
    let frame = crate::sidebar::frame::assemble_frame(
        seed.rows().filter_map(|row| row.pane.clone()).collect(),
        crate::utils::time::unix_now_ms(),
        "rimz-test",
    );
    let pre_tab = Arc::new(WorkspaceFold {
        sampled_at: Instant::now(),
        workspace: crate::sidebar::enrich::WorkspaceSnapshot(seed.clone()),
        frame: Some(Arc::new(frame)),
    });
    rig.state.seed(&pre_tab);
    let traced_seed = focus_records(&rig);
    assert_eq!(traced_seed.len(), 1, "the seed records its paint decision");
    assert_eq!(traced_seed[0]["source"], "published");
    assert_eq!(traced_seed[0]["phase"], "interim");
    assert_eq!(traced_seed[0]["seed"], true);
    assert_eq!(traced_seed[0]["own_view"], false);
    assert_eq!(
        traced_seed[0]["snapshot_focused_pane"],
        serde_json::to_value(&seed.focused_pane).unwrap()
    );
    assert!(traced_seed[0].get("baseline").is_none());
    assert!(traced_seed[0].get("selected_after").is_none());
    assert_eq!(
        rig.view().ctx().current.rows().count(),
        1,
        "the published card seeded"
    );
    rig.state.pane.next_frame = Instant::now();
    rig.paint(true);
    let mut parser = vt100::Parser::new(24.try_into().unwrap(), 54.try_into().unwrap(), 0);
    parser.process(&output.0.lock().unwrap());
    let seeded = parser.screen().contents();
    assert!(
        seeded.contains("claude"),
        "first paint is not blank:\n{seeded}"
    );
    for glyph in ['▌', '▐', '▎', '🮇'] {
        assert!(
            !seeded.contains(glyph),
            "seed has no selection glyph {glyph}:\n{seeded}"
        );
    }
    let seeded_focus = rig.view().ctx().current.focused_pane.clone();
    assert_eq!(rig.view().ui().selected_pane, None);
    assert_eq!(rig.view().ui().selected_index, None);
    assert!(!seeded.contains('┄'), "seed has no header seal:\n{seeded}");
    let agent_line = seeded
        .lines()
        .position(|line| line.contains("claude"))
        .unwrap();
    assert!(
        seeded
            .lines()
            .nth(agent_line + 1)
            .unwrap()
            .trim()
            .is_empty(),
        "the compact idle card rests at one line:\n{seeded}"
    );

    let published_focus = seed.focused_pane.clone();
    // The worker's cached fold carries the same pre-tab frame, which has no own pane yet.
    rig.deliver(FetchUpdate::Fold {
        fold: pre_tab,
        phase: FetchPhase::Final,
        source: SnapshotSource::Cached,
    });
    assert_eq!(
        focus_records(&rig),
        traced_seed,
        "the identical cached fold writes nothing"
    );
    assert_eq!(rig.view().ui().selected_pane, None);
    assert_eq!(rig.view().ui().selected_index, None);
    rig.state.pane.next_frame = Instant::now();
    rig.paint(true);
    parser.process(&output.0.lock().unwrap());
    assert_eq!(
        parser.screen().contents(),
        seeded,
        "the cached worker fold holds the resting frame"
    );
    assert_eq!(seeded_focus, published_focus);
    assert_eq!(rig.view().ctx().current.focused_pane, published_focus);

    let mut correction = rig.state.views.mirror().clone();
    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut correction.worktree_groups)[0].rows,
    )
    .push(snapshot_with_panes(&rig.ws, vec![shell.clone()]).worktree_groups[0].rows[0].clone());
    correction.focused_pane = Some(shell.pane_id.clone());
    correction.viewed_panes = vec![shell.pane_id.clone()];
    let correction_frame = crate::sidebar::frame::assemble_frame(
        correction
            .rows()
            .filter_map(|row| row.pane.clone())
            .chain(std::iter::once(own.clone()))
            .collect(),
        crate::utils::time::unix_now_ms(),
        "rimz-test",
    );
    let correction = crate::sidebar::enrich::project_local(
        crate::sidebar::enrich::WorkspaceSnapshot(correction),
        Some(&correction_frame),
        Some(&own.pane_id),
    );
    rig.fold(correction, SnapshotSource::Produced);
    let records = focus_records(&rig);
    assert_eq!(
        records.len(),
        2,
        "the correction records its paint decision"
    );
    assert_eq!(records[1]["source"], "produced");
    assert_eq!(records[1]["phase"], "final");
    assert_eq!(records[1]["seed"], false);
    assert_eq!(records[1]["own_view"], true);
    assert_eq!(
        records[1]["baseline"],
        serde_json::to_value(&shell.pane_id).unwrap()
    );
    assert_eq!(
        records[1]["selected_after"],
        serde_json::to_value(&shell.pane_id).unwrap()
    );
    assert!(records[1].get("selected_before").is_none());
    assert_eq!(rig.view().ui().selected_pane, Some(shell.pane_id));
    assert_eq!(rig.view().ui().selected_index, Some(0));
    rig.state.pane.next_frame = Instant::now();
    rig.paint(true);
    parser.process(&output.0.lock().unwrap());
    let corrected = parser.screen().contents();
    assert!(
        corrected.contains('▎') && corrected.contains('🮇'),
        "correction draws the lane:\n{corrected}"
    );
    assert!(
        corrected.contains('▌') && corrected.contains('▐'),
        "correction seats the shell:\n{corrected}"
    );
}

#[test]
fn new_tab_shell_row_seats_selection_on_the_first_produced_fold() {
    let own = pane("terminal_10", "tab_1", false);
    let shell = pane("terminal_11", "tab_1", true);
    let mut rig = Rig::with_own_pane(own.pane_id.clone());
    enable_focus_trace(&mut rig);
    let mut snapshot = snapshot_with_panes(&rig.ws, vec![shell.clone()]);
    snapshot.focused_pane = Some(shell.pane_id.clone());
    // The new tab is on screen.
    snapshot.viewed_panes = vec![shell.pane_id.clone()];
    snapshot.own_view = Some(crate::store::snapshot::SidebarOwnView {
        sibling_count: 1,
        working_pane_ids: vec![shell.pane_id.clone()],
        own_view_is_daemon: false,
    });

    rig.fold(snapshot, SnapshotSource::Produced);

    let records = focus_records(&rig);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["source"], "produced");
    assert_eq!(records[0]["panes_produced_at_ms"], 1);
    assert_eq!(
        records[0]["baseline"],
        serde_json::to_value(&shell.pane_id).unwrap()
    );
    assert_eq!(
        records[0]["selected_after"],
        serde_json::to_value(&shell.pane_id).unwrap()
    );
}

#[test]
fn new_shell_row_moves_past_cap_selection_without_a_rowless_fold() {
    let mut rig = Rig::new();
    enable_focus_trace(&mut rig);
    let mut panes = (1..=7)
        .map(|index| pane(&format!("terminal_{index}"), "tab_0", false))
        .collect::<Vec<_>>();
    let prior_focus = panes.last().unwrap().pane_id.clone();
    let mut prior = snapshot_with_panes(&rig.ws, panes.clone());
    prior.focused_pane = Some(prior_focus.clone());
    rig.fold(prior, SnapshotSource::Produced);
    assert_eq!(rig.view().ui().selected_pane, Some(prior_focus.clone()));

    let shell = pane("terminal_11", "tab_1", true);
    panes.push(shell.clone());
    let mut snapshot = snapshot_with_panes(&rig.ws, panes);
    snapshot.panes_produced_at_ms = Some(2);
    snapshot.focused_pane = Some(shell.pane_id.clone());
    rig.fold(snapshot, SnapshotSource::Produced);

    let records = focus_records(&rig);
    assert_eq!(records.len(), 2);
    assert_eq!(records[1]["panes_produced_at_ms"], 2);
    assert_eq!(
        records[1]["selected_before"],
        serde_json::to_value(&prior_focus).unwrap()
    );
    assert_eq!(
        records[1]["baseline"],
        serde_json::to_value(&shell.pane_id).unwrap()
    );
    assert_eq!(
        records[1]["selected_after"],
        serde_json::to_value(&shell.pane_id).unwrap()
    );
    assert!(
        records
            .iter()
            .all(|record| record.get("selected_after").is_some())
    );
}

#[test]
fn fused_focus_trace_names_the_event_applied_at_the_paint_decision() {
    let mut rig = Rig::new();
    enable_focus_trace(&mut rig);
    let shell = pane("terminal_11", "tab_1", true);
    let mut pulled = snapshot_with_panes(&rig.ws, vec![shell.clone()]);
    let now_ms = crate::utils::time::unix_now_ms();
    pulled.panes_produced_at_ms = Some(now_ms - 2);
    pulled.panes_observed_at_ms = Some(now_ms - 2);
    rig.fold(pulled, SnapshotSource::Cached);
    rig.event(SidebarEvent::FocusChanged {
        focused: vec![shell.pane_id.clone()],
        unfocused: Vec::new(),
    });
    let records = focus_records(&rig);
    assert_eq!(
        records.len(),
        2,
        "a synthetic focus fold records its decision"
    );
    assert_eq!(
        records[1]["snapshot_focused_pane"],
        serde_json::to_value(&shell.pane_id).unwrap()
    );
    assert_eq!(
        records[1]["baseline"],
        serde_json::to_value(&shell.pane_id).unwrap()
    );
    assert_eq!(
        records[1]["selected_after"],
        serde_json::to_value(&shell.pane_id).unwrap()
    );
    assert!(records[1]["fused_event_sent_at_ms"].as_u64().unwrap() >= now_ms);
}

#[test]
fn seed_commits_before_delivery_and_keeps_the_final_correction() {
    let own = pane("terminal_10", "tab_0", false);
    let mut rig = Rig::with_own_pane(own.pane_id.clone());
    rig.state.pane.config.refresh_ms_override = Some(37);
    rig.runtime.ensure_dirs().unwrap();
    let mut seed = agent_snapshot(&rig.ws);
    seed.now = Timestamp::now();
    seed.theme.display.refresh_ms = 100;
    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut seed.worktree_groups)[0].rows)[0]
        .name = "seed-card".into();
    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut seed.worktree_groups)[0].rows)[0]
        .unread = true;
    seed.pane_session_name = Some("rimz-test".into());
    seed.panes_observed_at_ms = Some(42);
    let agent_pane = seed.worktree_groups[0].rows[0]
        .pane
        .as_ref()
        .unwrap()
        .pane_id
        .clone();
    seed.focused_pane = Some(agent_pane.clone());
    seed.viewed_panes = vec![agent_pane.clone()];
    let mut frame = crate::sidebar::frame::assemble_frame(
        seed.rows().filter_map(|row| row.pane.clone()).collect(),
        seed.now.as_millisecond() as u64,
        "rimz-test",
    );
    frame.presence = Some(crate::store::snapshot::PresenceSample {
        human_clients: 1,
        last_input_ms: Some(seed.now.as_millisecond() as u64),
        client_input_ms: Default::default(),
        client_views: Vec::new(),
        sampled_at_ms: seed.now.as_millisecond() as u64,
    });
    let fold = WorkspaceFold {
        sampled_at: Instant::now(),
        workspace: crate::sidebar::enrich::WorkspaceSnapshot(seed),
        frame: Some(Arc::new(frame)),
    };

    rig.request_if_idle(FetchRequest::default());
    assert!(rig.next_request().is_some());
    rig.request_if_idle(FetchRequest::fresh_panes());
    assert!(rig.next_request().is_none());
    rig.state.pane.dirty = false;
    rig.state.seed(&fold);

    assert_eq!(
        rig.view().ctx().current.rows().count(),
        1,
        "the seed is projected for this pane before any delivery"
    );
    assert_eq!(
        rig.view().ctx().current.rows().next().unwrap().name,
        "seed-card"
    );
    assert_eq!(
        rig.view().ctx().current.focused_pane,
        Some(agent_pane.clone())
    );
    assert!(
        rig.view().ctx().current.rows().next().unwrap().unread,
        "the projection's stale focus must not read the seeded card"
    );
    assert_eq!(
        crate::sidebar::read_marks::ReadMarks::load(&rig.runtime).cleared_at_ms("agent-1"),
        None
    );
    assert_eq!(rig.view().ctx().current.theme.display.refresh_ms, 37);
    assert!(rig.state.pane.dirty, "the seed is paint-pending");
    assert_eq!(
        rig.view().ctx().current.presence,
        Some(crate::store::snapshot::SidebarPresence::Active)
    );
    assert!(rig.view().ctx().current.own_view.is_none());
    assert!(!rig.state.pane.self_close.seen_sibling);
    assert!(!rig.state.pane.should_exit);
    assert_eq!(
        rig.view().ctx().last_focus_observation.panes_observed_at_ms,
        Some(42)
    );
    assert_eq!(
        rig.view()
            .ctx()
            .last_focus_observation
            .pane_session_name
            .as_deref(),
        Some("rimz-test")
    );
    assert_eq!(
        rig.view().ctx().last_focus_observation.pane_ids,
        vec![agent_pane]
    );
    assert!(rig.view().ctx().last_focus_observation.presence_known);
    assert!(
        rig.next_request().is_none(),
        "a seed does not clear the latch"
    );

    let mut correction = agent_snapshot(&rig.ws);
    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut correction.worktree_groups)[0].rows,
    )[0]
    .name = "corrected-card".into();
    let corrected_pane = pane("terminal_11", "tab_0", false);
    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut correction.worktree_groups)[0].rows,
    )[0]
    .pane = Some(corrected_pane.clone());
    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut correction.worktree_groups)[0].rows,
    )[0]
    .unread = true;
    correction.focused_pane = Some(corrected_pane.pane_id.clone());
    correction.viewed_panes = vec![corrected_pane.pane_id.clone()];
    let correction_frame = crate::sidebar::frame::assemble_frame(
        vec![own.clone(), corrected_pane.clone()],
        crate::utils::time::unix_now_ms(),
        "rimz-test",
    );
    let correction = crate::sidebar::enrich::project_local(
        crate::sidebar::enrich::WorkspaceSnapshot(correction),
        Some(&correction_frame),
        Some(&own.pane_id),
    );
    rig.fold(correction, SnapshotSource::Produced);

    assert_eq!(
        rig.view().ctx().current.rows().next().unwrap().name,
        "corrected-card"
    );
    assert_eq!(rig.view().ui().selected_pane, Some(corrected_pane.pane_id));
    assert!(!rig.view().ctx().current.rows().next().unwrap().unread);
    assert!(
        crate::sidebar::read_marks::ReadMarks::load(&rig.runtime)
            .cleared_at_ms("agent-1")
            .is_some()
    );
    assert_eq!(
        rig.view().ctx().gate.reject_streak,
        0,
        "the changed pane set commits"
    );
    rig.request_if_idle(FetchRequest::fresh_panes());
    assert!(
        rig.next_request().unwrap().is_fresh_panes(),
        "only the final correction clears the latch"
    );
}

#[test]
fn seed_listing_the_own_pane_seats_its_focus_at_once() {
    let own = pane("terminal_10", "tab_0", false);
    let mut rig = Rig::with_own_pane(own.pane_id.clone());
    rig.runtime.ensure_dirs().unwrap();

    let mut seed = agent_snapshot(&rig.ws);
    seed.now = Timestamp::now();
    seed.pane_session_name = Some("rimz-test".into());
    let agent = seed.worktree_groups[0].rows[0].pane.clone().unwrap();
    seed.focused_pane = Some(agent.pane_id.clone());
    seed.viewed_panes = vec![agent.pane_id.clone()];
    let frame = crate::sidebar::frame::assemble_frame(
        vec![own, agent.clone()],
        seed.now.as_millisecond() as u64,
        "rimz-test",
    );
    rig.state.seed(&WorkspaceFold {
        sampled_at: Instant::now(),
        workspace: crate::sidebar::enrich::WorkspaceSnapshot(seed),
        frame: Some(Arc::new(frame)),
    });

    assert!(rig.view().ctx().current.own_view.is_some());
    assert_eq!(rig.view().ui().selected_pane, Some(agent.pane_id));
    assert_eq!(rig.view().ui().selected_index, Some(0));
    assert!(rig.state.pane.self_close.seen_sibling);
}

#[test]
fn snapshot_key_rebind_reaches_the_host_resolver() {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    let mut rig = Rig::new();
    let snapshot = snapshot_with_panes(
        &rig.ws,
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", false),
        ],
    );
    rig.fold(snapshot.clone(), SnapshotSource::Produced);
    let press = Wakeup::Press {
        code: KeyCode::Char('v'),
        mods: KeyModifiers::NONE,
    };
    rig.state
        .on_input(press.clone(), &mut rig.terminal)
        .unwrap();
    assert_eq!(rig.view().ui().selected_index, None);

    let mut rebound = snapshot;
    rebound.sidebar.keys.down = "v".to_owned();
    rig.fold(rebound, SnapshotSource::Produced);
    rig.state.on_input(press, &mut rig.terminal).unwrap();
    assert_eq!(rig.view().ui().selected_index, Some(0));
    assert_eq!(
        rig.view().ui().selected_pane,
        Some(pane("terminal_1", "tab_0", false).pane_id)
    );
    rig.input(KeyAction::Top);
    rig.state
        .on_input(
            Wakeup::Press {
                code: KeyCode::Char('j'),
                mods: KeyModifiers::NONE,
            },
            &mut rig.terminal,
        )
        .unwrap();
    assert_eq!(
        rig.view().ui().selected_index,
        Some(0),
        "the old binding is gone"
    );
}

#[test]
fn failure_after_an_unread_snapshot_keeps_state_context_and_completion() {
    let mut rig = Rig::new();
    let mut snapshot = agent_snapshot(&rig.ws);
    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0]
    .name = "unread-snapshot".into();
    rig.request_if_idle(FetchRequest::default());
    assert!(rig.next_request().is_some());
    rig.request_if_idle(FetchRequest::fresh_panes());
    assert!(rig.next_request().is_none());
    let filter = BodyLens::from(BodyFilter::Status(crate::agents::AgentStatus::Idle));
    rig.send(FetchUpdate::Shared {
        update: Box::new(FetchUpdate::Snapshot {
            snapshot: Box::new(snapshot),
            phase: FetchPhase::Final,
            source: SnapshotSource::Produced,
        }),
        context: Arc::new(super::super::super::fetch::FoldShared {
            inputs: Arc::new(super::super::super::fetch::FoldInputs {
                filter: filter.clone(),
                ..Default::default()
            }),
            ..Default::default()
        }),
    });
    rig.send(FetchUpdate::Failed {
        error: "fold failed".into(),
    });
    rig.state.on_snapshot();
    assert_eq!(
        rig.view()
            .ctx()
            .current
            .rows()
            .next()
            .map(|row| row.name.as_str()),
        Some("unread-snapshot"),
        "a terminal outcome must not erase the unread snapshot"
    );
    assert_eq!(
        rig.view().ui().make_up_filter,
        filter,
        "snapshot context is applied with its snapshot"
    );
    assert_eq!(rig.view().ctx().health.failure_streak, 1);
    rig.request_if_idle(FetchRequest::fresh_panes());
    assert!(
        rig.next_request()
            .expect("final outcome clears the latch")
            .is_fresh_panes()
    );
    assert!(rig.next_request().is_none());
}

#[test]
fn a_final_snapshot_never_yields_to_an_interim_or_loses_completion() {
    let mut rig = Rig::new();
    rig.request_if_idle(FetchRequest::default());
    assert!(rig.next_request().is_some());
    rig.request_if_idle(FetchRequest::fresh_panes());
    assert!(rig.next_request().is_none());
    for (name, phase) in [
        ("final-snapshot", FetchPhase::Final),
        ("interim-snapshot", FetchPhase::Interim),
    ] {
        let mut snapshot = agent_snapshot(&rig.ws);
        std::sync::Arc::make_mut(
            &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
        )[0]
        .name = name.into();
        rig.send(FetchUpdate::Snapshot {
            snapshot: Box::new(snapshot),
            phase,
            source: SnapshotSource::Cached,
        });
    }
    rig.state.on_snapshot();
    assert_eq!(
        rig.view().ctx().current.rows().next().unwrap().name,
        "final-snapshot",
        "an interim must not displace the pending final snapshot"
    );
    rig.request_if_idle(FetchRequest::fresh_panes());
    assert!(
        rig.next_request()
            .expect("completion survives an interim publication")
            .is_fresh_panes()
    );
}

#[test]
fn a_failed_completion_survives_a_later_interim_snapshot() {
    let mut rig = Rig::new();
    rig.request_if_idle(FetchRequest::default());
    assert!(rig.next_request().is_some());
    rig.request_if_idle(FetchRequest::fresh_panes());
    assert!(rig.next_request().is_none());
    rig.send(FetchUpdate::Failed {
        error: "fold failed".into(),
    });
    rig.send(FetchUpdate::Snapshot {
        snapshot: Box::new(agent_snapshot(&rig.ws)),
        phase: FetchPhase::Interim,
        source: SnapshotSource::Cached,
    });
    rig.state.on_snapshot();
    assert_eq!(
        rig.view().ctx().health.failure_streak,
        1,
        "an interim must not erase a pending failure"
    );
    assert_eq!(rig.view().ctx().current.rows().count(), 1);
    rig.request_if_idle(FetchRequest::fresh_panes());
    assert!(
        rig.next_request()
            .expect("failed final clears the latch")
            .is_fresh_panes()
    );
}

#[test]
fn disabled_observer_extracts_no_signature() {
    let mut rig = Rig::new();
    observe::take_extractions();
    rig.fold(agent_snapshot(&rig.ws), SnapshotSource::Cached);
    assert_eq!(
        observe::take_extractions(),
        (0, 0),
        "no writer means no extraction"
    );
}

#[test]
fn only_watched_metrics_and_presence_publications_fold_immediately() {
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;

    for watched in [true, false] {
        let mut rig = Rig::with_own_pane(own_pane.clone());
        rig.hide();
        if watched {
            rig.watch();
        }

        let mut folded = process_snapshot(&rig.ws, crate::utils::time::unix_now_ms());
        folded.own_view = rig.view().ctx().current.own_view.clone();
        folded.viewed_panes = rig.view().ctx().current.viewed_panes.clone();
        rig.view().ctx().current = folded.clone();
        rig.set_pulled(&folded);
        let crate::store::snapshot::RowCard::Process(card) = &mut std::sync::Arc::make_mut(
            &mut std::sync::Arc::make_mut(&mut folded.worktree_groups)[0].rows,
        )[0]
        .card
        else {
            panic!("process fixture");
        };
        card.cpu_pct = Some(73);
        folded.presence = Some(crate::store::snapshot::SidebarPresence::Detached);
        rig.fold(folded, SnapshotSource::Produced);

        assert_eq!(
            matches!(
                &rig.state.views.mirror().worktree_groups[0].rows[0].card,
                crate::store::snapshot::RowCard::Process(card) if card.cpu_pct == Some(73)
            ),
            watched
        );
        assert_eq!(
            rig.view().ctx().current.presence,
            watched.then_some(crate::store::snapshot::SidebarPresence::Detached)
        );
    }
}

#[test]
fn room_events_send_no_pane_request() {
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;

    for watched in [true, false] {
        let mut rig = Rig::with_own_pane(own_pane.clone());
        rig.hide();
        if watched {
            rig.watch();
        }

        rig.event(store_delta());

        assert!(rig.next_request().is_none());
    }
}

#[test]
fn maintenance_watchdog_requests_one_unwatched_fetch() {
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let mut rig = Rig::with_own_pane(own_pane);
    rig.hide();

    rig.state.pane.last_self_close_check = Instant::now() - Duration::from_secs(7);
    rig.maintenance();

    assert!(
        rig.next_request().is_some(),
        "watchdog dispatches one fetch"
    );
    assert!(
        rig.next_request().is_none(),
        "the watchdog requests only one fetch"
    );
}

#[test]
fn own_reveal_requests_fresh_panes() {
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let mut rig = Rig::with_own_pane(own_pane.clone());
    rig.hide();

    rig.event(SidebarEvent::FocusChanged {
        focused: vec![own_pane],
        unfocused: Vec::new(),
    });

    assert!(
        rig.next_request()
            .expect("own reveal requests a fetch")
            .is_fresh_panes()
    );
}

#[test]
fn width_target_event_reloads_the_target_without_a_producer_fetch() {
    let mut rig = Rig::new();
    crate::mux::width_target::pin(
        &rig.runtime,
        crate::mux::SidebarWidth::default(),
        std::num::NonZeroU16::new(90).expect("nonzero width"),
        200,
    )
    .expect("pin width target");

    rig.event(SidebarEvent::WidthTargetChanged);

    assert_eq!(
        rig.state.pane.width_control.max_legit_cols(),
        72,
        "the share stays unresolved until backend view geometry arrives",
    );
    assert!(
        rig.next_request().is_none(),
        "width propagation stays out of the producer path",
    );
}

#[test]
fn birth_seeds_the_shared_body_filter() {
    let mut rig = Rig::with_filter(BodyLens::from(BodyFilter::Unread));

    assert_eq!(
        rig.view().ui().make_up_filter,
        BodyLens::from(BodyFilter::Unread)
    );
}

#[test]
fn body_filter_event_adopts_the_shared_file_and_repaints() {
    let mut rig = Rig::new();
    rig.view().ctx().current = agent_snapshot(&rig.ws);
    rig.state.pane.dirty = false;
    let filter = BodyLens {
        filter: Some(BodyFilter::Status(crate::agents::AgentStatus::Idle)),
        query: Some("auth".to_owned()),
    };
    crate::sidebar::body_filter::write(&rig.runtime, &filter).expect("write shared filter");

    rig.event(SidebarEvent::BodyFilterChanged);

    assert_eq!(rig.view().ui().make_up_filter, filter);
    assert!(rig.state.pane.dirty);
    assert!(
        rig.next_request().is_none(),
        "filter propagation stays out of the producer path"
    );
}

#[test]
fn search_commit_writes_and_dispatches_the_lens() {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    let mut rig = Rig::new();
    rig.view().ctx().current = agent_snapshot(&rig.ws);
    let plane = super::super::super::plane::DataPlane::detached();
    let mut peer = rig.state.pane.config.clone();
    peer.instance_id = crate::SidebarInstanceId::new();
    let (wake, inbox) = std::sync::mpsc::sync_channel(64);
    let _peer = plane.subscribe(&peer, wake);
    rig.state.room_events = Some(plane.room_events(&rig.runtime));
    for code in [KeyCode::Char('/'), KeyCode::Char('a'), KeyCode::Enter] {
        rig.state
            .on_input(
                Wakeup::Press {
                    code,
                    mods: KeyModifiers::NONE,
                },
                &mut rig.terminal,
            )
            .unwrap();
    }
    assert_eq!(
        crate::sidebar::body_filter::load(&rig.runtime)
            .query
            .as_deref(),
        Some("a")
    );
    assert!(matches!(inbox.try_recv(), Ok(Wakeup::Event(envelope))
        if envelope.event == SidebarEvent::BodyFilterChanged));
}

#[test]
fn search_draft_survives_peer_adoption_and_focus_loss_keeps_commit() {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    let own = pane("terminal_1", "tab_0", false).pane_id;
    let mut rig = Rig::with_own_pane(own.clone());
    rig.view().ctx().current = agent_snapshot(&rig.ws);
    rig.state
        .on_input(
            Wakeup::Press {
                code: KeyCode::Char('/'),
                mods: KeyModifiers::NONE,
            },
            &mut rig.terminal,
        )
        .unwrap();
    assert_eq!(rig.view().ui().search_draft.as_deref(), Some(""));
    rig.view().ui().search_draft = Some("draft".to_owned());
    let committed = BodyLens {
        query: Some("cla".to_owned()),
        ..Default::default()
    };
    crate::sidebar::body_filter::write(&rig.runtime, &committed).unwrap();
    rig.event(SidebarEvent::BodyFilterChanged);
    assert_eq!(rig.view().ui().search_draft.as_deref(), Some("draft"));
    let snapshot = agent_snapshot(&rig.ws);
    rig.fold(snapshot, SnapshotSource::Produced);
    assert_eq!(rig.view().ui().search_draft.as_deref(), Some("draft"));
    rig.event(SidebarEvent::FocusChanged {
        focused: Vec::new(),
        unfocused: vec![own],
    });
    assert_eq!(rig.view().ui().search_draft, None);
    assert_eq!(rig.view().ui().make_up_filter, committed);
    assert_eq!(crate::sidebar::body_filter::load(&rig.runtime), committed);
}

#[test]
fn active_alert_discards_search_draft_and_preserves_committed_lens() {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    let committed = BodyLens {
        filter: Some(BodyFilter::Status(crate::agents::AgentStatus::Idle)),
        query: Some("cla".to_owned()),
    };
    let mut rig = Rig::with_filter(committed.clone());
    let committed_pane = Some(pane("terminal_9", "tab_0", false).pane_id);
    let mut snapshot = agent_snapshot(&rig.ws);
    snapshot.focused_pane = committed_pane.clone();
    rig.fold(snapshot, SnapshotSource::Produced);
    assert_eq!(rig.view().ui().selected_pane, committed_pane);
    for ch in ['/', '!'] {
        rig.state
            .on_input(
                Wakeup::Press {
                    code: KeyCode::Char(ch),
                    mods: KeyModifiers::NONE,
                },
                &mut rig.terminal,
            )
            .unwrap();
    }
    assert_eq!(rig.view().ui().search_draft.as_deref(), Some("cla!"));
    assert_eq!(rig.view().ui().selected_pane, None, "draft has no matches");
    rig.deliver(FetchUpdate::Failed {
        error: "snapshot failed".to_owned(),
    });
    assert!(!rig.state.alert_active());
    assert_eq!(rig.view().ui().search_draft.as_deref(), Some("cla!"));
    rig.state.pane.dirty = false;
    rig.deliver(FetchUpdate::Failed {
        error: "snapshot failed".to_owned(),
    });
    assert_eq!(rig.view().ui().search_draft, None);
    assert_eq!(rig.view().ui().make_up_filter, committed);
    assert_eq!(crate::sidebar::body_filter::load(&rig.runtime), committed);
    assert!(rig.state.alert_active());
    assert!(rig.state.pane.dirty);
    assert_eq!(rig.view().ui().selected_pane, committed_pane);
    rig.state
        .on_input(
            Wakeup::Press {
                code: KeyCode::Char('/'),
                mods: KeyModifiers::NONE,
            },
            &mut rig.terminal,
        )
        .unwrap();
    assert_eq!(rig.view().ui().search_draft, None);
}

#[test]
fn sustained_failures_keep_the_frame_and_alert_until_completed_production() {
    let clock = LoopClock::play(Timestamp::from_second(1_700_000_000).unwrap());
    let mut rig = Rig::with_clock(None, BodyLens::default(), None, clock.clone());
    let output = FrameOutput::default();
    rig.terminal = Terminal::with_options(
        PaneBackend::headless(output.clone()),
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 80, 24)),
        },
    )
    .unwrap();
    let snapshot = agent_snapshot(&rig.ws);
    let committed_names: Vec<_> = snapshot.rows().map(|row| row.name.clone()).collect();
    rig.fold(snapshot.clone(), SnapshotSource::Produced);
    for _ in 0..super::super::super::health::ALERT_AFTER_FAILURES {
        rig.deliver(FetchUpdate::Failed {
            error: "store not found".to_owned(),
        });
    }
    assert!(rig.state.alert_active());
    clock.advance(Duration::from_secs(31));
    rig.deliver(FetchUpdate::Failed {
        error: "store not found".to_owned(),
    });
    rig.state.fold_fused_now();
    assert!(
        !rig.state.pane.should_exit,
        "degradation must not detach the pane"
    );
    assert_eq!(
        rig.view()
            .ctx()
            .current
            .rows()
            .map(|row| row.name.clone())
            .collect::<Vec<_>>(),
        committed_names
    );
    rig.paint(true);
    let mut screen = vt100::Parser::new(24.try_into().unwrap(), 80.try_into().unwrap(), 0);
    screen.process(&output.0.lock().unwrap());
    let screen = screen.screen().contents();
    assert!(screen.contains("Sidebar degraded"), "{screen}");
    assert!(screen.contains(&committed_names[0]), "{screen}");

    rig.fold(snapshot.clone(), SnapshotSource::Cached);
    assert!(
        rig.state.alert_active(),
        "cached content cannot recover health"
    );
    rig.fold(snapshot, SnapshotSource::Produced);
    assert!(!rig.state.pane.should_exit);
    assert!(!rig.state.alert_active());
    assert!(
        rig.view()
            .ctx()
            .health
            .alert
            .as_ref()
            .unwrap()
            .recovered_at
            .is_some()
    );
}

#[test]
fn older_shared_inputs_do_not_undo_a_consumed_body_filter() {
    let mut rig = Rig::new();
    let snapshot = agent_snapshot(&rig.ws);
    rig.fold(snapshot.clone(), SnapshotSource::Produced);
    let inputs = Arc::new(super::super::super::fetch::FoldInputs::default());
    let filter = BodyLens::from(BodyFilter::Status(crate::agents::AgentStatus::Idle));
    crate::sidebar::body_filter::write(&rig.runtime, &filter).unwrap();
    rig.event(SidebarEvent::BodyFilterChanged);
    assert_eq!(rig.view().ui().make_up_filter, filter);

    rig.deliver(FetchUpdate::Shared {
        update: Box::new(FetchUpdate::Snapshot {
            snapshot: Box::new(snapshot),
            phase: FetchPhase::Final,
            source: SnapshotSource::Produced,
        }),
        context: Arc::new(super::super::super::fetch::FoldShared {
            inputs,
            ..Default::default()
        }),
    });
    assert_eq!(
        rig.view().ui().make_up_filter,
        filter,
        "an older shared cut must not undo the already consumed filter event"
    );
    assert_eq!(crate::sidebar::body_filter::load(&rig.runtime), filter);
}

#[test]
fn successful_fetch_converges_a_missed_body_filter_event() {
    let mut rig = Rig::new();
    let filter = BodyLens {
        filter: Some(BodyFilter::Status(crate::agents::AgentStatus::Idle)),
        query: Some("auth".to_owned()),
    };
    crate::sidebar::body_filter::write(&rig.runtime, &filter).expect("write shared filter");

    let snapshot = agent_snapshot(&rig.ws);
    rig.fold(snapshot, SnapshotSource::Produced);

    assert_eq!(rig.view().ui().make_up_filter, filter);
}

#[test]
fn shared_fold_adopts_query_in_the_visible_roster() {
    let mut rig = Rig::new();
    let snapshot = agent_snapshot(&rig.ws);
    let lens = BodyLens {
        query: Some("no such row".to_owned()),
        ..Default::default()
    };
    rig.shared_fold(
        snapshot,
        Arc::new(super::super::super::fetch::FoldInputs {
            filter: lens.clone(),
            ..Default::default()
        }),
    );
    {
        let mut guard = rig.view();
        let (ui, context) = guard.pair();
        assert_eq!(ui.visible_roster(&context.current).len(), 0);
    }
    assert_eq!(rig.view().ui().make_up_filter, lens);
}

#[test]
fn newer_shared_cut_converges_a_missed_filter_clear() {
    let mut rig = Rig::new();
    let snapshot = agent_snapshot(&rig.ws);
    rig.fold(snapshot.clone(), SnapshotSource::Produced);
    let filter = BodyLens::from(BodyFilter::Status(crate::agents::AgentStatus::Idle));
    crate::sidebar::body_filter::write(&rig.runtime, &filter).unwrap();
    rig.event(SidebarEvent::BodyFilterChanged);
    crate::sidebar::body_filter::write(&rig.runtime, &BodyLens::default()).unwrap();
    rig.shared_fold(snapshot, Arc::default());
    assert_eq!(rig.view().ui().make_up_filter, BodyLens::default());
}

#[test]
fn failed_or_rowless_birth_fold_does_not_publish_a_filter_clear() {
    let filter = BodyLens::from(BodyFilter::Status(crate::agents::AgentStatus::Waiting));
    let mut rig = Rig::with_filter(filter.clone());

    rig.deliver(FetchUpdate::Failed {
        error: "not ready".to_owned(),
    });
    assert_eq!(rig.view().ui().make_up_filter, BodyLens::default());
    assert_eq!(crate::sidebar::body_filter::load(&rig.runtime), filter);

    let rowless = snapshot(&rig.ws);
    rig.fold(rowless, SnapshotSource::Produced);
    assert_eq!(rig.view().ui().make_up_filter, BodyLens::default());
    assert_eq!(crate::sidebar::body_filter::load(&rig.runtime), filter);
}

#[test]
fn empty_body_filter_auto_clear_updates_the_shared_file() {
    let filter = BodyLens {
        filter: Some(BodyFilter::Status(crate::agents::AgentStatus::Waiting)),
        query: Some("auth".to_owned()),
    };
    let mut rig = Rig::with_filter(filter.clone());
    assert_eq!(crate::sidebar::body_filter::load(&rig.runtime), filter);

    let snapshot = agent_snapshot(&rig.ws);
    rig.fold(snapshot, SnapshotSource::Produced);

    let expected = BodyLens {
        query: Some("auth".to_owned()),
        ..Default::default()
    };
    assert_eq!(rig.view().ui().make_up_filter, expected);
    assert_eq!(crate::sidebar::body_filter::load(&rig.runtime), expected);
}

#[test]
fn focus_out_closes_help_popup() {
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let mut rig = Rig::with_own_pane(own_pane.clone());
    let snapshot = snapshot_with_focused_pane(&rig.ws, own_pane.clone());
    rig.set_pulled(&snapshot);
    rig.view().ctx().current = snapshot;
    rig.view().ui().help_visible = true;
    rig.state.pane.optimistic_watch_until = Some(Instant::now() + Duration::from_secs(1));

    rig.event(SidebarEvent::FocusChanged {
        focused: Vec::new(),
        unfocused: vec![own_pane],
    });

    assert!(!rig.view().ui().help_visible);
    assert!(rig.state.pane.optimistic_watch_until.is_none());
}

fn inactive_pair(mux: MuxName) -> super::scroll::SharedPanes {
    let mut pair = super::scroll::SharedPanes::new(mux);
    let now = pair.snapshot.now;
    let groups = Arc::make_mut(&mut pair.snapshot.worktree_groups);
    Arc::make_mut(&mut groups[0].rows).truncate(1);
    let template = groups[0].clone();
    for index in 1..=25 {
        let mut group = template.clone();
        group.key = format!("old-{index:02}");
        group.label = group.key.clone();
        group.root_lane = false;
        group.kind = crate::store::snapshot::SidebarWorktreeKind::Worktree;
        group.clean = Some(true);
        group.pr_state = Some(crate::store::snapshot::WorktreePrState::Merged);
        group.cohort_effort = Some(Default::default());
        let row = &mut Arc::make_mut(&mut group.rows)[0];
        row.id = group.key.clone();
        row.name = group.key.clone();
        row.last_activity = now - Duration::from_secs(7200 + index * 60);
        row.pane.as_mut().unwrap().pane_id = PaneId::from_parts(
            mux,
            match mux {
                MuxName::Tmux => format!("%{}", 200 + index),
                MuxName::Zellij => format!("terminal_old_{index}"),
            },
        );
        let engaged_at = row.last_activity;
        let card = row.as_agent_mut().unwrap();
        card.status = crate::agents::AgentStatus::Success;
        card.engaged_at = Some(engaged_at);
        groups.push(group);
    }
    pair.sample(&[(1, 0)]);
    pair.panes[0].state.on_snapshot();
    pair.paint(0);
    assert_eq!(
        pair.panes[0]
            .state
            .views
            .mirror()
            .worktree_groups
            .iter()
            .filter(|group| group.inactive.is_some())
            .count(),
        25
    );
    pair
}

fn inactive_clients(pair: &mut super::scroll::SharedPanes, focused: &[PaneId]) {
    let clients = (1..=focused.len())
        .map(|id| (id as u32, 0))
        .collect::<Vec<_>>();
    pair.sample_with(&clients, |frame| {
        for (client, pane) in frame.client_views.iter_mut().zip(focused) {
            client.pane_id = pane.clone();
        }
    });
    pair.panes[0].state.on_snapshot();
}

fn inactive_input(pair: &mut super::scroll::SharedPanes, wakeup: Wakeup) -> InputOutcome {
    let rig = &mut pair.panes[0];
    rig.state.prepare_action();
    rig.state.apply_input([wakeup], &mut rig.terminal).unwrap()
}

fn inactive_frame(pair: &mut super::scroll::SharedPanes, index: usize) -> String {
    let output = FrameOutput::default();
    pair.panes[index].terminal = Terminal::with_options(
        PaneBackend::headless(output.clone()),
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 80, 40)),
        },
    )
    .unwrap();
    pair.paint(index);
    let mut parser = vt100::Parser::new(40.try_into().unwrap(), 80.try_into().unwrap(), 0);
    parser.process(&output.0.lock().unwrap());
    parser.screen().contents()
}

fn assert_inactive_frame_closed(pair: &mut super::scroll::SharedPanes, index: usize) {
    let text = inactive_frame(pair, index);
    assert!(text.contains("[ ▸ 25 inactive ]"), "{text}");
    assert!(!text.contains('▾'), "{text}");
    assert!(pair.view(index).inactive_peek.is_none());
}

fn assert_inactive_frame_open(pair: &mut super::scroll::SharedPanes, index: usize) {
    let text = inactive_frame(pair, index);
    assert!(text.contains("[ ▾ 25 inactive ]"), "{text}");
    assert!(text.contains("old-"), "{text}");
    assert!(pair.view(index).inactive_peek.is_some());
}

#[test]
fn inactive_peek_stays_open_from_the_first_frame_on_a_tab_round_trip() {
    for mux in [MuxName::Tmux, MuxName::Zellij] {
        let mut pair = inactive_pair(mux);
        inactive_input(&mut pair, Wakeup::Key(KeyAction::ToggleInactive));
        assert!(inactive_frame(&mut pair, 0).contains("[ ▾ 25 inactive ]"));
        pair.panes[0].state.with_view_for_test(Route::READ, |_, v| {
            v.state.inactive_peek.as_mut().unwrap().pages = 2.try_into().unwrap();
            v.state.expanded_groups.insert("old-25".into());
        });

        pair.sample(&[(1, 1)]);
        assert_inactive_frame_open(&mut pair, 1);
        assert_eq!(pair.view(1).inactive_peek.unwrap().pages.get(), 2);
        assert!(pair.view(1).expanded_groups.contains("old-25"));
        pair.panes[1].state.on_snapshot();
        assert_inactive_frame_open(&mut pair, 1);
        assert_eq!(pair.view(1).inactive_peek.unwrap().pages.get(), 2);
        assert!(pair.view(1).expanded_groups.contains("old-25"));

        pair.sample(&[(1, 0)]);
        assert_inactive_frame_open(&mut pair, 0);
        assert_eq!(pair.view(0).inactive_peek.unwrap().pages.get(), 2);
        assert!(pair.view(0).expanded_groups.contains("old-25"));
        inactive_input(&mut pair, Wakeup::Key(KeyAction::ToggleInactive));
        assert_inactive_frame_closed(&mut pair, 0);
        assert!(!pair.view(0).expanded_groups.contains("old-25"));
        inactive_input(&mut pair, Wakeup::Key(KeyAction::ToggleInactive));
        assert_eq!(pair.view(0).inactive_peek.unwrap().pages.get(), 1);
    }
}

#[test]
fn inactive_peek_copies_toggle_independently_when_co_viewers_split_tabs() {
    for mux in [MuxName::Tmux, MuxName::Zellij] {
        let mut pair = inactive_pair(mux);
        pair.sample(&[(1, 0), (2, 0)]);
        pair.panes[0].state.on_snapshot();
        inactive_input(&mut pair, Wakeup::Key(KeyAction::ToggleInactive));
        assert!(inactive_frame(&mut pair, 0).contains("[ ▾ 25 inactive ]"));

        pair.sample(&[(1, 0), (2, 1)]);
        assert_inactive_frame_open(&mut pair, 1);
        pair.panes[1].state.on_snapshot();
        assert_inactive_frame_open(&mut pair, 1);
        assert_inactive_frame_open(&mut pair, 0);
        let rig = &mut pair.panes[1];
        rig.state.prepare_action();
        rig.state
            .apply_input([Wakeup::Key(KeyAction::ToggleInactive)], &mut rig.terminal)
            .unwrap();
        assert_inactive_frame_closed(&mut pair, 1);
        assert_inactive_frame_open(&mut pair, 0);

        pair.sample(&[(1, 0), (2, 0)]);
        assert_inactive_frame_open(&mut pair, 0);
    }
}

#[test]
fn inactive_peek_keeps_pages_on_pulled_focus_departures_until_toggled() {
    for mux in [MuxName::Tmux, MuxName::Zellij] {
        let mut pair = inactive_pair(mux);
        let own = pair.panes[0].state.pane.config.own_pane.clone();
        let work = pair.snapshot.worktree_groups[0].rows[0]
            .pane
            .as_ref()
            .unwrap()
            .pane_id
            .clone();
        inactive_clients(&mut pair, std::slice::from_ref(&work));
        inactive_input(&mut pair, Wakeup::Key(KeyAction::ToggleInactive));
        for _ in 0..3 {
            pair.clock.advance(Duration::from_secs(60));
            inactive_clients(&mut pair, std::slice::from_ref(&work));
            assert!(
                pair.view(0).inactive_peek.is_some(),
                "{mux:?}: stays open without sidebar focus"
            );
        }
        for _ in 0..3 {
            pair.clock.advance(Duration::from_secs(60));
            inactive_clients(&mut pair, std::slice::from_ref(&own));
            assert!(pair.view(0).inactive_peek.is_some(), "{mux:?}: no timer");
        }
        pair.panes[0].state.with_view_for_test(Route::READ, |_, v| {
            v.state.scroll_offset = usize::MAX;
            v.state.offset_authoritative = true;
        });
        pair.paint(0);
        pair.clock.advance(crate::sidebar::timing::CLICK_GUARD);
        let offset = pair.view(0).scroll_offset;
        let selection = pair.view(0).selected_pane;
        let (column, row) = pair.panes[0]
            .state
            .pane
            .ui
            .interactions
            .line_for_target(&render::HitTarget::MoreInactive)
            .unwrap();
        let outcome = inactive_input(&mut pair, Wakeup::MouseClick { column, row });
        assert!(outcome.effects.is_empty());
        assert_eq!(pair.view(0).scroll_offset, offset);
        assert_eq!(pair.view(0).selected_pane, selection);
        assert_eq!(pair.view(0).inactive_peek.unwrap().pages.get(), 2);
        inactive_clients(&mut pair, std::slice::from_ref(&own));
        assert!(pair.view(0).inactive_peek.is_some());
        pair.panes[0].state.with_view_for_test(Route::READ, |_, v| {
            v.state.scroll_offset = usize::MAX;
            v.state.offset_authoritative = true;
        });
        pair.paint(0);
        assert!(
            pair.panes[0]
                .state
                .pane
                .ui
                .interactions
                .line_for_target(&render::HitTarget::ToggleInactive)
                .is_none()
        );
        inactive_clients(&mut pair, std::slice::from_ref(&work));
        assert_eq!(pair.view(0).inactive_peek.unwrap().pages.get(), 2);
        inactive_clients(&mut pair, std::slice::from_ref(&own));
        pair.sample(&[(1, 1)]);
        pair.panes[1].state.on_snapshot();
        assert_eq!(pair.view(1).inactive_peek.unwrap().pages.get(), 2);
        pair.sample(&[(1, 0)]);
        pair.panes[0].state.on_snapshot();
        assert_eq!(pair.view(0).inactive_peek.unwrap().pages.get(), 2);
        inactive_input(&mut pair, Wakeup::Key(KeyAction::ToggleInactive));
        assert!(pair.view(0).inactive_peek.is_none());
        pair.paint(0);
        let ui = &pair.panes[0].state.pane.ui;
        assert!(
            ui.interactions
                .line_for_target(&render::HitTarget::ToggleInactive)
                .is_some()
        );
        assert!(pair.view(0).scroll_offset <= ui.drawn_scroll_max.unwrap());
        inactive_input(&mut pair, Wakeup::Key(KeyAction::ToggleInactive));
        assert_eq!(pair.view(0).inactive_peek.unwrap().pages.get(), 1);
    }
}

#[test]
fn inactive_peek_stays_while_another_client_reads_the_same_view() {
    for mux in [MuxName::Tmux, MuxName::Zellij] {
        let mut pair = inactive_pair(mux);
        let own = pair.panes[0].state.pane.config.own_pane.clone();
        let work = pair.snapshot.worktree_groups[0].rows[0]
            .pane
            .as_ref()
            .unwrap()
            .pane_id
            .clone();
        inactive_clients(&mut pair, &[own.clone(), own.clone()]);
        inactive_input(&mut pair, Wakeup::Key(KeyAction::ToggleInactive));
        inactive_clients(&mut pair, &[own.clone(), own.clone()]);
        inactive_clients(&mut pair, &[work.clone(), own]);
        assert!(
            pair.view(0).inactive_peek.is_some(),
            "{mux:?}: a reader remains"
        );
        inactive_clients(&mut pair, &[work.clone(), work]);
        assert!(pair.view(0).inactive_peek.is_some(), "{mux:?}: both left");
    }
}

#[test]
fn inactive_peek_stays_open_in_the_focus_overlay_fold() {
    for mux in [MuxName::Tmux, MuxName::Zellij] {
        let mut pair = inactive_pair(mux);
        let own = pair.panes[0].state.pane.config.own_pane.clone();
        let work = pair.snapshot.worktree_groups[0].rows[0]
            .pane
            .as_ref()
            .unwrap()
            .pane_id
            .clone();
        inactive_input(&mut pair, Wakeup::Key(KeyAction::ToggleInactive));
        inactive_clients(&mut pair, std::slice::from_ref(&own));
        let current = pair.panes[0].state.views.mirror();
        let observed = current
            .panes_observed_at_ms
            .or(current.panes_produced_at_ms)
            .unwrap();
        pair.clock.advance(Duration::from_millis(
            observed.saturating_sub(pair.clock.now_ms()) + 1,
        ));
        let rig = &mut pair.panes[0];
        let envelope = SidebarEventEnvelope::new(
            rig.ws.clone(),
            Some("rimz-test".into()),
            pair.clock.now_ms(),
            SidebarEvent::FocusChanged {
                focused: vec![work],
                unfocused: vec![own],
            },
        );
        rig.record_event(&envelope);
        assert!(
            !rig.state.parked(pair.clock.now_ms()),
            "{mux:?}: fresh overlay leaves the sidebar"
        );
        rig.state.on_event(&mut rig.terminal, envelope);
        assert!(
            pair.view(0).inactive_peek.is_some(),
            "{mux:?}: fused departure keeps the peek open"
        );
    }
}

#[test]
fn inactive_tab_arrival_preserves_focus_placement() {
    let mut frames = Vec::new();
    for mux in [MuxName::Tmux, MuxName::Zellij] {
        for open in [false, true] {
            frames.push((mux, open, inactive_jump_placement(mux, open, 0)));
        }
    }
    assert!(
        frames
            .iter()
            .all(|(_, _, (painted, expected))| *painted == Some(*expected)),
        "(mux, peek open, (painted, expected)): {frames:?}"
    );
}

#[test]
fn inactive_same_tab_jump_preserves_focus_placement() {
    let mut frames = Vec::new();
    for mux in [MuxName::Tmux, MuxName::Zellij] {
        for open in [false, true] {
            frames.push((mux, open, inactive_jump_placement(mux, open, 1)));
        }
    }
    assert!(
        frames
            .iter()
            .all(|(_, _, (painted, expected))| *painted == Some(*expected)),
        "(mux, peek open, (painted, expected)): {frames:?}"
    );
}

fn inactive_jump_placement(mux: MuxName, open: bool, home: usize) -> (Option<isize>, isize) {
    use crate::mux::focus_anchor::{FocusAnchor, FocusIntentState, FocusPlacement};

    let mut pair = inactive_pair(mux);
    let group = &mut Arc::make_mut(&mut pair.snapshot.worktree_groups)[0];
    let template = group.rows[0].clone();
    *Arc::make_mut(&mut group.rows) = (0..20)
        .map(|index| {
            let mut row = template.clone();
            row.id = format!("jump-card-{index}");
            row.name = row.id.clone();
            row.pane.as_mut().unwrap().pane_id = PaneId::from_parts(
                mux,
                match mux {
                    MuxName::Tmux => format!("%{}", 500 + index),
                    MuxName::Zellij => format!("terminal_jump_{index}"),
                },
            );
            row
        })
        .collect();
    let target = group.rows[16].clone();
    let target_pane = target.pane.as_ref().unwrap().pane_id.clone();
    pair.sample(&[(1, home)]);
    pair.panes[home].state.on_snapshot();
    if open {
        let rig = &mut pair.panes[home];
        rig.state.prepare_action();
        rig.state
            .apply_input([Wakeup::Key(KeyAction::ToggleInactive)], &mut rig.terminal)
            .unwrap();
    }
    pair.sample(&[(1, home)]);
    pair.panes[home].state.on_snapshot();
    pair.paint(home);
    pair.panes[home]
        .state
        .with_view_for_test(Route::READ, |_, v| v.state.scroll_offset = 30);

    pair.clock.advance(Duration::from_millis(1));
    let stamp = pair.clock.now_ms();
    crate::mux::focus_anchor::store(
        &pair.panes[1].runtime,
        &FocusAnchor {
            nonce: crate::ids::FocusNonce::new(),
            session_name: "rimz-test".into(),
            pane_id: target_pane.clone(),
            origin: FocusOrigin::User,
            repair_generation: None,
            issued_at_ms: stamp,
            applied_at_ms: Some(stamp),
            state: FocusIntentState::Applied,
            pre_action: Vec::new(),
            placement: Some(FocusPlacement {
                row_id: target.id.clone(),
                viewport_row: 8,
                held: Default::default(),
            }),
            order: None,
        },
    )
    .unwrap();
    pair.sample_with(&[(1, 1)], |frame| {
        frame.client_views[0].pane_id = target_pane.clone();
    });
    pair.panes[1].state.on_snapshot();
    assert_eq!(pair.view(1).selected_pane, Some(target_pane));
    assert!(pair.view(1).focus_placement.is_some());
    assert_eq!(pair.view(1).inactive_peek.is_some(), open);
    pair.paint(1);
    let ui = &pair.panes[1].state.pane.ui;
    (
        ui.painted_rows
            .iter()
            .find_map(|(id, row)| (id == &target.id).then_some(*row)),
        (ui.scroll_zone_top + 8).try_into().unwrap(),
    )
}

#[test]
fn inactive_receipt_tab_jump_keeps_the_peek_until_toggled_and_retains_the_focused_group() {
    for mux in [MuxName::Tmux, MuxName::Zellij] {
        let mut pair = inactive_pair(mux);
        let group = &mut Arc::make_mut(&mut pair.snapshot.worktree_groups)[1];
        let template = group.rows[0].clone();
        for index in 1..12 {
            let mut row = template.clone();
            row.id = format!("old-member-{index}");
            row.name = row.id.clone();
            row.pane.as_mut().unwrap().pane_id = PaneId::from_parts(
                mux,
                match mux {
                    MuxName::Tmux => format!("%{}", 300 + index),
                    MuxName::Zellij => format!("terminal_member_{index}"),
                },
            );
            Arc::make_mut(&mut group.rows).push(row);
        }
        let target = group.rows[0].pane.as_ref().unwrap().pane_id.clone();
        Arc::make_mut(&mut pair.snapshot.worktree_groups).swap(1, 11);
        let own = pair.panes[0].state.pane.config.own_pane.clone();
        inactive_clients(&mut pair, std::slice::from_ref(&own));
        inactive_input(&mut pair, Wakeup::Key(KeyAction::ToggleInactive));
        inactive_clients(&mut pair, std::slice::from_ref(&own));
        pair.paint(0);
        let (column, row) = pair.panes[0]
            .state
            .pane
            .ui
            .interactions
            .line_for_target(&render::HitTarget::ExpandGroup {
                key: "old-01".into(),
                row: "old-01".into(),
            })
            .unwrap();
        let outcome = inactive_input(&mut pair, Wakeup::MouseClick { column, row });
        assert_eq!(outcome.effects, vec![InputEffect::Focus(target.clone())]);
        assert!(pair.view(0).expanded_groups.contains("old-01"));
        pair.panes[0].state.with_view_for_test(Route::READ, |_, v| {
            v.state.scroll_offset = usize::MAX;
            v.state.offset_authoritative = true;
        });
        pair.paint(0);
        assert!(
            pair.panes[0]
                .state
                .pane
                .ui
                .interactions
                .line_for_target(&render::HitTarget::ToggleInactive)
                .is_none()
        );
        pair.sample_with(&[(1, 1)], |frame| {
            frame.client_views[0].pane_id = target.clone();
        });
        let text = inactive_frame(&mut pair, 1);
        assert!(text.contains("old-01"), "{mux:?}: {text}");
        assert!(
            pair.view(1).inactive_peek.is_some(),
            "{mux:?}: jump keeps the peek open"
        );
        assert_eq!(pair.view(1).selected_pane, Some(target.clone()));
        let away = pair
            .snapshot
            .worktree_groups
            .iter()
            .find(|group| group.key == "old-24")
            .unwrap()
            .rows[0]
            .pane
            .as_ref()
            .unwrap()
            .pane_id
            .clone();
        pair.sample_with(&[(1, 1)], |frame| {
            frame.client_views[0].pane_id = away.clone();
        });
        pair.paint(1);
        pair.clock.advance(Duration::from_secs(31));
        pair.sample_with(&[(1, 1)], |frame| {
            frame.client_views[0].pane_id = away.clone();
        });
        pair.paint(1);
        assert!(
            pair.view(1).expanded_groups.contains("old-01"),
            "{mux:?}: the user's unfold survives focus leaving and hold expiry"
        );
        pair.sample_with(&[(1, 1)], |frame| {
            frame.client_views[0].pane_id = target.clone();
        });
        pair.paint(1);
        let rig = &mut pair.panes[1];
        rig.state.prepare_action();
        rig.state
            .apply_input([Wakeup::Key(KeyAction::ToggleInactive)], &mut rig.terminal)
            .unwrap();
        assert!(pair.view(1).inactive_peek.is_none());
        assert!(!pair.view(1).expanded_groups.contains("old-01"));
        pair.paint(1);
        let view = pair.view(1);
        let rig = &mut pair.panes[1];
        assert!(
            rig.state
                .pane
                .ui
                .interactions
                .line_for_target(&render::HitTarget::ToggleInactive)
                .is_some()
        );
        assert!(
            view.scroll_offset < rig.state.pane.ui.drawn_scroll_max.unwrap(),
            "the chip reveal must nudge above the bottom, not merely clamp"
        );
        let snapshot = rig.state.views.mirror();
        let theme = rig.state.pane.ui.theme(&snapshot.theme);
        let text = render::compose_lines(
            snapshot,
            None,
            &view,
            &rig.state.pane.ui,
            theme.as_ref(),
            80,
            200,
        )
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
        assert!(text.contains("[ ▸ 25 inactive ]"), "{mux:?}: {text}");
        for index in 1..12 {
            assert!(
                text.contains(&format!("old-member-{index}")),
                "{mux:?}: {text}"
            );
        }
    }
}
