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

fn write_zellij_sidebar_only_topology(runtime: &RuntimePaths, sidebar_cols: u16) {
    write_zellij_topology_panes(
        runtime,
        sidebar_cols,
        None,
        crate::sidebar::timing::unix_now_ms(),
    );
}

fn write_zellij_topology_for_view(runtime: &RuntimePaths, view_cols: u16) {
    write_zellij_topology_for_view_at(runtime, view_cols, crate::sidebar::timing::unix_now_ms());
}

fn write_zellij_fullscreen_topology(runtime: &RuntimePaths, active: bool) -> u64 {
    let mut cache = match crate::sidebar::cache::read_pane_topology_cache(runtime, "rimz-test") {
        Some(cache) => cache,
        None => {
            write_zellij_topology(runtime);
            crate::sidebar::cache::read_pane_topology_cache(runtime, "rimz-test")
                .expect("topology cache")
        }
    };
    cache.produced_at_ms = cache
        .produced_at_ms
        .saturating_add(1)
        .max(crate::sidebar::timing::unix_now_ms());
    cache.panes[1].is_fullscreen = active;
    crate::sidebar::cache::write_pane_topology_cache(runtime, &cache)
        .expect("write fullscreen topology");
    cache.produced_at_ms
}

fn write_zellij_topology_for_view_at(runtime: &RuntimePaths, view_cols: u16, produced_at_ms: u64) {
    write_zellij_topology_panes(runtime, 80, Some(view_cols), produced_at_ms);
}

