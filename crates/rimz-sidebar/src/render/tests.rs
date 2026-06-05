use jiff::Timestamp;
use rimz::agents::{
    AgentContext, AgentCost, AgentCurrentUsage, AgentRateLimits, AgentTokenUsage, AgentTurnError,
    RateLimitWindow,
};
use rimz::feed::{AgentState, AgentStatus, FeedKind, PaneRef};
use rimz::ids::{MuxName, PaneId, ViewKind};
use rimz::{EventEnvelope, FeedItem, FeedStatus, SidebarSnapshot, Surface, WorkspaceId};
use serde_json::json;
use std::time::Duration;

use super::*;

fn fixed_workspace() -> WorkspaceId {
    WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap()
}

fn fixed_now() -> Timestamp {
    // Pin every test to one timestamp so the redaction filter has a
    // deterministic input to scrub.
    Timestamp::now()
}

fn snapshot_to_screen(snapshot: &SidebarSnapshot, width: u16, height: u16) -> String {
    snapshot_to_screen_with_alert(snapshot, None, width, height)
}

fn snapshot_to_screen_with_alert(
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    width: u16,
    height: u16,
) -> String {
    snapshot_to_screen_with_alert_and_ui(snapshot, alert, &UiState::default(), width, height)
}

fn snapshot_to_screen_with_alert_and_ui(
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    ui: &UiState,
    width: u16,
    height: u16,
) -> String {
    let mut bytes = Vec::new();
    let backend = CrosstermBackend::new(&mut bytes);
    let viewport = Viewport::Fixed(Rect::new(0, 0, width, height));
    let mut terminal = Terminal::with_options(backend, TerminalOptions { viewport }).unwrap();
    terminal.clear().unwrap();
    let mut ui = ui.clone();
    draw_to_terminal_with_ui(&mut terminal, snapshot, alert, &mut ui).unwrap();
    drop(terminal);
    let mut parser = vt100::Parser::new(height, width, 0);
    parser.process(&bytes);
    parser.screen().contents()
}

fn snapshot_text(screen: &str) -> String {
    screen
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_snapshot(name: &str, screen: String) {
    // Row ages and degraded elapsed values are intentionally relative.
    let screen = snapshot_text(&screen);
    insta::with_settings!({
        filters => vec![
            (r"degraded for \d+[smhd]", "degraded for <elapsed>"),
            // Budget-bar reset countdowns are a live two-unit duration in the
            // bar's right value column (`3h12m`, `3d3h`); scrub them so the
            // card snapshot stays stable across time.
            (r"\b\d+[dhms]\d+[dhms]\b", "<reset>"),
            // Single-unit live durations, anchored to where they render so
            // the identity line's deterministic window token (`1m`) stays
            // visible: an age after its clock-fill glyph, and the `5h`/`7d`
            // budget label ahead of its mana bar.
            (r"([◔◑◕●◉]) \d+[smhd]\b", "$1 <t>"),
            (r"\b\d+[hd](\s+[▰▱])", "<t>$1"),
        ],
    }, {
        insta::assert_snapshot!(name, screen);
    });
}

#[test]
fn no_color_theme_suppresses_color_not_shape_modifiers() {
    let style = Theme::fixed(true).style(Color::Red, Modifier::BOLD);

    assert_eq!(style.fg, None);
    assert!(style.add_modifier.contains(Modifier::BOLD));
}

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

fn snapshot_with(items: Vec<FeedItem>, agents: Vec<AgentState>) -> SidebarSnapshot {
    let mut snapshot = SidebarSnapshot::build_with_carryover(
        fixed_workspace(),
        items,
        Vec::new(),
        agents,
        Timestamp::now(),
    );
    snapshot.display_name = "query-engine".to_owned();
    snapshot
}

fn agent(
    id: &str,
    kind: &str,
    status: AgentStatus,
    worktree_path: Option<&str>,
    branch: Option<&str>,
    task: Option<&str>,
) -> AgentState {
    let now = fixed_now();
    AgentState {
        agent_id: id.into(),
        kind: rimz::ids::AgentKind::new_unchecked(kind),
        status,
        phase: rimz::agents::TurnPhase::Idle,
        pane: None,
        agent_pid: None,
        agent_process_start: None,
        runtime_owner: None,
        parent_agent_id: None,
        worktree_path: worktree_path.map(ToOwned::to_owned),
        worktree_branch: branch.map(ToOwned::to_owned),
        task: task.map(ToOwned::to_owned),
        prompt: None,
        model: None,
        effort: None,
        context_pct: None,
        context_window: None,
        total_tokens: None,
        todo_done: None,
        todo_total: None,
        context: None,
        subagent_description: None,
        subagent_started_at: None,
        turn_started_at: None,
        compacting_since: None,
        last_seen: now,
        last_activity: now,
        registered_at: Some(now),
    }
}

fn pane(raw: &str, command: &str, cwd: &str) -> PaneRef {
    PaneRef {
        pane_id: PaneId::from_parts(MuxName::Tmux, raw),
        session_name: "rimz-test".to_owned(),
        view_id: Some("@0".to_owned()),
        view_kind: Some(ViewKind::Window),
        view_name: None,
        is_focused: false,
        command: Some(command.to_owned()),
        cwd: Some(cwd.to_owned()),
        pane_pid: None,
        pane_process_start: None,
        rss_kb: None,
        cpu_pct: None,
        io_bps: None,
    }
}

/// A full Claude statusline enrichment for the rich-row tests. Reset instants
/// are placed days/hours ahead so the live countdown renders at a stable
/// length (the value itself is scrubbed by `assert_snapshot`).
fn claude_context(now: Timestamp) -> AgentContext {
    AgentContext {
        source: "claude".to_owned(),
        session_name: Some("ledger refactor".to_owned()),
        model_id: Some("claude-opus-4-8".to_owned()),
        model_display_name: Some("Opus 4.8 (1M context)".to_owned()),
        effort: Some("high".to_owned()),
        thinking_enabled: Some(false),
        output_style: None,
        vim_mode: None,
        agent_version: None,
        exceeds_200k_tokens: Some(false),
        cost: Some(AgentCost {
            total_cost_usd: Some(1.27),
            total_duration_ms: Some(12 * 60 * 1_000),
            total_api_duration_ms: None,
            total_lines_added: Some(214),
            total_lines_removed: Some(31),
        }),
        tokens: Some(AgentTokenUsage {
            context_window_size: Some(200_000),
            used_percentage: Some(38),
            remaining_percentage: Some(62),
            // A realistic per-call split: cache reads carry the context,
            // fresh input stays small. The input side sums to 76,500 so
            // the precise meter still reads 38.2% of the 200k window.
            current_usage: Some(AgentCurrentUsage {
                input_tokens: Some(1_700),
                output_tokens: Some(2_300),
                cache_creation_input_tokens: Some(6_600),
                cache_read_input_tokens: Some(68_200),
            }),
        }),
        rate_limits: Some(AgentRateLimits {
            windows: vec![
                RateLimitWindow {
                    used_percentage: Some(30),
                    resets_at: Some(now + Duration::from_secs(3 * 3_600 + 12 * 60)),
                    duration_mins: Some(5 * 60),
                },
                RateLimitWindow {
                    used_percentage: Some(60),
                    resets_at: Some(now + Duration::from_secs(3 * 86_400 + 4 * 3_600)),
                    duration_mins: Some(7 * 24 * 60),
                },
            ],
        }),
        pr: None,
        account: None,
        turn_error: None,
        observed_at: now,
    }
}

/// The Codex app-server enrichment: a 5-hour and a 7-day rate-limit window,
/// the official model display name, effort, and version — but no token usage or
/// cost (the app-server exposes neither read-only, so those stay `None` and
/// the gauge falls back to the rollout scalars). The mirror of `claude_context`
/// for the other transport.
fn codex_context(now: Timestamp) -> AgentContext {
    AgentContext {
        source: "codex".to_owned(),
        session_name: None,
        model_id: Some("gpt-5.5-codex".to_owned()),
        model_display_name: Some("GPT-5.5 Codex".to_owned()),
        effort: Some("xhigh".to_owned()),
        thinking_enabled: None,
        output_style: None,
        vim_mode: None,
        agent_version: Some("0.135.0".to_owned()),
        exceeds_200k_tokens: None,
        cost: None,
        tokens: None,
        rate_limits: Some(AgentRateLimits {
            windows: vec![
                RateLimitWindow {
                    used_percentage: Some(42),
                    resets_at: Some(now + Duration::from_secs(3 * 3_600 + 12 * 60)),
                    duration_mins: Some(5 * 60),
                },
                RateLimitWindow {
                    used_percentage: Some(7),
                    resets_at: Some(now + Duration::from_secs(3 * 86_400 + 4 * 3_600)),
                    duration_mins: Some(7 * 24 * 60),
                },
            ],
        }),
        pr: None,
        account: None,
        turn_error: None,
        observed_at: now,
    }
}

#[test]
fn render_worktree_attention_map() {
    let workspace = fixed_workspace();
    let mut native = FeedItem::new(
        workspace.clone(),
        Surface::NativeUi,
        FeedKind::Permission,
        "psql DROP TABLE invoices",
        "claude",
        "agent-hook",
    );
    native.worktree_path = Some("/home/me/query-engine".to_owned());
    native.updated_at = fixed_now() - Duration::from_secs(12 * 60);
    let mut script = FeedItem::new(
        workspace,
        Surface::Script,
        FeedKind::Question,
        "Deploy staging?",
        "deploy.sh",
        "cli",
    );
    script.options = vec!["yes".to_owned(), "no".to_owned()];
    script.updated_at = fixed_now() - Duration::from_secs(5 * 60);
    let mut running = agent(
        "codex-1",
        "codex",
        AgentStatus::Running,
        Some("/home/me/query-engine"),
        Some("main"),
        Some("add tests"),
    );
    running.model = Some("GPT-5.5".to_owned());
    running.effort = Some("high".to_owned());
    running.last_activity = fixed_now() - Duration::from_secs(8);

    let snapshot = snapshot_with(vec![native, script], vec![running]);

    assert_snapshot(
        "worktree_attention_map",
        snapshot_to_screen(&snapshot, 38, 20),
    );
}

#[test]
fn render_agent_capability_and_window() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Failed,
        Some("/repo/feature-migration"),
        Some("feature-migration"),
        Some("db migrate"),
    );
    claude.model = Some("Opus".to_owned());
    claude.effort = Some("xhigh".to_owned());
    // The hook-derived window renders as the identity line's `1M` token.
    claude.context_window = Some(1_000_000);
    claude.last_activity = fixed_now() - Duration::from_secs(4 * 60);
    let snapshot = snapshot_with(Vec::new(), vec![claude]);

    assert_snapshot("agent_capability", snapshot_to_screen(&snapshot, 34, 12));
}

