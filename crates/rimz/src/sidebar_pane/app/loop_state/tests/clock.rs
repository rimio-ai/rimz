use super::*;
use std::sync::atomic::Ordering;

fn compacting_rig() -> (Rig, FrameOutput, LoopClock) {
    let clock = LoopClock::play("2026-06-09T12:00:00Z".parse().unwrap());
    let mut rig = Rig::with_clock(None, BodyLens::default(), None, clock.clone());
    let mut snapshot = agent_snapshot(&rig.ws);
    snapshot.now = clock.timestamp();
    snapshot
        .rows_mut()
        .next()
        .unwrap()
        .as_agent_mut()
        .unwrap()
        .compacting = true;
    rig.state.with_view_for_test(Route::READ, |_, v| {
        v.state
            .toggles
            .expanded_groups
            .insert(snapshot.worktree_groups[0].key.clone());
        v.context.current = snapshot;
        v.context.anim_start = clock.now();
    });
    let output = FrameOutput::default();
    rig.terminal.backend_mut().play_output(output.clone());
    (rig, output, clock)
}

fn painted_head(output: &FrameOutput) -> String {
    let mut parser = vt100::Parser::new(24.try_into().unwrap(), 80.try_into().unwrap(), 0);
    parser.process(&output.0.lock().unwrap());
    let screen = parser.screen().contents();
    screen
        .lines()
        .find(|line| line.contains("claude"))
        .unwrap_or_else(|| panic!("painted agent card: {screen}"))
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned()
}

#[test]
fn head_steps_start_at_zero_and_never_skip_late_paints() {
    let wave = ["▁", "▃", "▄", "▅", "▆", "▇", "▆", "▅", "▄", "▃"];
    for spacing in [2, 5] {
        let (mut rig, output, clock) = compacting_rig();
        let refresh = animation_frame(rig.state.views.mirror());
        clock.advance(7 * refresh);
        for step in 0..=wave.len() {
            rig.state.pane.dirty = true;
            rig.state.paint_now(&mut rig.terminal, clock.now()).unwrap();
            assert_eq!(
                painted_head(&output),
                wave[step % wave.len()],
                "spacing {spacing}, step {step}"
            );
            clock.advance(spacing * refresh);
        }
    }
}

#[test]
fn head_steps_hold_during_input_bursts_within_each_phase() {
    let (mut rig, output, clock) = compacting_rig();
    let wave = ["▁", "▃", "▄", "▅", "▆", "▇", "▆", "▅", "▄", "▃", "▁"];
    let mut previous = 0;
    for (step, glyph) in wave.into_iter().enumerate() {
        for offset in [0, 16, 96] {
            let elapsed = step as u64 * 100 + offset;
            clock.advance(Duration::from_millis(elapsed - previous));
            previous = elapsed;
            rig.state
                .on_input(Wakeup::Resize, &mut rig.terminal)
                .unwrap();
            assert_eq!(painted_head(&output), glyph, "input at {elapsed} ms");
        }
    }
}

#[test]
fn compacting_wave_paints_every_frame_on_its_own_grid() {
    let (mut rig, output, clock) = compacting_rig();
    let refresh = animation_frame(rig.state.views.mirror());
    rig.state.pane.dirty = true;
    rig.state.paint_now(&mut rig.terminal, clock.now()).unwrap();
    for glyph in ["▁", "▃", "▄", "▅", "▆", "▇", "▆", "▅", "▄", "▃"] {
        assert_eq!(painted_head(&output), glyph);
        let (active, timeout) = rig.state.frame_timing();
        assert!(
            active,
            "the drawn compacting head keeps the scheduler active"
        );
        assert_eq!(timeout, refresh, "compacting stays on the base grid");
        clock.advance(timeout);
        let (painted, _) = rig
            .state
            .advance(&mut super::scroll::queued([]), &mut rig.terminal)
            .unwrap();
        assert!(painted, "the scheduled frame paints without data or input");
    }
    assert_eq!(painted_head(&output), "▁", "the whole wave wraps");
}