fn write_zellij_topology_panes(
    runtime: &RuntimePaths,
    sidebar_cols: u16,
    view_cols: Option<u16>,
    produced_at_ms: u64,
) {
    use crate::mux::zellij::pane_topology::{PaneTopologyCache, PaneTopologyPane};

    let pane = |id, pane_x, pane_columns, title: &str| PaneTopologyPane {
        id,
        is_plugin: false,
        is_fullscreen: false,
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
            produced_at_ms,
            writer: None,
            focused_pane: None,
            clients: None,
            panes: std::iter::once(pane(1, 0, u64::from(sidebar_cols), "rimz-sidebar"))
                .chain(view_cols.map(|view_cols| {
                    pane(
                        2,
                        u64::from(sidebar_cols),
                        u64::from(view_cols.saturating_sub(sidebar_cols)),
                        "work",
                    )
                }))
                .collect(),
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
fn fullscreen_hold_ignores_width_changes_until_topology_releases() {
    let now = Instant::now();
    let mut control = WidthControl::new(Some(target(72)));

    assert!(!control.observe_fullscreen(true, 200));
    assert!(control.is_fullscreen_held());
    assert!(!control.needs_adjustment(200));
    assert_eq!(control.decide(200, now), None);
    control.rearm();
    assert!(control.is_fullscreen_held());

    assert!(control.observe_fullscreen(false, 200));
    assert!(!control.is_fullscreen_held());
    assert_eq!(control.decide(200, now), Some((200, 72)));
}

#[test]
fn fullscreen_hold_never_arms_idle_retry_and_clear_resumes_convergence() {
    let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
    let diag = crate::diag::DiagSink::disabled();
    let held_at_ms = write_zellij_fullscreen_topology(&runtime, true);

    controller.backstop(Some(200), Some(1), Some(held_at_ms), &diag);
    assert!(controller.convergence.is_fullscreen_held());
    assert!(!controller.convergence.in_flight());
    assert_eq!(controller.idle_retry_deadline, None);

    controller.idle_retry_deadline = Some(Instant::now() - IDLE_RETRY);
    controller.backstop(Some(200), Some(1), Some(held_at_ms), &diag);
    assert_eq!(controller.idle_retry_deadline, None);
    assert!(!controller.convergence.in_flight());

    let released_at_ms = write_zellij_fullscreen_topology(&runtime, false);
    controller.backstop(Some(200), Some(1), Some(released_at_ms), &diag);
    assert!(!controller.convergence.is_fullscreen_held());
    assert!(controller.convergence.in_flight());
}

#[test]
fn fullscreen_hold_rejects_width_intent_without_pinning_the_room_share() {
    let (dir, runtime, mut controller) = controller(MuxName::Zellij);
    let diag = crate::diag::DiagSink::under(
        dir.path().to_path_buf(),
        runtime.workspace_id.clone(),
        "rimz-test",
        None,
    );
    let held_at_ms = write_zellij_fullscreen_topology(&runtime, true);

    controller.backstop(Some(200), Some(1), Some(held_at_ms), &diag);
    controller.adjust(200, WidthAdjust::Wider, &diag);

    assert!(controller.convergence.is_fullscreen_held());
    assert_eq!(crate::sidebar::width_target::pinned(&runtime), None);
    let text = std::fs::read_to_string(diag.log_path().expect("diagnostic path"))
        .expect("fullscreen rejection diagnostic");
    let event = serde_json::from_str::<crate::diag::record::DiagEnvelope>(
        text.lines().next().expect("diagnostic event"),
    )
    .expect("diagnostic envelope")
    .event;
    assert!(matches!(
        event,
        crate::diag::record::DiagEvent::SidebarWidthIntent {
            trigger: SidebarWidthIntentTrigger::Wider,
            base_cols: 200,
            verdict: SidebarWidthIntentVerdict::RejectedFullscreen,
            ..
        }
    ));
}

#[test]
fn fullscreen_hold_cancels_pending_classification_across_structural_change() {
    let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
    write_zellij_topology(&runtime);
    let diag = crate::diag::DiagSink::disabled();
    controller.last_siblings = Some(1);
    controller.reload_target(&crate::config::ThemeConfig::default(), None, &diag);
    controller.observe(83, SidebarWidthControlTrigger::ResizeFeedback, &diag);
    assert!(controller.classification_deadline.is_some());

    let held_at_ms = write_zellij_fullscreen_topology(&runtime, true);
    controller.classification_deadline = Some(Instant::now());
    controller.backstop(Some(200), Some(2), Some(held_at_ms), &diag);

    assert!(controller.convergence.is_fullscreen_held());
    assert_eq!(controller.classification_deadline, None);
    assert_eq!(controller.classification_resize_at_ms, None);
    assert_eq!(crate::sidebar::width_target::pinned(&runtime), None);
    assert!(!controller.convergence.in_flight());
}

#[test]
fn narrower_after_an_exact_pin_issues_a_resize() {
    let now = Instant::now();
    let step = crate::mux::WidthStep {
        cols: 10,
        stop_step_cols: 11,
        exact: false,
        view_cols: 213,
        fullscreen_active: None,
    };
    let narrowed = crate::mux::width::adjust_target_cols(
        64,
        WidthAdjust::Narrower,
        step,
        crate::mux::width::MIN_ADJUSTABLE_WIDTH,
    )
    .expect("narrower target");
    let mut control = WidthControl::new(Some(target(64)));
    control.seed_native_step(step.stop_step_cols);

    control.retarget(Some(narrowed));

    assert_eq!(narrowed, target(53));
    assert_eq!(control.decide(64, now), Some((64, 53)));
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

    let permille =
        crate::sidebar::width_target::pin(&runtime, target(82), 200).expect("pin width target");
    assert_eq!(
        crate::sidebar::width_target::pinned(&runtime),
        Some(permille)
    );
    let mut payload = [0_u8; 1024];
    let received = socket.recv(&mut payload).expect("receive target broadcast");
    let envelope: SidebarEventEnvelope =
        serde_json::from_slice(&payload[..received]).expect("decode target broadcast");
    assert_eq!(envelope.event, SidebarEvent::WidthTargetChanged);
    drop(dir);
}

#[test]
fn zellij_uses_live_step_and_clamps_floor_crossing() {
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
    let prior_share =
        crate::sidebar::width_target::pin(&runtime, prior, 200).expect("pin prior target");
    controller.reload_target(&crate::config::ThemeConfig::default(), None, &diag);
    controller.adjust(30, WidthAdjust::Narrower, &diag);
    assert_eq!(
        crate::sidebar::width_target::pinned(&runtime),
        Some(crate::mux::WidthPermille::from_percent(12)),
        "the inexact step pins the 24-column floor instead of being ignored",
    );
    assert_ne!(
        crate::sidebar::width_target::pinned(&runtime),
        Some(prior_share)
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
    assert!(!controller.note_structural(
        crate::sidebar::timing::unix_now_ms(),
        Some(80),
        &crate::diag::DiagSink::disabled(),
    ));

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
    assert_eq!(controller.current_view_cols, Some(200));
    assert_eq!(controller.last_siblings, Some(1));
}

#[test]
fn sidebar_only_startup_never_latches_the_floor_as_the_view_basis() {
    let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
    write_zellij_sidebar_only_topology(&runtime, 10);
    let diag = crate::diag::DiagSink::disabled();

    controller.backstop(Some(10), Some(0), None, &diag);
    assert_eq!(controller.convergence.target(), None);
    assert_eq!(controller.current_view_cols, None);

    // Model a target poisoned by the former single-pane inference, then prove
    // that a broadcast reload consults the completed tab instead of reusing it.
    controller.convergence.retarget(Some(target(24)));
    controller.current_view_cols = Some(10);
    write_zellij_topology_for_view(&runtime, 320);
    controller.reload_target(&crate::config::ThemeConfig::default(), Some(96), &diag);

    assert_eq!(controller.current_view_cols, Some(320));
    assert_eq!(controller.convergence.target(), Some(target(72)));
}

#[test]
fn zellij_geometry_seeds_the_ceiling_stop_band() {
    let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
    write_zellij_topology_for_view(&runtime, 213);
    let diag = crate::diag::DiagSink::disabled();

    controller.backstop(Some(64), Some(1), None, &diag);

    assert_eq!(controller.convergence.target(), Some(target(54)));
    assert_eq!(controller.convergence.stop_step(), 11);
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
    assert_eq!(controller.current_view_cols, None);
}

#[test]
fn attach_viewport_change_never_converges_against_the_pre_attach_snapshot() {
    let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
    let diag = crate::diag::DiagSink::disabled();
    write_zellij_topology_for_view_at(&runtime, 50, controller.started_at_ms);
    controller.backstop(Some(24), Some(1), None, &diag);
    assert_eq!(controller.convergence.target(), Some(target(24)));

    let structural_at_ms = controller.started_at_ms.saturating_add(10);
    write_zellij_topology_for_view_at(&runtime, 319, structural_at_ms.saturating_sub(1));
    controller.note_structural(structural_at_ms, Some(64), &diag);

    assert_eq!(controller.convergence.target(), Some(target(24)));
    assert!(!controller.convergence.in_flight());
    assert!(controller.baseline_probe_deadline.is_some());

    controller.baseline_probe_deadline = Some(Instant::now() - Duration::from_millis(1));
    controller.backstop(Some(64), Some(1), None, &diag);

    assert_eq!(
        controller.convergence.target(),
        Some(target(24)),
        "the retry must keep the structural event's stronger floor",
    );
    assert!(
        !controller.convergence.in_flight(),
        "the rejected pre-attach snapshot must not issue a delayed shrink",
    );

    write_zellij_topology_for_view_at(&runtime, 319, structural_at_ms);
    controller.note_structural(structural_at_ms, Some(32), &diag);

    assert_eq!(controller.current_view_cols, Some(319));
    assert_eq!(controller.convergence.target(), Some(target(72)));
    assert!(controller.convergence.in_flight(), "the correction grows");
}

#[test]
fn parked_controller_repicks_a_changed_viewport_on_idle_retry() {
    let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
    let diag = crate::diag::DiagSink::disabled();
    write_zellij_topology_for_view_at(&runtime, 50, controller.started_at_ms);
    controller.backstop(Some(24), Some(1), None, &diag);
    let adopted_share =
        crate::sidebar::width_target::resolve(&runtime, crate::mux::SidebarWidth::default(), None)
            .share;
    controller.convergence.learned_step = Some(16);
    controller.convergence.idle = Some(WidthIdle {
        at: 32,
        reason: WidthIdleReason::ReachedTolerance,
    });

    write_zellij_topology_for_view(&runtime, 319);
    controller.idle_retry_deadline = Some(Instant::now() - Duration::from_millis(1));
    controller.backstop(Some(32), Some(1), None, &diag);

    assert_eq!(controller.current_view_cols, Some(319));
    assert_eq!(controller.convergence.target(), Some(target(72)));
    assert!(controller.convergence.in_flight());
    assert_eq!(
        crate::sidebar::width_target::resolve(&runtime, crate::mux::SidebarWidth::default(), None,)
            .share,
        adopted_share,
        "a floorless recovery reads the share without rewriting it",
    );
}

#[test]
fn unproven_viewport_never_rewrites_the_room_share() {
    let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
    let diag = crate::diag::DiagSink::disabled();
    write_zellij_topology_for_view_at(&runtime, 50, controller.started_at_ms.saturating_sub(1));

    controller.backstop(Some(24), Some(1), None, &diag);

    assert_eq!(controller.convergence.target(), None);
    assert!(!runtime.sidebar_width_path().exists());
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
    controller.last_siblings = Some(1);
    let diag = crate::diag::DiagSink::disabled();
    controller.reload_target(&crate::config::ThemeConfig::default(), None, &diag);

    controller.observe(83, SidebarWidthControlTrigger::ResizeFeedback, &diag);
    controller.classification_deadline = Some(Instant::now());
    controller.backstop(Some(83), Some(1), Some(u64::MAX), &diag);

    assert_eq!(
        crate::sidebar::width_target::pinned(&runtime),
        Some(crate::mux::WidthPermille::from_cols(
            target(83),
            target(200)
        )),
    );
    assert_eq!(controller.classification_deadline, None);
    assert!(
        !controller.convergence.in_flight(),
        "adopting a drag inside the seeded band must not nudge it",
    );
    controller.backstop(Some(83), Some(1), Some(u64::MAX), &diag);
    assert_eq!(
        crate::sidebar::width_target::pinned(&runtime),
        Some(crate::mux::WidthPermille::from_cols(
            target(83),
            target(200)
        )),
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
    crate::sidebar::width_target::pin(&runtime, target(83), 200).expect("pin external target");
    controller.reload_target(&crate::config::ThemeConfig::default(), Some(83), &diag);

    assert_eq!(controller.convergence.target(), Some(target(83)));
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
    controller.last_siblings = Some(1);
    let diag = crate::diag::DiagSink::disabled();
    controller.reload_target(&crate::config::ThemeConfig::default(), None, &diag);

    controller.observe(83, SidebarWidthControlTrigger::ResizeFeedback, &diag);
    controller.classification_deadline = Some(Instant::now());
    let structural_at_ms = crate::sidebar::timing::unix_now_ms();
    write_zellij_topology_for_view_at(&runtime, 200, structural_at_ms);
    controller.backstop(Some(83), Some(2), Some(structural_at_ms), &diag);

    assert_eq!(crate::sidebar::width_target::pinned(&runtime), None);
    assert_eq!(controller.convergence.target(), Some(target(50)));
}

#[test]
fn sibling_change_backstop_converges_without_resize_feedback() {
    let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
    write_zellij_topology(&runtime);
    let diag = crate::diag::DiagSink::disabled();

    controller.backstop(Some(50), Some(3), None, &diag);
    let structural_at_ms = crate::sidebar::timing::unix_now_ms();
    write_zellij_topology_for_view_at(&runtime, 200, structural_at_ms);
    controller.backstop(Some(80), Some(2), Some(structural_at_ms), &diag);

    assert_eq!(controller.structural_at_ms, Some(structural_at_ms));
    assert_eq!(controller.convergence.target(), Some(target(50)));
    assert!(controller.convergence.in_flight());
    assert_eq!(crate::sidebar::width_target::pinned(&runtime), None);
}

#[test]
fn structural_event_converges_without_resize_feedback() {
    let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
    write_zellij_topology(&runtime);
    let diag = crate::diag::DiagSink::disabled();

    controller.backstop(Some(50), Some(3), None, &diag);
    let structural_at_ms = crate::sidebar::timing::unix_now_ms();
    write_zellij_topology_for_view_at(&runtime, 200, structural_at_ms);
    controller.note_structural(structural_at_ms, Some(80), &diag);

    assert_eq!(controller.structural_at_ms, Some(structural_at_ms));
    assert_eq!(controller.convergence.target(), Some(target(50)));
    assert!(controller.convergence.in_flight());
    assert_eq!(crate::sidebar::width_target::pinned(&runtime), None);
}

#[test]
fn stalled_structural_resize_is_not_adopted_on_later_feedback() {
    let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
    write_zellij_topology(&runtime);
    let diag = crate::diag::DiagSink::disabled();

    controller.backstop(Some(50), Some(3), None, &diag);
    let structural_at_ms = crate::sidebar::timing::unix_now_ms();
    write_zellij_topology_for_view_at(&runtime, 200, structural_at_ms);
    controller.backstop(Some(83), Some(2), Some(structural_at_ms), &diag);
    controller
        .convergence
        .in_flight
        .as_mut()
        .expect("structural correction in flight")
        .at = Instant::now() - FEEDBACK_TIMEOUT;
    controller.observe(83, SidebarWidthControlTrigger::Backstop, &diag);
    controller
        .convergence
        .in_flight
        .as_mut()
        .expect("no-progress retry in flight")
        .at = Instant::now() - FEEDBACK_TIMEOUT;
    controller.observe(83, SidebarWidthControlTrigger::Backstop, &diag);
    assert!(controller.convergence.is_idle());

    controller.observe(83, SidebarWidthControlTrigger::ResizeFeedback, &diag);
    controller.classification_deadline = Some(Instant::now());
    controller.backstop(Some(83), Some(2), Some(u64::MAX), &diag);

    assert_eq!(crate::sidebar::width_target::pinned(&runtime), None);
    assert_eq!(controller.convergence.target(), Some(target(50)));
    assert!(controller.convergence.in_flight());
}

#[test]
fn idle_off_spec_controller_retries_after_the_backstop_deadline() {
    let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
    write_zellij_topology(&runtime);
    let diag = crate::diag::DiagSink::disabled();

    controller.backstop(Some(50), Some(1), None, &diag);
    controller.convergence.idle = Some(WidthIdle {
        at: 83,
        reason: WidthIdleReason::NoProgress,
    });
    controller.idle_retry_deadline = Some(Instant::now() - Duration::from_millis(1));

    controller.backstop(Some(83), Some(1), Some(u64::MAX), &diag);

    assert!(controller.convergence.in_flight());
    assert!(
        controller
            .idle_retry_deadline
            .is_some_and(|deadline| deadline > Instant::now())
    );
}

#[test]
fn pending_mouse_classification_suppresses_a_due_idle_retry() {
    let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
    write_zellij_topology(&runtime);
    let diag = crate::diag::DiagSink::disabled();

    controller.backstop(Some(50), Some(1), None, &diag);
    controller.observe(83, SidebarWidthControlTrigger::ResizeFeedback, &diag);
    let resize_at_ms = controller
        .classification_resize_at_ms
        .expect("resize starts classification");
    controller.classification_deadline = Some(Instant::now());
    controller.idle_retry_deadline = Some(Instant::now() - Duration::from_millis(1));

    controller.backstop(
        Some(83),
        Some(1),
        Some(resize_at_ms.saturating_add(1)),
        &diag,
    );

    assert!(controller.classification_deadline.is_some());
    assert_eq!(controller.idle_retry_deadline, None);
    assert!(!controller.convergence.in_flight());
    assert_eq!(crate::sidebar::width_target::pinned(&runtime), None);
}

#[test]
fn settled_view_resize_reresolves_an_unpinned_target() {
    let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
    write_zellij_topology(&runtime);
    controller.last_siblings = Some(1);
    let diag = crate::diag::DiagSink::disabled();
    controller.reload_target(&crate::config::ThemeConfig::default(), None, &diag);
    write_zellij_topology_for_view(&runtime, 240);

    controller.observe(80, SidebarWidthControlTrigger::ResizeFeedback, &diag);
    controller.classification_deadline = Some(Instant::now());
    controller.backstop(Some(80), Some(1), Some(u64::MAX), &diag);

    assert_eq!(crate::sidebar::width_target::pinned(&runtime), None);
    assert_eq!(controller.convergence.target(), Some(target(60)));
}

#[test]
fn structural_marker_does_not_swallow_a_concurrent_view_change() {
    let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
    write_zellij_topology(&runtime);
    controller.last_siblings = Some(1);
    let diag = crate::diag::DiagSink::disabled();
    controller.reload_target(&crate::config::ThemeConfig::default(), None, &diag);
    write_zellij_topology_for_view(&runtime, 240);

    controller.observe(80, SidebarWidthControlTrigger::ResizeFeedback, &diag);
    controller.structural_at_ms = Some(u64::MAX);
    controller.classification_deadline = Some(Instant::now());
    controller.backstop(Some(80), Some(1), Some(u64::MAX), &diag);

    assert_eq!(crate::sidebar::width_target::pinned(&runtime), None);
    assert_eq!(controller.current_view_cols, Some(240));
    assert_eq!(controller.convergence.target(), Some(target(60)));
}

#[test]
fn settled_view_resize_scales_a_pinned_target() {
    let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
    write_zellij_topology(&runtime);
    controller.last_siblings = Some(1);
    let share =
        crate::sidebar::width_target::pin(&runtime, target(80), 200).expect("pin width target");
    let diag = crate::diag::DiagSink::disabled();
    controller.reload_target(&crate::config::ThemeConfig::default(), None, &diag);
    write_zellij_topology_for_view(&runtime, 240);

    controller.observe(96, SidebarWidthControlTrigger::ResizeFeedback, &diag);
    controller.classification_deadline = Some(Instant::now());
    controller.backstop(Some(96), Some(1), Some(u64::MAX), &diag);

    assert_eq!(crate::sidebar::width_target::pinned(&runtime), Some(share));
    assert_eq!(controller.convergence.target(), Some(target(96)));
}

#[test]
fn settled_resize_without_geometry_never_adopts() {
    let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
    controller.current_view_cols = Some(200);
    controller.last_siblings = Some(1);
    controller.convergence.retarget(Some(target(50)));
    let diag = crate::diag::DiagSink::disabled();
    controller.reload_target(&crate::config::ThemeConfig::default(), None, &diag);

    controller.observe(83, SidebarWidthControlTrigger::ResizeFeedback, &diag);
    controller.classification_deadline = Some(Instant::now());
    controller.backstop(Some(83), Some(1), Some(u64::MAX), &diag);

    assert_eq!(crate::sidebar::width_target::pinned(&runtime), None);
    assert!(controller.classification_deadline.is_some());
}

#[test]
fn stale_pane_observation_waits_without_adopting_or_nudging() {
    let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
    write_zellij_topology(&runtime);
    controller.last_siblings = Some(1);
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
    assert!(!controller.convergence.in_flight());
}

#[test]
fn merely_newer_sibling_observation_waits_without_adopting_or_nudging() {
    let (_dir, runtime, mut controller) = controller(MuxName::Zellij);
    write_zellij_topology(&runtime);
    controller.last_siblings = Some(1);
    let diag = crate::diag::DiagSink::disabled();
    controller.reload_target(&crate::config::ThemeConfig::default(), None, &diag);

    controller.observe(83, SidebarWidthControlTrigger::ResizeFeedback, &diag);
    let resize_at_ms = controller
        .classification_resize_at_ms
        .expect("resize starts classification");
    controller.classification_deadline = Some(Instant::now());
    controller.backstop(
        Some(83),
        Some(1),
        Some(resize_at_ms.saturating_add(1)),
        &diag,
    );

    assert_eq!(crate::sidebar::width_target::pinned(&runtime), None);
    assert!(controller.classification_deadline.is_some());
    assert_eq!(controller.classification_resize_at_ms, Some(resize_at_ms));
    assert!(
        !controller.convergence.in_flight(),
        "pending adoption evidence must not fight a genuine mouse drag",
    );
}

#[test]
fn observed_step_sets_the_nearest_reachable_band() {
    let now = Instant::now();
    let mut control = WidthControl::new(Some(target(72)));
    assert_eq!(control.decide(50, now), Some((50, 72)));
    assert_eq!(
        control.decide(60, now + Duration::from_millis(10)),
        Some((60, 72))
    );
    assert_eq!(control.decide(68, now + Duration::from_millis(20)), None);
}

#[test]
fn seeded_native_step_parks_within_half_a_step_of_the_target() {
    let now = Instant::now();
    let mut control = WidthControl::new(Some(target(80)));
    control.seed_native_step(10);

    assert_eq!(control.stop_step(), 10);
    assert_eq!(control.decide(83, now), None);

    let mut below = WidthControl::new(Some(target(80)));
    below.seed_native_step(10);
    assert_eq!(below.decide(79, now), None);

    let mut outside = WidthControl::new(Some(target(80)));
    outside.seed_native_step(10);
    assert_eq!(outside.decide(74, now), Some((74, 80)));
    assert_eq!(outside.decide(79, now + Duration::from_millis(10)), None);
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
fn a_short_learned_step_does_not_narrow_the_seeded_band() {
    let now = Instant::now();
    let mut control = WidthControl::new(Some(target(80)));
    control.seed_native_step(10);
    assert_eq!(control.decide(60, now), Some((60, 80)));

    assert_eq!(
        control.decide(66, now + Duration::from_millis(10)),
        Some((66, 80)),
    );
    assert_eq!(control.stop_step(), 10);
}

#[test]
fn step_estimate_uses_a_symmetric_band() {
    let now = Instant::now();
    let mut inside = WidthControl::new(Some(target(80)));
    inside.seed_native_step(2);
    assert_eq!(inside.decide(81, now), None);

    let mut below = WidthControl::new(Some(target(80)));
    below.seed_native_step(2);
    assert_eq!(below.decide(79, now), None);

    let mut next_step = WidthControl::new(Some(target(80)));
    next_step.seed_native_step(2);
    assert_eq!(next_step.decide(82, now), Some((82, 80)));
}

#[test]
fn one_column_stop_step_keeps_exact_backends_exact() {
    let now = Instant::now();
    let mut control = WidthControl::new(Some(target(80)));
    control.seed_native_step(1);

    assert_eq!(control.decide(80, now), None);
    assert_eq!(control.decide(81, now), Some((81, 80)));
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
    assert!(control.reverse_max_distance.is_none());
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
fn regressed_step_reverses_once_then_parks() {
    let now = Instant::now();
    let mut control = WidthControl::new(Some(target(80)));
    assert_eq!(control.decide(83, now), Some((83, 80)));
    assert_eq!(
        control.decide(70, now + Duration::from_millis(10)),
        Some((70, 80)),
    );
    assert!(control.reverse_max_distance.is_some());

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
fn reverse_step_that_lands_farther_keeps_converging() {
    let now = Instant::now();
    let mut control = WidthControl::new(Some(target(80)));
    assert_eq!(control.decide(83, now), Some((83, 80)));
    assert_eq!(
        control.decide(70, now + Duration::from_millis(10)),
        Some((70, 80)),
    );

    assert_eq!(
        control.decide(95, now + Duration::from_millis(20)),
        Some((95, 80)),
    );
    assert_eq!(control.stop_step(), 25);
    assert_eq!(
        control.traces.back(),
        Some(&WidthTransition::StepIssued {
            from: 95,
            target: 80,
        }),
    );
}

#[test]
fn reverse_step_that_improves_but_stays_below_target_keeps_converging() {
    let now = Instant::now();
    let mut control = WidthControl::new(Some(target(80)));
    assert_eq!(control.decide(83, now), Some((83, 80)));
    assert_eq!(
        control.decide(70, now + Duration::from_millis(10)),
        Some((70, 80)),
    );
    assert!(control.reverse_max_distance.is_some());

    assert_eq!(
        control.decide(75, now + Duration::from_millis(20)),
        Some((75, 80)),
    );
    assert!(control.reverse_max_distance.is_none());
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

    assert_eq!(control.decide(68, now + Duration::from_millis(20)), None);
    assert_eq!(
        control.take_trace(),
        Some(WidthTransition::FeedbackLearned {
            settled: 68,
            learned_step: 8,
        })
    );
    assert_eq!(
        control.take_trace(),
        Some(WidthTransition::Idle {
            at: 68,
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
