use super::*;
use crate::sidebar_pane::app::fixtures::{agent_snapshot, pane, snapshot, workspace};

#[test]
fn animation_gate_uses_observed_phase_for_money_and_scrollbar() {
    let ws = workspace();
    let snapshot = snapshot(&ws);
    let mut ui = selected_ui();
    ui.theme(&snapshot.theme);
    ui.tally.observe(1.0, 0);
    ui.tally.observe(5.0, 1);
    assert!(animation_interval(&snapshot, &ui, 1, false).is_some());
    assert!(animation_interval(&snapshot, &ui, 100, false).is_none());
    ui.animation_phase = 100;
    assert!(animation_interval(&snapshot, &ui, 1, false).is_some());

    ui.tally = Default::default();
    ui.cost_rolls
        .observe(std::iter::once(("agent".to_owned(), 1.0)), 0);
    ui.cost_rolls
        .observe(std::iter::once(("agent".to_owned(), 5.0)), 1);
    assert!(animation_interval(&snapshot, &ui, 1, false).is_some());
    assert!(animation_interval(&snapshot, &ui, 100, false).is_none());

    ui.cost_rolls = Default::default();
    ui.scrollbar.observe(0, 0);
    ui.scrollbar.observe(1, 1);
    assert!(animation_interval(&snapshot, &ui, 1, false).is_some());
    assert!(animation_interval(&snapshot, &ui, 100, false).is_none());
    ui.animation_phase = 0;
    assert!(animation_interval(&snapshot, &ui, 100, false).is_none());
}

#[test]
fn pet_cadence_beats_breath_but_yields_to_fast_and_money() {
    let ws = workspace();
    let mut snapshot = agent_snapshot(&ws);
    snapshot.theme.pets.enabled = true;
    snapshot.theme.animations.waiting =
        Some(toml::from_str("effect = \"breathe\"\n").expect("animation spec"));
    snapshot.worktree_groups[0].rows[0]
        .as_agent_mut()
        .unwrap()
        .status = crate::agents::AgentStatus::Waiting;
    let mut ui = UiState {
        selected_index: Some(0),
        pet: Some(crate::sidebar_pane::pets::PetView {
            body: None,
            caption: None,
            frame_interval: Some(Duration::from_millis(625)),
        }),
        ..Default::default()
    };
    ui.theme(&snapshot.theme);
    assert_eq!(
        animation_interval(&snapshot, &ui, ui.animation_phase, false).expect("animated"),
        Duration::from_millis(625)
    );
    snapshot.worktree_groups[0].rows[0]
        .as_agent_mut()
        .unwrap()
        .status = crate::agents::AgentStatus::Running;
    assert_eq!(
        animation_interval(&snapshot, &ui, ui.animation_phase, false).expect("animated"),
        crate::sidebar::timing::animation_frame(snapshot.theme.display.resolved_refresh_ms())
    );
    snapshot.worktree_groups[0].rows[0]
        .as_agent_mut()
        .unwrap()
        .status = crate::agents::AgentStatus::Waiting;
    ui.tally.observe(1.0, 0);
    ui.tally.observe(5.0, 1);
    ui.animation_phase = 1;
    let money = crate::sidebar::timing::money_animation_frame(
        snapshot.theme.display.resolved_refresh_ms(),
        CLICK_PHASES,
    );
    for pet in [Duration::from_millis(625), Duration::from_millis(50)] {
        ui.pet.as_mut().unwrap().frame_interval = Some(pet);
        assert_eq!(
            animation_interval(&snapshot, &ui, ui.animation_phase, false).expect("animated"),
            pet.min(money)
        );
    }
}

