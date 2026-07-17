use super::input::{KeyAction, Wakeup};
use super::*;
use crate::sidebar_pane::app::fixtures::{
    agent_snapshot, pane, snapshot, snapshot_with_panes, workspace,
};
use crate::sidebar_pane::pets::{
    BEGIN_SYNC, END_SYNC, PetAssets, PetPixelView, PixelRenderCaps, placeholder_cluster,
};
use jiff::Timestamp;

#[test]
fn deferred_fetch_deadline_caps_event_loop_timeout() {
    let now = Instant::now();
    assert_eq!(
        fetch_deadline_timeout(
            Duration::from_secs(10),
            Some(now + Duration::from_secs(3)),
            now,
        ),
        Duration::from_secs(3),
    );
    assert_eq!(
        fetch_deadline_timeout(Duration::from_secs(10), Some(now), now),
        FRAME_MIN_TIMEOUT,
    );
}

fn focus_fixture() -> (SidebarSnapshot, PaneId, PaneId, PaneId) {
    let ws = workspace();
    let sidebar = PaneId::from_parts(MuxName::Zellij, "terminal_10");
    let first_work = PaneId::from_parts(MuxName::Zellij, "terminal_11");
    let second_work = PaneId::from_parts(MuxName::Zellij, "terminal_12");
    let mut snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_11", "tab_1", false),
            pane("terminal_12", "tab_1", false),
        ],
    );
    snapshot.own_view = Some(crate::SidebarOwnView {
        sibling_count: 3,
        working_pane_ids: vec![first_work.clone(), second_work.clone()],
        own_view_is_daemon: false,
    });
    (snapshot, sidebar, first_work, second_work)
}

#[test]
fn tick_for_clamps_zero_and_honours_explicit_seconds() {
    assert_eq!(tick_for(5), Duration::from_secs(5));
    assert_eq!(tick_for(0), Duration::from_secs(1));
}

#[test]
fn frame_grid_advances_one_frame_or_snaps_past_missed_frames() {
    let base = Instant::now();
    let frame = crate::sidebar::timing::animation_frame(crate::sidebar::timing::DEFAULT_REFRESH_MS);
    assert_eq!(next_frame_after(base, base, frame), base + frame);
    let now = base + frame * 5;
    assert_eq!(next_frame_after(base, now, frame), now + frame);
}

#[test]
fn frame_interval_uses_breath_for_pulse_and_fast_for_work() {
    let ws = workspace();
    let mut slow = snapshot(&ws);
    slow.theme.animations.waiting =
        Some(toml::from_str("effect = \"breathe\"\n").expect("animation spec"));
    slow.worktree_groups = vec![crate::SidebarWorktreeGroup {
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        kind: crate::SidebarWorktreeKind::Worktree,
        status_counts: Vec::new(),
        rows: vec![crate::SidebarRow {
            id: "claude-1".to_owned(),
            name: "claude".to_owned(),
            pane: None,
            worktree_path: Some("/repo/main".to_owned()),
            worktree_branch: Some("main".to_owned()),
            channel: None,
            unread: false,
            inactive: false,
            archived: false,
            attention_score: 0,
            last_activity: Timestamp::now(),
            card: crate::RowCard::Agent(Box::new(crate::AgentCard {
                status: crate::agents::AgentStatus::Waiting,
                phase: crate::agents::TurnPhase::Idle,
                task: Some("allow cargo fmt".to_owned()),
                ..crate::AgentCard::default()
            })),
        }],
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
        worktree_backed: false,
        finished: false,
        clean: None,
        landed: None,
        trunk_sync: None,
        pr_state: None,
        pr_ci: None,
        pr_number: None,
    }];
    assert!(is_animating(&slow, &UiState::default(), 0, false));
    assert_eq!(
        frame_interval(&slow, &UiState::default(), false),
        crate::sidebar::timing::animation_frame(crate::sidebar::timing::DEFAULT_REFRESH_MS),
        "a cold theme cache stays on the safe base grid until the first paint warms it"
    );

    let mut ui = UiState::default();
    ui.theme(&slow.theme);

    assert_eq!(
        frame_interval(&slow, &ui, false),
        crate::sidebar::timing::BREATH_ANIMATION_FRAME
    );

    slow.worktree_groups[0].rows[0]
        .as_agent_mut()
        .unwrap()
        .status = crate::agents::AgentStatus::Running;
    assert_eq!(
        frame_interval(&slow, &ui, false),
        crate::sidebar::timing::animation_frame(crate::sidebar::timing::DEFAULT_REFRESH_MS)
    );
}