#[test]
fn render_enriched_selected_agent_card() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/feature-migration"),
        Some("feature-migration"),
        Some("db migrate"),
    );
    // Transcript scalars are the coarse fallback; the statusline enriches the
    // display name (`Opus` → `Opus 4.8`). Effort stays with the hook-derived
    // configured value (`xhigh`) even when the statusline reports a capped
    // model-effective level (`high`).
    claude.model = Some("Opus".to_owned());
    claude.effort = Some("xhigh".to_owned());
    claude.context_pct = Some(38);
    claude.total_tokens = Some(12_400);
    claude.todo_done = Some(3);
    claude.todo_total = Some(5);
    claude.context = Some(claude_context(fixed_now()));
    let mut snapshot = snapshot_with(Vec::new(), vec![claude]);
    snapshot.worktree_groups[0].diff_added = Some(127);
    snapshot.worktree_groups[0].diff_removed = Some(43);
    snapshot.worktree_groups[0].commits_ahead = Some(3);
    snapshot.worktree_groups[0].commits_behind = Some(1);

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

    // The worktree's git story sits on the group header: the ⇡/⇣ commit
    // delta leads the worktree-total diff.
    assert!(rendered.contains("⇡3 ⇣1  +127 -43"), "header:\n{rendered}");
    // Line 1 carries identity + capability + cost; line 2 is the session
    // name; the model display name sheds its window qualifier (`Opus 4.8
    // (1M context)` → `Opus 4.8`) — the dedicated window token (the
    // statusline's 200k reading) carries the figure.
    assert!(rendered.contains("Opus 4.8"));
    assert!(!rendered.contains("(1M"));
    assert!(!rendered.contains("context"));
    assert!(rendered.contains("xhigh"), "effort:\n{rendered}");
    assert!(rendered.contains("· 200k"), "window token:\n{rendered}");
    // Per-row cost now reads at full cent resolution, like every other spend.
    assert!(rendered.contains("$1.27"));
    // Line 2 is the full-width description; todo dots inline at L2.
    assert!(rendered.contains("ledger refactor"));
    assert!(rendered.contains("●●●○○ 3/5"));
    // The context bar carries the `▣` label and the percent used as its
    // value (always — the window size moved to the token line below); the
    // fill carries the same reading.
    assert!(rendered.contains("▣ "));
    // The account-scoped 5h/7d budgets are gone from the row — they live in
    // the provider dashboard now.
    assert!(!rendered.contains("5h↻"));
    assert!(!rendered.contains("7d↻"));
    // The card carries the context line at rest: ▤ the filled window
    // (input + cache-write + cache-read — the ▣ meter's numerator, so the
    // 38.2% above and this 76k are one measurement), a · seam, then the
    // latest call's composition ordered by how the window filled — ◌
    // cache read, ◍ cache write, ↘ fresh input, ↗ output. The ◇ totals
    // stay the cockpit/ledger vocabulary; the window size no longer rides
    // this line.
    assert!(
        rendered.contains("▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k"),
        "context line:\n{rendered}"
    );
    assert!(!rendered.contains('◇'), "no fleet total on the card");
    assert!(
        !rendered.contains("ctx"),
        "window size left the token line:\n{rendered}"
    );
    assert_snapshot("enriched_selected_agent_card", rendered);
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
    let mut live = stamped;
    live.cpu_pct = Some(11);
    live.rss_kb = Some(1_153_024); // 1.1 GiB
    live.io_bps = Some(3 * 1_048_576);
    let snapshot = snapshot_with(Vec::new(), vec![claude]).with_live_panes(vec![live], None);

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
fn render_process_row_pins_resource_stats_at_l2() {
    // An active process pane at L2 width pins `C  n%  M  nM  ⇅  nM/s` right
    // on line 1 — the slot an agent card gives its `$cost` — while the full
    // command rides the detail line below, so a build's resource load reads
    // at a glance without leaving the sidebar. The fixed-grid shape and tone
    // are asserted separately in `proc_stats_hold_a_fixed_dim_grid`.
    let mut busy = pane("%1", "cargo build --release", "/repo/main");
    busy.cpu_pct = Some(34);
    busy.rss_kb = Some(512 * 1_024);
    busy.io_bps = Some(8 * 1_048_576);
    let snapshot = snapshot_with(Vec::new(), Vec::new()).with_live_panes(vec![busy], None);

    let rendered = snapshot_to_screen(&snapshot, 56, 14);

    assert!(
        rendered.contains("C  34%  M 512M  ⇅   8M/s"),
        "the stats pin right on the primary line:\n{rendered}"
    );
    assert!(
        rendered.contains("cargo build --release"),
        "the detail line carries the full command:\n{rendered}"
    );
    assert_snapshot("process_row_resource_stats", rendered);
}

#[test]
fn proc_stats_hold_a_fixed_dim_grid() {
    // The whole stats cluster stays in the dim process tone — markers
    // included; the colored lead glyph carries the row's liveness — and each
    // figure right-aligns into its fixed slot so a changing magnitude never
    // walks the cluster sideways.
    let mut busy = pane("%1", "cargo build --release", "/repo/main");
    busy.cpu_pct = Some(34);
    busy.rss_kb = Some(512 * 1_024);
    busy.io_bps = Some(8 * 1_048_576);
    let snapshot = snapshot_with(Vec::new(), Vec::new()).with_live_panes(vec![busy], None);
    let row = &snapshot.worktree_groups[0].rows[0];

    let theme = Theme::fixed(false);
    let spans = sections::proc_stats_spans(&theme, row);
    let text: String = spans.iter().map(|span| span.content.as_ref()).collect();
    assert_eq!(text, "C  34%  M 512M  ⇅   8M/s");
    let dim = theme.dim();
    assert!(
        spans.iter().all(|span| span.style == dim),
        "markers and figures alike stay in the dim process tone"
    );

    // A figure changing magnitude re-aligns within its slot; the grid width
    // holds, so the right-pinned cluster never moves.
    let mut hotter = row.clone();
    hotter.cpu_pct = Some(100);
    hotter.rss_kb = Some(1_153_024); // 1.1 GiB
    hotter.io_bps = Some(450 * 1_024);
    let shifted: String = sections::proc_stats_spans(&theme, &hotter)
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert_eq!(shifted, "C 100%  M 1.1G  ⇅ 450k/s");
    assert_eq!(text.chars().count(), shifted.chars().count());

    // A metric not yet sampled (rates on the first tick) blank-fills its
    // slot, so the columns hold still from the first reading on.
    let mut first_tick = row.clone();
    first_tick.cpu_pct = None;
    first_tick.io_bps = None;
    let blanked: String = sections::proc_stats_spans(&theme, &first_tick)
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert_eq!(blanked, "        M 512M          ");
    assert_eq!(blanked.chars().count(), text.chars().count());

    // NO_COLOR keeps the shape and sheds every tone.
    let plain = sections::proc_stats_spans(&Theme::fixed(true), row);
    assert!(plain.iter().all(|span| span.style.fg.is_none()));
}

#[test]
fn render_commands_divider_between_agents_and_processes() {
    // A worktree group holding both agent cards and bare process rows seams
    // the two with a faint `┄ commands ┄┄┄` divider: processes read at
    // agent-card strength now, so the seam — not a dim tone — marks the
    // group's command tail. Rows sort agents-first, so the seam lands once.
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
        rendered.contains("┄ commands ┄"),
        "the seam splits the agent cards from the process rows:\n{rendered}"
    );
    assert!(
        rendered.contains("○ zsh"),
        "the process row renders below the seam:\n{rendered}"
    );
    assert_snapshot("commands_divider", rendered);
}

/// The seam keeps its faint tone through the gutter: `with_gutter` rebuilds the
/// line to prepend the gutter cell, so it must carry a line-level style onto the
/// content spans rather than dropping it — the drop painted the seam (and the
/// dim `+K more`) at full strength.
#[test]
fn commands_divider_stays_faint_through_the_gutter() {
    let theme = Theme::fixed(false);
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

    let mut lines = Vec::new();
    let mut map = Vec::new();
    let mut row_index = 0;
    worktree_group_lines(
        &theme,
        &snapshot.worktree_groups[0],
        &snapshot.providers,
        44,
        &snapshot.sidebar.context,
        &mut row_index,
        0,
        0,
        &mut lines,
        &mut map,
    );

    let seam = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.contains("┄ commands"))
        .expect("the seam renders between agents and processes");
    assert_eq!(
        seam.style,
        theme.faint(),
        "the seam wears the faint chrome tone"
    );
}

#[test]
fn render_worktree_equal_to_trunk() {
    // A fully-landed worktree — zero commits ahead, zero diff against the
    // fork point — collapses the header's git cluster to `≡ <trunk>`:
    // nothing left to land, safe to remove. Behind deliberately doesn't
    // count against it, so the marker holds even as the trunk moves on.
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Idle,
        Some("/home/me/query-engine-wt/feature-migration"),
        Some("feature-migration"),
        None,
    );
    codex.last_activity = fixed_now() - Duration::from_secs(30);
    let mut snapshot = snapshot_with(Vec::new(), vec![codex]);
    snapshot.worktree_groups[0].diff_added = Some(0);
    snapshot.worktree_groups[0].diff_removed = Some(0);
    snapshot.worktree_groups[0].commits_ahead = Some(0);
    snapshot.worktree_groups[0].commits_behind = Some(5);
    snapshot.worktree_groups[0].trunk = Some("main".to_owned());

    let rendered = snapshot_to_screen(&snapshot, 38, 14);

    assert!(rendered.contains("≡ main"), "header:\n{rendered}");
    assert!(
        !rendered.contains("+0 -0"),
        "the landed marker replaces the zero diff"
    );
    assert!(
        !rendered.contains('⇣'),
        "behind stays out of the landed header"
    );
    assert_snapshot("worktree_equal_to_trunk", rendered);
}

#[test]
fn render_trunk_worktree_skips_the_landed_marker() {
    // The trunk worktree is trivially "landed on itself," so the `≡`
    // marker would be noise there: a main-branch group with zero stats
    // keeps a bare header, and the marker stays reserved for a removable
    // feature worktree.
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Idle,
        Some("/home/me/query-engine"),
        Some("main"),
        None,
    );
    codex.last_activity = fixed_now() - Duration::from_secs(30);
    let mut snapshot = snapshot_with(Vec::new(), vec![codex]);
    snapshot.worktree_groups[0].diff_added = Some(0);
    snapshot.worktree_groups[0].diff_removed = Some(0);
    snapshot.worktree_groups[0].commits_ahead = Some(0);
    snapshot.worktree_groups[0].commits_behind = Some(0);
    snapshot.worktree_groups[0].trunk = Some("main".to_owned());

    let rendered = snapshot_to_screen(&snapshot, 38, 14);

    assert!(
        !rendered.contains('≡'),
        "no landed marker on the trunk worktree:\n{rendered}"
    );
    assert!(rendered.contains("⑂ main"), "header:\n{rendered}");
}

