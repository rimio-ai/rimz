use super::*;
use crate::ids::{SidebarInstanceId, WorkspaceId};
use crate::sidebar::events::{SidebarEvent, SidebarEventEnvelope};
use std::os::unix::net::UnixDatagram;

fn target(cols: u16) -> NonZeroU16 {
    NonZeroU16::new(cols).expect("nonzero target")
}

fn controller(mux: MuxName) -> (tempfile::TempDir, RuntimePaths, WidthController) {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).expect("runtime");
    runtime.ensure_dirs().expect("runtime dirs");
    let pane = match mux {
        MuxName::Tmux => PaneId::from_parts(mux, "%1"),
        MuxName::Zellij => PaneId::from_parts(mux, "terminal_1"),
    };
    let controller = WidthController::new(
        runtime.clone(),
        "rimz-test".to_owned(),
        Some(pane),
        mux,
        crate::mux::SidebarWidth::default(),
    );
    (dir, runtime, controller)
}

fn write_zellij_topology(runtime: &RuntimePaths) {
    write_zellij_topology_for_view(runtime, 200);
}

fn write_zellij_topology_for_view(runtime: &RuntimePaths, view_cols: u16) {
    use crate::mux::zellij::pane_topology::{PaneTopologyCache, PaneTopologyPane};

    let pane = |id, pane_x, pane_columns, title: &str| PaneTopologyPane {
        id,
        is_plugin: false,
        is_held: false,
        exited: false,
        is_suppressed: false,
        is_floating: false,
        tab_position: 0,
        tab_name: None,
        pane_columns: Some(pane_columns),
        pane_x: Some(pane_x),
        title: Some(title.to_owned()),
        pane_command: None,
        pane_cwd: None,
        pane_pid: None,
        terminal_command: None,
    };
    crate::sidebar::cache::write_pane_topology_cache(
        runtime,
        &PaneTopologyCache {
            session_name: "rimz-test".to_owned(),
            produced_at_ms: crate::sidebar::timing::unix_now_ms(),
            writer: None,
            focused_pane: None,
            clients: None,
            panes: vec![
                pane(1, 0, 80, "rimz-sidebar"),
                pane(2, 80, u64::from(view_cols.saturating_sub(80)), "work"),
            ],
        },
    )
    .expect("write pane topology");
}

#[test]
fn one_target_converges_from_both_directions_and_none_stays_idle() {
    let now = Instant::now();
    let mut control = WidthControl::new(Some(target(72)));
    assert_eq!(control.decide(80, now), Some((80, 72)));

    let mut control = WidthControl::new(Some(target(72)));
    assert_eq!(control.decide(60, now), Some((60, 72)));

    let mut control = WidthControl::new(None);
    assert_eq!(control.decide(60, now), None);
}

#[test]
fn width_target_pin_broadcasts_without_a_producer_fetch() {
    let (dir, runtime, _controller) = controller(MuxName::Tmux);
    let instance = SidebarInstanceId::new();
    let socket_path = runtime.sock_dir.join("width-target-test.sock");
    let socket = UnixDatagram::bind(&socket_path).expect("bind wakeup socket");
    socket
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("set socket timeout");
    crate::sidebar::write_heartbeat(
        &runtime,
        runtime.workspace_id.clone(),
        &instance,
        MuxName::Tmux,
        "rimz-test",
        &socket_path,
        None,
    )
    .expect("write heartbeat");

    let permille = crate::sidebar::width_target::pin(&runtime, target(82), MuxName::Tmux, 200)
        .expect("pin width target");
    assert_eq!(crate::sidebar::width_target::load(&runtime), Some(permille));
    let mut payload = [0_u8; 1024];
    let received = socket.recv(&mut payload).expect("receive target broadcast");
    let envelope: SidebarEventEnvelope =
        serde_json::from_slice(&payload[..received]).expect("decode target broadcast");
    assert_eq!(envelope.event, SidebarEvent::WidthTargetChanged);
    drop(dir);
}