#[test]
fn frame_interval_uses_breath_for_pulse_and_fast_for_work() {
    let ws = workspace();
    let mut slow = snapshot(&ws);
    slow.theme.animations.waiting =
        Some(toml::from_str("effect = \"breathe\"\n").expect("animation spec"));
    slow.worktree_groups = vec![crate::store::snapshot::SidebarWorktreeGroup {
        pr_stack: Default::default(),
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        label_qualifier: None,
        kind: crate::store::snapshot::SidebarWorktreeKind::Worktree,
        team: None,
        cohort_effort: None,
        pipeline: None,
        status_counts: Vec::new(),
        rows: vec![crate::store::snapshot::SidebarRow {
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
            card: crate::store::snapshot::RowCard::Agent(Box::new(
                crate::store::snapshot::AgentCard {
                    status: crate::agents::AgentStatus::Waiting,
                    phase: crate::agents::TurnPhase::Idle,
                    task: Some("allow cargo fmt".to_owned()),
                    ..crate::store::snapshot::AgentCard::default()
                },
            )),
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
        pr_queue: None,
        ci: None,
        pr_number: None,
        pr_url: None,
    }];
    assert!(animation_interval(&slow, &selected_ui(), 0, false).is_some());
    assert_eq!(
        animation_interval(&slow, &selected_ui(), 0, false).expect("animated"),
        crate::sidebar::timing::animation_frame(
            crate::config::DisplayConfig::default().resolved_refresh_ms()
        ),
        "a cold theme cache stays on the safe base grid until the first paint warms it"
    );

    let mut ui = selected_ui();
    ui.theme(&slow.theme);

    assert_eq!(
        animation_interval(&slow, &ui, ui.animation_phase, false).expect("animated"),
        crate::sidebar::timing::BREATH_ANIMATION_FRAME
    );

    slow.worktree_groups[0].rows[0]
        .as_agent_mut()
        .unwrap()
        .status = crate::agents::AgentStatus::Running;
    assert_eq!(
        animation_interval(&slow, &ui, ui.animation_phase, false).expect("animated"),
        crate::sidebar::timing::animation_frame(
            crate::config::DisplayConfig::default().resolved_refresh_ms()
        )
    );
}

/// Shell jobs and live children animate inside an open delegation section
/// whatever the parent's status; timers and signals stay static, and a closed
/// or resting section holds nothing in motion.
#[test]
fn open_delegation_motion_drives_the_animation_gate_under_a_sleeping_parent() {
    let ws = workspace();
    let mut snapshot = agent_snapshot(&ws);
    let row_id = snapshot.worktree_groups[0].rows[0].id.clone();
    let agent = snapshot.worktree_groups[0].rows[0].as_agent_mut().unwrap();
    agent.status = crate::agents::AgentStatus::Sleeping;
    agent.user_turn_started_at = Some(snapshot.now);
    agent
        .background_shells
        .push(crate::agents::BackgroundShell {
            id: "b1".to_owned(),
            command: Some("cargo test".to_owned()),
            description: None,
            started_at: snapshot.now,
        });
    let mut ui = selected_ui();
    ui.theme(&snapshot.theme);
    let fast = animation_interval(&snapshot, &ui, ui.animation_phase, false).expect("animated");
    assert_eq!(
        animation_cadence(
            &snapshot,
            &ui.cached_theme(&snapshot.theme).unwrap().animations
        ),
        AnimationCadence::None
    );
    assert!(animation_interval(&snapshot, &ui, 0, false).is_some());
    assert_eq!(
        fast,
        crate::sidebar::timing::animation_frame(snapshot.theme.display.resolved_refresh_ms())
    );

    ui.delegation_overrides.insert(row_id.clone(), false);
    assert!(animation_interval(&snapshot, &ui, 0, false).is_none());
    ui.delegation_overrides.clear();
    ui.selected_index = Some(usize::MAX);
    assert!(animation_interval(&snapshot, &ui, 0, false).is_none());
    ui.delegation_overrides.insert(row_id, true);
    assert!(animation_interval(&snapshot, &ui, 0, false).is_some());

    let agent = snapshot.worktree_groups[0].rows[0].as_agent_mut().unwrap();
    agent.background_shells.clear();
    for (trigger, moves) in [
        (
            crate::agents::PendingWaitTrigger::Subagent {
                active_at: snapshot.now,
                deadline_at: None,
                settled: None,
            },
            false,
        ),
        (
            crate::agents::PendingWaitTrigger::Team {
                stage: Some("Review".into()),
            },
            false,
        ),
        (
            crate::agents::PendingWaitTrigger::Command {
                command: "cargo test".to_owned(),
            },
            true,
        ),
        (crate::agents::PendingWaitTrigger::Pid { pid: 16776 }, true),
        (
            crate::agents::PendingWaitTrigger::Check {
                command: "nc -z localhost 3000".to_owned(),
            },
            true,
        ),
        (
            crate::agents::PendingWaitTrigger::File {
                path: "/repo/app.log".into(),
                grep: None,
            },
            true,
        ),
        (
            crate::agents::PendingWaitTrigger::Timer {
                due: snapshot.now,
                delay: None,
            },
            false,
        ),
        (
            crate::agents::PendingWaitTrigger::Signal {
                selector: "pr.merged".to_owned(),
            },
            false,
        ),
    ] {
        let agent = snapshot.worktree_groups[0].rows[0].as_agent_mut().unwrap();
        agent.pending_waits = vec![crate::agents::PendingWait {
            name: "wait".to_owned(),
            trigger,
            armed_at: None,
        }];
        assert_eq!(
            animation_interval(&snapshot, &ui, 0, false).is_some(),
            moves
        );
    }

    let agent = snapshot.worktree_groups[0].rows[0].as_agent_mut().unwrap();
    agent.pending_waits.clear();
    agent.sub_agent_count = 1;
    agent.sub_agents = vec![
        serde_json::from_value(serde_json::json!({
            "id": "child", "name": "child", "status": "running",
            "last_activity": snapshot.now,
        }))
        .unwrap(),
    ];
    assert!(animation_interval(&snapshot, &ui, 0, false).is_some());
    // Only a running child's head moves on the fast grid; a live child resting
    // in any other status, like a finished one, leaves the open section cold.
    for status in [
        crate::agents::AgentStatus::Idle,
        crate::agents::AgentStatus::Sleeping,
        crate::agents::AgentStatus::Waiting,
        crate::agents::AgentStatus::Paused,
        crate::agents::AgentStatus::Success,
    ] {
        snapshot.worktree_groups[0].rows[0]
            .as_agent_mut()
            .unwrap()
            .sub_agents[0]
            .status = status;
        assert!(
            animation_interval(&snapshot, &ui, 0, false).is_none(),
            "{status:?}"
        );
    }
}

#[test]
fn selected_blank_idle_agent_keeps_breath_grid_awake() {
    let ws = workspace();
    let mut snapshot = agent_snapshot(&ws);
    let agent = snapshot.worktree_groups[0].rows[0].as_agent_mut().unwrap();
    agent.task = None;
    agent.description = None;
    agent.prompt = None;

    let mut selected = selected_ui();
    selected.theme(&snapshot.theme);
    assert!(animation_interval(&snapshot, &selected, 0, false).is_some());
    assert_eq!(
        animation_interval(&snapshot, &selected, selected.animation_phase, false)
            .expect("animated"),
        crate::sidebar::timing::BREATH_ANIMATION_FRAME
    );

    let mut off_selection = UiState {
        selected_index: Some(99),
        ..Default::default()
    };
    off_selection.theme(&snapshot.theme);
    assert!(animation_interval(&snapshot, &off_selection, 0, false).is_none());

    snapshot.worktree_groups[0].rows[0]
        .as_agent_mut()
        .unwrap()
        .task = Some("warm up".to_owned());
    selected.theme(&snapshot.theme);
    assert!(animation_interval(&snapshot, &selected, 0, false).is_none());
}

#[test]
fn expanded_blank_team_member_keeps_breath_grid_awake() {
    let ws = workspace();
    let mut snapshot = agent_snapshot(&ws);
    let group = &mut snapshot.worktree_groups[0];
    group.rows[0].as_agent_mut().unwrap().team = Some("forge".to_owned());

    let mut teammate = group.rows[0].clone();
    teammate.id = "agent-2".to_owned();
    teammate.pane = Some(pane("terminal_10", "tab_0", false));
    let teammate_card = teammate.as_agent_mut().unwrap();
    teammate_card.team = Some("forge".to_owned());
    teammate_card.task = None;
    teammate_card.description = None;
    teammate_card.prompt = None;
    group.rows.push(teammate);

    let mut ui = selected_ui();
    ui.theme(&snapshot.theme);

    assert!(animation_interval(&snapshot, &ui, 0, false).is_some());
    assert_eq!(
        animation_interval(&snapshot, &ui, ui.animation_phase, false).expect("animated"),
        crate::sidebar::timing::BREATH_ANIMATION_FRAME
    );
}

#[test]
fn help_popup_keeps_animation_grid_hot() {
    let ws = workspace();
    let snapshot = snapshot(&ws);
    let mut ui = UiState {
        selected_index: Some(0),
        help_visible: true,
        ..Default::default()
    };
    ui.theme(&snapshot.theme);

    assert!(animation_interval(&snapshot, &ui, 0, false).is_some());
    assert_eq!(
        animation_interval(&snapshot, &ui, ui.animation_phase, false).expect("animated"),
        crate::sidebar::timing::animation_frame(
            crate::config::DisplayConfig::default().resolved_refresh_ms()
        )
    );
}

#[test]
fn pet_frame_interval_uses_pet_cadence_and_honours_static_motion() {
    let ws = workspace();
    let mut snapshot = snapshot(&ws);
    snapshot.theme.pets.enabled = true;
    let mut ui = UiState {
        selected_index: Some(0),
        pet: Some(crate::sidebar_pane::pets::PetView {
            body: Some(crate::sidebar_pane::pets::PetBody::Cell(vec![vec![
                crate::sidebar_pane::pets::PetCell {
                    ch: '▀',
                    fg: ratatui::style::Color::White,
                    bg: ratatui::style::Color::Black,
                },
            ]])),
            caption: Some("resting".to_owned()),
            frame_interval: Some(Duration::from_millis(625)),
        }),
        ..Default::default()
    };
    ui.theme(&snapshot.theme);

    assert!(animation_interval(&snapshot, &ui, 0, false).is_some());
    assert_eq!(
        animation_interval(&snapshot, &ui, ui.animation_phase, false).expect("animated"),
        Duration::from_millis(625)
    );

    let mut jumping_ui = ui.clone();
    jumping_ui.pet.as_mut().expect("pet").frame_interval = Some(Duration::from_millis(286));
    assert_eq!(
        animation_interval(&snapshot, &jumping_ui, jumping_ui.animation_phase, false)
            .expect("animated"),
        Duration::from_millis(286)
    );

    snapshot.theme.animations.idle =
        Some(toml::from_str("effect = \"static\"\n").expect("animation spec"));
    ui.theme(&snapshot.theme);
    ui.pet.as_mut().expect("pet").frame_interval = None;
    assert!(animation_interval(&snapshot, &ui, 0, false).is_none());

    snapshot.theme.animations.thinking =
        Some(toml::from_str("effect = \"static\"\n").expect("animation spec"));
    ui.theme(&snapshot.theme);
    assert!(
        animation_interval(&snapshot, &ui, 0, false).is_none(),
        "a static effect with omitted frames quiets spinner-role pets too"
    );
}

#[test]
fn active_alert_suppresses_hidden_pet_animation_cadence() {
    let ws = workspace();
    let mut snapshot = snapshot(&ws);
    snapshot.theme.pets.enabled = true;
    let mut ui = UiState {
        selected_index: Some(0),
        pet: Some(crate::sidebar_pane::pets::PetView {
            body: Some(crate::sidebar_pane::pets::PetBody::Cell(vec![vec![
                crate::sidebar_pane::pets::PetCell {
                    ch: '▀',
                    fg: ratatui::style::Color::White,
                    bg: ratatui::style::Color::Black,
                },
            ]])),
            caption: Some("resting".to_owned()),
            frame_interval: Some(Duration::from_millis(625)),
        }),
        ..Default::default()
    };
    ui.theme(&snapshot.theme);
    let alert_active = Alert::active("snapshot failed", snapshot.now).is_active();

    assert!(dashboard_present(&snapshot, false));
    assert!(!dashboard_present(&snapshot, alert_active));
    assert!(animation_interval(&snapshot, &ui, 0, alert_active).is_none());
    assert_eq!(
        animation_interval(&snapshot, &ui, ui.animation_phase, alert_active).unwrap_or(
            crate::sidebar::timing::animation_frame(snapshot.theme.display.resolved_refresh_ms())
        ),
        crate::sidebar::timing::animation_frame(
            crate::config::DisplayConfig::default().resolved_refresh_ms()
        )
    );

    if ui.cached_theme(&snapshot.theme).unwrap().pet_body_enabled() {
        assert!(animation_interval(&snapshot, &ui, 0, false).is_some());
        assert_eq!(
            animation_interval(&snapshot, &ui, ui.animation_phase, false).expect("animated"),
            Duration::from_millis(625)
        );
    }
}

#[test]
fn animation_cadence_separates_fast_work_from_breath_motion() {
    let running = snapshot_with(vec![agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    )]);
    assert_eq!(animation_cadence_for_test(&running), AnimationCadence::Fast);

    let mut waiting = snapshot_with(vec![agent(
        "claude-1",
        "claude",
        AgentStatus::Waiting,
        Some("/repo/main"),
        Some("main"),
        Some("allow cargo fmt"),
    )]);
    assert_eq!(
        animation_cadence_for_test(&waiting),
        AnimationCadence::None,
        "a read waiting row honours its resolved effect; the default single-frame static head paints nothing per-frame"
    );
    waiting.theme.animations.waiting =
        Some(toml::from_str::<AnimationSpec>("effect = \"breathe\"\n").expect("animation spec"));
    assert_eq!(
        animation_cadence_for_test(&waiting),
        AnimationCadence::Breath
    );
    waiting.theme.animations.waiting =
        Some(toml::from_str::<AnimationSpec>("effect = \"static\"\n").expect("animation spec"));
    assert_eq!(animation_cadence_for_test(&waiting), AnimationCadence::None);
    waiting.theme.animations.waiting = Some(
        toml::from_str::<AnimationSpec>("frames = \"?¿\"\neffect = \"static\"\n")
            .expect("animation spec"),
    );
    assert_eq!(
        animation_cadence_for_test(&waiting),
        AnimationCadence::Breath
    );

    let idle_empty = snapshot_with(vec![agent(
        "codex-1",
        "codex",
        AgentStatus::Idle,
        Some("/repo/main"),
        Some("main"),
        None,
    )]);
    assert_eq!(
        animation_cadence_for_test(&idle_empty),
        AnimationCadence::None
    );

    let mut reset_attention = idle_empty.clone();
    let mut codex = provider_panel("codex", "Codex", 33, true, false, Some((100, 20)));
    codex.reset_credits = Some(crate::ResetCredits {
        count: 1,
        soonest_expiry: None,
        expiries: Vec::new(),
        effect: crate::agents::RedeemEffect::RestartsWindow,
    });
    reset_attention.providers = vec![codex];
    assert_eq!(
        animation_cadence_for_test(&reset_attention),
        AnimationCadence::None,
        "a reset credit beside a spent window is a still marker in a quiet room"
    );

    let mut calm = snapshot_with(vec![agent(
        "claude-1",
        "claude",
        AgentStatus::Success,
        Some("/repo/main"),
        Some("main"),
        Some("done"),
    )]);
    assert_eq!(animation_cadence_for_test(&calm), AnimationCadence::None);

    // An unread `✓` result never leads the attention ladder: it settles to the
    // static bright crest, asking nothing of the breath grid. A static unread
    // row keeping the grid warm forever was the whole perf cost the lead-row
    // reservation removes.
    calm.worktree_groups[0].rows[0].unread = true;
    assert_eq!(
        animation_cadence_for_test(&calm),
        AnimationCadence::None,
        "an unread result settles to a static crest — no motion to keep the grid warm"
    );

    // The single lead unread row — the oldest actionable ask — wears the
    // continuous unread effect, so it does keep the breath grid alive.
    let mut lead = snapshot_with(vec![agent(
        "claude-1",
        "claude",
        AgentStatus::Waiting,
        Some("/repo/main"),
        Some("main"),
        Some("allow cargo fmt"),
    )]);
    lead.worktree_groups[0].rows[0].unread = true;
    assert_eq!(
        animation_cadence_for_test(&lead),
        AnimationCadence::Breath,
        "the lead unread ask flows its shimmer beam — continuous motion the grid serves"
    );
    // ...unless that effect is the held `bright` crest, which is static — then
    // even the lead asks nothing of the grid.
    lead.theme.animations.unread = Some(crate::config::UnreadEffect::Bright);
    assert_eq!(
        animation_cadence_for_test(&lead),
        AnimationCadence::None,
        "the `bright` unread crest holds still, so even the lead leaves the grid asleep"
    );
    // ...or its role is quieted to `static`, which stills the lead's motion too.
    lead.theme.animations.unread = None;
    lead.theme.animations.waiting =
        Some(toml::from_str::<AnimationSpec>("effect = \"static\"\n").expect("animation spec"));
    assert_eq!(
        animation_cadence_for_test(&lead),
        AnimationCadence::None,
        "a static-quieted waiting role stills the lead's unread motion"
    );

    let mut idle = snapshot_with(vec![agent(
        "claude-1",
        "claude",
        AgentStatus::Idle,
        Some("/repo/main"),
        Some("main"),
        None,
    )]);
    assert_eq!(animation_cadence_for_test(&idle), AnimationCadence::None);
    idle.theme.animations.idle =
        Some(toml::from_str::<AnimationSpec>("effect = \"breathe\"\n").expect("animation spec"));
    assert_eq!(animation_cadence_for_test(&idle), AnimationCadence::Breath);
}

#[test]
fn expanded_row_awaiting_first_prompt_tracks_selected_bare_idle_card() {
    let bare_idle = snapshot_with(vec![agent(
        "claude-1",
        "claude",
        AgentStatus::Idle,
        Some("/repo/main"),
        Some("main"),
        None,
    )]);

    assert!(expanded_row_awaiting_first_prompt(
        &bare_idle,
        &selected_ui()
    ));
    assert!(!expanded_row_awaiting_first_prompt(
        &bare_idle,
        &UiState {
            selected_index: Some(99),
            ..Default::default()
        }
    ));

    let described = snapshot_with(vec![agent(
        "claude-1",
        "claude",
        AgentStatus::Idle,
        Some("/repo/main"),
        Some("main"),
        Some("warm up"),
    )]);
    assert!(!expanded_row_awaiting_first_prompt(
        &described,
        &selected_ui()
    ));

    let mut used = snapshot_with(vec![agent(
        "claude-1",
        "claude",
        AgentStatus::Idle,
        Some("/repo/main"),
        Some("main"),
        None,
    )]);
    used.worktree_groups[0].rows[0]
        .as_agent_mut()
        .expect("agent row")
        .usage
        .total_tokens = Some(1);
    assert!(!expanded_row_awaiting_first_prompt(&used, &selected_ui()));

    let running = snapshot_with(vec![agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        None,
    )]);
    assert!(!expanded_row_awaiting_first_prompt(
        &running,
        &selected_ui()
    ));
}

