//! Which sidebar events provoke a producer fetch, when, and how a burst
//! coalesces into one.

use super::*;

#[test]
fn coalesced_idle_interim_and_final_complete_without_reapplying_content() {
    let mut rig = Rig::new();
    let mut snapshot = agent_snapshot(&rig.ws);
    rig.fold(snapshot.clone(), SnapshotSource::Produced);
    rig.fetch.request(FetchRequest::default(), false);
    assert!(rig.next_request().is_some());
    for phase in [FetchPhase::Interim, FetchPhase::Final] {
        snapshot.now += jiff::SignedDuration::from_secs(1);
        rig.result_tx
            .send(FetchUpdate::Snapshot {
                snapshot: Box::new(snapshot.clone()),
                phase,
                source: SnapshotSource::Produced,
            })
            .unwrap();
    }
    rig.state.on_snapshot(&mut rig.fetch);
    assert_eq!(rig.state.current.now, snapshot.now);
    assert_eq!(
        rig.state.content_applications, 1,
        "coalescing must retain clock-only delivery"
    );
    rig.fetch.request(FetchRequest::default(), false);
    assert!(
        rig.next_request().is_some(),
        "unchanged final completed the prior request"
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
        rig.state.content_applications, 1,
        "idle delivery skips content"
    );
    assert_eq!(rig.state.current.now, snapshot.now);

    snapshot.display_name = "renamed room".into();
    rig.fold(snapshot.clone(), SnapshotSource::Produced);
    assert_eq!(rig.state.current.display_name, "renamed room");
    assert_eq!(rig.state.content_applications, 2);

    let sample = crate::store::snapshot::PresenceSample {
        human_clients: 1,
        last_input_ms: Some(100),
        sampled_at_ms: 100,
    };
    snapshot.presence = Some(crate::store::snapshot::SidebarPresence::classify(
        sample, 199, 100,
    ));
    rig.fold(snapshot.clone(), SnapshotSource::Produced);
    snapshot.presence = Some(crate::store::snapshot::SidebarPresence::classify(
        sample, 200, 100,
    ));
    rig.fold(snapshot.clone(), SnapshotSource::Produced);
    assert_eq!(rig.state.current.presence, snapshot.presence);
    assert_eq!(rig.state.content_applications, 4);
}