#[test]
fn zellij_uses_live_step_and_rejects_floor_crossing() {
    let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
    write_zellij_topology(&runtime);
    let diag = crate::diag::DiagSink::disabled();

    controller.adjust(80, WidthAdjust::Wider, &diag);
    assert_eq!(
        crate::sidebar::width_target::pinned(&runtime),
        Some(crate::mux::WidthPermille::from_percent(45)),
    );
    controller.adjust(80, WidthAdjust::Wider, &diag);
    assert_eq!(
        crate::sidebar::width_target::pinned(&runtime),
        Some(crate::mux::WidthPermille::from_percent(50)),
        "repeated keys compound on persisted pending intent",
    );
    let prior = NonZeroU16::new(30).expect("prior target");
    let prior_share = crate::sidebar::width_target::pin(&runtime, prior, MuxName::Zellij, 200)
        .expect("pin prior target");
    controller.reload_target(&crate::config::ThemeConfig::default(), None, &diag);
    controller.adjust(30, WidthAdjust::Narrower, &diag);
    assert_eq!(
        crate::sidebar::width_target::load(&runtime),
        Some(prior_share),
    );
}

#[test]
fn zellij_intent_without_topology_never_pins_a_phantom_share() {
    let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
    let diag = crate::diag::DiagSink::disabled();

    controller.adjust(80, WidthAdjust::Narrower, &diag);
    assert_eq!(crate::sidebar::width_target::pinned(&runtime), None);
    controller.adjust(80, WidthAdjust::Wider, &diag);
    assert_eq!(crate::sidebar::width_target::pinned(&runtime), None);
}

#[test]
fn observation_without_an_owned_pane_stays_idle() {
    let (_dir, runtime, _) = controller(MuxName::Tmux);
    let mut controller = WidthController::new(
        runtime,
        "rimz-test".to_owned(),
        None,
        MuxName::Tmux,
        crate::mux::SidebarWidth::default(),
    );

    controller.observe(
        80,
        SidebarWidthControlTrigger::ResizeFeedback,
        &crate::diag::DiagSink::disabled(),
    );

    assert_eq!(controller.feedback_deadline(), None);
}

#[test]
fn first_backend_geometry_resolves_the_initial_target() {
    let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
    write_zellij_topology(&runtime);
    let diag = crate::diag::DiagSink::disabled();

    assert_eq!(controller.convergence.target(), None);
    controller.backstop(Some(80), Some(1), None, &diag);

    assert_eq!(controller.convergence.target(), Some(target(50)));
    assert_eq!(controller.last_classified, Some((200, 1)));
}

#[test]
fn missing_backend_geometry_retries_the_baseline_at_most_once_per_second() {
    let (_dir, _runtime, mut controller) = controller(MuxName::Zellij);
    let diag = crate::diag::DiagSink::disabled();

    controller.backstop(Some(80), Some(1), None, &diag);
    let retry = controller
        .baseline_probe_deadline
        .expect("failed baseline re-arms the probe");
    assert!(retry > Instant::now());

    controller.backstop(Some(80), Some(1), None, &diag);
    assert_eq!(
        controller.baseline_probe_deadline,
        Some(retry),
        "an immediate render iteration does not probe again",
    );
    assert_eq!(controller.last_classified, None);
}

#[test]
fn legitimate_paint_width_tracks_the_target_or_falls_back_to_the_cap() {
    let (_dir, _runtime, mut controller) = controller(MuxName::Zellij);

    assert_eq!(controller.max_legit_cols(), controller.width.max_cols.get(),);
    controller.convergence.retarget(Some(target(50)));
    assert_eq!(controller.max_legit_cols(), 50);
    controller.convergence.retarget(Some(target(90)));
    assert_eq!(controller.max_legit_cols(), 90);
}

