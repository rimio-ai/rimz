use super::*;

#[test]
fn remote_control_host_pane_is_filtered_not_rendered() {
    // A `claude remote-control` pane is host infrastructure, not a coding
    // agent: the snapshot reducer filters it out, so it never reads as a
    // `claude` row. Remote control surfaces as a `⇅ rc` flag on the provider
    // dashboard (covered by the section tests), never as its own row.
    let snapshot = snapshot_with(Vec::new(), Vec::new()).with_live_panes(
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
        let mut snapshot = snapshot_with(Vec::new(), Vec::new()).with_live_panes(vec![pane], None);
        let row = &mut snapshot.worktree_groups[0].rows[0];
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
    use ratatui::style::{Color, Modifier};

    let busy = pane("%1", "cargo build --release", "/repo/main");
    let mut snapshot = snapshot_with(Vec::new(), Vec::new()).with_live_panes(vec![busy], None);
    let row = &mut snapshot.worktree_groups[0].rows[0];
    let process = row.as_process_mut().unwrap();
    process.cpu_pct = Some(34);
    process.rss_kb = Some(512 * 1_024);
    process.io_bps = Some(8 * 1_048_576);
    let row = &snapshot.worktree_groups[0].rows[0];

    let theme = Theme::fixed(false);
    let spans = sections::proc_stats_spans(&theme, row);
    let text: String = spans.iter().map(|span| span.content.as_ref()).collect();
    assert_eq!(text, "C  34%  M 512M  ⇅   8M/s");
    let dim = theme.dim();
    let markers = [
        ("C ", Color::Blue),
        ("M ", Color::Green),
        ("⇅ ", Color::Magenta),
    ];
    for (marker, tone) in markers {
        let span = spans
            .iter()
            .find(|span| span.content.as_ref() == marker)
            .unwrap_or_else(|| panic!("marker {marker:?} missing from the grid"));
        assert_eq!(
            span.style,
            theme.style(tone, Modifier::DIM),
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
fn render_process_rows_below_agents_without_a_seam() {
    // A worktree group holding both agent cards and bare process rows spends
    // no chrome line on the boundary: rows sort agents-first and the command
    // tail reads apart by its DIM weight.
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
    let snapshot =
        snapshot_with(Vec::new(), vec![claude]).with_live_panes(vec![stamped, shell], None);

    let rendered = snapshot_to_screen(&snapshot, 44, 18);

    assert!(
        !rendered.contains("┄ commands"),
        "no seam line spends a row on the agent/process boundary:\n{rendered}"
    );
    assert!(
        rendered.contains("○ zsh"),
        "the process row renders below the agent card:\n{rendered}"
    );
    assert_snapshot("agents_process_tail", rendered);
}
/// The command tail reads one step below the agent cards: the lead glyph
/// carries the DIM modifier over its semantic tone (quiet-green idle,
/// work-clay active) and the program name wears the soft middle tier — which
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
        let snapshot = snapshot_with(Vec::new(), vec![claude])
            .with_live_panes(vec![stamped, shell, build], None);

        let mut lines = Vec::new();
        let mut map = Vec::new();
        let mut row_index = 0;
        worktree_group_lines(
            &theme,
            &snapshot.worktree_groups[0],
            &snapshot.providers,
            snapshot.now,
            44,
            &snapshot.sidebar.context,
            snapshot.sidebar.card_density,
            None,
            &mut row_index,
            0,
            0,
            &CostRolls::default(),
            &mut lines,
            &mut map,
        );

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
            theme.soft(),
            "the program name drops to the soft tier below full strength (no_color={no_color})"
        );
        assert_eq!(
            span_style("○"),
            labels::status_style(&theme, AgentStatus::Idle).add_modifier(Modifier::DIM),
            "the idle lead keeps its quiet green one DIM step down (no_color={no_color})"
        );
        let active_lead = theme.style(theme::ORANGE, Modifier::DIM);
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
    let mut sidebar = crate::config::SidebarConfig::default();
    sidebar.animations.working = Some(
        toml::from_str::<AnimationSpec>(
            "frames = \"AB\"\ncolor = 196\neffect = \"blink\"\nspeed = \"fast\"\n",
        )
        .expect("working animation spec"),
    );
    let theme = Theme::fixed_for_sidebar(false, &sidebar);
    let snapshot = snapshot_with(Vec::new(), Vec::new()).with_live_panes(
        vec![pane("%1", "cargo build --release", "/repo/main")],
        None,
    );

    let mut lines = Vec::new();
    let mut map = Vec::new();
    let mut row_index = 0;
    worktree_group_lines(
        &theme,
        &snapshot.worktree_groups[0],
        &snapshot.providers,
        snapshot.now,
        44,
        &snapshot.sidebar.context,
        snapshot.sidebar.card_density,
        None,
        &mut row_index,
        0,
        0,
        &CostRolls::default(),
        &mut lines,
        &mut map,
    );

    let lead = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.as_ref() == "A")
        .expect("the process row uses the custom working frame");
    assert_eq!(lead.style.fg, Some(Color::Indexed(196)));
    assert!(lead.style.add_modifier.contains(Modifier::BOLD));
    assert!(
        lead.style.add_modifier.contains(Modifier::DIM),
        "process rows stay one visual step below agent cards"
    );
}

#[test]
fn render_process_row_shows_without_hint() {
    let snapshot = snapshot_with(Vec::new(), Vec::new())
        .with_live_panes(vec![pane("%1", "zsh", "/repo/main")], None);
    let rendered = snapshot_to_screen(&snapshot, 80, 18);

    assert!(rendered.contains("○ zsh"));
}
#[test]
fn render_agent_process_rows_present() {
    let snapshot = snapshot_with(Vec::new(), Vec::new()).with_live_panes(
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
    let busy = snapshot_with(Vec::new(), Vec::new()).with_live_panes(
        vec![pane("%1", "cargo build --release", "/repo/main")],
        None,
    );
    assert!(has_live_animation(&busy));

    // A bare shell is presence, not motion: it stays on the calm data tick.
    let idle = snapshot_with(Vec::new(), Vec::new())
        .with_live_panes(vec![pane("%1", "zsh", "/repo/main")], None);
    assert!(!has_live_animation(&idle));
}