#[test]
fn selected_blank_idle_agent_keeps_breath_grid_awake() {
    let ws = workspace();
    let mut snapshot = agent_snapshot(&ws);
    let agent = snapshot.worktree_groups[0].rows[0].as_agent_mut().unwrap();
    agent.task = None;
    agent.description = None;
    agent.prompt = None;

    let mut selected = UiState {
        selected_index: 0,
        ..Default::default()
    };
    selected.theme(&snapshot.theme);
    assert!(is_animating(&snapshot, &selected, 0, false));
    assert_eq!(
        frame_interval(&snapshot, &selected, false),
        crate::sidebar::timing::BREATH_ANIMATION_FRAME
    );

    let mut off_selection = UiState {
        selected_index: 99,
        ..Default::default()
    };
    off_selection.theme(&snapshot.theme);
    assert!(!is_animating(&snapshot, &off_selection, 0, false));

    snapshot.worktree_groups[0].rows[0]
        .as_agent_mut()
        .unwrap()
        .task = Some("warm up".to_owned());
    selected.theme(&snapshot.theme);
    assert!(!is_animating(&snapshot, &selected, 0, false));
}

#[test]
fn help_popup_dismisses_and_consumes_any_user_input() {
    let ws = workspace();
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", false),
        ],
    );

    for wakeup in [
        Wakeup::ReloadKey,
        Wakeup::Key(KeyAction::Down),
        Wakeup::Key(KeyAction::Other),
        Wakeup::MouseClick { column: 1, row: 0 },
        Wakeup::Scroll { down: true },
    ] {
        let mut ui = UiState {
            help_visible: true,
            selected_index: 0,
            scroll_offset: 4,
            interactions: render::FrameInteractions::from_parts(vec![Some(1)], Vec::new()),
            ..Default::default()
        };

        let outcome = handle_wakeup(wakeup, &mut ui, &snapshot);

        assert_eq!(outcome, InputOutcome::redraw());
        assert!(!ui.help_visible);
        assert_eq!(ui.selected_index, 0, "key input was consumed");
        assert_eq!(ui.scroll_offset, 4, "scroll input was consumed");
        assert_eq!(ui.manual_scroll, None, "scroll input was consumed");
        assert_eq!(ui.browse, None, "key input was consumed");
    }
}

#[test]
fn help_popup_keeps_animation_grid_hot() {
    let ws = workspace();
    let snapshot = snapshot(&ws);
    let mut ui = UiState {
        help_visible: true,
        ..Default::default()
    };
    ui.theme(&snapshot.theme);

    assert!(is_animating(&snapshot, &ui, 0, false));
    assert_eq!(
        frame_interval(&snapshot, &ui, false),
        crate::sidebar::timing::animation_frame(crate::sidebar::timing::DEFAULT_REFRESH_MS)
    );
}

#[test]
fn pet_frame_interval_uses_pet_cadence_and_honours_static_motion() {
    let ws = workspace();
    let mut snapshot = snapshot(&ws);
    snapshot.theme.pets.enabled = true;
    let mut ui = UiState {
        pet: Some(crate::sidebar_pane::pets::PetView {
            body: Some(crate::sidebar_pane::pets::PetBody::Cell(vec![vec![
                crate::sidebar_pane::pets::PetCell {
                    ch: '▀',
                    fg: ratatui::style::Color::White,
                    bg: ratatui::style::Color::Black,
                },
            ]])),
            caption: Some("resting".to_owned()),
            loading: false,
            action: crate::sidebar_pane::pets::PetAction::Idle,
            active_track: "idle",
        }),
        ..Default::default()
    };
    ui.theme(&snapshot.theme);

    if render::pet_body_enabled(&snapshot) {
        assert!(is_animating(&snapshot, &ui, 0, false));
        assert_eq!(
            frame_interval(&snapshot, &ui, false),
            Duration::from_millis(625)
        );

        let mut jumping_ui = ui.clone();
        jumping_ui.pet.as_mut().expect("pet").active_track = "jumping";
        assert_eq!(
            frame_interval(&snapshot, &jumping_ui, false),
            Duration::from_millis(286)
        );
    } else {
        assert!(
            !is_animating(&snapshot, &ui, 0, false),
            "NO_COLOR suppresses pet body animation"
        );
        let mut loading_ui = ui.clone();
        let pet = loading_ui.pet.as_mut().expect("pet");
        pet.body = None;
        pet.loading = true;
        assert!(is_animating(&snapshot, &loading_ui, 0, false));
        assert_eq!(
            frame_interval(&snapshot, &loading_ui, false),
            crate::sidebar::timing::animation_frame(crate::sidebar::timing::DEFAULT_REFRESH_MS)
        );
    }

    snapshot.theme.animations.idle =
        Some(toml::from_str("effect = \"static\"\n").expect("animation spec"));
    ui.theme(&snapshot.theme);
    assert!(!is_animating(&snapshot, &ui, 0, false));

    snapshot.theme.animations.thinking =
        Some(toml::from_str("effect = \"static\"\n").expect("animation spec"));
    ui.pet.as_mut().expect("pet").action = crate::sidebar_pane::pets::PetAction::Thinking;
    ui.theme(&snapshot.theme);
    assert!(
        !is_animating(&snapshot, &ui, 0, false),
        "a static effect with omitted frames quiets spinner-role pets too"
    );
}