#[test]
fn tab_read_dwell_ignores_wall_steps() {
    let clock = LoopClock::play("2026-06-09T12:00:00Z".parse().unwrap());
    let own = PaneId::from_parts(MuxName::Zellij, "terminal_99");
    let mut rig = Rig::with_clock(Some(own.clone()), BodyLens::default(), None, clock.clone());
    rig.view().ctx().current = agent_snapshot(&rig.ws);
    rig.view().ctx().current.own_view = Some(SidebarOwnView {
        sibling_count: 1,
        working_pane_ids: vec![],
        own_view_is_daemon: false,
    });
    rig.view().ctx().current.viewed_panes = vec![own];
    rig.view().ui().viewing_own_tab = Some(false);
    let marks = ReadMarks::load(&rig.runtime);
    let mut clear = ReadClear::default();
    let wall = clock.timestamp();
    rig.state.with_view_for_test(Route::READ, |pane, v| {
        pane.sweep_tab_read_receipts(v, None, &marks, wall, &mut clear);
    });
    assert_eq!(
        rig.view().ctx().tab_read_dwell_until,
        Some(clock.now() + TAB_READ_DWELL)
    );
    rig.state.with_view_for_test(Route::READ, |pane, v| {
        pane.sweep_tab_read_receipts(
            v,
            None,
            &marks,
            wall + Duration::from_secs(3_600),
            &mut clear,
        );
    });
    assert!(
        rig.view().ctx().tab_read_dwell_until.is_some(),
        "a forward wall step is not a read"
    );
    clock.advance(TAB_READ_DWELL);
    rig.state.with_view_for_test(Route::READ, |pane, v| {
        pane.sweep_tab_read_receipts(
            v,
            None,
            &marks,
            wall - Duration::from_secs(3_600),
            &mut clear,
        );
    });
    assert!(
        rig.view().ctx().tab_read_dwell_until.is_none(),
        "a backward wall step cannot extend the dwell"
    );
}

#[test]
fn body_filter_observation_is_not_backdated_to_the_fold() {
    let mut rig = Rig::new();
    let filter = BodyLens {
        query: Some("no such row".to_owned()),
        ..Default::default()
    };
    crate::sidebar::body_filter::write(&rig.runtime, &filter).unwrap();
    let previous = rig.state.views.mirror().clone();
    let stale = Instant::now() - Duration::from_secs(1);
    let before_load = Instant::now();
    rig.state.fold_transition(|pane, v, prepared| {
        pane.reconcile_selection_and_order(v, prepared, &previous, None, None, false, true, stale);
    });
    assert!(rig.view().ctx().filter_observed_at.unwrap() >= before_load);
    rig.shared_fold(
        agent_snapshot(&rig.ws),
        Arc::new(super::super::super::fetch::FoldInputs {
            sampled_at: before_load,
            ..Default::default()
        }),
    );
    assert_eq!(
        rig.view().ui().toggles.make_up_filter,
        filter,
        "an older worker cut cannot undo the filter just loaded"
    );
    {
        let mut guard = rig.view();
        let (ui, context) = guard.pair();
        assert_eq!(ui.visible_roster(&context.current).len(), 0);
    }
}

