//! Resize holds keep a grow from painting torn width.

use super::*;

#[test]
fn resize_hold_releases_only_on_a_post_engage_pane_stamp() {
    // The hold engages at pane stamp 100; only a pull observed after that
    // proves the resize verdict landed. A producer fold requested for fresh
    // panes before the grow can land after it still counting the closed
    // sibling; its pre-engage stamp keeps the hold, so no wide frame paints.
    for (observed_at_ms, source, releases) in [
        (101, SnapshotSource::Cached, true),
        (99, SnapshotSource::Cached, false),
        (101, SnapshotSource::Produced, true),
        (99, SnapshotSource::Produced, false),
    ] {
        let mut rig = Rig::new();
        rig.state.current = agent_snapshot(&rig.ws);
        rig.state.self_close.seen_sibling = true;
        rig.state.paint_hold.engage(Instant::now(), 100);

        let snapshot = agent_snapshot_observed(&rig.ws, observed_at_ms);
        rig.fold(snapshot, source);

        assert_eq!(
            !rig.state.paint_hold.is_engaged(),
            releases,
            "{source:?} pane stamp {observed_at_ms} against an engage at 100"
        );
    }
}

#[test]
fn resize_hold_releases_on_escape_hatch_accepting_post_engage_stamp() {
    let mut rig = Rig::new();
    let mut prior = agent_snapshot(&rig.ws);
    prior.panes_observed_at_ms = Some(90);
    rig.state.current = prior;
    rig.state.paint_hold.engage(Instant::now(), 100);

    let snapshot = process_snapshot(&rig.ws, 150);
    rig.fold(snapshot, SnapshotSource::Cached);
    assert!(
        rig.state.paint_hold.is_engaged(),
        "the rejected fold stays held"
    );
    assert_eq!(rig.state.gate.reject_streak, 1);
    assert_eq!(
        rig.state
            .overlay_baseline
            .as_ref()
            .and_then(|snapshot| snapshot.panes_observed_at_ms),
        Some(150),
        "the held incoming pull becomes the lazy realtime baseline"
    );

    let snapshot = process_snapshot(&rig.ws, 151);
    rig.fold(snapshot, SnapshotSource::Cached);
    assert!(
        rig.state.paint_hold.is_engaged(),
        "the second rejected fold still stays held"
    );
    assert_eq!(rig.state.gate.reject_streak, 2);
    let now_ms = jiff::Timestamp::now().as_millisecond();
    rig.state.gate.rejecting_since =
        Some(jiff::Timestamp::from_millisecond(now_ms - 1_000).unwrap());

    let snapshot = process_snapshot(&rig.ws, 152);
    rig.fold(snapshot, SnapshotSource::Cached);
    assert!(
        !rig.state.paint_hold.is_engaged(),
        "the escape-hatch accepted fold releases by pane stamp"
    );
    assert!(
        rig.state.overlay_baseline.is_none(),
        "an accepted overlay-free pull releases the full baseline"
    );
}

#[test]
fn arm_paint_hold_on_grow_engages_only_beyond_the_legitimate_width() {
    // (label, prev width, sibling seen, grow to, arms)
    let cases = [
        ("grow beyond the cap arms the hold", 60, true, 120, true),
        (
            "same-width paint does not arm the hold",
            120,
            true,
            120,
            false,
        ),
        ("shrink paint does not arm the hold", 120, true, 60, false),
        (
            "startup grow paints immediately before any sibling has been observed",
            60,
            false,
            120,
            false,
        ),
    ];

    for (label, prev_width, seen_sibling, grow_to, arms) in cases {
        let mut rig = Rig::new();
        rig.state.prev_width = Some(prev_width);
        rig.state.self_close.seen_sibling = seen_sibling;

        assert_eq!(
            rig.state.arm_paint_hold_on_grow(grow_to, Instant::now()),
            arms,
            "{label}"
        );
        assert_eq!(rig.state.paint_hold.is_engaged(), arms, "{label}");
        assert_eq!(
            rig.state.prev_width,
            Some(prev_width),
            "resize wakeup still owns prev_width advancement: {label}"
        );
    }
}