#[test]
fn selected_pet_action_follows_the_focused_card() {
    let statuses = |statuses: &[(AgentStatus, crate::agents::TurnPhase)]| {
        snapshot_with(
            statuses
                .iter()
                .enumerate()
                .map(|(index, (status, phase))| {
                    let mut agent = agent(
                        &format!("agent-{index}"),
                        "claude",
                        *status,
                        Some("/repo/main"),
                        Some("main"),
                        None,
                    );
                    agent.phase = *phase;
                    agent
                })
                .collect(),
        )
    };

    let snapshot = statuses(&[
        (AgentStatus::Waiting, crate::agents::TurnPhase::Idle),
        (AgentStatus::Running, crate::agents::TurnPhase::Reasoning),
        (AgentStatus::Running, crate::agents::TurnPhase::Acting),
    ]);
    let ui = UiState {
        selected_index: Some(0),
        ..UiState::default()
    };
    assert_eq!(
        selected_pet_action(&snapshot, &ui),
        crate::sidebar_pane::pets::PetAction::Ask
    );
    let ui = UiState {
        selected_index: Some(1),
        ..UiState::default()
    };
    assert_eq!(
        selected_pet_action(&snapshot, &ui),
        crate::sidebar_pane::pets::PetAction::Thinking
    );
    let ui = UiState {
        selected_index: Some(2),
        ..UiState::default()
    };
    assert_eq!(
        selected_pet_action(&snapshot, &ui),
        crate::sidebar_pane::pets::PetAction::Running
    );

    let mut compacting = statuses(&[(AgentStatus::Running, crate::agents::TurnPhase::Acting)]);
    compacting.worktree_groups[0].rows[0]
        .as_agent_mut()
        .expect("agent row")
        .compacting = true;
    assert_eq!(
        selected_pet_action(&compacting, &selected_ui()),
        crate::sidebar_pane::pets::PetAction::Review
    );
    let mut compacting_waiting =
        statuses(&[(AgentStatus::Waiting, crate::agents::TurnPhase::Idle)]);
    compacting_waiting.worktree_groups[0].rows[0]
        .as_agent_mut()
        .expect("agent row")
        .compacting = true;
    assert_eq!(
        selected_pet_action(&compacting_waiting, &selected_ui()),
        crate::sidebar_pane::pets::PetAction::Review
    );

    let mut subagent = statuses(&[(AgentStatus::Running, crate::agents::TurnPhase::Acting)]);
    subagent.worktree_groups[0].rows[0]
        .as_agent_mut()
        .expect("agent row")
        .sub_agents
        .push(crate::store::snapshot::SidebarSubAgent {
            pane: None,
            turn_error_label: None,
            id: "child-1".to_owned(),
            prior_turn: false,
            name: "Explore".to_owned(),
            petname: None,
            provider_native: true,
            stalled: false,
            status: AgentStatus::Running,
            phase: crate::agents::TurnPhase::Reasoning,
            task: None,
            profile: None,
            model: None,
            effort: None,
            description: None,
            tokens: None,
            context_window: None,
            cost_usd: None,
            elapsed_secs: None,
            started_at: None,
            last_activity: fixed_now(),
            registered_at: Some(fixed_now()),
        });
    assert_eq!(
        selected_pet_action(&subagent, &selected_ui()),
        crate::sidebar_pane::pets::PetAction::Waiting
    );

    let parked = statuses(&[(AgentStatus::Running, crate::agents::TurnPhase::Parked)]);
    assert_eq!(
        selected_pet_action(&parked, &selected_ui()),
        crate::sidebar_pane::pets::PetAction::Idle
    );
    let sleeping = statuses(&[(AgentStatus::Sleeping, crate::agents::TurnPhase::Parked)]);
    assert_eq!(
        selected_pet_action(&sleeping, &selected_ui()),
        crate::sidebar_pane::pets::PetAction::Idle
    );
}

