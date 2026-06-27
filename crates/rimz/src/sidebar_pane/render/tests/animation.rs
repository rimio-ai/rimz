use super::*;

#[test]
fn effects_pass_never_changes_the_composed_text() {
    // The color-only invariant behind every golden frame: the truecolor
    // effects pass at its busiest — a fresh `waiting` transition flash decaying
    // over its card — must leave the composed text byte-identical to a render
    // without it. A tachyonfx effect that mutated a glyph (dissolve, char
    // evolution) would fail here.
    let make = |status: AgentStatus| {
        snapshot_with(
            Vec::new(),
            vec![agent(
                "claude-1",
                "claude",
                status,
                Some("/repo/main"),
                Some("main"),
                Some("db migrate"),
            )],
        )
    };
    let idle = make(AgentStatus::Idle);
    let waiting = make(AgentStatus::Waiting);

    let with_effects = {
        let mut bytes = Vec::new();
        let backend = CrosstermBackend::new(&mut bytes);
        let viewport = Viewport::Fixed(Rect::new(0, 0, 44, 18));
        let mut terminal = Terminal::with_options(backend, TerminalOptions { viewport }).unwrap();
        terminal.clear().unwrap();
        let mut ui = UiState::default();
        // Frame 1 records the room; frame 2 flips the agent to `waiting`,
        // spawning the flash at full tone; frame 3 paints it mid-decay. The
        // pass is driven directly so the guard holds whatever truecolor signal
        // the test shell has.
        for (snapshot, phase) in [(&idle, 0), (&waiting, 6), (&waiting, 7)] {
            let mut effects = std::mem::take(&mut ui.effects);
            ui.animation_phase = phase;
            terminal
                .draw(|frame| {
                    draw_with_ui(frame, snapshot, None, &mut ui);
                    let area = frame.area();
                    effects.apply(
                        snapshot,
                        &Theme::fixed(false),
                        None,
                        &ui.line_map,
                        None,
                        phase,
                        frame.buffer_mut(),
                        area,
                    );
                })
                .unwrap();
            ui.effects = effects;
        }
        assert!(
            ui.effects.any_active(),
            "the guard must exercise a live, mid-decay flash"
        );
        drop(terminal);
        let mut parser = vt100::Parser::new(18, 44, 0);
        parser.process(&bytes);
        parser.screen().contents()
    };

    let without_effects = snapshot_to_screen_with_alert_and_ui(
        &waiting,
        None,
        &UiState {
            animation_phase: 7,
            ..UiState::default()
        },
        44,
        18,
    );
    assert_eq!(
        snapshot_text(&with_effects),
        snapshot_text(&without_effects),
        "effects are color-only; the composed text may never drift"
    );
}
#[test]
fn glow_gates_transition_flashes_not_the_steady_pulse() {
    // The steady attention pulse is owned by base composition. The post-render
    // pass still handles transition flashes, gated end-to-end through
    // `Theme::for_sidebar` -> `effects_enabled`.
    use crate::config::{GlowMode, ThemeMode};
    // The end-to-end gate keeps one env coupling `always` honours: `NO_COLOR`
    // beats every mode. Surface a colorful-output-suppressing harness env as
    // itself rather than as a mysterious color assertion below.
    assert!(
        std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty()),
        "this test needs NO_COLOR unset: the in-draw gate honours it over every glow mode"
    );
    let mut snapshot = snapshot_with(
        Vec::new(),
        vec![agent(
            "claude-1",
            "claude",
            AgentStatus::Waiting,
            Some("/repo/main"),
            Some("main"),
            Some("db migrate"),
        )],
    );
    snapshot.theme.mode = ThemeMode::Truecolor;
    let glyph_fg = |snapshot: &SidebarSnapshot| -> vt100::Color {
        let mut bytes = Vec::new();
        let backend = CrosstermBackend::new(&mut bytes);
        let viewport = Viewport::Fixed(Rect::new(0, 0, 44, 18));
        let mut terminal = Terminal::with_options(backend, TerminalOptions { viewport }).unwrap();
        terminal.clear().unwrap();
        let mut ui = UiState {
            // On the blink's bright on-pole, where the lift is unmistakable.
            animation_phase: 6,
            ..UiState::default()
        };
        draw_to_terminal_with_ui(&mut terminal, snapshot, None, &mut ui).unwrap();
        drop(terminal);
        let mut parser = vt100::Parser::new(18, 44, 0);
        parser.process(&bytes);
        let screen = parser.screen();
        // Screen cells index by column, not byte — the `▌` gutter ahead of
        // the glyph is 3 bytes but 1 column, so count chars to land on the
        // `?` cell itself.
        let (row, col) = screen
            .contents()
            .lines()
            .enumerate()
            .filter(|(_, line)| line.contains("? claude"))
            .find_map(|(row, line)| Some((row, line.chars().position(|c| c == '?')?)))
            .expect("the waiting card renders its `? claude` identity line");
        screen.cell(row as u16, col as u16).unwrap().fgcolor()
    };

    snapshot.theme.display.glow = GlowMode::Always;
    let always = glyph_fg(&snapshot);
    snapshot.theme.display.glow = GlowMode::Never;
    let never = glyph_fg(&snapshot);
    assert_eq!(
        always, never,
        "glow no longer owns the continuous pulse; the base glyph color stays stable"
    );

    let flash_active = |glow| {
        let mut idle = snapshot_with(
            Vec::new(),
            vec![agent(
                "claude-1",
                "claude",
                AgentStatus::Idle,
                Some("/repo/main"),
                Some("main"),
                Some("db migrate"),
            )],
        );
        idle.theme.mode = ThemeMode::Truecolor;
        idle.theme.display.glow = glow;
        let mut waiting = snapshot.clone();
        waiting.theme.display.glow = glow;
        let mut bytes = Vec::new();
        let backend = CrosstermBackend::new(&mut bytes);
        let viewport = Viewport::Fixed(Rect::new(0, 0, 44, 18));
        let mut terminal = Terminal::with_options(backend, TerminalOptions { viewport }).unwrap();
        terminal.clear().unwrap();
        let mut ui = UiState::default();
        draw_to_terminal_with_ui(&mut terminal, &idle, None, &mut ui).unwrap();
        ui.animation_phase = 1;
        draw_to_terminal_with_ui(&mut terminal, &waiting, None, &mut ui).unwrap();
        ui.effects.any_active()
    };
    assert!(
        flash_active(GlowMode::Always),
        "glow = \"always\" enables the transition flash tier"
    );
    assert!(
        !flash_active(GlowMode::Never),
        "glow = \"never\" skips transition observation and painting"
    );
}
#[test]
fn animation_cadence_separates_fast_work_from_breath_motion() {
    let running = snapshot_with(
        Vec::new(),
        vec![agent(
            "claude-1",
            "claude",
            AgentStatus::Running,
            Some("/repo/main"),
            Some("main"),
            Some("db migrate"),
        )],
    );
    assert_eq!(animation_cadence(&running), AnimationCadence::Fast);

    let mut waiting = snapshot_with(
        Vec::new(),
        vec![agent(
            "claude-1",
            "claude",
            AgentStatus::Waiting,
            Some("/repo/main"),
            Some("main"),
            Some("allow cargo fmt"),
        )],
    );
    assert_eq!(animation_cadence(&waiting), AnimationCadence::Breath);
    waiting.theme.animations.waiting =
        Some(toml::from_str::<AnimationSpec>("effect = \"static\"\n").expect("animation spec"));
    assert_eq!(animation_cadence(&waiting), AnimationCadence::None);
    waiting.theme.animations.waiting = Some(
        toml::from_str::<AnimationSpec>("frames = \"?¿\"\neffect = \"static\"\n")
            .expect("animation spec"),
    );
    assert_eq!(animation_cadence(&waiting), AnimationCadence::Breath);

    let idle_empty = snapshot_with(
        Vec::new(),
        vec![agent(
            "codex-1",
            "codex",
            AgentStatus::Idle,
            Some("/repo/main"),
            Some("main"),
            None,
        )],
    );
    assert_eq!(animation_cadence(&idle_empty), AnimationCadence::None);

    let mut calm = snapshot_with(
        Vec::new(),
        vec![agent(
            "claude-1",
            "claude",
            AgentStatus::Success,
            Some("/repo/main"),
            Some("main"),
            Some("done"),
        )],
    );
    assert_eq!(animation_cadence(&calm), AnimationCadence::None);

    // An unread `✓` result never leads the attention ladder: it settles to the
    // static bright crest, asking nothing of the breath grid. A static unread
    // row keeping the grid warm forever was the whole perf cost the lead-row
    // reservation removes.
    calm.worktree_groups[0].rows[0].unread = true;
    assert_eq!(
        animation_cadence(&calm),
        AnimationCadence::None,
        "an unread result settles to a static crest — no motion to keep the grid warm"
    );

    // The single lead unread row — the oldest actionable ask — wears the
    // continuous unread effect, so it does keep the breath grid alive.
    let mut lead = snapshot_with(
        Vec::new(),
        vec![agent(
            "claude-1",
            "claude",
            AgentStatus::Waiting,
            Some("/repo/main"),
            Some("main"),
            Some("allow cargo fmt"),
        )],
    );
    lead.worktree_groups[0].rows[0].unread = true;
    assert_eq!(
        animation_cadence(&lead),
        AnimationCadence::Breath,
        "the lead unread ask flows its shimmer beam — continuous motion the grid serves"
    );
    // ...unless that effect is the held `bright` crest, which is static — then
    // even the lead asks nothing of the grid.
    lead.theme.animations.unread = Some(crate::config::UnreadEffect::Bright);
    assert_eq!(
        animation_cadence(&lead),
        AnimationCadence::None,
        "the `bright` unread crest holds still, so even the lead leaves the grid asleep"
    );
    // ...or its role is quieted to `static`, which stills the lead's motion too.
    lead.theme.animations.unread = None;
    lead.theme.animations.waiting =
        Some(toml::from_str::<AnimationSpec>("effect = \"static\"\n").expect("animation spec"));
    assert_eq!(
        animation_cadence(&lead),
        AnimationCadence::None,
        "a static-quieted waiting role stills the lead's unread motion"
    );

    let mut idle = snapshot_with(
        Vec::new(),
        vec![agent(
            "claude-1",
            "claude",
            AgentStatus::Idle,
            Some("/repo/main"),
            Some("main"),
            None,
        )],
    );
    assert_eq!(animation_cadence(&idle), AnimationCadence::None);
    idle.theme.animations.idle =
        Some(toml::from_str::<AnimationSpec>("effect = \"breathe\"\n").expect("animation spec"));
    assert_eq!(animation_cadence(&idle), AnimationCadence::Breath);
}