#[test]
fn settled_drag_pins_once_after_the_debounce() {
    let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
    write_zellij_topology(&runtime);
    controller.last_classified = Some((200, 1));
    let diag = crate::diag::DiagSink::disabled();
    controller.reload_target(&crate::config::ThemeConfig::default(), None, &diag);

    controller.observe(83, SidebarWidthControlTrigger::ResizeFeedback, &diag);
    controller.classification_deadline = Some(Instant::now());
    controller.backstop(Some(83), Some(1), Some(u64::MAX), &diag);

    assert_eq!(
        crate::sidebar::width_target::pinned(&runtime),
        Some(crate::mux::WidthPermille::from_percent(40)),
    );
    assert_eq!(controller.classification_deadline, None);
    assert!(
        !controller.convergence.in_flight(),
        "adopting a drag inside the seeded band must not nudge it",
    );
    controller.backstop(Some(83), Some(1), Some(u64::MAX), &diag);
    assert_eq!(
        crate::sidebar::width_target::pinned(&runtime),
        Some(crate::mux::WidthPermille::from_percent(40)),
    );
    assert!(
        !controller.convergence.in_flight(),
        "the next backstop must leave the adopted width parked",
    );
}

#[test]
fn broadcast_reload_uses_the_seeded_native_band() {
    let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
    write_zellij_topology(&runtime);
    let diag = crate::diag::DiagSink::disabled();

    controller.backstop(Some(50), Some(1), None, &diag);
    crate::sidebar::width_target::pin(&runtime, target(83), MuxName::Zellij, 200)
        .expect("pin external target");
    controller.reload_target(&crate::config::ThemeConfig::default(), Some(83), &diag);

    assert_eq!(controller.convergence.target(), Some(target(80)));
    assert_eq!(controller.convergence.stop_step(), 10);
    assert!(!controller.convergence.in_flight());
}

#[test]
fn drag_inside_the_native_band_never_arms_classification() {
    let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
    write_zellij_topology(&runtime);
    let diag = crate::diag::DiagSink::disabled();

    controller.backstop(Some(50), Some(1), None, &diag);
    controller.observe(54, SidebarWidthControlTrigger::ResizeFeedback, &diag);

    assert_eq!(controller.classification_deadline, None);
    assert_eq!(controller.classification_resize_at_ms, None);
    assert_eq!(crate::sidebar::width_target::pinned(&runtime), None);
}

#[test]
fn settled_structural_resize_converges_without_adopting() {
    let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
    write_zellij_topology(&runtime);
    controller.last_classified = Some((200, 1));
    let diag = crate::diag::DiagSink::disabled();
    controller.reload_target(&crate::config::ThemeConfig::default(), None, &diag);

    controller.observe(83, SidebarWidthControlTrigger::ResizeFeedback, &diag);
    controller.classification_deadline = Some(Instant::now());
    controller.backstop(Some(83), Some(2), Some(u64::MAX), &diag);

    assert_eq!(crate::sidebar::width_target::pinned(&runtime), None);
    assert_eq!(controller.convergence.target(), Some(target(50)));
}

#[test]
fn settled_view_resize_reresolves_an_unpinned_target() {
    let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
    write_zellij_topology_for_view(&runtime, 240);
    controller.last_classified = Some((200, 1));
    let diag = crate::diag::DiagSink::disabled();
    controller.reload_target(&crate::config::ThemeConfig::default(), None, &diag);

    controller.observe(80, SidebarWidthControlTrigger::ResizeFeedback, &diag);
    controller.classification_deadline = Some(Instant::now());
    controller.backstop(Some(80), Some(1), Some(u64::MAX), &diag);

    assert_eq!(crate::sidebar::width_target::pinned(&runtime), None);
    assert_eq!(controller.convergence.target(), Some(target(60)));
}

#[test]
fn settled_view_resize_scales_a_pinned_target() {
    let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
    write_zellij_topology_for_view(&runtime, 240);
    controller.last_classified = Some((200, 1));
    let share = crate::sidebar::width_target::pin(&runtime, target(80), MuxName::Zellij, 200)
        .expect("pin width target");
    let diag = crate::diag::DiagSink::disabled();
    controller.reload_target(&crate::config::ThemeConfig::default(), None, &diag);

    controller.observe(96, SidebarWidthControlTrigger::ResizeFeedback, &diag);
    controller.classification_deadline = Some(Instant::now());
    controller.backstop(Some(96), Some(1), Some(u64::MAX), &diag);

    assert_eq!(crate::sidebar::width_target::pinned(&runtime), Some(share));
    assert_eq!(controller.convergence.target(), Some(target(96)));
}