#[test]
fn active_alert_suppresses_hidden_pet_animation_cadence() {
    let ws = workspace();
    let mut snapshot = snapshot(&ws);
    snapshot.theme.pets.enabled = true;
    let mut ui = UiState {
        pet: Some(crate::sidebar_pane::pets::PetView {
            body: Some(crate::sidebar_pane::pets::PetBody::Cell(vec![vec![
                crate::sidebar_pane::pets::PetCell {
                    ch: '▀',
                    fg: ratatui::style::Color::White,
                    bg: ratatui::style::Color::Black,
                },
            ]])),
            caption: Some("resting".to_owned()),
            loading: false,
            action: crate::sidebar_pane::pets::PetAction::Idle,
            active_track: "idle",
        }),
        ..Default::default()
    };
    ui.theme(&snapshot.theme);
    let alert_active = render::Alert::active("snapshot failed", snapshot.now).is_active();

    assert!(render::dashboard_present(&snapshot, false));
    assert!(!render::dashboard_present(&snapshot, alert_active));
    assert!(!is_animating(&snapshot, &ui, 0, alert_active));
    assert_eq!(
        frame_interval(&snapshot, &ui, alert_active),
        crate::sidebar::timing::animation_frame(crate::sidebar::timing::DEFAULT_REFRESH_MS)
    );

    if render::pet_body_enabled(&snapshot) {
        assert!(is_animating(&snapshot, &ui, 0, false));
        assert_eq!(
            frame_interval(&snapshot, &ui, false),
            Duration::from_millis(625)
        );
    }
}

#[test]
fn refresh_pet_view_uses_fixed_pet_size_when_dashboard_present() {
    let ws = workspace();
    let mut snapshot = snapshot(&ws);
    snapshot.theme.pets.enabled = true;
    let mut ui = UiState::default();
    let mut painter = paint::FramePainter::new(PixelRenderCaps::default(), true);

    painter.refresh_view(&mut ui, &snapshot, false);

    let pet = ui.pet.expect("pet view");
    assert_eq!(pet.body, None);
    if render::pet_body_enabled(&snapshot) {
        assert!(pet.loading);
    } else {
        assert!(!pet.loading, "NO_COLOR suppresses pet body loading");
    }
    assert_eq!(pet.caption.as_deref(), Some("resting"));
}

#[test]
fn refresh_view_gates_pixel_meter_frame_with_caps_and_master_switch() {
    let ws = workspace();
    let mut snapshot = snapshot(&ws);
    let mut ui = UiState::default();
    let mut painter = paint::FramePainter::new(
        PixelRenderCaps {
            pixel_transport: true,
            kitty_term: true,
        },
        false,
    );

    painter.refresh_view(&mut ui, &snapshot, false);
    let raster = crate::sidebar_pane::pixel::meter::MeterRaster::new(
        2,
        0.5,
        [1, 2, 3],
        Vec::new(),
        [4, 5, 6],
    );
    let first_id = ui
        .meter_pixels
        .as_mut()
        .expect("meter pixels")
        .intern(raster.clone())
        .expect("first raster");

    painter.refresh_view(&mut ui, &snapshot, false);
    assert_eq!(
        ui.meter_pixels
            .as_mut()
            .expect("persistent meter pixels")
            .intern(raster),
        Some(first_id),
        "refreshing the view keeps the content interning table"
    );

    snapshot.theme.display.pixel = crate::config::PixelMode::Off;
    painter.refresh_view(&mut ui, &snapshot, false);
    assert!(ui.meter_pixels.is_none());
}

