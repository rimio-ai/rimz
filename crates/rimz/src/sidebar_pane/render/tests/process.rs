use super::*;
use crate::sidebar_pane::render::theme::Component;

#[test]
fn process_and_not_tracked_heads_draw_no_room_slot() {
    for not_tracked in [false, true] {
        let mut snapshot = not_tracked_snapshot("codex", "allow reporting");
        if !not_tracked {
            std::sync::Arc::make_mut(
                &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
            )[0]
            .as_process_mut()
            .unwrap()
            .not_tracked = None;
        }
        let lines = group_lines(&snapshot, &Theme::fixed(false), usize::MAX);
        let head = lines
            .iter()
            .find(|line| line.spans.iter().any(|span| span.content == "codex"))
            .unwrap();
        assert!(
            head.spans.iter().all(|span| span.content != " 1"),
            "{head:?}"
        );
    }
}

fn not_tracked_snapshot(label: &str, fix: &str) -> SidebarSnapshot {
    let mut snapshot =
        snapshot_with(Vec::new()).with_live_panes(vec![pane("%1", "codex", "/repo/main")], None);
    let row = &mut std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0];
    row.name = label.to_owned();
    let process = row.as_process_mut().unwrap();
    process.not_tracked = Some(crate::agents::NotTracked {
        reason: crate::agents::NotTrackedReason::HooksMissing,
        fix: fix.to_owned(),
    });
    process.command_detail = Some("hidden command detail".to_owned());
    process.cpu_pct = Some(34);
    process.rss_kb = Some(512 * 1_024);
    process.io_bps = Some(8 * 1_048_576);
    snapshot
}

#[test]
fn not_tracked_rows_keep_two_lines_across_widths_and_selection() {
    let snapshot = not_tracked_snapshot(
        "untracked-agent-pane",
        "rimz hooks install codex, then restart",
    );
    for width in [24, 30, 31, 32, 33, 54] {
        for selected in [false, true] {
            let view = if selected {
                selected_ui()
            } else {
                ViewState::default()
            };
            let screen = snapshot_to_screen_with_alert_and_ui(
                &snapshot,
                None,
                &view,
                &UiState::default(),
                width,
                16,
            );
            assert!(
                screen.contains(" · not tracked"),
                "width={width}:\n{screen}"
            );
            let lines = screen.lines().collect::<Vec<_>>();
            let title = lines
                .iter()
                .position(|line| line.contains("not tracked"))
                .unwrap();
            assert!(lines[title + 1].contains("rimz hooks install"), "{screen}");
            assert_eq!(lines[title + 1].contains("tip:"), width >= 32, "{screen}");
            assert!(!screen.contains("hidden command detail"), "{screen}");
            assert!(!screen.contains("512M"), "{screen}");
            if width == 24 {
                assert!(lines[title].contains("○ untra… · not tracked"), "{screen}");
            }
            assert_snapshot(
                &format!(
                    "not_tracked_{width}_{}",
                    if selected { "selected" } else { "resting" }
                ),
                screen,
            );
        }
    }
}

#[test]
fn not_tracked_fix_cuts_words_commas_and_overwide_first_words() {
    for (name, width, fix, expected) in [
        (
            "words",
            30,
            "rimz hooks install codex, then restart",
            "rimz hooks install codex…",
        ),
        (
            "comma",
            24,
            "allow reporting, then restart",
            "allow reporting…",
        ),
        (
            "long_word",
            30,
            "abcdefghijklmnopqrstuvwxyz0123456789",
            "abcdefghijklmnopqrstuvwxy…",
        ),
        (
            "wide_word",
            24,
            "設定設定設定設定設定設定設定設定",
            "設定設定設定設定設…",
        ),
    ] {
        let snapshot = not_tracked_snapshot("codex", fix);
        let screen = snapshot_to_screen(&snapshot, width, 16);
        assert!(screen.contains(expected), "{name}:\n{screen}");
        assert_snapshot(&format!("not_tracked_fix_{name}"), screen);
    }
}