#[test]
fn selected_pet_action_follows_the_focused_card() {
    let statuses = |statuses: &[(AgentStatus, crate::agents::TurnPhase)]| {
        snapshot_with(
            Vec::new(),
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
        crate::sidebar_pane::pets::PetAction::Waiting
    );
}

#[test]
fn selected_pet_action_follows_process_cards() {
    let mut snapshot = snapshot_with(
        Vec::new(),
        vec![agent(
            "agent-1",
            "claude",
            AgentStatus::Idle,
            Some("/repo/main"),
            Some("main"),
            None,
        )],
    );
    snapshot.worktree_groups[0].rows = vec![crate::SidebarRow {
        id: "process-1".to_owned(),
        name: "cargo".to_owned(),
        pane: None,
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: Some("main".to_owned()),
        channel: None,
        unread: false,
        inactive: false,
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
    let snapshot = snapshot_with(Vec::new(), vec![claude]);
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
    let snapshot = snapshot_with(Vec::new(), vec![claude]);
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
    let mut snapshot = snapshot_with(Vec::new(), vec![claude]);
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
    let snapshot = snapshot_with(Vec::new(), vec![claude]);

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
    let snapshot = snapshot_with(Vec::new(), vec![claude]);
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
    let snapshot = snapshot_with(Vec::new(), vec![claude]);
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
    let snapshot = snapshot_with(Vec::new(), vec![parent, kid]);
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