#[test]
fn render_api_error_dead_turn_card() {
    // A turn that died on a provider API error fires no Stop hook; the
    // projection escalates the row to the attention `!` and line 2 quotes
    // the upstream error text (dim) instead of the task fall-through, so
    // the card says why without a jump.
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    claude.last_activity = fixed_now() - Duration::from_secs(60);
    let mut context = claude_context(fixed_now());
    context.turn_error = Some(AgentTurnError {
        at: fixed_now() - Duration::from_secs(10),
        label: Some("API Error: Overloaded".to_owned()),
    });
    claude.context = Some(context);
    let snapshot = snapshot_with(Vec::new(), vec![claude]);

    let rendered = snapshot_to_screen(&snapshot, 54, 14);

    assert!(
        rendered.contains("! claude"),
        "the dead turn escalates to the attention glyph:\n{rendered}"
    );
    assert!(
        rendered.contains("API Error: Overloaded"),
        "line 2 quotes the upstream error text:\n{rendered}"
    );
    assert!(
        !rendered.contains("ledger refactor"),
        "the reason takes the line over the session-name fall-through:\n{rendered}"
    );
    assert_snapshot("api_error_dead_turn_card", rendered);
}

#[test]
fn render_selected_card_shows_subagent_description_tokens_and_elapsed() {
    // A selected parent expands its `⧉ subagents` list. Each child reads two
    // lines: the live head (the thinking sparkle while it reasons, `✓` once
    // it lands), the type, and its `subagentStatusLine` description, then the
    // token spend `◇`, model, and effort left with the clock-fill elapsed
    // work pinned right. The first child is finished, so its elapsed is
    // frozen at `last_activity − started` (exactly 60s here, independent of
    // wall-clock) — a deterministic ` 1m` in the fixed three-cell slot — and
    // it carries the effort its `SubagentStop` reported; the second is still
    // running ~30s in, so it reads the sub-minute `<1m` (never seconds), and
    // the two right-pinned clusters stack into one column.
    let mut parent = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    parent.context = Some(claude_context(fixed_now()));

    let mut child = agent(
        "child-1",
        "claude",
        AgentStatus::Success,
        None,
        None,
        Some("Explore"),
    );
    child.parent_agent_id = Some("claude-1".into());
    child.subagent_description = Some("locate the render seam".to_owned());
    child.subagent_started_at = Some(fixed_now() - Duration::from_secs(90));
    child.last_activity = fixed_now() - Duration::from_secs(30);
    child.last_seen = fixed_now() - Duration::from_secs(30);
    child.total_tokens = Some(12_400);
    // A bare model id — the renderer prettifies it through `model_label`.
    child.model = Some("claude-opus-4-8".to_owned());
    // Claude reports the child's effort on its `SubagentStop`.
    child.effort = Some("high".to_owned());

    let mut fresh = agent(
        "child-2",
        "claude",
        AgentStatus::Running,
        None,
        None,
        Some("review"),
    );
    fresh.parent_agent_id = Some("claude-1".into());
    // Mid-reasoning: the child's leading cell is the thinking sparkle (frame 0
    // of the animation at the test's fixed phase), not the static `⢿`.
    fresh.phase = rimz::agents::TurnPhase::Reasoning;
    fresh.subagent_description = Some("audit the trust hash".to_owned());
    fresh.subagent_started_at = Some(fixed_now() - Duration::from_secs(30));
    fresh.total_tokens = Some(3_100);
    // A sibling on a different model — the per-child label tells them apart.
    fresh.model = Some("claude-haiku-4-5".to_owned());

    let snapshot = snapshot_with(Vec::new(), vec![parent, child, fresh]);
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: 0,
            ..Default::default()
        },
        54,
        20,
    );

    assert!(
        rendered.contains("⧉ subagents (2)"),
        "the expanded card lists its children:\n{rendered}"
    );
    // Line 1: type + the description of what the parent asked it to do.
    assert!(
        rendered.contains("Explore — locate the render seam"),
        "line 1 carries the description:\n{rendered}"
    );
    // The running child's leading cell is the thinking sparkle (frame 0 at the
    // test's fixed animation phase), the agent-row head vocabulary verbatim.
    assert!(
        rendered.contains("· review — audit the trust hash"),
        "a reasoning child wears the thinking head:\n{rendered}"
    );
    // Line 2: token spend, model, and effort left, elapsed work right-pinned.
    // 12_400 → `12.4k`, the bare id prettified to `Opus 4.8`, the stop-reported
    // effort after it, 60s frozen → ` 1m` right-aligned in the fixed slot.
    assert!(
        rendered.contains("◇ 12.4k · Opus 4.8 · high"),
        "line 2 carries the token spend, model, and effort:\n{rendered}"
    );
    assert!(
        rendered.contains("◇ 3.1k · Haiku 4.5"),
        "the sibling carries its own model:\n{rendered}"
    );
    assert!(
        rendered.contains("◔  1m"),
        "line 2 carries the frozen elapsed in the fixed slot:\n{rendered}"
    );
    // The running child reads the sub-minute `<1m`, never a seconds figure.
    assert!(
        rendered.contains("◔ <1m"),
        "a sub-minute child reads `<1m`:\n{rendered}"
    );
    assert_snapshot("subagent_two_line_entry", rendered);
}

#[test]
fn line_one_prefers_session_name_over_task() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    claude.context = Some(claude_context(fixed_now()));
    let snapshot = snapshot_with(Vec::new(), vec![claude]);
    let rendered = snapshot_to_screen(&snapshot, 44, 12);

    assert!(rendered.contains("ledger refactor"));
    assert!(!rendered.contains("db migrate"));
}

/// An unnamed session whose turn has ended (the activity-bound `task` cleared)
/// keeps its latest prompt on line two instead of falling to an em dash, until
/// a real session name exists.
#[test]
fn line_two_falls_back_to_the_latest_prompt_when_unnamed() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        None, // idle cleared the task; no session name (no context)
    );
    claude.prompt = Some("wire the bridge".to_owned());
    let snapshot = snapshot_with(Vec::new(), vec![claude]);
    let rendered = snapshot_to_screen(&snapshot, 44, 12);

    assert!(rendered.contains("wire the bridge"));
    assert!(
        !rendered.contains('—'),
        "the prompt stands in for the em dash"
    );
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
    assert!(!rendered.contains('↻'));
    assert!(!rendered.contains('$'));
}

/// The card's age cluster pairs a clock-fill glyph (the face fills with the
/// idle span) with a tone stepping the same quarters: dim while a resume
/// would still hit cache, yellow from the second quarter, amber past the
/// half hour, red past the hour — the cost warning that resuming will
/// likely re-read the whole context uncached.
#[test]
fn context_line_age_tone_steps_with_the_clock_quarters() {
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
    assert_eq!(
        age_style(4 * 60, '◔'),
        theme.dim(),
        "warm cache stays chrome"
    );
    assert_eq!(
        age_style(25 * 60, '◑'),
        theme.style(Color::Yellow, Modifier::empty()),
        "yellow with the half-full face"
    );
    assert_eq!(
        age_style(40 * 60, '◕'),
        theme.style(theme::ORANGE, Modifier::empty()),
        "amber past the half hour"
    );
    assert_eq!(
        age_style(2 * 60 * 60, '◉'),
        theme.style(Color::Red, Modifier::empty()),
        "red once a resume would pay for the context again"
    );
}

#[test]
fn codex_app_server_context_links_to_rich_card() {
    // Codex's app-server enrichment rides the same `AgentContext` field as
    // Claude's statusline, so it lights up the rich card with no renderer
    // change: the official display name and effort on the capability line,
    // and both usage windows in the selected detail block. Token usage and
    // cost have no read-only source, so the gauge and detail fall back to the
    // rollout scalars.
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

    // The app-server display name supersedes the raw catalog id, and effort
    // surfaces — neither was on the rollout-only row.
    assert!(rendered.contains("GPT-5.5 Codex"));
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

#[test]
fn render_omits_history_sections() {
    let workspace = fixed_workspace();
    let mut answered = FeedItem::new(
        workspace.clone(),
        Surface::Script,
        FeedKind::Question,
        "Deploy staging?",
        "deploy.sh",
        "cli",
    );
    answered.status = FeedStatus::Resolved;
    let event = EventEnvelope::new(
        workspace.clone(),
        "rimz-test",
        "rimz",
        "cli",
        "event.emit",
        json!({ "kind": "build.started", "title": "Building web" }),
    );
    let mut snapshot = SidebarSnapshot::build_with_carryover(
        workspace,
        vec![answered],
        vec![event],
        vec![],
        Timestamp::now(),
    );
    snapshot.display_name = "query-engine".to_owned();
    let rendered = snapshot_to_screen(&snapshot, 38, 10);

    assert!(!rendered.contains("all clear"));
    assert!(!rendered.contains("Recent activity"));
    assert!(!rendered.contains("Recently answered"));
}

#[test]
fn render_active_alert_shows_banner_below_snapshot() {
    let snapshot = snapshot_with(Vec::new(), Vec::new());
    let alert = Alert {
        reason: "snapshot failed: ledger not found".to_owned(),
        since: fixed_now() - Duration::from_secs(8),
        recovered_at: None,
    };

    assert_snapshot(
        "degraded_banner",
        snapshot_to_screen_with_alert(&snapshot, Some(&alert), 80, 18),
    );
}

#[test]
fn render_recovered_alert_lingers_with_dismiss_hint() {
    let snapshot = snapshot_with(Vec::new(), Vec::new());
    let alert = Alert {
        reason: "snapshot failed: ledger not found".to_owned(),
        since: fixed_now() - Duration::from_secs(20),
        recovered_at: Some(fixed_now() - Duration::from_secs(8)),
    };
    let rendered = snapshot_to_screen_with_alert(&snapshot, Some(&alert), 80, 18);

    assert!(rendered.contains("last alert"), "{rendered}");
    assert!(rendered.contains("x dismiss"), "{rendered}");
    // Recovered means the room is live again: the first-run hint returns.
    assert!(rendered.contains("rimz hooks install"), "{rendered}");
}

#[test]
fn render_no_alert_omits_banner() {
    let snapshot = snapshot_with(Vec::new(), Vec::new());
    let rendered = snapshot_to_screen_with_alert(&snapshot, None, 80, 18);
    assert!(
        !rendered.contains("Sidebar degraded"),
        "no alert must not render the banner:\n{rendered}"
    );
}

#[test]
fn render_first_run_nudge_points_at_install_when_unwired() {
    // No hooks wired (the default): running an agent registers nothing, so
    // the hint must point at `rimz hooks install`, not "run claude or codex".
    let snapshot = snapshot_with(Vec::new(), Vec::new());
    assert!(!snapshot.agent_hooks_ready);
    let rendered = snapshot_to_screen(&snapshot, 80, 18);

    assert!(!rendered.contains("all clear"));
    assert!(rendered.contains("rimz hooks install"));
    assert!(!rendered.contains("run claude or codex"));
    assert_snapshot("first_run_nudge", rendered);
}

#[test]
fn render_process_row_keeps_first_run_hint() {
    let snapshot = snapshot_with(Vec::new(), Vec::new())
        .with_live_panes(vec![pane("%1", "zsh", "/repo/main")], None);
    let rendered = snapshot_to_screen(&snapshot, 80, 18);

    assert!(rendered.contains("○ zsh"));
    assert!(rendered.contains("rimz hooks install"));
}

#[test]
fn render_agent_process_rows_suppress_first_run_hint() {
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
    assert!(!rendered.contains("no agents yet"));
    assert!(!rendered.contains("rimz hooks install"));
    assert!(!rendered.contains("run claude or codex"));
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

#[test]
fn render_footer_and_help_overlay() {
    let workspace = fixed_workspace();
    let mut native = FeedItem::new(
        workspace,
        Surface::NativeUi,
        FeedKind::Permission,
        "allow?",
        "codex",
        "agent-hook",
    );
    native.worktree_branch = Some("main".to_owned());
    let snapshot = snapshot_with(vec![native], Vec::new());
    let rendered = snapshot_to_screen(&snapshot, 80, 18);
    // A waiting permission is an attention row, so the footer carries the
    // triage key alongside the resting help hint.
    assert!(rendered.contains("␣ next ?!"), "{rendered}");
    assert!(rendered.contains("? for help"), "{rendered}");

    let help = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: 0,
            help_visible: true,
            animation_phase: 0,
            line_map: Vec::new(),
            ..Default::default()
        },
        80,
        20,
    );
    assert!(help.contains("keys & legend"));
    assert!(help.contains("? waiting"));
    assert!(help.contains("○ idle"));
    assert!(help.contains("┄ commands ┄"));
    assert!(!help.contains("posture"), "the posture legend is gone");
}