#[test]
fn settled_resize_without_geometry_never_adopts() {
    let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
    controller.last_classified = Some((200, 1));
    let diag = crate::diag::DiagSink::disabled();
    controller.reload_target(&crate::config::ThemeConfig::default(), None, &diag);

    controller.observe(83, SidebarWidthControlTrigger::ResizeFeedback, &diag);
    controller.classification_deadline = Some(Instant::now());
    controller.backstop(Some(83), Some(1), Some(u64::MAX), &diag);

    assert_eq!(crate::sidebar::width_target::pinned(&runtime), None);
    assert!(controller.classification_deadline.is_some());
}

#[test]
fn stale_pane_observation_converges_and_rearms_without_adopting() {
    let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
    write_zellij_topology(&runtime);
    controller.last_classified = Some((200, 1));
    let diag = crate::diag::DiagSink::disabled();
    controller.reload_target(&crate::config::ThemeConfig::default(), None, &diag);

    controller.observe(83, SidebarWidthControlTrigger::ResizeFeedback, &diag);
    let resize_at_ms = controller
        .classification_resize_at_ms
        .expect("resize starts classification");
    controller.classification_deadline = Some(Instant::now());
    controller.backstop(Some(83), Some(1), Some(resize_at_ms), &diag);

    assert_eq!(crate::sidebar::width_target::pinned(&runtime), None);
    assert!(controller.classification_deadline.is_some());
    assert_eq!(controller.classification_resize_at_ms, Some(resize_at_ms));
}

#[test]
fn observed_step_sets_the_upward_reachable_band() {
    let now = Instant::now();
    let mut control = WidthControl::new(Some(target(72)));
    assert_eq!(control.decide(50, now), Some((50, 72)));
    assert_eq!(
        control.decide(60, now + Duration::from_millis(10)),
        Some((60, 72))
    );
    assert_eq!(
        control.decide(68, now + Duration::from_millis(20)),
        Some((68, 72))
    );
    assert_eq!(control.decide(76, now + Duration::from_millis(30)), None);
}

#[test]
fn seeded_native_step_parks_between_the_target_and_next_step() {
    let now = Instant::now();
    let mut control = WidthControl::new(Some(target(80)));
    control.seed_native_step(10);

    assert_eq!(control.stop_step(), 10);
    assert_eq!(control.decide(83, now), None);

    let mut below = WidthControl::new(Some(target(80)));
    below.seed_native_step(10);
    assert_eq!(below.decide(79, now), Some((79, 80)));

    let mut next_step = WidthControl::new(Some(target(80)));
    next_step.seed_native_step(10);
    assert_eq!(next_step.decide(90, now), Some((90, 80)));
}

#[test]
fn native_step_seed_survives_retargeting() {
    let now = Instant::now();
    let mut control = WidthControl::new(Some(target(80)));
    control.seed_native_step(10);

    control.retarget(Some(target(90)));

    assert_eq!(control.stop_step(), 10);
    assert_eq!(control.decide(94, now), None);
}

#[test]
fn learned_step_refines_the_seeded_band() {
    let now = Instant::now();
    let mut control = WidthControl::new(Some(target(80)));
    control.seed_native_step(10);
    assert_eq!(control.decide(60, now), Some((60, 80)));

    assert_eq!(
        control.decide(66, now + Duration::from_millis(10)),
        Some((66, 80)),
    );
    assert_eq!(control.stop_step(), 6);
}

#[test]
fn step_estimate_uses_an_upward_band() {
    let now = Instant::now();
    let mut inside = WidthControl::new(Some(target(80)));
    inside.seed_native_step(2);
    assert_eq!(inside.decide(81, now), None);

    let mut below = WidthControl::new(Some(target(80)));
    below.seed_native_step(2);
    assert_eq!(below.decide(79, now), Some((79, 80)));

    let mut next_step = WidthControl::new(Some(target(80)));
    next_step.seed_native_step(2);
    assert_eq!(next_step.decide(82, now), Some((82, 80)));
}