#[test]
fn attach_sized_grow_repaints_with_a_seen_sibling() {
    let mut rig = Rig::new().width(57);
    rig.state.prev_width = Some(10);
    rig.state.self_close.seen_sibling = true;
    rig.state.last_heartbeat = Some(Instant::now());

    rig.state
        .on_resize(&mut rig.fetch, &mut rig.terminal, Some(57))
        .expect("handle attach resize");

    assert_eq!(
        rig.state.last_heartbeat, None,
        "a resize restamps the heartbeat's pane size on this pass"
    );
    assert!(
        !rig.state.paint_hold.is_engaged(),
        "a grow within the legitimate cap paints immediately"
    );
    assert!(!rig.state.dirty, "the resize wakeup repaints synchronously");
    assert_eq!(rig.state.prev_width, Some(57));
    assert!(
        rig.next_request()
            .expect("fresh pane request")
            .is_fresh_panes()
    );
}

#[test]
fn empty_close_suppresses_widened_paint_until_exit() {
    let mut rig = Rig::new();
    rig.state.current = agent_snapshot(&rig.ws);
    rig.state.self_close.seen_sibling = true;
    rig.state.paint_hold.engage(Instant::now(), 100);

    let mut empty = agent_snapshot_observed(&rig.ws, 200);
    empty.own_view = Some(empty_own_view());
    rig.fold(empty, SnapshotSource::Produced);

    assert!(
        rig.state.should_exit,
        "seen-sibling zero exits on the producer-verified empty fold"
    );
    assert!(
        !rig.state.self_close.confirming_empty(),
        "seen-sibling empty tabs skip the confirm window"
    );

    rig.paint(true);

    assert!(
        rig.state.dirty,
        "the closing fold suppresses full-width paint instead of clearing dirty"
    );
}

#[test]
fn resize_reprobe_adopts_probed_pet_render_caps() {
    let enabled = PixelRenderCaps {
        pixel_transport: true,
        kitty_clients: true,
    };

    for (label, initial, probed) in [
        (
            "upgrade from the default",
            PixelRenderCaps::default(),
            enabled,
        ),
        (
            "downgrade from enabled",
            enabled,
            PixelRenderCaps::default(),
        ),
    ] {
        let mut rig = Rig::new();
        rig.state.paint.set_caps(initial);
        let mut observed = None;

        rig.state.refresh_pet_render_caps_with(
            crate::MuxName::Tmux,
            "rimz-test",
            &rig.terminal,
            |mux, session, _| {
                observed = Some((mux, session.to_owned()));
                probed
            },
        );

        assert_eq!(
            observed,
            Some((crate::MuxName::Tmux, "rimz-test".to_owned())),
            "the probe receives the live mux and session: {label}"
        );
        assert_eq!(rig.state.paint.caps(), probed, "{label}");
    }
}

#[test]
fn stale_tmux_caps_reprobe_is_bounded_and_adopts_changes() {
    let mut rig = Rig::new();
    let enabled = PixelRenderCaps {
        pixel_transport: true,
        kitty_clients: true,
    };
    let stale = std::time::Instant::now() + std::time::Duration::from_secs(11);

    assert!(rig.state.refresh_pet_render_caps_if_stale_with(
        crate::MuxName::Tmux,
        "rimz-test",
        stale,
        &rig.terminal,
        |_, _, _| enabled,
    ));
    assert_eq!(rig.state.paint.caps(), enabled);
    assert!(!rig.state.refresh_pet_render_caps_if_stale_with(
        crate::MuxName::Tmux,
        "rimz-test",
        stale,
        &rig.terminal,
        |_, _, _| panic!("fresh caps must not re-probe"),
    ));
    assert!(!rig.state.refresh_pet_render_caps_if_stale_with(
        crate::MuxName::Zellij,
        "rimz-test",
        stale + std::time::Duration::from_secs(11),
        &rig.terminal,
        |_, _, _| panic!("Zellij caps must not re-probe"),
    ));
}