#[test]
fn pixel_layout_shift_uses_ratatui_diff_without_full_clear() {
    let ws = workspace();
    let mut snapshot = snapshot(&ws);
    snapshot.providers = vec![crate::sidebar::test_support::provider_panel(
        "codex",
        Vec::new(),
    )];
    snapshot.theme.pets.enabled = true;
    snapshot.theme.pets.pet = "codex".to_owned();
    snapshot.theme.pets.glyphs = crate::config::PetsGlyphMode::Pixel;
    let pixel = PetPixelView {
        pet_id: "codex".to_owned(),
        sprite_index: 0,
        image_id: 0x120000,
        size: crate::sidebar_pane::pets::PetGridSize { cols: 2, rows: 1 },
    };
    let mut ui = UiState {
        pet: Some(crate::sidebar_pane::pets::PetView {
            body: Some(crate::sidebar_pane::pets::PetBody::Pixel(pixel.clone())),
            caption: Some("resting".to_owned()),
            loading: false,
            action: crate::sidebar_pane::pets::PetAction::Idle,
            active_track: "idle",
        }),
        ..Default::default()
    };
    let mut painter = paint::FramePainter::with_assets(
        PetAssets::test_loaded_pixel_frame("codex"),
        PixelRenderCaps::default(),
        true,
    );
    #[derive(Clone)]
    struct SharedBuffer(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);
    impl std::io::Write for SharedBuffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let output = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let backend = CrosstermBackend::new(SharedBuffer(output.clone()));
    let viewport = ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 80, 12));
    let mut terminal =
        Terminal::with_options(backend, ratatui::TerminalOptions { viewport }).expect("terminal");

    painter
        .draw_and_paint(&mut terminal, &snapshot, None, &mut ui)
        .expect("first draw");
    let first_len = output.borrow().len();

    snapshot.providers[0].windows = vec![
        crate::agents::RateLimitWindow {
            used_percentage: Some(25),
            duration_mins: Some(300),
            ..Default::default()
        },
        crate::agents::RateLimitWindow {
            used_percentage: Some(40),
            duration_mins: Some(10_080),
            ..Default::default()
        },
    ];
    painter
        .draw_and_paint(&mut terminal, &snapshot, None, &mut ui)
        .expect("shifted draw");
    let second_len = output.borrow().len();

    painter
        .draw_and_paint(&mut terminal, &snapshot, None, &mut ui)
        .expect("steady draw");
    let output = output.borrow();
    let second = String::from_utf8_lossy(&output[first_len..second_len]);
    let steady = String::from_utf8_lossy(&output[second_len..]);

    assert!(
        !second.contains("\u{1b}[2J"),
        "layout shift must not full-clear the terminal"
    );
    if render::pet_body_enabled(&snapshot) {
        assert!(
            second.contains(&placeholder_cluster(0, 0)),
            "ratatui owns and rewrites shifted placeholder cells: {}",
            second.escape_debug()
        );
    }
    assert!(
        !second.contains("\u{1b}_G"),
        "resident sprite is not re-transmitted on layout shift"
    );
    assert!(
        !steady.contains(&placeholder_cluster(0, 0)),
        "unchanged frame emits no placeholder bytes"
    );
    assert!(
        !steady.contains("\u{1b}_G"),
        "unchanged frame emits no kitty graphics bytes"
    );
    let output = String::from_utf8_lossy(&output);
    assert_eq!(
        output
            .matches(std::str::from_utf8(BEGIN_SYNC).unwrap())
            .count(),
        3
    );
    assert_eq!(
        output
            .matches(std::str::from_utf8(END_SYNC).unwrap())
            .count(),
        3
    );
    assert!(
        !output.contains("a=d,d=i"),
        "layout shifts must keep resident kitty images alive"
    );
}

#[test]
fn heartbeat_write_due_on_first_or_aged_write_only() {
    assert!(heartbeat_write_due(None));
    assert!(!heartbeat_write_due(Some(Instant::now())));
    assert!(heartbeat_write_due(Some(
        Instant::now() - HEARTBEAT_WRITE_INTERVAL
    )));
}