#[test]
fn render_first_run_nudge_invites_launch_when_wired() {
    // Hooks wired but no agent launched yet: the hint invites running one.
    let mut snapshot = snapshot_with(Vec::new(), Vec::new());
    snapshot.agent_hooks_ready = true;
    let rendered = snapshot_to_screen(&snapshot, 80, 18);

    assert!(!rendered.contains("all clear"));
    assert!(rendered.contains("run claude or codex"));
    assert!(!rendered.contains("rimz hooks install"));
    assert_snapshot("first_run_nudge_wired", rendered);
}

#[test]
fn render_active_alert_empty_suppresses_first_run_nudge() {
    // An empty body under an active alert is a failed snapshot, not an
    // empty room — the nudge would misreport. The banner speaks instead.
    let snapshot = snapshot_with(Vec::new(), Vec::new());
    let alert = Alert::active("snapshot failed: ledger not found");
    let rendered = snapshot_to_screen_with_alert(&snapshot, Some(&alert), 80, 18);

    assert!(!rendered.contains("run claude or codex"));
    assert!(!rendered.contains("rimz hooks install"));
}

#[test]
fn render_group_cap_shows_overflow_indicator() {
    let agents = (0..9)
        .map(|i| {
            let mut agent = agent(
                &format!("codex-{i}"),
                "codex",
                AgentStatus::Running,
                Some("/repo/main"),
                Some("main"),
                Some(&format!("task-{i}")),
            );
            agent.last_activity = fixed_now() - Duration::from_secs(i);
            agent
        })
        .collect::<Vec<_>>();
    let snapshot = snapshot_with(Vec::new(), agents);

    // Tall enough that the six capped rows (3 compact lines each, stacked
    // with no inter-card gap) plus the `+3 more` overflow all fit, so the
    // indicator the test is named for actually renders.
    let rendered = snapshot_to_screen(&snapshot, 36, 38);
    assert!(rendered.contains("+3 more"), "{rendered}");
    assert_snapshot("group_cap_with_overflow", rendered);
}

/// L0 density (~24 columns): line 1 still names the row by status glyph
/// and clipped name, and label-less meter chrome from line 2 is dropped
/// when capability data is absent.
#[test]
fn render_l0_density_keeps_identity_when_narrow() {
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("compile"),
    );
    codex.last_activity = fixed_now() - Duration::from_secs(3);
    let snapshot = snapshot_with(Vec::new(), vec![codex]);
    // Tall enough that the card clears the bottom-pinned footer after the
    // cockpit's blank-line + two-line summary header (the agent row is what we
    // measure).
    let rendered = snapshot_to_screen(&snapshot, 24, 11);

    assert!(
        // phase 0 of the working spinner is the first frame `⣾`.
        rendered.contains("⣾ codex"),
        "L0 keeps status glyph + name:\n{rendered}"
    );
    assert!(
        rendered.contains("main"),
        "L0 keeps the worktree label:\n{rendered}"
    );
    assert!(
        !rendered.contains(" · "),
        "L0 drops the capability tokens entirely:\n{rendered}"
    );
    assert_snapshot("l0_density_minimal_row", rendered);
}

fn ui_at_phase(phase: u64) -> UiState {
    UiState {
        selected_index: 0,
        help_visible: false,
        animation_phase: phase,
        line_map: Vec::new(),
        ..Default::default()
    }
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
        fixed_now() - Duration::from_secs(rimz::feed::STALL_WINDOW_SECS as u64 + 60);
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

/// An idle agent on a spent account projects to rate-limited: the row leads
/// with the `⏸` pause and the cockpit gains an `⏸` bucket. It is static —
/// parked, with nothing to do but wait for the reset.
#[test]
fn rate_limited_agent_reads_as_a_static_pause() {
    let now = fixed_now();
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Idle,
        Some("/repo/main"),
        Some("main"),
        None,
    );
    claude.context = Some(AgentContext {
        rate_limits: Some(AgentRateLimits {
            windows: vec![RateLimitWindow {
                used_percentage: Some(100),
                resets_at: Some(now + Duration::from_secs(2 * 3_600)),
                duration_mins: Some(5 * 60),
            }],
        }),
        ..claude_context(now)
    });
    let snapshot = snapshot_with(Vec::new(), vec![claude]);
    let first = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(0), 44, 10);
    let second = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(2), 44, 10);
    assert_eq!(first, second, "a parked agent's head must not animate");
    assert!(
        first.contains('⏸'),
        "the rate-limited row and cockpit show the pause:\n{first}"
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

/// A fully-enriched single-agent group, rendered as raw card lines at a
/// fixed width. Returns the group lines (header first), each flattened to its
/// text — the seam the structural card tests share.
fn card_lines(selected_index: usize) -> Vec<String> {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    claude.context = Some(claude_context(fixed_now()));
    let snapshot = snapshot_with(Vec::new(), vec![claude]);
    let theme = Theme::fixed(true);
    let mut row_index = 0;
    let mut lines = Vec::new();
    let mut map = Vec::new();
    worktree_group_lines(
        &theme,
        &snapshot.worktree_groups[0],
        &snapshot.providers,
        54,
        &snapshot.sidebar.context,
        &mut row_index,
        selected_index,
        0,
        &mut lines,
        &mut map,
    );
    lines
        .into_iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

/// The load-bearing no-flicker guarantee: selecting a row only *appends*
/// lines beneath the card — the resting fold lines (identity, description,
/// ctx bar, token line) keep their exact content, differing only by the
/// selection gutter.
#[test]
fn selecting_a_row_only_appends_never_reshapes_the_fold_lines() {
    let unselected = card_lines(usize::MAX);
    let selected = card_lines(0);

    // Selecting the worktree adds the lane gutter and the dotted seal to its
    // header — chrome, not a card line — but never touches the label itself.
    assert!(unselected[0].contains("main"), "{:?}", unselected[0]);
    assert!(selected[0].contains("main"), "{:?}", selected[0]);
    assert!(
        !unselected[0].contains('┄'),
        "an unselected worktree header is unsealed: {:?}",
        unselected[0]
    );
    assert!(
        selected[0].contains('┄'),
        "the selected worktree header is sealed: {:?}",
        selected[0]
    );
    // Row lines differ only by the leading one-cell gutter; strip it.
    let strip = |line: &String| line.chars().skip(1).collect::<String>();
    let fold: Vec<String> = unselected[1..].iter().map(strip).collect();
    let full: Vec<String> = selected[1..].iter().map(strip).collect();
    // The resting fold is identity + description + ctx bar + the context line.
    assert_eq!(
        fold.len(),
        4,
        "the fold is four card lines (incl. the context line): {fold:?}"
    );
    // The context line rides the resting fold, not a reveal-on-select detail.
    assert!(
        fold.iter().any(|line| line.contains("▤ ")),
        "the context line is part of the resting fold: {fold:?}"
    );
    // This card has no subagents, so selection appends nothing — it only
    // lights the gutter (already stripped), never reshaping a fold line.
    assert_eq!(
        fold, full,
        "selection only appends; it never reshapes the fold lines"
    );
}

/// The expanded card lists the agent's subagents (status glyph + type),
/// nested under the parent and shown only when the row is selected — the
/// resting card never reveals them, preserving the no-reflow invariant.
#[test]
fn expanded_card_lists_subagents_only_when_selected() {
    let parent = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    // A paneless child of the parent, still running — it nests onto the
    // parent's card during snapshot projection.
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
    let theme = Theme::fixed(true);
    let render = |selected_index: usize| {
        let mut row_index = 0;
        let mut lines = Vec::new();
        let mut map = Vec::new();
        worktree_group_lines(
            &theme,
            &snapshot.worktree_groups[0],
            &snapshot.providers,
            54,
            &snapshot.sidebar.context,
            &mut row_index,
            selected_index,
            0,
            &mut lines,
            &mut map,
        );
        lines
            .into_iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let selected = render(0);
    assert!(
        selected.contains("subagents"),
        "expanded card lists subagents:\n{selected}"
    );
    assert!(
        selected.contains("Explore"),
        "the subagent type is shown:\n{selected}"
    );

    let resting = render(usize::MAX);
    assert!(
        !resting.contains("subagents"),
        "the resting card hides the subagent list:\n{resting}"
    );
}

/// The resting card is four lines (identity, description, ctx bar, token
/// line); selecting it appends only the subagent list, so the deepest data is
/// one keystroke away without ever reshaping a resting line.
#[test]
fn resting_card_is_four_lines_and_selection_only_appends() {
    // Card lines, excluding the group header.
    let resting = card_lines(usize::MAX).len() - 1;
    let selected = card_lines(0).len() - 1;
    assert_eq!(resting, 4, "identity, description, ctx, token line");
    // This single-agent fixture has no subagents, so selection appends
    // nothing — the resting height already carries every per-row stat.
    assert_eq!(selected, 4);
}

/// Render one worktree group's lines, asserting the hit-test map stays in
/// lockstep so callers can read either the spans or their text.
fn group_lines(
    snapshot: &SidebarSnapshot,
    theme: &Theme,
    selected_index: usize,
) -> Vec<Line<'static>> {
    let mut row_index = 0;
    let mut lines = Vec::new();
    let mut map = Vec::new();
    worktree_group_lines(
        theme,
        &snapshot.worktree_groups[0],
        &snapshot.providers,
        54,
        &snapshot.sidebar.context,
        &mut row_index,
        selected_index,
        0,
        &mut lines,
        &mut map,
    );
    assert_eq!(map.len(), lines.len(), "map stays in lockstep with lines");
    lines
}

fn line_texts(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

/// A just-started idle agent — idle, on the `Some(0)` baseline gauge with no
/// usage behind it — sheds the 0% context bar and the zeroed stats, resting at
/// identity + description alone with nothing to append on selection. The same
/// 0% reading while *running* still paints the bar, so the suppression is gated
/// on idle, not merely on a zero percent.
#[test]
fn just_started_idle_agent_sheds_the_gauge_and_zeroed_stats() {
    let theme = Theme::fixed(true);
    let mk = |status| {
        let state = agent(
            "claude-1",
            "claude",
            status,
            Some("/repo/main"),
            Some("main"),
            Some("warm up"),
        );
        snapshot_with(Vec::new(), vec![state])
    };

    let idle = mk(AgentStatus::Idle);
    let resting = line_texts(&group_lines(&idle, &theme, usize::MAX));
    let expanded = line_texts(&group_lines(&idle, &theme, 0));

    assert!(
        resting
            .iter()
            .all(|line| !line.contains('▣') && !line.contains('▢')),
        "fresh idle card hides the context bar:\n{}",
        resting.join("\n")
    );
    // Header + identity + description — no gauge or stats at rest.
    assert_eq!(resting.len(), 3, "{resting:?}");
    let joined = expanded.join("\n");
    assert!(
        !joined.contains('▣') && !joined.contains('▤'),
        "expanded fresh idle card hides the bar and the zeroed stats:\n{joined}"
    );
    // A fresh idle card has nothing to append on selection — no stats, no age,
    // no subagents — so expanding it adds no line.
    assert_eq!(expanded.len(), 3, "{expanded:?}");

    let running = line_texts(&group_lines(&mk(AgentStatus::Running), &theme, usize::MAX));
    assert!(
        running.iter().any(|line| line.contains('▢')),
        "a running 0% agent keeps its bar (the hollow ▢ at 0%):\n{}",
        running.join("\n")
    );
}

/// Consecutive cards in a group are separated by one blank line. The group is
/// unselected here, so the separator carries the plain-space gutter (a lane
/// spine would tint it) — exactly one all-blank line, never more.
#[test]
fn consecutive_cards_get_one_blank_separator() {
    let theme = Theme::fixed(true);
    let one = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("task one"),
    );
    let two = agent(
        "claude-2",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("task two"),
    );
    let snapshot = snapshot_with(Vec::new(), vec![one, two]);
    let rendered = line_texts(&group_lines(&snapshot, &theme, usize::MAX));

    let names: Vec<usize> = rendered
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains("claude"))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        names.len(),
        2,
        "two cards in the group:\n{}",
        rendered.join("\n")
    );
    let blanks: Vec<usize> = rendered
        .iter()
        .enumerate()
        .filter(|(_, line)| line.trim().is_empty())
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        blanks,
        vec![names[1] - 1],
        "one blank line sits between the cards:\n{}",
        rendered.join("\n")
    );
}

