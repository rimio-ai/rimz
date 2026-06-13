use super::*;
use crate::sidebar_pane::render::theme::Component;

#[test]
fn agent_context_line_renders_compaction_markers() {
    for (count, expected) in [
        (0, None),
        (1, Some("▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k · ↻ 1")),
        (2, Some("▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k · ↻ 2")),
    ] {
        let mut claude = agent(
            "claude-1",
            "claude",
            AgentStatus::Running,
            Some("/repo/main"),
            Some("main"),
            Some("db migrate"),
        );
        claude.context = Some(claude_context(fixed_now()));
        claude.compaction_count = count;
        let snapshot = snapshot_with(Vec::new(), vec![claude]);
        let rendered = snapshot_to_screen(&snapshot, 56, 14);

        if let Some(expected) = expected {
            assert!(
                rendered.contains(expected),
                "compaction {count} trails the context composition:\n{rendered}"
            );
            if count == 2 {
                assert_snapshot("agent_card_context_compactions", rendered);
            }
        } else {
            assert!(
                !rendered.contains('↻'),
                "an uncompacted session shows no compaction marker:\n{rendered}"
            );
        }
    }
}
#[test]
fn render_agent_card_context_line_pins_age_not_resource_stats() {
    // Resource stats are process-row vocabulary: even when the agent's
    // stamped pane carries a full `/proc` sample, none of it reaches the
    // card — the context line keeps the age clock as its one right pin.
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    claude.context = Some(claude_context(fixed_now()));
    claude.last_activity = fixed_now() - Duration::from_secs(90);
    let stamped = pane("%1", "claude", "/repo/main");
    claude.pane = Some(stamped.clone());
    let live = stamped;
    let snapshot = snapshot_with(Vec::new(), vec![claude]).with_live_panes(vec![live], None);
    let row = &snapshot.worktree_groups[0].rows[0];
    assert!(row.as_process().is_none());

    let rendered = snapshot_to_screen(&snapshot, 56, 14);

    assert!(
        rendered.contains("▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k"),
        "the token breakdown keeps the line's left side:\n{rendered}"
    );
    assert!(
        !rendered.contains("C 11%"),
        "the pane's resource stats stay off the card:\n{rendered}"
    );
    assert!(
        rendered.contains("◔ 1m"),
        "the age clock keeps the right pin:\n{rendered}"
    );
    assert_snapshot("agent_card_context_age", rendered);
}
#[test]
fn codex_line_two_prefers_thread_preview_over_thread_name_and_task() {
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    let mut context = codex_context(fixed_now());
    context.session_name = Some("TUI prototype".to_owned());
    context.session_preview = Some("Create a TUI".to_owned());
    codex.context = Some(context);
    let snapshot = snapshot_with(Vec::new(), vec![codex]);
    let rendered = snapshot_to_screen(&snapshot, 44, 12);

    assert!(rendered.contains("Create a TUI"));
    assert!(!rendered.contains("TUI prototype"));
    assert!(!rendered.contains("db migrate"));
}
#[test]
fn codex_line_two_prefers_thread_name_over_task_when_preview_is_absent() {
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    let mut context = codex_context(fixed_now());
    context.session_name = Some("TUI prototype".to_owned());
    codex.context = Some(context);
    let snapshot = snapshot_with(Vec::new(), vec![codex]);
    let rendered = snapshot_to_screen(&snapshot, 44, 12);

    assert!(rendered.contains("TUI prototype"));
    assert!(!rendered.contains("db migrate"));
}
#[test]
fn selected_agent_without_context_keeps_bare_token_total() {
    // An agent with no context sidecar yet (a Codex session before its first
    // app-server refresh, or any agent that publishes none) degrades to the
    // bare ▤ rollup total standing in for the filled window — no cost, no
    // usage windows.
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("add tests"),
    );
    codex.model = Some("GPT-5.5".to_owned());
    codex.total_tokens = Some(5_000);
    assert!(codex.context.is_none());
    let snapshot = snapshot_with(Vec::new(), vec![codex]);
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: 0,
            help_visible: false,
            animation_phase: 0,
            line_map: Vec::new(),
            ..Default::default()
        },
        44,
        14,
    );

    assert!(rendered.contains("▤ 5k"));
    // No split fields, so no composition columns trail the bare total.
    for marker in ['◌', '◍', '↘', '↗'] {
        assert!(
            !rendered.contains(marker),
            "a splitless row keeps the bare total alone:\n{rendered}"
        );
    }
    assert!(!rendered.contains('↻'));
    assert!(!rendered.contains('$'));
}
#[test]
fn codex_card_renders_the_per_call_composition() {
    // A Codex row carries the per-call split from its rollout's
    // `last_token_usage` on the lifecycle rail — no rich context blob. The
    // context line legends it: `▤` the window numerator (cache reads + fresh
    // input, exactly what the `▣` percent scales — not the call total, which
    // includes output), then `◌`/`↘`/`↗`. No `◍` column: the protocol reports
    // no per-call cache-write, so it drops rather than reading a false zero.
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("add tests"),
    );
    codex.model = Some("GPT-5.5".to_owned());
    codex.context_pct = Some(50);
    codex.context_window = Some(258_400);
    codex.total_tokens = Some(130_000);
    codex.cache_read_input_tokens = Some(120_000);
    codex.fresh_input_tokens = Some(9_200);
    codex.output_tokens = Some(800);
    assert!(codex.context.is_none());
    let snapshot = snapshot_with(Vec::new(), vec![codex]);
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: 0,
            help_visible: false,
            animation_phase: 0,
            line_map: Vec::new(),
            ..Default::default()
        },
        44,
        14,
    );

    assert!(
        rendered.contains("▤ 129k · ◌ 120k ↘ 9k ↗ 800"),
        "the context line legends the split:\n{rendered}"
    );
    assert!(
        !rendered.contains('◍'),
        "no per-call cache-write exists, so the column drops:\n{rendered}"
    );
    assert_snapshot("codex_card_context_composition", rendered);
}
/// The bar's composition comes from the row-level split too: with no rich blob,
/// a Codex card's fill leads with the cache-read health run, then the
/// fresh-input accent — and no cache-write segment exists to paint — so the
/// context line stays the bar's legend by construction. Style-level, since text
/// goldens cannot see the segment colors.
#[test]
fn codex_calm_bar_splits_into_row_level_segments() {
    let theme = Theme::fixed(false);
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("add tests"),
    );
    codex.context_pct = Some(50);
    codex.context_window = Some(258_400);
    codex.cache_read_input_tokens = Some(120_000);
    codex.fresh_input_tokens = Some(9_200);
    codex.output_tokens = Some(800);
    let snapshot = snapshot_with(Vec::new(), vec![codex]);

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

    let bar_styles: Vec<_> = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .filter(|span| span.content.contains('━'))
        .map(|span| span.style)
        .collect();
    let input = theme.style(theme.component(Component::Input), Modifier::empty());
    let cache_write = theme.style(theme.component(Component::CacheWrite), Modifier::empty());
    assert!(
        bar_styles.contains(&input),
        "the fresh-input accent colors the bar tail"
    );
    assert!(
        !bar_styles.contains(&cache_write),
        "no cache-write segment exists to paint"
    );
    let input_at = bar_styles
        .iter()
        .position(|style| *style == input)
        .expect("the fresh-input accent");
    assert!(
        input_at >= 1 && bar_styles[..input_at].iter().all(|style| *style != input),
        "the cache-read health run leads the bar before fresh input: {bar_styles:?}"
    );
}
/// When all per-call input buckets are present, the context bar reads left to
/// right like the context line: the cache-read health run, then the
/// cache-write and fresh-input accents in order. Style-level because the
/// terminal text only shows the `━` run and its narrow segment caps.
#[test]
fn calm_context_bar_orders_cache_read_before_cache_write() {
    let theme = Theme::fixed(false);
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("index docs"),
    );
    let mut context = claude_context(fixed_now());
    context.tokens = Some(AgentTokenUsage {
        context_window_size: Some(100_000),
        used_percentage: Some(30),
        remaining_percentage: Some(70),
        current_usage: Some(AgentCurrentUsage {
            input_tokens: Some(10_000),
            output_tokens: Some(0),
            cache_creation_input_tokens: Some(10_000),
            cache_read_input_tokens: Some(10_000),
        }),
    });
    claude.context = Some(context);
    let snapshot = snapshot_with(Vec::new(), vec![claude]);

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

    let bar_styles: Vec<_> = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .filter(|span| span.content.contains('━'))
        .map(|span| span.style)
        .collect();
    let cache_write = theme.style(theme.component(Component::CacheWrite), Modifier::empty());
    let input = theme.style(theme.component(Component::Input), Modifier::empty());
    let write_at = bar_styles
        .iter()
        .position(|style| *style == cache_write)
        .expect("the cache-write accent");
    let input_at = bar_styles
        .iter()
        .position(|style| *style == input)
        .expect("the fresh-input accent");
    assert!(
        write_at < input_at,
        "cache-write precedes fresh input along the bar: {bar_styles:?}"
    );
    assert!(
        write_at >= 1
            && bar_styles[..write_at]
                .iter()
                .all(|style| *style != cache_write && *style != input),
        "the cache-read health run leads the bar before the accents: {bar_styles:?}"
    );
}
/// The card's age cluster pairs a clock-fill glyph (the face fills with the
/// idle span) with a continuous tone: the dim resting weight while a resume
/// would still hit cache, then a warn-caution-alarm ramp to the hour — the cost
/// warning that resuming will likely re-read the whole context uncached.
#[test]
fn context_line_age_tone_slides_with_the_clock_age() {
    let theme = Theme::fixed(false);
    let age_style = |idle_secs: u64, clock: char| {
        let mut codex = agent(
            "codex-1",
            "codex",
            AgentStatus::Idle,
            Some("/repo/main"),
            Some("main"),
            Some("add tests"),
        );
        codex.context_pct = Some(21);
        codex.total_tokens = Some(5_000);
        codex.last_activity = fixed_now() - Duration::from_secs(idle_secs);
        let snapshot = snapshot_with(Vec::new(), vec![codex]);
        group_lines(&snapshot, &theme, usize::MAX)
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content.contains(clock))
            .map(|span| span.style)
            .unwrap_or_else(|| panic!("the context line carries the {clock} age"))
    };
    let heat = |age_secs: i64| {
        theme.style(
            theme.warm_heat_tone(age_heat_amount_for_test(age_secs)),
            Modifier::empty(),
        )
    };
    assert_eq!(
        age_style(4 * 60, '◔'),
        theme.muted(),
        "warm cache rests at the dim weight"
    );
    assert_eq!(
        age_style(25 * 60, '◑'),
        heat(25 * 60),
        "warm ramp tone with the half-full face"
    );
    assert_eq!(
        age_style(40 * 60, '◕'),
        heat(40 * 60),
        "mid-ramp tone past the half hour"
    );
    assert_eq!(
        age_style(2 * 60 * 60, '◉'),
        theme.alarm(Modifier::empty()),
        "red once a resume would pay for the context again"
    );
}
#[test]
fn codex_app_server_context_links_to_rich_card() {
    // Codex's split enrichment rides the same `AgentContext` field as Claude's
    // statusline, so it lights up the rich card with no renderer change: the
    // official display name, actual configured effort, and both usage windows in
    // the selected detail block. Token usage and cost are absent in this
    // fixture, so the gauge and detail fall back to the rollout scalars.
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("add tests"),
    );
    // Rollout scalars are the coarse fallback the app-server context upgrades.
    codex.model = Some("gpt-5.5-codex".to_owned());
    codex.context_pct = Some(21);
    codex.total_tokens = Some(48_000);
    codex.context = Some(codex_context(fixed_now()));
    let snapshot = snapshot_with(Vec::new(), vec![codex]);
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: 0,
            help_visible: false,
            animation_phase: 0,
            line_map: Vec::new(),
            ..Default::default()
        },
        54,
        14,
    );

    // The app-server display name supersedes the raw catalog id (its hyphen
    // traded for a space, matching `Opus 4.8`), and locally sourced effort
    // surfaces — neither was on the rollout-only row.
    assert!(rendered.contains("GPT 5.5 Codex"));
    assert!(!rendered.contains("gpt-5.5-codex"));
    assert!(rendered.contains("xhigh"));
    // The 5h/7d windows are account-scoped now: they leave the row for the
    // provider dashboard, so no reset mark rides a row.
    assert!(!rendered.contains('↻'));
    assert!(!rendered.contains("5h"));
    assert!(!rendered.contains("7d"));
    // No read-only token usage or cost: the bare rollout total (`▤ 48k`,
    // integer form) stands in for the context line, and no cost pins to the
    // row.
    assert!(rendered.contains("▤ 48k"));
    assert!(!rendered.contains('↗'));
    assert!(!rendered.contains('$'));
}