#[test]
fn renderer_local_reconciliation_state_forces_full_content() {
    type Hold = fn(&mut LoopState);
    let blockers: [(&str, Hold); 8] = [
        ("should_exit", |state| state.should_exit = true),
        ("overlay_baseline", |state| {
            state.overlay_baseline = Some(state.current.clone());
        }),
        ("gate", |state| state.gate.reject_streak = 1),
        ("failure_streak", |state| state.health.failure_streak = 1),
        ("order_hold", |state| {
            state.ui.order_hold = Some(crate::sidebar_pane::render::OrderHold {
                frozen: crate::sidebar_pane::render::FrozenOrder::default(),
                expires_ms: i64::MAX,
            });
        }),
        ("tab_read_dwell", |state| {
            state.tab_read_dwell_until = Some(Instant::now() + Duration::from_secs(60));
        }),
        ("self_close", |state| {
            state.self_close.should_close(Some(0), Instant::now());
        }),
        ("focus_repair", |state| {
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
            hold(&mut rig.state);
            for _ in 0..2 {
                snapshot.now += jiff::SignedDuration::from_secs(1);
                rig.fold(snapshot.clone(), SnapshotSource::Produced);
            }
            (rig.state.content_applications != 2).then_some(name)
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
fn synthetic_fold_releases_baseline_after_overlay_expires() {
    let mut rig = Rig::new();
    let mut process = pane("terminal_9", "tab_0", false);
    process.command = Some("sh".to_owned());
    let pane_id = process.pane_id.clone();
    let snapshot = snapshot_with_panes(&rig.ws, vec![process]);
    rig.fold(snapshot.clone(), SnapshotSource::Produced);
    assert!(rig.state.overlay_baseline.is_none());
    rig.result_tx
        .send(FetchUpdate::Snapshot {
            snapshot: Box::new(snapshot),
            phase: FetchPhase::Final,
            source: SnapshotSource::Produced,
        })
        .unwrap();

    rig.event(SidebarEvent::CommandChanged {
        pane_id: pane_id.clone(),
        command: "vim".to_owned(),
    });
    rig.state.on_snapshot(&mut rig.fetch);
    assert_eq!(
        rig.state.content_applications, 2,
        "a local event promotes queued clocks back to content for fusion"
    );
    assert!(rig.state.overlay_baseline.is_some());
    assert_eq!(
        rig.state
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
    assert!(rig.state.overlay_baseline.is_none());
    assert_eq!(
        rig.state
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

fn focus_records(rig: &Rig) -> Vec<serde_json::Value> {
    std::fs::read_to_string(crate::diag::focus_trace::log_path(rig._dir.path()))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()["event"].clone())
        .filter(|event| event["kind"] == "fold_decided")
        .collect()
}

fn enable_focus_trace(rig: &mut Rig) {
    rig.state.diag = crate::diag::DiagSink::under(
        rig._dir.path().to_owned(),
        rig.ws.clone(),
        "rimz-test",
        Some(rig.state.config.instance_id.clone()),
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
        rig.state.ui.baseline_pane,
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
    assert_eq!(rig.state.ui.baseline_pane, expected);
    assert_eq!(rig.state.ui.selected_pane, expected);
    if case == "frameless" {
        assert_eq!(rig.state.ui.selected_index, None);
        return;
    }
    rig.state.next_frame = Instant::now();
    rig.paint(true);
    let mut parser = vt100::Parser::new(24, 54, 0);
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
    rig.state.seed(WorkspaceFold {
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
    assert_eq!(rig.state.current.rows().count(), 1, "the cards seeded");
    assert_eq!(rig.state.ui.selected_pane, None);
    assert_eq!(rig.state.ui.selected_index, None);
    assert!(rig.state.current.own_view.is_none());
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
    rig.state.seed(WorkspaceFold {
        workspace: crate::sidebar::enrich::WorkspaceSnapshot(seed.clone()),
        frame: Some(Arc::new(frame)),
    });
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
        rig.state.current.rows().count(),
        1,
        "the published card seeded"
    );
    rig.state.next_frame = Instant::now();
    rig.paint(true);
    let mut parser = vt100::Parser::new(24, 54, 0);
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
    let seeded_focus = rig.state.current.focused_pane.clone();
    assert_eq!(rig.state.ui.selected_pane, None);
    assert_eq!(rig.state.ui.selected_index, None);
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
    rig.fold(seed, SnapshotSource::Cached);
    assert_eq!(
        focus_records(&rig),
        traced_seed,
        "the identical cached fold writes nothing"
    );
    assert_eq!(rig.state.ui.selected_pane, None);
    assert_eq!(rig.state.ui.selected_index, None);
    rig.state.next_frame = Instant::now();
    rig.paint(true);
    parser.process(&output.0.lock().unwrap());
    assert_eq!(
        parser.screen().contents(),
        seeded,
        "the cached worker fold holds the resting frame"
    );
    assert_eq!(seeded_focus, published_focus);
    assert_eq!(rig.state.current.focused_pane, published_focus);

    let mut correction = rig.state.current.clone();
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
    assert_eq!(rig.state.ui.selected_pane, Some(shell.pane_id));
    assert_eq!(rig.state.ui.selected_index, Some(1));
    rig.state.next_frame = Instant::now();
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
    assert_eq!(rig.state.ui.selected_pane, Some(prior_focus.clone()));

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
    rig.state.config.refresh_ms_override = Some(37);
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
        sampled_at_ms: seed.now.as_millisecond() as u64,
    });
    let fold = WorkspaceFold {
        workspace: crate::sidebar::enrich::WorkspaceSnapshot(seed),
        frame: Some(Arc::new(frame)),
    };

    rig.fetch.request(FetchRequest::force_fold(), false);
    assert!(rig.next_request().unwrap().forces_fold());
    rig.fetch.request(FetchRequest::fresh_panes(), true);
    rig.state.dirty = false;
    rig.state.seed(fold);

    assert_eq!(
        rig.state.current.rows().count(),
        1,
        "the seed is projected for this pane before any delivery"
    );
    assert_eq!(rig.state.current.rows().next().unwrap().name, "seed-card");
    assert_eq!(rig.state.current.focused_pane, Some(agent_pane.clone()));
    assert!(
        rig.state.current.rows().next().unwrap().unread,
        "the projection's stale focus must not read the seeded card"
    );
    assert_eq!(
        crate::sidebar::read_marks::ReadMarks::load(&rig.runtime).cleared_at_ms("agent-1"),
        None
    );
    assert_eq!(rig.state.current.theme.display.refresh_ms, 37);
    assert!(rig.state.dirty, "the seed is paint-pending");
    assert_eq!(
        rig.state.current.presence,
        Some(crate::store::snapshot::SidebarPresence::Active)
    );
    assert!(rig.state.current.own_view.is_none());
    assert!(!rig.state.self_close.seen_sibling);
    assert!(!rig.state.should_exit);
    assert_eq!(
        rig.state.last_focus_observation.panes_observed_at_ms,
        Some(42)
    );
    assert_eq!(
        rig.state
            .last_focus_observation
            .pane_session_name
            .as_deref(),
        Some("rimz-test")
    );
    assert_eq!(rig.state.last_focus_observation.pane_ids, vec![agent_pane]);
    assert!(rig.state.last_focus_observation.presence_known);
    assert!(
        rig.next_request().is_none(),
        "a seed does not complete the in-flight fetch"
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
        rig.state.current.rows().next().unwrap().name,
        "corrected-card"
    );
    assert_eq!(rig.state.ui.selected_pane, Some(corrected_pane.pane_id));
    assert!(!rig.state.current.rows().next().unwrap().unread);
    assert!(
        crate::sidebar::read_marks::ReadMarks::load(&rig.runtime)
            .cleared_at_ms("agent-1")
            .is_some()
    );
    assert_eq!(
        rig.state.gate.reject_streak, 0,
        "the changed pane set commits"
    );
    assert!(
        rig.next_request().unwrap().is_fresh_panes(),
        "only the final correction completes the fetch"
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
    rig.state.seed(WorkspaceFold {
        workspace: crate::sidebar::enrich::WorkspaceSnapshot(seed),
        frame: Some(Arc::new(frame)),
    });

    assert!(rig.state.current.own_view.is_some());
    assert_eq!(rig.state.ui.selected_pane, Some(agent.pane_id));
    assert_eq!(rig.state.ui.selected_index, Some(0));
    assert!(rig.state.self_close.seen_sibling);
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
        .on_input(press.clone(), &mut rig.terminal, &mut rig.fetch)
        .unwrap();
    assert_eq!(rig.state.ui.selected_index, None);

    let mut rebound = snapshot;
    rebound.sidebar.keys.down = "v".to_owned();
    rig.fold(rebound, SnapshotSource::Produced);
    rig.state
        .on_input(press, &mut rig.terminal, &mut rig.fetch)
        .unwrap();
    assert_eq!(rig.state.ui.selected_index, Some(0));
    assert_eq!(
        rig.state.ui.selected_pane,
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
            &mut rig.fetch,
        )
        .unwrap();
    assert_eq!(
        rig.state.ui.selected_index,
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
    rig.fetch.request(FetchRequest::default(), false);
    assert!(rig.next_request().is_some());
    rig.fetch.request(FetchRequest::fresh_panes(), true);
    let filter = BodyLens::from(BodyFilter::Status(crate::agents::AgentStatus::Idle));
    rig.result_tx
        .send(FetchUpdate::Shared {
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
        })
        .unwrap();
    rig.result_tx
        .send(FetchUpdate::Failed {
            error: "fold failed".into(),
        })
        .unwrap();
    rig.state.on_snapshot(&mut rig.fetch);
    assert_eq!(
        rig.state.current.rows().next().map(|row| row.name.as_str()),
        Some("unread-snapshot"),
        "a terminal outcome must not erase the unread snapshot"
    );
    assert_eq!(
        rig.state.ui.make_up_filter, filter,
        "snapshot context is applied with its snapshot"
    );
    assert_eq!(rig.state.health.failure_streak, 1);
    assert!(
        rig.next_request()
            .expect("final outcome releases the queued request")
            .is_fresh_panes()
    );
    assert!(rig.next_request().is_none());
}

#[test]
fn a_final_snapshot_never_yields_to_an_interim_or_loses_completion() {
    let mut rig = Rig::new();
    rig.fetch.request(FetchRequest::default(), false);
    assert!(rig.next_request().is_some());
    rig.fetch.request(FetchRequest::fresh_panes(), true);
    for (name, phase) in [
        ("final-snapshot", FetchPhase::Final),
        ("interim-snapshot", FetchPhase::Interim),
    ] {
        let mut snapshot = agent_snapshot(&rig.ws);
        std::sync::Arc::make_mut(
            &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
        )[0]
        .name = name.into();
        rig.result_tx
            .send(FetchUpdate::Snapshot {
                snapshot: Box::new(snapshot),
                phase,
                source: SnapshotSource::Cached,
            })
            .unwrap();
    }
    rig.state.on_snapshot(&mut rig.fetch);
    assert_eq!(
        rig.state.current.rows().next().unwrap().name,
        "final-snapshot",
        "an interim must not displace the pending final snapshot"
    );
    assert!(
        rig.next_request()
            .expect("completion survives an interim publication")
            .is_fresh_panes()
    );
}

#[test]
fn a_failed_completion_survives_a_later_interim_snapshot() {
    let mut rig = Rig::new();
    rig.fetch.request(FetchRequest::default(), false);
    assert!(rig.next_request().is_some());
    rig.fetch.request(FetchRequest::fresh_panes(), true);
    rig.result_tx
        .send(FetchUpdate::Failed {
            error: "fold failed".into(),
        })
        .unwrap();
    rig.result_tx
        .send(FetchUpdate::Snapshot {
            snapshot: Box::new(agent_snapshot(&rig.ws)),
            phase: FetchPhase::Interim,
            source: SnapshotSource::Cached,
        })
        .unwrap();
    rig.state.on_snapshot(&mut rig.fetch);
    assert_eq!(
        rig.state.health.failure_streak, 1,
        "an interim must not erase a pending failure"
    );
    assert_eq!(rig.state.current.rows().count(), 1);
    assert!(
        rig.next_request()
            .expect("failed final releases the request")
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
fn store_delta_requests_default_or_panes_changed_freshness() {
    use crate::agents::LifecycleSignal;

    for (event, expected) in [
        (store_delta(), FetchRequest::default()),
        (SidebarEvent::PanesChanged, FetchRequest::fresh_panes()),
        (
            SidebarEvent::StoreDelta {
                event_method: Some(crate::store::event::AGENT_LIFECYCLE_METHOD.to_owned()),
                agent_signal: Some(LifecycleSignal::Registered.tag().to_owned()),
            },
            FetchRequest::fresh_panes(),
        ),
        (
            SidebarEvent::StoreDelta {
                event_method: Some(crate::store::event::AGENT_LIFECYCLE_METHOD.to_owned()),
                agent_signal: Some(LifecycleSignal::Ended.tag().to_owned()),
            },
            FetchRequest::fresh_panes(),
        ),
    ] {
        let mut rig = Rig::new();

        rig.event(event.clone());

        let request = rig.requests.try_recv().expect("immediate event fetch");
        assert_eq!(
            request.is_fresh_panes(),
            expected.is_fresh_panes(),
            "{event:?}",
        );
        assert_eq!(request.forces_fold(), expected.forces_fold(), "{event:?}");
        assert!(rig.next_request().is_none(), "one fetch for {event:?}");
    }
}

#[test]
fn lifecycle_store_delta_preserves_fresh_pane_verification() {
    for signal in [
        crate::agents::LifecycleSignal::Registered.tag(),
        crate::agents::LifecycleSignal::Ended.tag(),
    ] {
        let mut rig = Rig::new();

        rig.event(SidebarEvent::StoreDelta {
            event_method: Some(crate::store::event::AGENT_LIFECYCLE_METHOD.to_owned()),
            agent_signal: Some(signal.to_owned()),
        });

        let request = rig.next_request().expect("immediate lifecycle fetch");
        assert!(request.is_fresh_panes(), "signal: {signal}");
    }
}

#[test]
fn watched_metrics_and_hidden_presence_publications_fold_immediately() {
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;

    for watched in [true, false] {
        let mut rig = Rig::with_own_pane(own_pane.clone());
        rig.hide();
        if watched {
            rig.watch();
        }

        let mut folded = process_snapshot(&rig.ws, crate::utils::time::unix_now_ms());
        folded.own_view = rig.state.current.own_view.clone();
        folded.viewed_panes = rig.state.current.viewed_panes.clone();
        rig.state.current = folded.clone();
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

        assert!(matches!(
            &rig.state.current.worktree_groups[0].rows[0].card,
            crate::store::snapshot::RowCard::Process(card) if card.cpu_pct == Some(73)
        ));
        assert_eq!(
            rig.state.current.presence,
            Some(crate::store::snapshot::SidebarPresence::Detached)
        );
    }
}

#[test]
fn watched_and_hidden_renderers_fetch_identity_free_events_immediately() {
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;

    for watched in [true, false] {
        let mut rig = Rig::with_own_pane(own_pane.clone());
        rig.hide();
        if watched {
            rig.watch();
        }

        rig.event(store_delta());

        assert!(rig.next_request().is_some());
        assert!(rig.fetch.next_deadline().is_none());
    }
}

#[test]
fn maintenance_watchdog_absorbs_deferred_unwatched_fetch() {
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let mut rig = Rig::with_own_pane(own_pane);
    rig.hide();

    rig.fetch.defer_until(
        FetchRequest::default(),
        Instant::now() + Duration::from_secs(1),
    );

    rig.state.last_self_close_check = Instant::now() - SELF_CLOSE_WATCHDOG;
    rig.maintenance();

    assert!(
        rig.next_request().is_some(),
        "watchdog dispatches one fetch"
    );
    assert!(rig.fetch.next_deadline().is_none());
    assert!(
        rig.next_request().is_none(),
        "the deferred nudge merges into the watchdog fetch"
    );
}

#[test]
fn focus_resume_flushes_pending_metrics_fetch() {
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let mut rig = Rig::with_own_pane(own_pane.clone());
    rig.hide();

    rig.fetch.defer_until(
        FetchRequest::default(),
        Instant::now() + Duration::from_secs(1),
    );

    rig.event(SidebarEvent::FocusChanged {
        focused: vec![own_pane],
        unfocused: Vec::new(),
    });

    assert!(rig.fetch.next_deadline().is_none());
    assert!(
        rig.next_request()
            .expect("focus flushed pending fetch")
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
        rig.state.width_control.max_legit_cols(),
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
    let rig = Rig::with_filter(BodyLens::from(BodyFilter::Unread));

    assert_eq!(
        rig.state.ui.make_up_filter,
        BodyLens::from(BodyFilter::Unread)
    );
}

#[test]
fn body_filter_event_adopts_the_shared_file_and_repaints() {
    let mut rig = Rig::new();
    rig.state.current = agent_snapshot(&rig.ws);
    rig.state.dirty = false;
    let filter = BodyLens {
        filter: Some(BodyFilter::Status(crate::agents::AgentStatus::Idle)),
        query: Some("auth".to_owned()),
    };
    crate::sidebar::body_filter::write(&rig.runtime, &filter).expect("write shared filter");

    rig.event(SidebarEvent::BodyFilterChanged);

    assert_eq!(rig.state.ui.make_up_filter, filter);
    assert!(rig.state.dirty);
    assert!(
        rig.next_request().is_none(),
        "filter propagation stays out of the producer path"
    );
}

#[test]
fn search_commit_writes_and_broadcasts_the_lens() {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    let mut rig = Rig::new();
    rig.state.current = agent_snapshot(&rig.ws);
    std::fs::create_dir_all(rig.state.host_wake_path.parent().unwrap()).unwrap();
    let socket = crate::sidebar_pane::host::bind_wake_socket(&rig.state.host_wake_path).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    write_heartbeat(
        &rig.state.config,
        &rig.runtime,
        &rig.state.host_wake_path,
        None,
    )
    .unwrap();
    for code in [KeyCode::Char('/'), KeyCode::Char('a'), KeyCode::Enter] {
        rig.state
            .on_input(
                Wakeup::Press {
                    code,
                    mods: KeyModifiers::NONE,
                },
                &mut rig.terminal,
                &mut rig.fetch,
            )
            .unwrap();
    }
    assert_eq!(
        crate::sidebar::body_filter::load(&rig.runtime)
            .query
            .as_deref(),
        Some("a")
    );
    let mut bytes = [0; 2048];
    let len = socket.recv(&mut bytes).unwrap();
    let envelope: SidebarEventEnvelope = serde_json::from_slice(&bytes[..len]).unwrap();
    assert_eq!(envelope.event, SidebarEvent::BodyFilterChanged);
}

#[test]
fn search_draft_survives_peer_adoption_and_focus_loss_keeps_commit() {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    let own = pane("terminal_1", "tab_0", false).pane_id;
    let mut rig = Rig::with_own_pane(own.clone());
    rig.state.current = agent_snapshot(&rig.ws);
    rig.state
        .on_input(
            Wakeup::Press {
                code: KeyCode::Char('/'),
                mods: KeyModifiers::NONE,
            },
            &mut rig.terminal,
            &mut rig.fetch,
        )
        .unwrap();
    assert_eq!(rig.state.ui.search_draft.as_deref(), Some(""));
    rig.state.ui.search_draft = Some("draft".to_owned());
    let committed = BodyLens {
        query: Some("cla".to_owned()),
        ..Default::default()
    };
    crate::sidebar::body_filter::write(&rig.runtime, &committed).unwrap();
    rig.event(SidebarEvent::BodyFilterChanged);
    assert_eq!(rig.state.ui.search_draft.as_deref(), Some("draft"));
    let snapshot = agent_snapshot(&rig.ws);
    rig.fold(snapshot, SnapshotSource::Produced);
    assert_eq!(rig.state.ui.search_draft.as_deref(), Some("draft"));
    rig.event(SidebarEvent::FocusChanged {
        focused: Vec::new(),
        unfocused: vec![own],
    });
    assert_eq!(rig.state.ui.search_draft, None);
    assert_eq!(rig.state.ui.make_up_filter, committed);
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
    assert_eq!(rig.state.ui.selected_pane, committed_pane);
    for ch in ['/', '!'] {
        rig.state
            .on_input(
                Wakeup::Press {
                    code: KeyCode::Char(ch),
                    mods: KeyModifiers::NONE,
                },
                &mut rig.terminal,
                &mut rig.fetch,
            )
            .unwrap();
    }
    assert_eq!(rig.state.ui.search_draft.as_deref(), Some("cla!"));
    assert_eq!(rig.state.ui.selected_pane, None, "draft has no matches");
    rig.deliver(FetchUpdate::Failed {
        error: "snapshot failed".to_owned(),
    });
    assert!(!rig.state.alert_active());
    assert_eq!(rig.state.ui.search_draft.as_deref(), Some("cla!"));
    rig.state.dirty = false;
    rig.deliver(FetchUpdate::Failed {
        error: "snapshot failed".to_owned(),
    });
    assert_eq!(rig.state.ui.search_draft, None);
    assert_eq!(rig.state.ui.make_up_filter, committed);
    assert_eq!(crate::sidebar::body_filter::load(&rig.runtime), committed);
    assert!(rig.state.alert_active());
    assert!(rig.state.dirty);
    assert_eq!(rig.state.ui.selected_pane, committed_pane);
    rig.state
        .on_input(
            Wakeup::Press {
                code: KeyCode::Char('/'),
                mods: KeyModifiers::NONE,
            },
            &mut rig.terminal,
            &mut rig.fetch,
        )
        .unwrap();
    assert_eq!(rig.state.ui.search_draft, None);
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
    assert_eq!(rig.state.ui.make_up_filter, filter);

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
        rig.state.ui.make_up_filter, filter,
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

    assert_eq!(rig.state.ui.make_up_filter, filter);
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
    assert_eq!(rig.state.ui.visible_roster(&rig.state.current).len(), 0);
    assert_eq!(rig.state.ui.make_up_filter, lens);
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
    assert_eq!(rig.state.ui.make_up_filter, BodyLens::default());
}

#[test]
fn failed_or_rowless_birth_fold_does_not_publish_a_filter_clear() {
    let filter = BodyLens::from(BodyFilter::Status(crate::agents::AgentStatus::Waiting));
    let mut rig = Rig::with_filter(filter.clone());

    rig.deliver(FetchUpdate::Failed {
        error: "not ready".to_owned(),
    });
    assert_eq!(rig.state.ui.make_up_filter, BodyLens::default());
    assert_eq!(crate::sidebar::body_filter::load(&rig.runtime), filter);

    let rowless = snapshot(&rig.ws);
    rig.fold(rowless, SnapshotSource::Produced);
    assert_eq!(rig.state.ui.make_up_filter, BodyLens::default());
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
    assert_eq!(rig.state.ui.make_up_filter, expected);
    assert_eq!(crate::sidebar::body_filter::load(&rig.runtime), expected);
}

#[test]
fn focus_out_closes_help_popup() {
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let mut rig = Rig::with_own_pane(own_pane.clone());
    let snapshot = snapshot_with_focused_pane(&rig.ws, own_pane.clone());
    rig.set_pulled(&snapshot);
    rig.state.current = snapshot;
    rig.state.ui.help_visible = true;
    rig.state.optimistic_watch_until = Some(Instant::now() + Duration::from_secs(1));

    rig.event(SidebarEvent::FocusChanged {
        focused: Vec::new(),
        unfocused: vec![own_pane],
    });

    assert!(!rig.state.ui.help_visible);
    assert!(rig.state.optimistic_watch_until.is_none());
}