/// The agent name wears its provider's brand color (Claude's clay), tying the
/// card to the provider dashboard. Read the expected index off the snapshot's
/// own panel so the test follows config overrides.
#[test]
fn agent_name_wears_its_provider_brand_color() {
    let theme = Theme::fixed(false); // color on, so the brand tone survives
    let state = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    let mut snapshot = snapshot_with(Vec::new(), vec![state]);
    // Provider panels are producer-only (`with_provider_aggregates`), so the
    // reducer-built snapshot carries none — set one as the producer would.
    snapshot.providers = vec![provider_panel(
        "claude",
        "Claude Code",
        173,
        true,
        true,
        None,
    )];
    let expected = snapshot.providers[0].color;

    let lines = group_lines(&snapshot, &theme, usize::MAX);
    let name = lines
        .iter()
        .flat_map(|line| &line.spans)
        .find(|span| span.content == "claude")
        .expect("the agent name span");
    assert_eq!(
        name.style.fg,
        Some(Color::Indexed(expected)),
        "the agent name wears the provider color"
    );
}

/// Build a metered provider panel from two rate-limit windows, for the
/// dashboard alignment and golden tests.
fn provider_panel(
    kind: &str,
    product_name: &str,
    color: u8,
    metered: bool,
    remote_control: bool,
    windows: Option<(u8, u8)>,
) -> rimz::SidebarProviderPanel {
    let now = fixed_now();
    let window = |used: u8, mins: u32, resets_in: Duration| RateLimitWindow {
        used_percentage: Some(used),
        resets_at: Some(now + resets_in),
        duration_mins: Some(mins),
    };
    rimz::SidebarProviderPanel {
        kind: kind.to_owned(),
        product_name: product_name.to_owned(),
        art: vec![
            " ▐▛███▜▌".to_owned(),
            "▝▜█████▛▘".to_owned(),
            "  ▘▘ ▝▝".to_owned(),
        ],
        color,
        version: Some("2.1.158".to_owned()),
        plan: Some("Claude Max".to_owned()),
        metered,
        remote_control,
        spending: Some(rimz::SpendTally {
            today: rimz::SpendWindow {
                usd: 3.5,
                tokens: 486_000,
                input: 422_000,
                output: 64_000,
                cache_write: 12_000,
                cache_read: 68_000,
                sessions: 12,
            },
            ..Default::default()
        }),
        windows: windows
            .map(|(five, seven)| {
                vec![
                    window(five, 5 * 60, Duration::from_secs(3 * 3_600 + 12 * 60)),
                    window(
                        seven,
                        7 * 24 * 60,
                        Duration::from_secs(3 * 86_400 + 4 * 3_600),
                    ),
                ]
            })
            .unwrap_or_default(),
    }
}

/// Every provider bar — `5h`, `7d` across blocks, and the unmetered `∞` —
/// shares one front (bar-start) column and one end (bar-end) column, so the
/// whole dashboard reads as one aligned grid. The structural payoff of the
/// shared bar grammar, now that the budgets live in the panel.
#[test]
fn provider_bars_share_one_front_and_end_column() {
    let theme = Theme::fixed(true);
    let panels = vec![
        provider_panel("claude", "Claude", 173, true, true, Some((25, 40))),
        provider_panel("codex", "Codex", 33, true, false, Some((55, 8))),
        provider_panel("pi", "Pi", 28, false, false, None),
    ];
    // Rendered narrow so the art column is dropped and the bar lines carry no
    // stray block glyphs from the emblem — the bar grid is what we measure.
    // The tabbed dashboard paints one panel at a time, so each panel renders
    // as its own active tab and the grid is asserted across those frames.
    let lines: Vec<String> = panels
        .iter()
        .flat_map(|panel| provider_panel_lines(&theme, &panels, Some(panel.kind.as_str()), 30).0)
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .filter(|line| line.contains('▰') || line.contains('▱') || line.contains('▒'))
        .collect();
    assert!(lines.len() >= 5, "two metered providers + one ∞: {lines:?}");
    // Bar start: the first bar cell (tick or shade), by char column.
    let start = |line: &str| {
        line.chars()
            .position(|c| matches!(c, '▰' | '▱' | '▒'))
            .unwrap()
    };
    let starts: Vec<usize> = lines.iter().map(|line| start(line)).collect();
    assert!(
        starts.iter().all(|&s| s == starts[0]),
        "provider bars share a front column: {starts:?}"
    );
    // Bar end: the last bar cell column.
    let end = |line: &str| {
        line.char_indices()
            .filter(|(_, c)| matches!(c, '▰' | '▱' | '▒'))
            .count()
            + start(line)
    };
    let ends: Vec<usize> = lines.iter().map(|line| end(line)).collect();
    assert!(
        ends.iter().all(|&e| e == ends[0]),
        "provider bars share an end column: {ends:?}"
    );
}

/// The metered bar rows of one panel (5h then 7d), rendered narrow so the art
/// column drops and each row's first span is its label. Filters to the lines
/// carrying bar glyphs.
fn metered_bar_rows(theme: &Theme, panel: &rimz::SidebarProviderPanel) -> Vec<Line<'static>> {
    provider_panel_lines(theme, std::slice::from_ref(panel), None, 30)
        .0
        .into_iter()
        .filter(|line| {
            line.spans
                .iter()
                .any(|span| span.content.contains('▰') || span.content.contains('▱'))
        })
        .collect()
}

/// The label foreground, the first bar-glyph foreground, and whether the row
/// carries a `↻` reset countdown — the three things req 1/2 turn on.
fn bar_row_facts(line: &Line<'static>) -> (Option<Color>, Option<Color>, bool) {
    let label_fg = line.spans.first().and_then(|span| span.style.fg);
    let glyph_fg = line
        .spans
        .iter()
        .find(|span| span.content.contains('▰') || span.content.contains('▱'))
        .and_then(|span| span.style.fg);
    let has_reset = line.spans.iter().any(|span| span.content.contains('↻'));
    (label_fg, glyph_fg, has_reset)
}