#[test]
fn selected_pet_action_follows_process_cards() {
    let mut snapshot = snapshot_with(vec![agent(
        "agent-1",
        "claude",
        AgentStatus::Idle,
        Some("/repo/main"),
        Some("main"),
        None,
    )]);
    snapshot.worktree_groups[0].rows = vec![crate::store::snapshot::SidebarRow {
        id: "process-1".to_owned(),
        name: "cargo".to_owned(),
        pane: None,
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: Some("main".to_owned()),
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        last_activity: fixed_now(),
        card: crate::store::snapshot::RowCard::Process(crate::store::snapshot::ProcessCard {
            state: crate::store::snapshot::ProcessState::Busy,
            ..crate::store::snapshot::ProcessCard::default()
        }),
    }];

    assert_eq!(
        selected_pet_action(&snapshot, &selected_ui()),
        crate::sidebar_pane::pets::PetAction::Running
    );
    snapshot.worktree_groups[0].rows[0]
        .as_process_mut()
        .expect("process row")
        .state = crate::store::snapshot::ProcessState::Stuck;
    assert_eq!(
        selected_pet_action(&snapshot, &selected_ui()),
        crate::sidebar_pane::pets::PetAction::Failed
    );
}
/// Honesty test: a running agent silent past the stall window is projected
/// to the attention bucket, so its cell reads as the attention `!` rather than
/// the working spinner — a wedged agent stops spinning and asks for a look.
/// The `!` pulses to draw the eye, but does not cycle the working braille.
#[test]
fn render_stalled_agent_reads_as_static_attention() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("waiting on tools"),
    );
    claude.last_activity = fixed_now()
        - Duration::from_secs(
            u64::from(
                crate::config::AttentionConfig::default()
                    .stalled_after_secs
                    .get(),
            ) + 60,
        );
    let snapshot = snapshot_with(vec![claude]);
    let first = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(0), 40, 16);
    let second = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(2), 40, 16);

    assert_eq!(first, second, "a stalled agent's cell must not spin");
    assert!(
        first.contains("! claude"),
        "stalled reads as attention:\n{first}"
    );
}