#[test]
fn upward_crossing_stops_inside_the_reachable_band() {
    let now = Instant::now();
    let mut control = WidthControl::new(Some(target(72)));
    assert_eq!(control.decide(68, now), Some((68, 72)));
    assert_eq!(control.decide(76, now + Duration::from_millis(10)), None);
    assert_eq!(control.decide(76, now + FEEDBACK_TIMEOUT * 2), None);
}

#[test]
fn upward_crossing_inside_the_band_parks_without_a_reverse() {
    let now = Instant::now();
    let mut control = WidthControl::new(Some(target(80)));
    assert_eq!(control.decide(60, now), Some((60, 80)));

    assert_eq!(control.decide(85, now + Duration::from_millis(10)), None);
    assert!(!control.reverse_issued);
    assert_eq!(
        control.take_trace(),
        Some(WidthTransition::StepIssued {
            from: 60,
            target: 80,
        }),
    );
    assert_eq!(
        control.take_trace(),
        Some(WidthTransition::FeedbackLearned {
            settled: 85,
            learned_step: 25,
        }),
    );
    assert_eq!(
        control.take_trace(),
        Some(WidthTransition::Idle {
            at: 85,
            reason: WidthIdleReason::ReachedTolerance,
        }),
    );
}

#[test]
fn downward_undershoot_reverses_once_then_parks() {
    let now = Instant::now();
    let mut control = WidthControl::new(Some(target(80)));
    assert_eq!(control.decide(83, now), Some((83, 80)));
    assert_eq!(
        control.decide(70, now + Duration::from_millis(10)),
        Some((70, 80)),
    );
    assert!(control.reverse_issued);

    assert_eq!(control.decide(83, now + Duration::from_millis(20)), None);
    assert_eq!(control.decide(83, now + FEEDBACK_TIMEOUT * 2), None);
    assert_eq!(
        control.take_trace(),
        Some(WidthTransition::StepIssued {
            from: 83,
            target: 80,
        }),
    );
    assert_eq!(
        control.take_trace(),
        Some(WidthTransition::FeedbackLearned {
            settled: 70,
            learned_step: 13,
        }),
    );
    assert_eq!(
        control.take_trace(),
        Some(WidthTransition::StepIssued {
            from: 70,
            target: 80,
        }),
    );
    assert_eq!(
        control.take_trace(),
        Some(WidthTransition::FeedbackLearned {
            settled: 83,
            learned_step: 13,
        }),
    );
    assert_eq!(
        control.take_trace(),
        Some(WidthTransition::Idle {
            at: 83,
            reason: WidthIdleReason::ReverseParked,
        }),
    );
}

#[test]
fn reverse_step_parks_after_overshooting_the_target() {
    let now = Instant::now();
    let mut control = WidthControl::new(Some(target(80)));
    assert_eq!(control.decide(83, now), Some((83, 80)));
    assert_eq!(
        control.decide(70, now + Duration::from_millis(10)),
        Some((70, 80)),
    );

    assert_eq!(control.decide(95, now + Duration::from_millis(20)), None);
    assert_eq!(control.stop_step(), 25);
    assert_eq!(
        control.traces.back(),
        Some(&WidthTransition::Idle {
            at: 95,
            reason: WidthIdleReason::ReverseParked,
        }),
    );
}

#[test]
fn reverse_step_that_stays_below_target_keeps_converging() {
    let now = Instant::now();
    let mut control = WidthControl::new(Some(target(80)));
    assert_eq!(control.decide(83, now), Some((83, 80)));
    assert_eq!(
        control.decide(70, now + Duration::from_millis(10)),
        Some((70, 80)),
    );
    assert!(control.reverse_issued);

    assert_eq!(
        control.decide(75, now + Duration::from_millis(20)),
        Some((75, 80)),
    );
    assert!(!control.reverse_issued);
    assert_eq!(
        control.traces.back(),
        Some(&WidthTransition::StepIssued {
            from: 75,
            target: 80,
        }),
    );
}

#[test]
fn unchanged_measurement_retries_once_then_stops() {
    let now = Instant::now();
    let mut control = WidthControl::new(Some(target(72)));
    assert_eq!(control.decide(50, now), Some((50, 72)));
    assert_eq!(control.decide(50, now + FEEDBACK_TIMEOUT / 2), None);
    assert_eq!(control.decide(50, now + FEEDBACK_TIMEOUT), Some((50, 72)));
    assert_eq!(control.decide(50, now + FEEDBACK_TIMEOUT * 2), None);
    assert_eq!(control.decide(50, now + FEEDBACK_TIMEOUT * 3), None);
}