/// Each `5h`/`7d` label mirrors its own bar's severity color, so a green and a
/// yellow window read as two differently-toned rows, not one dim slab.
#[test]
fn provider_label_mirrors_its_bar_color() {
    let theme = Theme::fixed(false);
    // 5h: 25% used → 75% left → green. 7d: 70% used → 30% left → yellow.
    let panel = provider_panel("claude", "Claude", 173, true, false, Some((25, 70)));
    let rows = metered_bar_rows(&theme, &panel);
    assert_eq!(rows.len(), 2, "a metered panel draws a 5h and a 7d row");
    let (five_label, five_glyph, _) = bar_row_facts(&rows[0]);
    let (seven_label, seven_glyph, _) = bar_row_facts(&rows[1]);
    assert_eq!(five_label, five_glyph, "5h label mirrors its bar");
    assert_eq!(seven_label, seven_glyph, "7d label mirrors its bar");
    assert_ne!(
        five_label, seven_label,
        "a green 5h and a yellow 7d label differ in tone"
    );
}

/// A spent weekly cap gates the short window: with 7d exhausted the 5h row is
/// painted exhausted — red, a full empty track, and no reset countdown —
/// regardless of the 5h window's own (here untouched) reading.
#[test]
fn seven_day_exhaustion_reddens_and_silences_the_five_hour_row() {
    let theme = Theme::fixed(false);
    // 5h is untouched (would be green with a countdown); 7d is fully spent.
    let panel = provider_panel("claude", "Claude", 173, true, false, Some((0, 100)));
    let rows = metered_bar_rows(&theme, &panel);
    assert_eq!(rows.len(), 2);
    let (five_label, _, five_has_reset) = bar_row_facts(&rows[0]);
    let (seven_label, _, _) = bar_row_facts(&rows[1]);
    assert!(!five_has_reset, "the cascaded 5h row drops its countdown");
    assert!(
        !rows[0].spans.iter().any(|span| span.content.contains('▰')),
        "the cascaded 5h bar is a full empty track, no fill"
    );
    assert_eq!(
        five_label, seven_label,
        "the cascaded 5h label reddens to match the exhausted 7d"
    );
}

/// A provider that reports a single window draws exactly one bar, labeled by
/// the window's own length — the model isn't pinned to a fixed set. (A
/// transient Codex server bug once widened its window to ~30 days; this is what
/// rendered, instead of mislabeling it `7d`.)
#[test]
fn single_window_panel_draws_one_bar_labeled_by_length() {
    let theme = Theme::fixed(false);
    let now = fixed_now();
    let mut codex = provider_panel("codex", "Codex", 33, true, false, None);
    codex.windows = vec![RateLimitWindow {
        used_percentage: Some(7),
        resets_at: Some(now + Duration::from_secs(28 * 86_400 + 4 * 3_600)),
        duration_mins: Some(43_800),
    }];
    let rows = metered_bar_rows(&theme, &codex);
    assert_eq!(rows.len(), 1, "one window → one bar");
    let label = rows[0]
        .spans
        .first()
        .expect("a label span")
        .content
        .trim()
        .to_owned();
    assert_eq!(label, "30d", "the ~30-day window is labeled 30d");
    let (_, _, has_reset) = bar_row_facts(&rows[0]);
    assert!(has_reset, "the bar carries its reset countdown");
}

/// A not-started window drops its countdown — these budgets begin counting
/// only on the first token, so until then the provider keeps `resets_at` slid a
/// full window-length ahead. It's detected by the reset distance, not a 0%
/// reading: the real Claude shape is `usedPercent: 1` with the reset still ~a
/// full 5h out (`4h59m`). Its bar shows full with no countdown.
#[test]
fn not_started_window_drops_its_countdown() {
    let theme = Theme::fixed(false);
    let now = fixed_now();
    let mut claude = provider_panel("claude", "Claude", 173, true, false, None);
    // The real not-started shape: ~1% used, reset slid a full 5h ahead (a hair
    // under, here 4h59m30s, the way a live reading reads).
    claude.windows = vec![RateLimitWindow {
        used_percentage: Some(1),
        resets_at: Some(now + Duration::from_secs(5 * 3_600 - 30)),
        duration_mins: Some(5 * 60),
    }];
    let rows = metered_bar_rows(&theme, &claude);
    assert_eq!(rows.len(), 1);
    let (_, _, has_reset) = bar_row_facts(&rows[0]);
    assert!(
        !has_reset,
        "a not-started window (reset ~ full 5h) shows no countdown"
    );
    assert!(
        rows[0].spans.iter().any(|span| span.content.contains('▰')),
        "the not-started window shows a full bar, not an empty/exhausted track"
    );
}

/// Codex reports `usedPercent: 99` with no `resetsAt` before the first token —
/// the bar should be full (not 1% remaining) and the countdown absent.
#[test]
fn codex_not_started_shows_full_bar() {
    let theme = Theme::fixed(false);
    let mut codex = provider_panel("codex", "Codex", 33, true, false, None);
    codex.windows = vec![RateLimitWindow {
        used_percentage: Some(99),
        resets_at: None,
        duration_mins: Some(5 * 60),
    }];
    let rows = metered_bar_rows(&theme, &codex);
    assert_eq!(rows.len(), 1);
    let (_, _, has_reset) = bar_row_facts(&rows[0]);
    assert!(!has_reset, "Codex not-started: no reset countdown");
    assert!(
        !rows[0].spans.iter().any(|span| span.content.contains('▱')),
        "Codex not-started: bar is full, no empty track cells"
    );
}

/// The full provider stats line (all spans joined) of one rendered panel.
fn stats_line(theme: &Theme, panel: &rimz::SidebarProviderPanel) -> String {
    provider_panel_lines(theme, std::slice::from_ref(panel), None, 40)
        .0
        .into_iter()
        .flat_map(|line| line.spans)
        .map(|span| span.content.into_owned())
        .collect()
}

/// The provider stats line reads today's transcript-history spend *and* token
/// burn from the JSONL `spending`, never the live active-session sum — the one
/// figure that also holds for a token-only provider (Codex) with no live cost.
#[test]
fn provider_stats_read_todays_jsonl_spend_and_tokens() {
    let theme = Theme::fixed(false);
    let mut codex = provider_panel("codex", "Codex", 33, false, false, None);
    codex.spending = Some(rimz::SpendTally {
        today: rimz::SpendWindow {
            usd: 4.20,
            tokens: 486_000,
            input: 422_000,
            output: 64_000,
            cache_write: 0,
            cache_read: 68_000,
            sessions: 5,
        },
        ..Default::default()
    });
    let stats = stats_line(&theme, &codex);
    assert!(stats.contains("$4.20"), "today's JSONL spend: {stats:?}");
    // The today line reads the coarse integer form (`◇ 486k`), with the split.
    assert!(stats.contains("486k"), "today's JSONL tokens: {stats:?}");
    assert!(stats.contains("↗ 64k"), "the output split: {stats:?}");
}

/// A started window — its reset has ticked well below the full window — keeps
/// its countdown, even at the same low 1% usage as a not-started one. Usage
/// alone can't tell them apart; the reset distance does.
#[test]
fn started_window_keeps_its_countdown() {
    let theme = Theme::fixed(false);
    let now = fixed_now();
    let mut claude = provider_panel("claude", "Claude", 173, true, false, None);
    claude.windows = vec![RateLimitWindow {
        used_percentage: Some(1),
        resets_at: Some(now + Duration::from_secs(4 * 3_600)),
        duration_mins: Some(5 * 60),
    }];
    let rows = metered_bar_rows(&theme, &claude);
    assert_eq!(rows.len(), 1);
    let (_, _, has_reset) = bar_row_facts(&rows[0]);
    assert!(
        has_reset,
        "a started window (reset well below full) shows its countdown"
    );
}

/// Usage above the ~1% not-started floor means the window has started — keep its
/// countdown even when the reset still reads a near-full window. The reset-distance
/// grace only applies to a window at or below the floor (0–1% used); any real
/// usage short-circuits to "started".
#[test]
fn used_window_keeps_countdown_despite_near_full_reset() {
    let theme = Theme::fixed(false);
    let now = fixed_now();
    let mut claude = provider_panel("claude", "Claude", 173, true, false, None);
    // 5% used with the reset slid a full 5h out: usage above the floor wins, so
    // this counts as started despite the near-full reset.
    claude.windows = vec![RateLimitWindow {
        used_percentage: Some(5),
        resets_at: Some(now + Duration::from_secs(5 * 3_600 - 30)),
        duration_mins: Some(5 * 60),
    }];
    let rows = metered_bar_rows(&theme, &claude);
    assert_eq!(rows.len(), 1);
    let (_, _, has_reset) = bar_row_facts(&rows[0]);
    assert!(
        has_reset,
        "usage above ~1% shows the countdown even with a near-full reset"
    );
}

/// Two providers on the dashboard fixture: the metered Claude (rc flag on,
/// 5h/7d windows) and the unmetered Codex with a plan, version, and today's
/// spending. Shared by the tabbed-dashboard tests.
fn two_provider_panels() -> Vec<rimz::SidebarProviderPanel> {
    vec![
        provider_panel("claude", "Claude", 173, true, true, Some((25, 40))),
        {
            let mut codex = provider_panel("codex", "Codex", 33, false, false, None);
            codex.plan = Some("ChatGPT Pro".to_owned());
            codex.version = Some("0.135.0".to_owned());
            codex.spending = Some(rimz::SpendTally {
                today: rimz::SpendWindow {
                    usd: 1.2,
                    tokens: 88_000,
                    input: 76_000,
                    output: 12_000,
                    cache_write: 0,
                    cache_read: 8_000,
                    sessions: 3,
                },
                ..Default::default()
            });
            codex
        },
    ]
}

/// The tab bar holds to one screen row: a tab that would overflow the panel
/// width is dropped whole — label and hit together — so the hit map stays in
/// lockstep with the frame however many kinds register or however narrow the
/// pane.
#[test]
fn tab_bar_drops_whole_tabs_that_overflow_the_width() {
    let theme = Theme::fixed(false);
    let panels = vec![
        provider_panel("claude", "Claude", 173, true, false, Some((25, 40))),
        provider_panel("codex", "Codex", 33, false, false, None),
        provider_panel("pi", "Pi", 28, false, false, None),
    ];
    // `▸claude` (7) + gap (2) + ` codex` (6) = 15 fits in 16; ` pi` would
    // land at 20, so it drops whole.
    let (lines, hits) = provider_panel_lines(&theme, &panels, Some("claude"), 16);
    let tab_line: String = lines[0]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert_eq!(tab_line, "▸claude   codex");
    assert_eq!(
        hits.iter().map(|hit| hit.kind.as_str()).collect::<Vec<_>>(),
        vec!["claude", "codex"],
        "the dropped tab carries no hit"
    );
    assert!(tab_line.chars().count() <= 16, "the bar never wraps");
}