/// A running agent animates: advancing the phase advances the working fill,
/// regardless of how recently it last reported (the freshness freeze is
/// gone — staleness escalates to `!` instead of stopping the spinner).
#[test]
fn render_live_heads_follow_phase_and_turn_phase() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("reading"),
    );
    claude.phase = crate::agents::TurnPhase::Reasoning;
    let snapshot = snapshot_with(vec![claude]);
    let first = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(0), 40, 16);
    let second = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(1), 40, 16);

    assert!(
        first.contains("⠁ claude"),
        "the first thinking frame is the braille orbit:\n{first}"
    );
    assert!(
        second.contains("⠂ claude"),
        "fast thinking speed advances on the next tick:\n{second}"
    );
}

#[test]
fn custom_thinking_animation_changes_the_row_glyph_style_and_no_color_shape() {
    let mut theme_config = crate::config::ThemeConfig::default();
    theme_config.animations.thinking = Some(
        toml::from_str::<AnimationSpec>(
            "frames = \"AB\"\ncolor = 196\neffect = \"breathe\"\nspeed = \"fast\"\n",
        )
        .expect("animation spec"),
    );

    let lit = Theme::fixed_for_theme(false, &theme_config);
    assert_eq!(
        labels::agent_glyph(
            &lit,
            AgentStatus::Running,
            crate::agents::TurnPhase::Reasoning,
            1,
        ),
        "B"
    );
    let pulse_trough = labels::agent_role_style_at(
        &lit,
        AgentStatus::Running,
        crate::agents::TurnPhase::Reasoning,
        0,
    );
    assert!(matches!(pulse_trough.fg, Some(Color::Indexed(_))));
    assert!(
        pulse_trough.add_modifier.contains(Modifier::DIM),
        "indexed depth carries the breathe as a weight modifier over the base tone"
    );
    let pulse_peak = labels::agent_role_style_at(
        &lit,
        AgentStatus::Running,
        crate::agents::TurnPhase::Reasoning,
        6,
    );
    assert_ne!(
        pulse_trough, pulse_peak,
        "the indexed breathe changes the style by weight (DIM at the trough), not color"
    );

    let plain = Theme::fixed_for_theme(true, &theme_config);
    let plain_style = labels::agent_role_style_at(
        &plain,
        AgentStatus::Running,
        crate::agents::TurnPhase::Reasoning,
        0,
    );
    assert_eq!(plain_style.fg, None, "NO_COLOR strips only color");
    assert_eq!(
        labels::agent_glyph(
            &plain,
            AgentStatus::Running,
            crate::agents::TurnPhase::Reasoning,
            1,
        ),
        "B",
        "NO_COLOR keeps the themed glyph shape"
    );

    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("reading"),
    );
    claude.phase = crate::agents::TurnPhase::Reasoning;
    let mut snapshot = snapshot_with(vec![claude]);
    snapshot.theme = theme_config;
    let screen = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(1), 40, 16);
    assert!(
        screen.contains("B claude"),
        "custom frame reaches the row:\n{screen}"
    );
}