#[test]
fn suppressed_produce_panic_hook_chains_without_renderer_diagnostic() {
    let _hook_guard = PANIC_HOOK_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let sink =
        crate::diag::DiagSink::under(dir.path().to_path_buf(), workspace(), "rimz-test", None);
    let prior_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let prior_called_hook = prior_called.clone();
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |_| {
        prior_called_hook.store(true, std::sync::atomic::Ordering::SeqCst);
    }));
    install_panic_diagnostic_hook(sink.clone());

    let result = with_produce_panic_diagnostic_suppressed(|| {
        std::panic::catch_unwind(|| panic!("caught produce panic"))
    });
    let _installed = std::panic::take_hook();
    std::panic::set_hook(original);

    assert!(result.is_err());
    assert!(
        prior_called.load(std::sync::atomic::Ordering::SeqCst),
        "the suppressed diagnostic branch still chains the previously installed panic hook"
    );
    assert!(
        !sink.log_path().unwrap().exists(),
        "caught producer panics are converted to fetch failures, not renderer-panic diagnostics"
    );
}

#[test]
fn notification_targeting_matches_mux_reachability_rules() {
    let (targeted_snapshot, _sidebar, first_work, _second_work) = focus_fixture();
    assert!(notification_targets_own_view(
        &targeted_snapshot,
        std::slice::from_ref(&first_work)
    ));
    assert!(!notification_targets_own_view(&targeted_snapshot, &[]));

    let foreign = PaneId::from_parts(MuxName::Zellij, "terminal_99");
    assert!(!notification_targets_own_view(
        &targeted_snapshot,
        &[foreign]
    ));

    let no_own_view = snapshot(&workspace());
    assert!(!notification_targets_own_view(
        &no_own_view,
        std::slice::from_ref(&first_work)
    ));

    assert!(desktop_notification_targets_renderer(
        MuxName::Tmux,
        &targeted_snapshot,
        &[]
    ));

    let foreign = PaneId::from_parts(MuxName::Tmux, "%99");
    assert!(desktop_notification_targets_renderer(
        MuxName::Tmux,
        &targeted_snapshot,
        &[foreign]
    ));

    let no_own_view = snapshot(&workspace());
    assert!(!desktop_notification_targets_renderer(
        MuxName::Tmux,
        &no_own_view,
        std::slice::from_ref(&first_work)
    ));

    assert!(desktop_notification_targets_renderer(
        MuxName::Zellij,
        &targeted_snapshot,
        std::slice::from_ref(&first_work)
    ));

    let foreign = PaneId::from_parts(MuxName::Zellij, "terminal_99");
    assert!(!desktop_notification_targets_renderer(
        MuxName::Zellij,
        &targeted_snapshot,
        &[foreign]
    ));
    assert!(!desktop_notification_targets_renderer(
        MuxName::Zellij,
        &targeted_snapshot,
        &[]
    ));
}