#[test]
fn play_clock_drives_the_loop_without_focus_or_notification_effects() {
    let epoch: Timestamp = "2026-06-09T12:00:00Z".parse().unwrap();
    let clock = LoopClock::play(epoch);
    let mut rig = Rig::with_clock(None, BodyLens::default(), None, clock.clone());
    let output = FrameOutput::default();
    rig.terminal = Terminal::with_options(
        PaneBackend::headless(output.clone()),
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 80, 24)),
        },
    )
    .unwrap();
    let mut snapshot = agent_snapshot(&rig.ws);
    snapshot.now = epoch;
    let row = &mut Arc::make_mut(&mut Arc::make_mut(&mut snapshot.worktree_groups)[0].rows)[0];
    row.unread = true;
    let crate::store::snapshot::RowCard::Agent(card) = &mut row.card else {
        panic!("agent fixture");
    };
    Arc::make_mut(card).status = crate::agents::AgentStatus::Waiting;
    let target = row.pane.as_ref().unwrap().pane_id.clone();
    snapshot.own_view = Some(SidebarOwnView {
        sibling_count: 1,
        working_pane_ids: vec![target.clone()],
        own_view_is_daemon: false,
    });
    rig.fold(snapshot, SnapshotSource::Produced);
    rig.state.pane.config.room.notification_prefs.enabled = true;
    rig.state.pane.config.room.notification_prefs.remind_secs = 1;
    rig.state.pane.config.room.notification_prefs.command = Some("exit 99".into());
    clock.advance(Duration::from_secs(1));
    assert_eq!(
        rig.state.pane.clock.timestamp(),
        epoch + Duration::from_secs(1)
    );
    rig.state.handle_notification(
        &mut rig.terminal,
        SidebarEvent::Notify {
            title: "waiting".into(),
            body: "waiting".into(),
            panes: vec![target.clone()],
            recheck_unread: false,
            notification_kind: Some("waiting".into()),
        },
    );
    assert!(
        output.0.lock().unwrap().is_empty(),
        "no terminal notifications"
    );

    rig.state.paint_now(&mut rig.terminal, clock.now()).unwrap();
    assert_eq!(
        rig.view().ui().frame_ms,
        render::FrameTime::from_millis(1_000),
        "paint uses scenario elapsed time"
    );
    let row = rig.state.pane.ui.interactions.line_for_row(0).unwrap() as u16;
    let (tx, rx) = std::sync::mpsc::sync_channel(4);
    let mut inbox = WakeupInbox::new(rx);
    tx.send(Wakeup::MouseClick { column: 2, row }).unwrap();
    let (painted, deadline) = rig.state.advance(&mut inbox, &mut rig.terminal).unwrap();
    assert!(painted);
    assert!(deadline > clock.now());
    assert!(
        rig.state.pane.last_heartbeat.is_none(),
        "play publishes no heartbeat"
    );
    assert_eq!(rig.view().ui().selected_pane, Some(target));
    assert!(
        rig.state.pane.clicked_row.is_some(),
        "click reached the focus effect"
    );
    assert_eq!(
        rig.view().ui().freeze.as_ref().unwrap().armed_ms,
        clock.now_ms() as i64
    );
    assert_eq!(clock.focus_calls.load(Ordering::Relaxed), 0);
    rig.state.on_resize(&mut rig.terminal, Some(80)).unwrap();
    assert!(
        rig.state.pane.caps_refresh_deadline.is_none(),
        "play never arms an external caps probe"
    );

    let wall = LoopClock::wall();
    wall.focus(|| {});
    assert_eq!(wall.focus_calls.load(Ordering::Relaxed), 1);
}

fn effect_rig() -> (Rig, FrameOutput, LoopClock) {
    let (mut rig, output, clock) = compacting_rig();
    rig.state.with_view_for_test(Route::READ, |_, v| {
        v.context
            .current
            .rows_mut()
            .next()
            .unwrap()
            .as_agent_mut()
            .unwrap()
            .compacting = false;
    });
    (rig, output, clock)
}

fn spend_at(rig: &mut Rig, usd: f64) {
    rig.state.with_view_for_test(Route::READ, |pane, v| {
        v.context.current.today_spend_live_usd = Some(usd);
        v.context
            .current
            .rows_mut()
            .next()
            .unwrap()
            .as_agent_mut()
            .unwrap()
            .delegated_cost_usd = Some(usd);
        v.context.fold_spend_rolls(v.state, &pane.clock);
    });
}

fn paint_effects(rig: &mut Rig, clock: &LoopClock) {
    rig.state.pane.dirty = true;
    rig.state.paint_now(&mut rig.terminal, clock.now()).unwrap();
}