/// The pinned per-provider dashboard, tabbed: the tab bar names every account
/// (the active tab behind the `▸` marker), and only the active provider's
/// block paints — here the selection-derived Claude tab, a metered block
/// (header with version and plan on the left, the `⇅ rc` flag pinned
/// top-right; the brand emblem; the `◎` session count leading today's stats;
/// 5h/7d "mana" bars draining toward their resets). The other account stays a
/// dim tab label, its block off screen.
#[test]
fn render_provider_dashboard_pins_panel_with_bars_and_rc_flag() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    claude.context = Some(claude_context(fixed_now()));
    let mut snapshot = snapshot_with(Vec::new(), vec![claude]);
    snapshot.providers = two_provider_panels();
    let rendered = snapshot_to_screen(&snapshot, 54, 34);

    // The tab bar: the selected pane runs Claude, so its tab wears the `▸`
    // marker and Codex rests beside it.
    assert!(
        rendered.contains("▸claude"),
        "the selection-derived active tab:\n{rendered}"
    );
    assert!(rendered.contains("codex"), "the resting tab:\n{rendered}");
    // The metered Claude block: header carries the version and plan on the
    // left with the `⇅ rc` remote-control flag pinned to the top-right corner,
    // the stats line leads with today's `◎` session count, then the 5h/7d
    // budget bars drain.
    assert!(
        rendered.contains("Claude v2.1.158 · Claude Max"),
        "{rendered}"
    );
    assert!(
        rendered.contains("⇅ rc"),
        "rc flag pinned right:\n{rendered}"
    );
    assert!(rendered.contains('◎'), "the session count:\n{rendered}");
    assert!(rendered.contains("5h"), "{rendered}");
    assert!(rendered.contains("7d"), "{rendered}");
    assert!(rendered.contains('▰'), "a draining mana bar:\n{rendered}");
    assert!(rendered.contains('↻'), "a reset countdown:\n{rendered}");
    // The inactive Codex block stays off screen — only its tab label shows.
    assert!(
        !rendered.contains("Codex v0.135.0"),
        "the inactive tab paints no block:\n{rendered}"
    );
    assert!(!rendered.contains('∞'), "no unmetered bar:\n{rendered}");
    assert_snapshot("provider_dashboard", rendered);
}

/// A manual tab pick (`←`/`→` or a click on the label) swaps the dashboard to
/// that provider's block: the `▸` marker moves to `codex` and the unmetered
/// block (the `∞` icon at the front, an empty `▱` track, no countdown) paints
/// where Claude's was, fleet ledger and footer untouched.
#[test]
fn render_provider_dashboard_manual_tab_shows_the_picked_block() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    claude.context = Some(claude_context(fixed_now()));
    let mut snapshot = snapshot_with(Vec::new(), vec![claude]);
    snapshot.providers = two_provider_panels();
    let ui = UiState {
        dashboard_tab: Some(DashboardTab {
            kind: "codex".to_owned(),
            derived_at_start: Some("claude".to_owned()),
        }),
        ..Default::default()
    };
    let rendered = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, 54, 34);

    assert!(
        rendered.contains("▸codex"),
        "the picked tab wears the marker:\n{rendered}"
    );
    assert!(
        rendered.contains("Codex v0.135.0 · ChatGPT Pro"),
        "{rendered}"
    );
    assert!(rendered.contains('∞'), "infinity at the front:\n{rendered}");
    assert!(rendered.contains('▱'), "the empty ∞ track:\n{rendered}");
    assert!(
        !rendered.contains("Claude v2.1.158"),
        "the unpicked block stays off screen:\n{rendered}"
    );
    assert_snapshot("provider_dashboard_codex_tab", rendered);
}

/// With no manual pick, the tab focus follows the selected pane's provider:
/// a selected Codex row lands the `▸` on the codex tab and paints its block,
/// however the panels are ordered.
#[test]
fn render_provider_dashboard_tab_follows_the_selected_agent() {
    let codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("add tests"),
    );
    let mut snapshot = snapshot_with(Vec::new(), vec![codex]);
    snapshot.providers = two_provider_panels();
    let rendered = snapshot_to_screen(&snapshot, 54, 34);

    assert!(
        rendered.contains("▸codex"),
        "the selected agent's tab is active:\n{rendered}"
    );
    assert!(
        rendered.contains("Codex v0.135.0 · ChatGPT Pro"),
        "{rendered}"
    );
    assert!(
        !rendered.contains("Claude v2.1.158"),
        "the other block stays off screen:\n{rendered}"
    );
}

/// A dashboard with a single account keeps its block bare — no tab bar: there
/// is nothing to switch to, so the header line alone names the provider.
#[test]
fn render_single_provider_dashboard_has_no_tab_bar() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    claude.context = Some(claude_context(fixed_now()));
    let mut snapshot = snapshot_with(Vec::new(), vec![claude]);
    snapshot.providers = vec![provider_panel(
        "claude",
        "Claude",
        173,
        true,
        true,
        Some((25, 40)),
    )];
    let rendered = snapshot_to_screen(&snapshot, 54, 34);
    assert!(
        !rendered.contains('▸'),
        "one account, no tab bar:\n{rendered}"
    );
    assert!(
        rendered.contains("Claude v2.1.158 · Claude Max"),
        "{rendered}"
    );
}

/// The fleet ledger pinned at the bottom of the dashboard: the static
/// `W:` (week) and `M:` (month) rows, each reading `◎ sessions  ◇ ↘ ↗ ◌
/// $spend` across every provider — precise one-decimal token figures and the
/// bold money-green spend, right-aligned into one aligned grid. Today's
/// headline stays in the cockpit's animated `$`, never repeated here.
#[test]
fn render_fleet_ledger_pins_week_month_rows_under_the_dashboard() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    claude.context = Some(claude_context(fixed_now()));
    let mut snapshot = snapshot_with(Vec::new(), vec![claude]);
    snapshot.providers = vec![provider_panel(
        "claude",
        "Claude",
        173,
        true,
        true,
        Some((25, 40)),
    )];
    snapshot.value_tally = Some(rimz::SpendTally {
        today: rimz::SpendWindow {
            usd: 40.23,
            tokens: 3_300_000,
            input: 300_000,
            output: 3_000_000,
            cache_write: 120_000,
            cache_read: 6_800_000,
            sessions: 12,
        },
        week: rimz::SpendWindow {
            usd: 312.40,
            tokens: 21_000_000,
            input: 2_300_000,
            output: 18_700_000,
            cache_write: 900_000,
            cache_read: 51_000_000,
            sessions: 92,
        },
        month: rimz::SpendWindow {
            usd: 1_240.57,
            tokens: 33_000_000,
            input: 4_300_000,
            output: 28_700_000,
            cache_write: 1_900_000,
            cache_read: 121_000_000,
            sessions: 212,
        },
        year: rimz::SpendWindow {
            usd: 4_821.90,
            tokens: 47_200_000,
            input: 7_200_000,
            output: 40_000_000,
            cache_write: 3_000_000,
            cache_read: 210_000_000,
            sessions: 980,
        },
    });
    let rendered = snapshot_to_screen(&snapshot, 60, 34);

    // The `W:` and `M:` rows: each labelled left, the `$` spend pinned right.
    assert!(rendered.contains("W:"), "the week ledger row:\n{rendered}");
    assert!(rendered.contains("M:"), "the month ledger row:\n{rendered}");
    assert!(
        rendered.contains("$312.40"),
        "this week's spend:\n{rendered}"
    );
    assert!(
        rendered.contains("$1,240.57"),
        "this month's spend:\n{rendered}"
    );
    // Session counts and the precise (one-decimal) token total.
    assert!(rendered.contains("212"), "month session count:\n{rendered}");
    assert!(
        rendered.contains("33.0M"),
        "month token total, precise form:\n{rendered}"
    );
    // The `year` window is no longer surfaced — the ledger tops out at month.
    assert!(
        !rendered.contains("$4,821.90"),
        "the year pile is gone from the ledger:\n{rendered}"
    );
    assert_snapshot("fleet_ledger", rendered);
}

/// The borderless repo header (dashboard L1): the workspace name behind `⌘`
/// on the left, then the project path pinned to the right edge of the same
/// line — no `⌂` glyph, the dim path opposite the name reads as a path.
#[test]
fn repo_header_shows_name_then_path() {
    let mut snapshot = snapshot_with(Vec::new(), Vec::new());
    snapshot.project_root = Some(std::path::PathBuf::from("/srv/code/query-engine"));
    let rendered = snapshot_to_screen(&snapshot, 44, 12);
    let first = rendered.lines().next().unwrap_or_default();
    let name_at = first.find("⌘ query-engine").expect("name on line 1");
    let path_at = first
        .find("/srv/code/query-engine")
        .expect("path on line 1");
    assert!(name_at < path_at, "name leads, path pins right: {first:?}");
    assert!(
        !rendered.contains('⌂'),
        "the ⌂ path glyph is gone:\n{rendered}"
    );
}

#[test]
fn home_abbreviation_collapses_only_a_home_prefix() {
    assert_eq!(
        abbreviate_under("/home/dev/code/query-engine", Some("/home/dev")),
        "~/code/query-engine"
    );
    assert_eq!(abbreviate_under("/home/dev", Some("/home/dev")), "~");
    // A path that merely shares a textual prefix is not under home.
    assert_eq!(
        abbreviate_under("/home/developer/x", Some("/home/dev")),
        "/home/developer/x"
    );
    // Outside home, or no home, passes through.
    assert_eq!(
        abbreviate_under("/srv/code", Some("/home/dev")),
        "/srv/code"
    );
    assert_eq!(abbreviate_under("/srv/code", None), "/srv/code");
}