/// A card's `$cost` counts up through its eased roll: with a climb seeded
/// from $1.00 toward the snapshot's $1.27, the first click paints $1.11 (the
/// ease-out curve's first point over the 27¢ gap, rounded to cents) and a
/// settled frame paints the exact target — never a value past it. The golden
/// card snapshots stay on the unseeded path, where the painted cost is the
/// target itself.
#[test]
fn render_card_cost_ticks_toward_the_target() {
    let now = fixed_now();
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("compiling"),
    );
    claude.last_activity = now;
    claude.context = Some(claude_context(now));
    let snapshot = snapshot_with(vec![claude]);

    let mut ui = ui_at_phase(0);
    ui.cost_rolls
        .observe(vec![("claude-1".to_owned(), 1.0)].into_iter(), 0);
    ui.cost_rolls
        .observe(vec![("claude-1".to_owned(), 1.27)].into_iter(), 0);

    // One full click in — the roll sweeps every second animation phase.
    ui.animation_phase = 2;
    let mid = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, 44, 20);
    assert!(
        mid.contains("$1.11"),
        "one click in, the cost reads the curve's first point:\n{mid}"
    );

    ui.animation_phase = 60;
    let settled = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, 44, 20);
    assert!(
        settled.contains("$1.27"),
        "settled, the cost reads the exact target:\n{settled}"
    );
}
/// A running agent paused mid-turn on a provider limit leads with the `⏸`
/// pause and the cockpit gains an `⏸` bucket. It is static — parked, with
/// nothing to do until the provider recovers or the window resets.
#[test]
fn paused_agent_reads_as_a_static_pause() {
    let now = fixed_now();
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        None,
    );
    claude.last_activity = now - Duration::from_secs(60);
    claude.context = Some(AgentContext {
        turn_error: Some(AgentTurnError {
            class: TurnErrorClass::PausedOverloaded,
            at: now - Duration::from_secs(10),
            label: Some("API Error: Overloaded".to_owned()),
        }),
        ..claude_context(now)
    });
    let snapshot = snapshot_with(vec![claude]);
    let first = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(0), 44, 16);
    let second = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(2), 44, 16);
    assert_eq!(first, second, "a parked agent's head must not animate");
    assert!(
        first.contains('⏸'),
        "the paused row and cockpit show the pause:\n{first}"
    );
}
/// A running agent mid-compaction shows the pulsing compacting head instead
/// of the working spinner: it animates, and the working braille never
/// appears (the overlay replaced it). Short-lived, so it never enters the
/// cockpit tally.
#[test]
fn transient_live_heads_replace_the_working_spinner() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("condensing context"),
    );
    claude.compacting_since = Some(fixed_now());
    let snapshot = snapshot_with(vec![claude]);
    let first = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(0), 44, 16);
    let second = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(1), 44, 16);
    assert_ne!(first, second, "the compacting head animates");
    // The pulse bar (`▁` at phase 0) leads the row — unique to the compacting
    // head, so its presence proves the overlay replaced the working spinner.
    // (The cockpit's working *bucket* still shows `⢿`, which is expected.)
    assert!(
        first.contains('▁'),
        "the compacting head shows the pulse bar:\n{first}"
    );

    let parent = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("orchestrating"),
    );
    let mut kid = agent(
        "kid-1",
        "claude",
        AgentStatus::Running,
        None,
        None,
        Some("Explore"),
    );
    kid.parent_agent_id = Some("claude-1".into());
    let snapshot = snapshot_with(vec![parent, kid]);
    // Phase 2 of the wave is a distinctive braille edge, unique to the
    // delegated-wait head (the cockpit's working bucket still shows `⢿`).
    let first = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(2), 44, 16);
    let second = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(4), 44, 16);
    assert_ne!(first, second, "the delegated-wait head animates");
    assert!(
        first.contains('⢁'),
        "the parent shows the delegated-wait wave, not the working spinner:\n{first}"
    );
}