#[test]
fn one_step_stays_in_flight_until_feedback() {
    let now = Instant::now();
    let mut control = WidthControl::new(Some(target(72)));
    assert_eq!(control.decide(50, now), Some((50, 72)));
    assert_eq!(control.decide(50, now + Duration::from_millis(999)), None);
    assert_eq!(control.feedback_deadline(), Some(now + FEEDBACK_TIMEOUT));
}

#[test]
fn retarget_resets_progress_guards() {
    let now = Instant::now();
    let mut control = WidthControl::new(Some(target(72)));
    assert_eq!(control.decide(50, now), Some((50, 72)));
    assert_eq!(control.decide(80, now + Duration::from_millis(10)), None);
    control.retarget(Some(target(60)));
    assert_eq!(
        control.decide(50, now + Duration::from_millis(20)),
        Some((50, 60))
    );
}

#[test]
fn retarget_keeps_an_issued_step_in_flight() {
    let now = Instant::now();
    let mut control = WidthControl::new(Some(target(72)));
    assert_eq!(control.decide(50, now), Some((50, 72)));
    control.retarget(Some(target(60)));
    assert_eq!(control.decide(50, now + Duration::from_millis(10)), None);
}

#[test]
fn unchanged_retarget_preserves_progress() {
    let now = Instant::now();
    let mut control = WidthControl::new(Some(target(72)));
    assert_eq!(control.decide(50, now), Some((50, 72)));
    control.retarget(Some(target(72)));
    assert_eq!(control.decide(50, now + Duration::from_millis(10)), None);
    assert_eq!(control.steps_issued, 1);
}

#[test]
fn transitions_cover_issue_feedback_and_idle_outcomes() {
    let now = Instant::now();
    let mut control = WidthControl::new(Some(target(72)));
    assert_eq!(control.decide(50, now), Some((50, 72)));
    assert_eq!(
        control.take_trace(),
        Some(WidthTransition::StepIssued {
            from: 50,
            target: 72,
        })
    );

    assert_eq!(
        control.decide(60, now + Duration::from_millis(10)),
        Some((60, 72))
    );
    assert_eq!(
        control.take_trace(),
        Some(WidthTransition::FeedbackLearned {
            settled: 60,
            learned_step: 10,
        })
    );
    assert_eq!(
        control.take_trace(),
        Some(WidthTransition::StepIssued {
            from: 60,
            target: 72,
        })
    );

    assert_eq!(
        control.decide(68, now + Duration::from_millis(20)),
        Some((68, 72))
    );
    assert_eq!(
        control.take_trace(),
        Some(WidthTransition::FeedbackLearned {
            settled: 68,
            learned_step: 8,
        })
    );
    assert_eq!(
        control.take_trace(),
        Some(WidthTransition::StepIssued {
            from: 68,
            target: 72,
        })
    );

    assert_eq!(control.decide(76, now + Duration::from_millis(30)), None);
    assert_eq!(
        control.take_trace(),
        Some(WidthTransition::FeedbackLearned {
            settled: 76,
            learned_step: 8,
        })
    );
    assert_eq!(
        control.take_trace(),
        Some(WidthTransition::Idle {
            at: 76,
            reason: WidthIdleReason::ReachedTolerance,
        })
    );
}

#[test]
fn step_budget_bounds_continuous_progress() {
    let now = Instant::now();
    let mut control = WidthControl::new(Some(target(200)));
    assert_eq!(control.decide(10, now), Some((10, 200)));
    for step in 1..MAX_STEPS {
        let width = 10 + u16::from(step);
        assert_eq!(
            control.decide(width, now + Duration::from_millis(u64::from(step))),
            Some((width, 200))
        );
    }
    assert_eq!(
        control.decide(10 + u16::from(MAX_STEPS), now + Duration::from_secs(1)),
        None
    );
}