#[test]
fn settled_money_survives_a_long_uptime_grid_raise() {
    let (mut rig, output, clock) = effect_rig();
    clock.advance(Duration::from_secs(3_590));
    spend_at(&mut rig, 1.0);
    spend_at(&mut rig, 5.0);
    clock.advance(Duration::from_secs(10));
    paint_effects(&mut rig, &clock);
    rig.view().ctx().current.theme.display.refresh_ms = 200;
    paint_effects(&mut rig, &clock);
    let mut parser = vt100::Parser::new(24.try_into().unwrap(), 80.try_into().unwrap(), 0);
    parser.process(&output.0.lock().unwrap());
    let screen = parser.screen().contents();
    assert_eq!(
        screen.matches("$5.00").count(),
        2,
        "headline and card stay settled: {screen}"
    );
    rig.state.with_view_for_test(Route::READ, |_, v| {
        let now = v.state.frame_ms;
        let refresh_ms = v.context.current.theme.display.resolved_refresh_ms();
        assert!(!v.state.tally.any_rolling(now, refresh_ms));
        assert!(!v.state.cost_rolls.any_rolling(now, refresh_ms));
        assert!(!v.state.tally.today_usd.flashing(now, refresh_ms));
        assert!(!v.state.cost_rolls.flashing("agent-1", now, refresh_ms));
    });
    assert!(
        !rig.state.frame_timing().0,
        "settled money releases its grid"
    );
}

#[test]
fn settled_scrollbar_survives_a_long_uptime_grid_raise() {
    let (mut rig, _, clock) = effect_rig();
    clock.advance(Duration::from_secs(3_600));
    paint_effects(&mut rig, &clock);
    rig.state.with_view_for_test(Route::READ, |_, v| {
        v.state.scrollbar.observe(1, v.state.frame_ms);
        v.state.scrollbar.observe(0, v.state.frame_ms);
    });
    clock.advance(Duration::from_secs(3));
    rig.view().ctx().current.theme.display.refresh_ms = 200;
    paint_effects(&mut rig, &clock);
    rig.state.with_view_for_test(Route::READ, |_, v| {
        frame_clock(clock.now(), v.context.anim_start, 200).stamp(v.state);
        assert!(
            !v.state.scrollbar.visible(v.state.frame_ms, 200),
            "the bar stays hidden"
        );
    });
    assert!(
        !rig.state.frame_timing().0,
        "settled fade releases the base grid"
    );
}

/// A recorded card's seconds countdown advances on a fold whose content did not change: the
/// clock-only delivery marks the pane dirty, and the loop's own due check repaints it.
#[test]
fn a_recorded_cache_countdown_repaints_on_a_clock_only_fold() {
    let at = |stamp: &str| stamp.parse::<Timestamp>().unwrap();
    let clock = LoopClock::play(at("2026-11-11T16:44:10Z"));
    let mut rig = Rig::with_clock(None, BodyLens::default(), None, clock.clone());
    let mut snapshot = agent_snapshot(&rig.ws);
    snapshot.now = clock.timestamp();
    snapshot.theme.display.card_density = crate::config::CardDensityMode::Expanded;
    let turn_end = at("2026-11-11T15:45:27.992Z");
    let row = snapshot.rows_mut().next().unwrap();
    row.last_activity = turn_end;
    let card = row.as_agent_mut().unwrap();
    card.status = crate::agents::AgentStatus::Success;
    card.own_last_activity = Some(turn_end);
    card.usage.context_pct = Some(40);
    card.cache = Some(crate::store::snapshot::CacheClock {
        ceiling_secs: 3600,
        last_request_at: Some(at("2026-11-11T15:44:52.830Z")),
        warm_until: None,
        rewarm_cost_usd: None,
        recorded: true,
    });
    let key = snapshot.worktree_groups[0].key.clone();
    rig.state.with_view_for_test(Route::READ, |_, v| {
        v.state.toggles.expanded_groups.insert(key);
    });
    let output = FrameOutput::default();
    rig.terminal.backend_mut().play_output(output.clone());
    let mut screens = Vec::new();
    for _ in 0..2 {
        rig.fold(snapshot.clone(), SnapshotSource::Produced);
        let active = rig.frame_active();
        clock.advance(Duration::from_secs(1));
        rig.paint(active);
        let mut parser = vt100::Parser::new(24.try_into().unwrap(), 80.try_into().unwrap(), 0);
        parser.process(&output.0.lock().unwrap());
        screens.push(parser.screen().contents());
        clock.advance(Duration::from_secs(4));
        snapshot.now += jiff::SignedDuration::from_secs(5);
    }
    assert_eq!(
        rig.state.pane.content_applications, 1,
        "the second fold applies clock-only"
    );
    assert!(screens[0].contains("expires in 43s"), "{}", screens[0]);
    assert!(screens[1].contains("expires in 38s"), "{}", screens[1]);
}
