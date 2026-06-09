use super::*;

#[test]
fn effects_pass_never_changes_the_composed_text() {
    // The color-only invariant behind every golden frame: the truecolor
    // effects pass at its busiest — a fresh `waiting` transition flash decaying
    // over its card plus the attention glow mid-swell — must leave the composed
    // text byte-identical to a render without it. A tachyonfx effect that
    // mutated a glyph (dissolve, char evolution) would fail here.
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
        // spawning the flash at full tone; frame 3 paints it mid-decay with
        // the glow at the breath's half-swell. The pass is driven directly so
        // the guard holds whatever `COLORTERM` the test shell has.
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
fn glow_always_recolors_the_attention_glyph_without_colorterm() {
    // The golden guard above proves the pass never changes the text; this is
    // its color twin — proof the pass actually paints. Driven end-to-end
    // through the real in-draw gate (`Theme::for_sidebar` →
    // `effects_enabled`): `glow = "always"` must lift the waiting glyph to a
    // truecolor tone with no `COLORTERM` in the environment (the SSH case the
    // mode exists for), and `never` must leave the composed indexed tone
    // untouched.
    use crate::config::GlowMode;
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
    let glyph_fg = |snapshot: &SidebarSnapshot| -> vt100::Color {
        let mut bytes = Vec::new();
        let backend = CrosstermBackend::new(&mut bytes);
        let viewport = Viewport::Fixed(Rect::new(0, 0, 44, 18));
        let mut terminal = Terminal::with_options(backend, TerminalOptions { viewport }).unwrap();
        terminal.clear().unwrap();
        let mut ui = UiState {
            // Mid-breath: the swell's peak, where the lift is unmistakable.
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

    snapshot.sidebar.glow = GlowMode::Always;
    let lifted = glyph_fg(&snapshot);
    snapshot.sidebar.glow = GlowMode::Never;
    let resting = glyph_fg(&snapshot);

    assert!(
        matches!(lifted, vt100::Color::Rgb(..)),
        "the forced pass lifts the glyph to a truecolor tone, got {lifted:?}"
    );
    assert_ne!(
        lifted, resting,
        "glow = \"always\" must visibly recolor what \"never\" leaves alone"
    );
}
#[test]
fn animation_cadence_separates_fast_work_from_slow_cosmetic_motion() {
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

    let waiting = snapshot_with(
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
    assert_eq!(animation_cadence(&waiting), AnimationCadence::Slow);

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

    let calm = snapshot_with(
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
}
/// Honesty test: a running agent silent past the stall window is projected
/// to the attention bucket, so its cell reads as the attention `!` rather than
/// the working spinner — a wedged agent stops spinning and asks for a look.
/// (The `!` slowly blinks to draw the eye, but does not cycle the working
/// braille; phases 0 and 2 both fall in the blink's shown window.)
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
        fixed_now() - Duration::from_secs(u64::from(crate::feed::DEFAULT_STALL_AFTER_SECS) + 60);
    let snapshot = snapshot_with(Vec::new(), vec![claude]);
    let first = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(0), 40, 10);
    let second = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(2), 40, 10);

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
fn render_running_head_spins_with_the_phase() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("compiling"),
    );
    claude.last_activity = fixed_now() - Duration::from_secs(30);
    let snapshot = snapshot_with(Vec::new(), vec![claude]);
    let first = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(0), 40, 10);
    let second = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(1), 40, 10);

    assert_ne!(
        first, second,
        "a running agent's head must advance with the phase"
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
    let first = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(0), 44, 10);
    let second = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(2), 44, 10);
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
fn compacting_head_pulses_over_the_working_spinner() {
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
    let first = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(0), 44, 10);
    let second = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(1), 44, 10);
    assert_ne!(first, second, "the compacting head animates");
    // The pulse bar (`▁` at phase 0) leads the row — unique to the compacting
    // head, so its presence proves the overlay replaced the working spinner.
    // (The cockpit's working *bucket* still shows `⢿`, which is expected.)
    assert!(
        first.contains('▁'),
        "the compacting head shows the pulse bar:\n{first}"
    );
}
/// A running parent with a live subagent shows the quiet delegated-wait head,
/// not the working spinner — the work is in the child below. It animates, and
/// the working braille never appears on the parent's collapsed row.
#[test]
fn waiting_on_subagents_head_replaces_the_working_spinner() {
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
    // Phase 2 of the wave is a distinctive backtick, unique to the
    // delegated-wait head (the cockpit's working bucket still shows `⢿`).
    let first = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(2), 44, 10);
    let second = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(4), 44, 10);
    assert_ne!(first, second, "the delegated-wait head animates");
    assert!(
        first.contains('`'),
        "the parent shows the delegated-wait wave, not the working spinner:\n{first}"
    );
}
