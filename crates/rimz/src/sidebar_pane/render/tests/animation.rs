use super::*;

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
    });
    reset_attention.providers = vec![codex];
    assert_eq!(
        animation_cadence_for_test(&reset_attention),
        AnimationCadence::Breath,
        "a useful reset credit keeps its blink grid alive in a quiet room"
    );
    reset_attention.providers[0]
        .reset_credits
        .as_mut()
        .unwrap()
        .count = 0;
    assert_eq!(
        animation_cadence_for_test(&reset_attention),
        AnimationCadence::None
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
        &UiState {
            selected_index: 0,
            ..Default::default()
        }
    ));
    assert!(!expanded_row_awaiting_first_prompt(
        &bare_idle,
        &UiState {
            selected_index: 99,
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
        &UiState {
            selected_index: 0,
            ..Default::default()
        }
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
    assert!(!expanded_row_awaiting_first_prompt(
        &used,
        &UiState {
            selected_index: 0,
            ..Default::default()
        }
    ));

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
        &UiState {
            selected_index: 0,
            ..Default::default()
        }
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
        selected_index: 0,
        ..UiState::default()
    };
    assert_eq!(
        selected_pet_action(&snapshot, &ui),
        crate::sidebar_pane::pets::PetAction::Ask
    );
    let ui = UiState {
        selected_index: 1,
        ..UiState::default()
    };
    assert_eq!(
        selected_pet_action(&snapshot, &ui),
        crate::sidebar_pane::pets::PetAction::Thinking
    );
    let ui = UiState {
        selected_index: 2,
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
        selected_pet_action(&compacting, &UiState::default()),
        crate::sidebar_pane::pets::PetAction::Review
    );
    let mut compacting_waiting =
        statuses(&[(AgentStatus::Waiting, crate::agents::TurnPhase::Idle)]);
    compacting_waiting.worktree_groups[0].rows[0]
        .as_agent_mut()
        .expect("agent row")
        .compacting = true;
    assert_eq!(
        selected_pet_action(&compacting_waiting, &UiState::default()),
        crate::sidebar_pane::pets::PetAction::Review
    );

    let mut subagent = statuses(&[(AgentStatus::Running, crate::agents::TurnPhase::Acting)]);
    subagent.worktree_groups[0].rows[0]
        .as_agent_mut()
        .expect("agent row")
        .sub_agents
        .push(crate::SidebarSubAgent {
            id: "child-1".to_owned(),
            name: "Explore".to_owned(),
            status: AgentStatus::Running,
            phase: crate::agents::TurnPhase::Reasoning,
            task: None,
            model: None,
            effort: None,
            description: None,
            total_tokens: None,
            cost_usd: None,
            elapsed_secs: None,
            started_at: None,
            last_activity: fixed_now(),
            registered_at: Some(fixed_now()),
        });
    assert_eq!(
        selected_pet_action(&subagent, &UiState::default()),
        crate::sidebar_pane::pets::PetAction::Waiting
    );

    let parked = statuses(&[(AgentStatus::Running, crate::agents::TurnPhase::Parked)]);
    assert_eq!(
        selected_pet_action(&parked, &UiState::default()),
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
    snapshot.worktree_groups[0].rows = vec![crate::SidebarRow {
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
        card: crate::RowCard::Process(crate::ProcessCard {
            state: crate::ProcessState::Busy,
            ..crate::ProcessCard::default()
        }),
    }];

    assert_eq!(
        selected_pet_action(&snapshot, &UiState::default()),
        crate::sidebar_pane::pets::PetAction::Running
    );
    snapshot.worktree_groups[0].rows[0]
        .as_process_mut()
        .expect("process row")
        .state = crate::ProcessState::Stuck;
    assert_eq!(
        selected_pet_action(&snapshot, &UiState::default()),
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
    claude.last_activity =
        fixed_now() - Duration::from_secs(u64::from(crate::agents::DEFAULT_STALL_AFTER_SECS) + 60);
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