#[test]
fn not_tracked_stuck_row_keeps_tip_without_stats_in_color_and_plain() {
    let mut snapshot = not_tracked_snapshot("codex", "allow reporting, then restart");
    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0]
    .as_process_mut()
    .unwrap()
    .state = crate::store::snapshot::ProcessState::Stuck;
    for plain in [false, true] {
        let pane_ui = UiState {
            theme_cache: Some((snapshot.theme.clone(), Rc::new(Theme::fixed(plain)))),
            ..Default::default()
        };
        let screen =
            snapshot_to_screen_with_alert_and_ui(&snapshot, None, &selected_ui(), &pane_ui, 54, 16);
        assert!(screen.contains("! codex · not tracked"), "{screen}");
        assert!(
            screen.contains("tip: allow reporting, then restart"),
            "{screen}"
        );
        assert!(!screen.contains("512M"), "{screen}");
        assert!(!screen.contains("hidden command detail"), "{screen}");
        assert_snapshot(
            &format!(
                "not_tracked_stuck_{}",
                if plain { "plain" } else { "color" }
            ),
            screen,
        );
    }
}

#[test]
fn remote_control_host_pane_is_filtered_not_rendered() {
    // A `claude remote-control` pane is host infrastructure, not a coding
    // agent: the snapshot reducer filters it out, so it never reads as a
    // `claude` row. Remote control surfaces as a `⇅ rc` flag on the provider
    // dashboard (covered by the section tests), never as its own row.
    let snapshot = snapshot_with(Vec::new()).with_live_panes(
        vec![
            pane("%1", "zsh", "/repo/main"),
            pane("%2", "claude remote-control --spawn worktree", "/repo/main"),
        ],
        None,
    );
    let screen = snapshot_to_screen(&snapshot, 32, 24);
    assert!(
        screen.contains("○ zsh"),
        "the plain shell still renders:\n{screen}"
    );
    assert!(
        !screen.contains("○ claude"),
        "the rc host must not read as a claude process row:\n{screen}",
    );
    assert!(
        !screen.contains("remote control"),
        "the rc host is filtered, not a pinned row:\n{screen}",
    );
}
#[test]
fn process_row_resource_stats_follow_busy_state() {
    for (command, cpu, expected, unexpected, snapshot_name) in [
        (
            "cargo build --release",
            34,
            "C  34%  M 512M  ⇅   8M/s",
            "",
            Some("process_row_resource_stats"),
        ),
        ("zsh", 1, "○ zsh", "C   1%|M 512M|8M/s", None),
    ] {
        let pane = pane("%1", command, "/repo/main");
        let mut snapshot = snapshot_with(Vec::new())
            .with_worktree_roots(vec!["/repo/main".into()], Default::default())
            .with_live_panes(vec![pane], None);
        let row = &mut std::sync::Arc::make_mut(
            &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
        )[0];
        let process = row.as_process_mut().unwrap();
        process.cpu_pct = Some(cpu);
        process.rss_kb = Some(512 * 1_024);
        process.io_bps = Some(8 * 1_048_576);

        let rendered = snapshot_to_screen(&snapshot, 56, 14);

        assert!(
            rendered.contains(expected),
            "process row expectation failed:\n{rendered}"
        );
        if command == "cargo build --release" {
            assert!(
                rendered.contains(command),
                "the detail line carries the full command:\n{rendered}"
            );
        }
        for needle in unexpected.split('|').filter(|needle| !needle.is_empty()) {
            assert!(
                !rendered.contains(needle),
                "idle process rows hide {needle}:\n{rendered}"
            );
        }
        if let Some(name) = snapshot_name {
            assert_snapshot(name, rendered);
        }
    }
}
#[test]
fn proc_stats_hold_a_fixed_dim_grid() {
    // Each marker wears its own DIM-weighted tone — `C` sky, `M` sage, `⇅`
    // violet — while figures and seams stay in the dim process tone, and each
    // figure right-aligns into its fixed slot so a changing magnitude never
    // walks the cluster sideways.
    use ratatui::style::Modifier;

    let busy = pane("%1", "cargo build --release", "/repo/main");
    let mut snapshot = snapshot_with(Vec::new()).with_live_panes(vec![busy], None);
    let row = &mut std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0];
    let process = row.as_process_mut().unwrap();
    process.cpu_pct = Some(34);
    process.rss_kb = Some(512 * 1_024);
    process.io_bps = Some(8 * 1_048_576);
    let row = &snapshot.worktree_groups[0].rows[0];

    let theme = Theme::fixed(false);
    let spans = sections::proc_stats_spans(&theme, row);
    let text: String = spans.iter().map(|span| span.content.as_ref()).collect();
    assert_eq!(text, "C  34%  M 512M  ⇅   8M/s");
    let dim = theme.muted();
    let markers = [
        ("C ", Component::ProcCpu),
        ("M ", Component::ProcMem),
        ("⇅ ", Component::ProcIo),
    ];
    for (marker, component) in markers {
        let span = spans
            .iter()
            .find(|span| span.content.as_ref() == marker)
            .unwrap_or_else(|| panic!("marker {marker:?} missing from the grid"));
        assert_eq!(
            span.style,
            theme.styled(component, Modifier::DIM),
            "marker {marker:?} wears its own DIM-weighted tone"
        );
    }
    assert!(
        spans
            .iter()
            .filter(|span| markers.iter().all(|(marker, _)| span.content != *marker))
            .all(|span| span.style == dim),
        "figures and seams stay in the dim process tone"
    );

    // A figure changing magnitude re-aligns within its slot; the grid width
    // holds, so the right-pinned cluster never moves.
    let mut hotter = row.clone();
    let process = hotter.as_process_mut().unwrap();
    process.cpu_pct = Some(100);
    process.rss_kb = Some(1_153_024); // 1.1 GiB
    process.io_bps = Some(450 * 1_024);
    let shifted: String = sections::proc_stats_spans(&theme, &hotter)
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert_eq!(shifted, "C 100%  M 1.1G  ⇅ 450k/s");
    assert_eq!(text.chars().count(), shifted.chars().count());

    // A partial reading (rates still warming on the first tick) stays hidden:
    // CPU, memory, and IO appear together or not at all.
    let mut first_tick = row.clone();
    let process = first_tick.as_process_mut().unwrap();
    process.cpu_pct = None;
    process.io_bps = None;
    let blanked: String = sections::proc_stats_spans(&theme, &first_tick)
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert_eq!(blanked, "");

    // NO_COLOR keeps the shape and sheds every tone.
    let plain = sections::proc_stats_spans(&Theme::fixed(true), row);
    assert!(plain.iter().all(|span| span.style.fg.is_none()));
}
#[test]
fn render_process_rows_between_calm_cards_without_a_seam() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    claude.registered_at = Some(fixed_now() - Duration::from_secs(20));
    let stamped = pane("%1", "claude", "/repo/main");
    claude.pane = Some(stamped.clone());
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Idle,
        Some("/repo/main"),
        Some("main"),
        None,
    );
    codex.registered_at = Some(fixed_now() - Duration::from_secs(1));
    let codex_pane = pane("%3", "codex", "/repo/main");
    codex.pane = Some(codex_pane.clone());
    let mut shell = pane("%2", "zsh", "/repo/main");
    shell.first_seen_at_ms = Some((fixed_now().as_millisecond() - 10_000) as u64);
    let snapshot =
        snapshot_with(vec![claude, codex]).with_live_panes(vec![stamped, shell, codex_pane], None);

    let rendered = snapshot_to_screen(&snapshot, 44, 24);

    assert!(
        !rendered.contains("┄ commands"),
        "no seam line spends a row on the agent/process boundary:\n{rendered}"
    );
    assert!(
        rendered.contains("○ zsh"),
        "the process row renders:\n{rendered}"
    );
    assert!(rendered.find("codex").unwrap() < rendered.find("zsh").unwrap());
    assert!(rendered.find("zsh").unwrap() < rendered.find("claude").unwrap());
    assert_snapshot("agents_process_interleaved", rendered);
}
/// A process row reads quieter than an agent card: the lead glyph
/// carries the DIM modifier over its status style (neutral idle, work-clay
/// active) and the program name wears the soft middle tier — which
/// itself falls back to the bare DIM weight under `NO_COLOR`, so the
/// stripped color still sets processes apart.
#[test]
fn process_rows_dim_a_step_below_agent_cards() {
    for no_color in [false, true] {
        let theme = Theme::fixed(no_color);
        let mut claude = agent(
            "claude-1",
            "claude",
            AgentStatus::Running,
            Some("/repo/main"),
            Some("main"),
            Some("db migrate"),
        );
        let stamped = pane("%1", "claude", "/repo/main");
        claude.pane = Some(stamped.clone());
        let shell = pane("%2", "zsh", "/repo/main");
        let build = pane("%3", "cargo build --release", "/repo/main");
        let snapshot =
            snapshot_with(vec![claude]).with_live_panes(vec![stamped, shell, build], None);

        let cost_rolls = CostRolls::default();
        let ctx = test_row_ctx(&snapshot, &theme, 44, 0, 0, &cost_rolls);
        let lines = worktree_group_block(&ctx, &snapshot.worktree_groups[0], false, None).lines;

        let span_style = |content: &str| {
            lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .find(|span| span.content.as_ref() == content)
                .map(|span| span.style)
                .unwrap_or_else(|| panic!("the tail renders a {content} span"))
        };
        assert_eq!(
            span_style("zsh"),
            theme.body(),
            "the program name drops to the soft tier below full strength (no_color={no_color})"
        );
        assert_eq!(
            span_style("○"),
            labels::status_style(&theme, AgentStatus::Idle).add_modifier(Modifier::DIM),
            "the idle lead keeps its neutral style one DIM step down (no_color={no_color})"
        );
        let active_lead = theme.style(theme.clay(), Modifier::DIM);
        assert!(
            lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .any(|span| span.style == active_lead),
            "the active pane's spinner wears the dimmed work clay (no_color={no_color})"
        );
    }
}

