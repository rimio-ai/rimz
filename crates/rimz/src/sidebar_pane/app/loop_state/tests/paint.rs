//! What gets to paint: the glanceable content key that wakes an off-screen
//! pane, and the resize hold that keeps a grow from painting torn width.

use super::*;

/// One hidden-paint verdict: how the off-screen content changed, how long ago
/// the last background paint was, and whether that change earns a paint.
struct HiddenPaintCase {
    label: &'static str,
    prior: SidebarSnapshot,
    current: SidebarSnapshot,
    /// Age of the last background paint, or `None` for never painted.
    last_paint_age: Option<Duration>,
    paints: bool,
    /// Whether the paint went through the background path, which stamps
    /// `last_bg_paint`. The detached row paints through the foreground path.
    stamps_background: bool,
}

#[test]
fn hidden_paint_follows_the_glanceable_content_key() {
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let ws = workspace();
    let agent = |status| hidden_attached_agent_snapshot(&ws, status);
    let process = |state| hidden_attached_process_snapshot(&ws, state);
    let phased = |phase| {
        let mut snapshot = agent(crate::agents::AgentStatus::Running);
        set_agent_phase(&mut snapshot, phase);
        snapshot
    };
    let mut detached = agent(crate::agents::AgentStatus::Waiting);
    detached.presence = Some(crate::store::snapshot::SidebarPresence::Detached);

    let cases = [
        HiddenPaintCase {
            label: "idle to running is a status change the off-screen tab must show",
            prior: agent(crate::agents::AgentStatus::Idle),
            current: agent(crate::agents::AgentStatus::Running),
            last_paint_age: None,
            paints: true,
            stamps_background: true,
        },
        HiddenPaintCase {
            label: "phase-only background change stays pending",
            prior: phased(crate::agents::TurnPhase::Reasoning),
            current: phased(crate::agents::TurnPhase::Acting),
            last_paint_age: Some(crate::sidebar::timing::BACKGROUND_PAINT_MIN_INTERVAL),
            paints: false,
            stamps_background: false,
        },
        HiddenPaintCase {
            label: "throttled background change stays pending",
            prior: agent(crate::agents::AgentStatus::Idle),
            current: agent(crate::agents::AgentStatus::Waiting),
            last_paint_age: Some(Duration::ZERO),
            paints: false,
            stamps_background: false,
        },
        HiddenPaintCase {
            label: "idle-to-stuck process state paints",
            prior: process(crate::store::snapshot::ProcessState::Idle),
            current: process(crate::store::snapshot::ProcessState::Stuck),
            last_paint_age: None,
            paints: true,
            stamps_background: true,
        },
        HiddenPaintCase {
            label: "detached dirty paints through the foreground path, not the background one",
            prior: detached.clone(),
            current: detached,
            last_paint_age: None,
            paints: true,
            stamps_background: false,
        },
    ];

    for case in cases {
        let mut rig = Rig::with_own_pane(own_pane.clone());
        rig.state.current = case.current;
        rig.state.last_bg_key = Some(background_content_key(&case.prior));
        rig.state.last_bg_paint = case.last_paint_age.map(|age| Instant::now() - age);
        rig.state.dirty = true;
        let stamp_before = rig.state.last_bg_paint;
        let label = case.label;

        rig.paint(false);

        assert_eq!(!rig.state.dirty, case.paints, "{label}");
        if case.stamps_background {
            assert!(rig.state.last_bg_paint.is_some(), "{label}");
            assert_eq!(
                rig.state.last_bg_key.as_ref(),
                Some(&background_content_key(&rig.state.current)),
                "{label}"
            );
        } else if case.paints {
            assert_eq!(rig.state.last_bg_paint, None, "{label}");
        } else {
            assert_eq!(rig.state.last_bg_paint, stamp_before, "{label}");
        }
    }
}

#[test]
fn resize_hold_releases_only_on_a_post_engage_pane_stamp() {
    // The hold engages at pane stamp 100; only a pull observed after that
    // proves the resize verdict landed.
    for (observed_at_ms, releases) in [(101, true), (99, false)] {
        let mut rig = Rig::new();
        rig.state.current = agent_snapshot(&rig.ws);
        rig.state.paint_hold.engage(Instant::now(), 100);

        let snapshot = agent_snapshot_observed(&rig.ws, observed_at_ms);
        rig.fold(snapshot, PaneFrame::Held, SnapshotSource::Published);

        assert_eq!(
            !rig.state.paint_hold.is_engaged(),
            releases,
            "pane stamp {observed_at_ms} against an engage at 100"
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
    rig.fold(snapshot, PaneFrame::Held, SnapshotSource::Published);
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
    rig.fold(snapshot, PaneFrame::Held, SnapshotSource::Published);
    assert!(
        rig.state.paint_hold.is_engaged(),
        "the second rejected fold still stays held"
    );
    assert_eq!(rig.state.gate.reject_streak, 2);
    let now_ms = jiff::Timestamp::now().as_millisecond();
    rig.state.gate.rejecting_since =
        Some(jiff::Timestamp::from_millisecond(now_ms - 1_000).unwrap());

    let snapshot = process_snapshot(&rig.ws, 152);
    rig.fold(snapshot, PaneFrame::Held, SnapshotSource::Published);
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

    rig.state
        .on_resize(
            &rig.config,
            &mut rig.fetch,
            &mut rig.terminal,
            Some(57),
            Instant::now(),
            &crate::diag::DiagSink::disabled(),
        )
        .expect("handle attach resize");

    assert!(
        !rig.state.paint_hold.is_engaged(),
        "a grow within the legitimate cap paints immediately"
    );
    assert!(!rig.state.dirty, "the resize wakeup repaints synchronously");
    assert_eq!(rig.state.prev_width, Some(57));
    assert!(
        rig.next_request()
            .expect("fresh pane request")
            .is_producer_fresh_panes()
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
    rig.fold(empty, PaneFrame::Fresh, SnapshotSource::Produced);

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
        |_, _, _| enabled,
    ));
    assert_eq!(rig.state.paint.caps(), enabled);
    assert!(!rig.state.refresh_pet_render_caps_if_stale_with(
        crate::MuxName::Tmux,
        "rimz-test",
        stale,
        |_, _, _| panic!("fresh caps must not re-probe"),
    ));
    assert!(!rig.state.refresh_pet_render_caps_if_stale_with(
        crate::MuxName::Zellij,
        "rimz-test",
        stale + std::time::Duration::from_secs(11),
        |_, _, _| panic!("Zellij caps must not re-probe"),
    ));
}

#[test]
fn zellij_capability_probe_does_not_enable_unimplemented_pixel_rendering() {
    let caps =
        crate::sidebar_pane::app::initial_pet_render_caps(crate::MuxName::Zellij, "rimz-test");
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