#[test]
fn bell_rings_only_for_unread_owned_panes_off_daemon_views() {
    use crate::agents::AgentStatus;

    let ws = workspace();
    let work = PaneId::from_parts(MuxName::Zellij, "terminal_11");
    let foreign = PaneId::from_parts(MuxName::Zellij, "terminal_99");

    let scene = |unread: bool, status: AgentStatus, daemon: bool| {
        let mut snap = snapshot(&ws);
        snap.panes_produced_at_ms = Some(1);
        snap.own_view = Some(crate::SidebarOwnView {
            sibling_count: 2,
            working_pane_ids: vec![work.clone()],
            own_view_is_daemon: daemon,
        });
        snap.worktree_groups = vec![crate::SidebarWorktreeGroup {
            key: "/repo/main".to_owned(),
            label: "main".to_owned(),
            kind: crate::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![crate::SidebarRow {
                id: "agent-1".to_owned(),
                name: "claude".to_owned(),
                pane: Some(pane("terminal_11", "tab_1", false)),
                worktree_path: Some("/repo/main".to_owned()),
                worktree_branch: Some("main".to_owned()),
                channel: None,
                unread,
                inactive: false,
                archived: false,
                attention_score: 0,
                last_activity: Timestamp::now(),
                card: crate::RowCard::Agent(Box::new(crate::AgentCard {
                    status,
                    phase: crate::agents::TurnPhase::Idle,
                    ..crate::AgentCard::default()
                })),
            }],
            diff_added: None,
            diff_removed: None,
            commits_ahead: None,
            commits_behind: None,
            trunk: None,
            worktree_backed: false,
            finished: false,
            clean: None,
            landed: None,
            trunk_sync: None,
            pr_state: None,
            pr_ci: None,
            pr_number: None,
        }];
        snap
    };

    // Agent path: rings only while the owned row is unread.
    let unread_waiting = scene(true, AgentStatus::Waiting, false);
    assert_eq!(
        bell_decision(&unread_waiting, std::slice::from_ref(&work), true),
        BellDecision::Fired
    );

    // Resumed to running and no longer unread — a thinking agent never rings.
    let running = scene(false, AgentStatus::Running, false);
    assert_eq!(
        bell_decision(&running, std::slice::from_ref(&work), true),
        BellDecision::NotUnread
    );

    // A foreign pane the view does not own never rings.
    assert_eq!(
        bell_decision(&unread_waiting, std::slice::from_ref(&foreign), true),
        BellDecision::PaneNotInView
    );

    // Link/reminder path bypasses the unread re-check and rings on an owned pane.
    assert!(bell_decision(&running, std::slice::from_ref(&work), false).fired());

    // A daemon-only view (rimzd) never rings, on either path.
    let daemon = scene(true, AgentStatus::Waiting, true);
    assert_eq!(
        bell_decision(&daemon, std::slice::from_ref(&work), true),
        BellDecision::DaemonView
    );
    assert!(!bell_decision(&daemon, std::slice::from_ref(&work), false).fired());

    // No own view at all never rings.
    assert_eq!(
        bell_decision(&snapshot(&ws), std::slice::from_ref(&work), false),
        BellDecision::NoOwnView
    );
}

#[test]
fn focus_stranded_targets_recent_own_pane_events_only() {
    let (snapshot, sidebar, _first_work, second_work) = focus_fixture();
    let ui = UiState {
        baseline_pane: Some(second_work.clone()),
        ..UiState::default()
    };

    assert_eq!(
        focus_stranded_target(&snapshot, &ui, &sidebar, Some(&sidebar), 1_000, 1_050),
        Some(second_work.clone()),
    );

    let foreign = PaneId::from_parts(MuxName::Zellij, "terminal_99");
    let ui = UiState {
        baseline_pane: Some(second_work.clone()),
        ..UiState::default()
    };
    assert_eq!(
        focus_stranded_target(&snapshot, &ui, &sidebar, Some(&foreign), 1_000, 1_050),
        None,
    );

    let now = 1_000 + duration_millis(FOCUS_STRANDED_EVENT_TTL) + 1;
    assert_eq!(
        focus_stranded_target(&snapshot, &ui, &sidebar, Some(&sidebar), 1_000, now),
        None,
    );
}

#[test]
fn focus_stranded_falls_back_to_working_sibling_when_baseline_is_missing() {
    let (snapshot, sidebar, first_work, _second_work) = focus_fixture();
    let ui = UiState {
        baseline_pane: Some(PaneId::from_parts(MuxName::Zellij, "terminal_99")),
        ..UiState::default()
    };

    assert_eq!(
        focus_stranded_target(&snapshot, &ui, &sidebar, Some(&sidebar), 1_000, 1_050),
        Some(first_work),
    );

    let (mut snapshot, sidebar, _first_work, _second_work) = focus_fixture();
    if let Some(view) = &mut snapshot.own_view {
        view.working_pane_ids.clear();
    }

    assert_eq!(
        focus_stranded_target(
            &snapshot,
            &UiState::default(),
            &sidebar,
            Some(&sidebar),
            1_000,
            1_050,
        ),
        None,
    );
}

#[test]
fn focus_stranded_skips_when_client_focus_is_ambiguous() {
    let (mut snapshot, sidebar, _first_work, second_work) = focus_fixture();
    snapshot.viewed_panes = vec![
        sidebar.clone(),
        PaneId::from_parts(MuxName::Zellij, "terminal_42"),
    ];
    let ui = UiState {
        baseline_pane: Some(second_work),
        ..UiState::default()
    };

    assert_eq!(
        focus_stranded_target(&snapshot, &ui, &sidebar, Some(&sidebar), 1_000, 1_050),
        None,
    );
}