#[test]
fn active_process_rows_use_the_configured_working_animation_style() {
    let mut theme_config = crate::config::ThemeConfig::default();
    theme_config.animations.working = Some(
        toml::from_str::<AnimationSpec>(
            "frames = \"AB\"\ncolor = 196\neffect = \"breathe\"\nspeed = \"fast\"\n",
        )
        .expect("working animation spec"),
    );
    let theme = Theme::fixed_for_theme(false, &theme_config);
    let snapshot = snapshot_with(Vec::new()).with_live_panes(
        vec![pane("%1", "cargo build --release", "/repo/main")],
        None,
    );

    let cost_rolls = CostRolls::default();
    let ctx = test_row_ctx(&snapshot, &theme, 44, 0, 0, &cost_rolls);
    let lines = worktree_group_block(&ctx, &snapshot.worktree_groups[0], false, None).lines;

    let lead = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.as_ref() == "A")
        .expect("the process row uses the custom working frame");
    assert!(matches!(lead.style.fg, Some(Color::Indexed(_))));
    assert!(
        lead.style.add_modifier.contains(Modifier::DIM),
        "process rows stay one visual step below agent cards"
    );
}

#[test]
fn render_agent_process_rows_present() {
    let snapshot = snapshot_with(Vec::new()).with_live_panes(
        vec![
            pane("%1", "claude", "/repo/main"),
            pane("%2", "node", "/repo/main"),
        ],
        None,
    );
    let rendered = snapshot_to_screen(&snapshot, 80, 18);

    assert!(rendered.contains("○ claude"));
    assert!(rendered.contains("○ node"));
}
#[test]
fn active_process_row_keeps_the_animation_tick_alive() {
    // A pane doing real work spins a braille frame, so the serve loop must hold
    // the fast animation tick for it just as it does for a running agent —
    // otherwise the spin crawls on the slow data tick.
    let busy = snapshot_with(Vec::new()).with_live_panes(
        vec![pane("%1", "cargo build --release", "/repo/main")],
        None,
    );
    assert_eq!(animation_cadence_for_test(&busy), AnimationCadence::Fast);

    // A bare shell is presence, not motion: it stays on the calm data tick.
    let idle =
        snapshot_with(Vec::new()).with_live_panes(vec![pane("%1", "zsh", "/repo/main")], None);
    assert_eq!(animation_cadence_for_test(&idle), AnimationCadence::None);
}