#[test]
fn resize_caps_probe_waits_for_quiet_and_dirties_the_changed_frame() {
    // Every instant handed to the refresh is taken from the deadline the resize
    // armed, never from the clock: the settle window is already running while
    // `on_resize` paints. The clock read before each resize only bounds that
    // deadline from below, which is what holds the window's length.
    assert_ne!(RESIZE_CAPS_SETTLE, Duration::ZERO);
    let mut rig = Rig::new();
    let armed = |rig: &Rig| {
        rig.state
            .caps_refresh_deadline
            .expect("a resize arms the caps deadline")
    };
    for _ in 0..3 {
        let before = Instant::now();
        rig.state
            .on_resize(&mut rig.fetch, &mut rig.terminal, Some(40))
            .unwrap();
        let deadline = armed(&rig);
        assert!(
            deadline >= before + RESIZE_CAPS_SETTLE,
            "a resize arms a full settle window"
        );
        assert!(!rig.state.refresh_pet_render_caps_if_stale_with(
            crate::MuxName::Tmux,
            "rimz-test",
            deadline - Duration::from_nanos(1),
            &rig.terminal,
            |_, _, _| panic!("resize burst must not probe yet"),
        ));
    }
    rig.state.dirty = false;
    let enabled = PixelRenderCaps {
        pixel_transport: true,
        kitty_clients: true,
    };
    let settled = armed(&rig);
    assert!(rig.state.refresh_pet_render_caps_if_stale_with(
        crate::MuxName::Tmux,
        "rimz-test",
        settled,
        &rig.terminal,
        |_, _, _| enabled,
    ));
    assert!(rig.state.dirty);
    assert!(!rig.state.refresh_pet_render_caps_if_stale_with(
        crate::MuxName::Tmux,
        "rimz-test",
        settled,
        &rig.terminal,
        |_, _, _| panic!("settled burst must probe only once"),
    ));
}

#[test]
fn settled_resize_probe_dirties_the_frame_when_only_the_cell_aspect_changed() {
    let mut rig = Rig::new();
    let probed = rig.terminal.backend().cell_aspect();
    let stale = match probed {
        Some(_) => None,
        None => crate::config::CellAspect::from_ratio(2.0),
    };
    rig.state.paint.set_probed_aspect(stale);
    rig.state
        .on_resize(&mut rig.fetch, &mut rig.terminal, Some(40))
        .unwrap();
    rig.state.dirty = false;
    let caps = rig.state.paint.caps();

    assert!(rig.state.refresh_pet_render_caps_if_stale_with(
        crate::MuxName::Tmux,
        "rimz-test",
        Instant::now() + Duration::from_millis(510),
        &rig.terminal,
        |_, _, _| caps,
    ));
    assert!(rig.state.dirty);
}

#[test]
fn zellij_capability_probe_does_not_enable_unimplemented_pixel_rendering() {
    let caps = crate::sidebar_pane::pixel::probe::RoomCaps::default().detect(
        crate::MuxName::Zellij,
        "rimz-test",
        PixelRenderCaps::default(),
        None,
    );
    assert_eq!(caps, PixelRenderCaps::default());

    for glyphs in [
        crate::config::PetsGlyphMode::Auto,
        crate::config::PetsGlyphMode::Pixel,
        crate::config::PetsGlyphMode::Sextant,
    ] {
        assert_eq!(
            crate::sidebar_pane::pets::effective_render_tier(
                glyphs,
                crate::config::PixelMode::Auto,
                caps,
                true,
            ),
            crate::sidebar_pane::pets::PetRenderTier::Cell,
            "{glyphs:?} must stay on the implemented Zellij placement path"
        );
    }

    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let mut rig = Rig::with_own_pane(own_pane);
    rig.state.ui.meter_pixels = Some(crate::sidebar_pane::pixel::meter::MeterPixels::new(1));
    let snapshot = rig.state.current.clone();
    rig.state
        .paint
        .refresh_view(&mut rig.state.ui, &snapshot, false);

    assert!(rig.state.ui.meter_pixels.is_none());
}