/// The cockpit summary leads with today's sessions (`◎ N`, under the name)
/// over the live-agent count (`¤ N`); the cockpit below it splits the
/// make-up at a fixed height — the left cluster (`? ! ○ ⏸`, each glyph
/// spaced from its count, a zero reading `? 0`) and the busy/done tail
/// (`✽ ⢿ ✓`) — so the body never shifts as agents change state.
#[test]
fn fleet_header_is_fixed_and_splits_the_make_up() {
    // Borderless layout: row 0 is the name, row 1 a blank line, row 2 the `◎`
    // summary, row 3 the `¤` summary, row 4 the hairline rule. An empty room
    // reads `◎ 0` on row 2 with no make-up beneath, so the body never moves.
    let empty = snapshot_with(Vec::new(), Vec::new());
    let empty_screen = snapshot_to_screen(&empty, 40, 12);
    assert!(
        empty_screen.lines().nth(2).unwrap().contains("◎ 0"),
        "{empty_screen}"
    );
    assert!(
        empty_screen.lines().nth(3).unwrap().contains("¤ 0"),
        "{empty_screen}"
    );

    let working = agent(
        "w",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("a"),
    );
    let mut reasoning = agent(
        "t",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("b"),
    );
    // The thinking head is a per-row animation, never a cockpit bucket: a
    // pre-edit turn still tallies as working.
    reasoning.phase = rimz::agents::TurnPhase::Reasoning;
    let snapshot = snapshot_with(Vec::new(), vec![working, reasoning]);
    let screen = snapshot_to_screen(&snapshot, 40, 12);
    // Row 2 is the `◎` summary, row 3 the `¤` summary; row 5 is the bucket
    // make-up (row 1 is the blank line, row 4 the hairline rule).
    assert!(screen.lines().nth(3).unwrap().contains("¤ 2"), "{screen}");
    let buckets = screen.lines().nth(5).unwrap();
    // Left cluster: waiting/failed/idle and the parked rate-limited each show
    // their count (a zero reads `? 0`); both running agents tally into the
    // one working (⢿) bucket.
    assert!(buckets.contains("? 0"), "{buckets}");
    assert!(buckets.contains("! 0"), "{buckets}");
    assert!(buckets.contains("○ 0"), "{buckets}");
    // The rate-limited glyph carries the U+FE0E text-presentation selector.
    assert!(buckets.contains("⏸\u{FE0E} 0"), "{buckets}");
    assert!(buckets.contains("⢿ 2"), "{buckets}");
    assert!(!buckets.contains('✽'), "no thinking bucket: {buckets}");
    // The default selection lands on the first row, so its worktree reads as
    // one lane: the header gains the dotted seal and a `▏` lane spine.
    assert!(
        screen.lines().any(|line| line.contains("main")),
        "fleet header wrapped or shifted:\n{screen}"
    );
    assert!(
        screen.contains('▏'),
        "the selected worktree shows the lane spine:\n{screen}"
    );
}

/// The cockpit `?`/`!` buckets echo the per-row glyph escalation: each
/// wears its oldest contributing row's age heat over the same yellow floor
/// — yellow while fresh, amber past the half hour, red past the hour.
#[test]
fn attention_bucket_wears_the_oldest_rows_age_heat() {
    let theme = Theme::fixed(false);
    let bucket_fg = |idle_secs: u64| {
        let mut waiting = agent(
            "w",
            "claude",
            AgentStatus::Waiting,
            Some("/repo/main"),
            Some("main"),
            Some("a"),
        );
        waiting.last_activity = fixed_now() - Duration::from_secs(idle_secs);
        let snapshot = snapshot_with(Vec::new(), vec![waiting]);
        fleet_header_lines(&theme, &snapshot.worktree_groups, 60)
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content.contains('?'))
            .map(|span| span.style.fg)
            .expect("the make-up line carries the ? bucket")
    };
    let style = |color| theme.style(color, Modifier::BOLD).fg;
    assert_eq!(bucket_fg(5 * 60), style(Color::Yellow), "yellow floor");
    assert_eq!(
        bucket_fg(40 * 60),
        style(theme::ORANGE),
        "amber past the half hour"
    );
    assert_eq!(
        bucket_fg(2 * 60 * 60),
        style(Color::Red),
        "red past the hour"
    );
}

/// A compacting agent counts as **working** (`⢿`) in the cockpit — the
/// compaction pulse, like the thinking sparkle, is a per-row head and never
/// a bucket.
#[test]
fn compacting_agent_counts_as_working() {
    let mut compacting = agent(
        "c",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("t"),
    );
    compacting.compacting_since = Some(fixed_now());
    compacting.phase = rimz::agents::TurnPhase::Reasoning;
    let snapshot = snapshot_with(Vec::new(), vec![compacting]);
    let screen = snapshot_to_screen(&snapshot, 40, 12);
    // Row 5 is the make-up: name(0), blank(1), `¤`(2), `◎`(3), hairline(4).
    let buckets = screen.lines().nth(5).unwrap();
    assert!(
        buckets.contains("⢿ 1"),
        "compacting counts as working: {buckets}"
    );
    assert!(!buckets.contains('✽'), "no thinking bucket: {buckets}");
}

#[test]
fn auto_scroll_nudges_the_selection_minimally_into_view() {
    // A hand-built scroll-zone map: a leading gap, row 0 on lines 1-2, row 1
    // on lines 3-4, row 2 an expanded card on lines 5-8.
    let map = vec![
        None,
        Some(0),
        Some(0),
        Some(1),
        Some(1),
        Some(2),
        Some(2),
        Some(2),
        Some(2),
    ];
    // Fully visible: the window doesn't move.
    assert_eq!(auto_scroll_to_selection(&map, 1, 0, 5), 0);
    // Above the window: scroll up to the card's first line.
    assert_eq!(auto_scroll_to_selection(&map, 0, 4, 5), 1);
    // Below the window: scroll down just enough for its last line.
    assert_eq!(auto_scroll_to_selection(&map, 2, 0, 5), 4);
    // Taller than the viewport: pin the card's first line to the top.
    assert_eq!(auto_scroll_to_selection(&map, 2, 0, 3), 5);
    // Absent from the zone: hold the clamped offset.
    assert_eq!(auto_scroll_to_selection(&map, 9, 2, 5), 2);
    // Degenerate zero-height viewport: hold.
    assert_eq!(auto_scroll_to_selection(&map, 1, 2, 0), 2);
}

#[test]
fn scroll_thumb_reads_top_and_bottom_true() {
    // 10 zone lines through a 5-row viewport: the thumb spans half the track.
    assert_eq!(scroll_thumb(0, 10, 5), (0, 2));
    // At the bottom (offset == max) the thumb pins to the track's last rows.
    assert_eq!(scroll_thumb(5, 10, 5), (3, 2));
    // Midway it sits between, flush at neither end.
    assert_eq!(scroll_thumb(2, 10, 5), (1, 2));
    // A huge zone never shrinks the thumb below one row.
    assert_eq!(scroll_thumb(0, 1_000, 4), (0, 1));
}

/// Six short running cards across two worktrees — taller than the small frames
/// the scroll goldens render, so the cards overflow the viewport between the
/// pinned cockpit and footer. Same-tier paneless groups order by label, so
/// `alpha` (task-0..2) leads `beta` (task-3..5) and the task number reads as
/// the visible row index.
fn overflowing_fleet() -> SidebarSnapshot {
    let now = fixed_now();
    let mut agents = Vec::new();
    for i in 0..6 {
        let (path, branch) = if i < 3 {
            ("/repo/alpha", "alpha")
        } else {
            ("/repo/beta", "beta")
        };
        let mut codex = agent(
            &format!("codex-{i}"),
            "codex",
            AgentStatus::Running,
            Some(path),
            Some(branch),
            Some(&format!("task-{i}")),
        );
        codex.last_activity = now - Duration::from_secs(8);
        agents.push(codex);
    }
    snapshot_with(Vec::new(), agents)
}

#[test]
fn render_scroll_overflow_shows_bar() {
    // More cards than the frame holds: the cockpit stays pinned at the top,
    // the footer at the bottom, and the cards scroll between them behind a
    // right-margin scrollbar — the thumb at the top of the track, since the
    // selection (row 0) holds the window at the zone's start.
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &overflowing_fleet(),
        None,
        &UiState::default(),
        38,
        18,
    );
    let lines: Vec<&str> = rendered.lines().collect();
    assert!(
        lines[0].contains("⌘ query-engine"),
        "cockpit pinned:\n{rendered}"
    );
    assert!(lines[5].contains("⢿ 6"), "make-up pinned:\n{rendered}");
    assert!(
        lines.last().unwrap().contains("? for help"),
        "footer pinned:\n{rendered}"
    );
    assert!(rendered.contains('▐'), "the thumb renders:\n{rendered}");
    assert!(rendered.contains('▕'), "the track renders:\n{rendered}");
    assert_snapshot("scroll_overflow_shows_bar", rendered);
}

#[test]
fn render_scroll_offset_follows_selection_to_bottom() {
    // Selecting the last row auto-scrolls its card fully into view and the
    // thumb pins to the bottom of the track.
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &overflowing_fleet(),
        None,
        &UiState {
            selected_index: 5,
            ..Default::default()
        },
        38,
        18,
    );
    assert!(
        rendered.contains("task-5"),
        "the selected last card is in view:\n{rendered}"
    );
    assert!(
        !rendered.contains("task-0"),
        "the zone's head scrolled off:\n{rendered}"
    );
    assert_snapshot("scroll_offset_follows_selection_to_bottom", rendered);
}

#[test]
fn render_scroll_pins_tall_expanded_card_top() {
    // A selected card whose expanded subagent list outgrows the viewport pins
    // its first line — the group header — to the top of the scroll zone.
    let now = fixed_now();
    let mut parent = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    parent.last_activity = now - Duration::from_secs(8);
    let mut agents = vec![parent];
    for i in 0..4_u64 {
        let mut child = agent(
            &format!("child-{i}"),
            "claude",
            AgentStatus::Running,
            None,
            None,
            Some("Explore"),
        );
        child.parent_agent_id = Some("claude-1".into());
        child.subagent_description = Some(format!("survey area {i}"));
        child.subagent_started_at = Some(now - Duration::from_secs(240 - i * 30));
        child.last_activity = now;
        child.total_tokens = Some(1_000 * (i + 1));
        agents.push(child);
    }
    let snapshot = snapshot_with(Vec::new(), agents);

    let rendered =
        snapshot_to_screen_with_alert_and_ui(&snapshot, None, &UiState::default(), 54, 13);
    // The viewport opens on the worktree header (the card block's first line)
    // — the gap above it scrolled off — and the subagent list fills down.
    let lines: Vec<&str> = rendered.lines().collect();
    assert!(
        lines[6].contains("⑂ main"),
        "the tall card's first line pins to the viewport top:\n{rendered}"
    );
    assert!(
        rendered.contains("⧉ subagents (4)"),
        "the expanded list is what overflows:\n{rendered}"
    );
    assert_snapshot("scroll_pins_tall_expanded_card_top", rendered);
}

#[test]
fn render_scroll_manual_offset_holds() {
    // A wheel pin holds the user's window even though the selection (row 0)
    // sits above the fold — the peek wins until the selection changes.
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &overflowing_fleet(),
        None,
        &UiState {
            scroll_offset: 6,
            manual_scroll: Some(ManualScroll {
                selection_at_start: None,
            }),
            ..Default::default()
        },
        38,
        18,
    );
    assert!(
        !rendered.contains("task-0"),
        "the selected card stays beyond the fold while pinned:\n{rendered}"
    );
    assert_snapshot("scroll_manual_offset_holds", rendered);
}
