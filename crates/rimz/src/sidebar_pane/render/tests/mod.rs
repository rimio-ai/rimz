use crate::agents::{
    AgentContext, AgentCost, AgentCurrentUsage, AgentRateLimits, AgentSessionUsage,
    AgentTokenUsage, AgentTurnError, RateLimitWindow, TurnErrorClass,
};
use crate::agents::{AgentState, AgentStatus};
use crate::config::{AnimationSpec, ScrollbarMode};
use crate::ids::{MuxName, PaneId, ViewKind};
use crate::pane::PaneRef;
use crate::{SidebarSnapshot, WorkspaceId};
use jiff::Timestamp;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use std::rc::Rc;
use std::time::Duration;

use super::chrome::abbreviate_under;
use super::sections::{
    RowCtx, Tier, content_width, dashboard_panel_lines_with_footer, fleet_header_lines,
    fleet_store_lines, reset_expiry_heat_amount, worktree_group_lines,
};
use crate::sidebar_pane::pixel::meter::MeterPixels;

mod agent_card;
mod alert;
mod animation;
mod ansi;
mod budget;
mod capping;
mod fleet;
mod fuzz;
mod gallery;
mod link;
mod presence;
mod process;
mod provider;
mod scroll;
mod worktree;

use super::*;

fn fixed_workspace() -> WorkspaceId {
    WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap()
}

fn fixed_now() -> Timestamp {
    // Pin every test to one timestamp so the redaction filter has a
    // deterministic input to scrub.
    Timestamp::from_second(1_700_000_000).unwrap()
}

fn provider_panel_lines(
    theme: &Theme,
    providers: &[crate::SidebarProviderPanel],
    active_kind: Option<&str>,
    tabbed: bool,
    width: usize,
    zones: &crate::config::BudgetBarConfig,
    now: Timestamp,
) -> (Vec<Line<'static>>, Vec<ProviderTabHit>) {
    let active_tab = active_kind.map(str::to_owned);
    let (lines, hits) = dashboard_panel_lines_with_footer(
        theme,
        providers,
        active_tab.as_ref(),
        tabbed,
        None,
        None,
        false,
        None,
        width,
        zones,
        now,
    );
    (lines, hits)
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
    Backend::clear_region(terminal.backend_mut(), ClearType::All).unwrap();
    let mut ui = fixed_theme_ui(snapshot, ui);
    draw_to_terminal_with_ui(&mut terminal, snapshot, alert, &mut ui).unwrap();
    drop(terminal);
    let mut parser = vt100::Parser::new(height, width, 0);
    parser.process(&bytes);
    parser.screen().contents()
}

fn fixed_theme_ui(snapshot: &SidebarSnapshot, ui: &UiState) -> UiState {
    let mut ui = ui.clone();
    ui.theme_cache.get_or_insert_with(|| {
        (
            snapshot.theme.clone(),
            Rc::new(Theme::fixed_for_theme(false, &snapshot.theme)),
        )
    });
    ui
}

fn snapshot_text(screen: &str) -> String {
    screen
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}

fn animation_cadence_for_test(snapshot: &SidebarSnapshot) -> AnimationCadence {
    let theme = Theme::fixed_for_theme(false, &snapshot.theme);
    animation_cadence(snapshot, &theme.animations)
}

fn assert_snapshot(name: &str, screen: String) {
    // Row ages and degraded elapsed values are intentionally relative.
    let screen = snapshot_text(&screen);
    insta::with_settings!({
        snapshot_path => "../snapshots",
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

fn snapshot_with(mut agents: Vec<AgentState>) -> SidebarSnapshot {
    let mut panes = Vec::new();
    for (idx, agent) in agents.iter_mut().enumerate() {
        if agent.parent_agent_id.is_some() {
            continue;
        }
        let live = agent.pane.clone().unwrap_or_else(|| {
            let raw = format!("%agent-{idx}");
            pane(
                &raw,
                agent.kind.as_str(),
                agent.worktree_path.as_deref().unwrap_or("/repo/main"),
            )
        });
        agent.pane = Some(live.clone());
        panes.push(live);
    }
    let mut snapshot =
        SidebarSnapshot::build_with_carryover(fixed_workspace(), Vec::new(), agents, fixed_now());
    if !panes.is_empty() {
        snapshot = snapshot.with_live_panes(panes, None);
    }
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
        status,
        worktree_path: worktree_path.map(ToOwned::to_owned),
        worktree_branch: branch.map(ToOwned::to_owned),
        task: task.map(ToOwned::to_owned),
        ..crate::testkit::agent_state(kind, id, now)
    }
}

fn pane(raw: &str, command: &str, cwd: &str) -> PaneRef {
    PaneRef {
        pane_id: PaneId::from_parts(MuxName::Tmux, raw),
        session_name: "rimz-test".to_owned(),
        view_id: Some("@0".to_owned()),
        view_kind: Some(ViewKind::Window),
        view_name: None,
        title: None,
        is_focused: false,
        is_floating: false,
        command: Some(command.to_owned()),
        foreground_cmdline: None,
        spawn_command: None,
        cwd: Some(cwd.to_owned()),
        pane_pid: None,
        pane_process_start: None,
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    }
}

/// A full Claude statusline enrichment for the rich-row tests. Reset instants
/// are placed days/hours ahead so the live countdown renders at a stable
/// length (the value itself is scrubbed by `assert_snapshot`).
fn claude_context(now: Timestamp) -> AgentContext {
    AgentContext {
        source: "claude".to_owned(),
        session_name: Some("store refactor".to_owned()),
        session_preview: None,
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
            total_lines_added: Some(214),
            total_lines_removed: Some(31),
            ..AgentCost::default()
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
            session_usage: None,
        }),
        rate_limits: Some(AgentRateLimits {
            windows: vec![
                RateLimitWindow {
                    used_percentage: Some(30),
                    resets_at: Some(now + Duration::from_secs(3 * 3_600 + 12 * 60)),
                    duration_mins: Some(5 * 60),
                    ..Default::default()
                },
                RateLimitWindow {
                    used_percentage: Some(60),
                    resets_at: Some(now + Duration::from_secs(3 * 86_400 + 4 * 3_600)),
                    duration_mins: Some(7 * 24 * 60),
                    ..Default::default()
                },
            ],
        }),
        pr: None,
        account: None,
        turn_opened_by: Vec::new(),
        turn_error: None,
        turn_complete: None,
        plan_proposed: None,
        native_permission_wait: None,
        turn_interrupted: None,
        observed_at: now,
    }
}

/// The Codex rich context sidecar: app-server-owned rate-limit windows, official
/// model display name, and version, plus rollout-observed live effort — but no
/// token usage or cost in this fixture. The gauge falls back to rollout scalars
/// for missing fields.
fn codex_context(now: Timestamp) -> AgentContext {
    AgentContext {
        source: "codex".to_owned(),
        session_name: None,
        session_preview: None,
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
                    ..Default::default()
                },
                RateLimitWindow {
                    used_percentage: Some(7),
                    resets_at: Some(now + Duration::from_secs(3 * 86_400 + 4 * 3_600)),
                    duration_mins: Some(7 * 24 * 60),
                    ..Default::default()
                },
            ],
        }),
        pr: None,
        account: None,
        turn_opened_by: Vec::new(),
        turn_error: None,
        turn_complete: None,
        plan_proposed: None,
        native_permission_wait: None,
        turn_interrupted: None,
        observed_at: now,
    }
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

fn test_row_ctx<'a>(
    snapshot: &'a SidebarSnapshot,
    theme: &'a Theme,
    width: usize,
    selected_index: usize,
    animation_phase: u64,
    cost_rolls: &'a CostRolls,
) -> RowCtx<'a> {
    RowCtx {
        theme,
        providers: &snapshot.providers,
        now: snapshot.now,
        width,
        tier: Tier::for_width(content_width(width)),
        bands: &snapshot.theme.display.context_meter,
        card_density: snapshot.theme.display.card_density,
        filter: None,
        held: None,
        selected_index,
        animation_phase,
        cost_rolls,
        lead_unread: lead_unread(&snapshot.worktree_groups).map(|(id, _)| id),
    }
}

/// Render one worktree group's lines, asserting the hit-test map stays in
/// lockstep so callers can read either the spans or their text.
fn group_lines(
    snapshot: &SidebarSnapshot,
    theme: &Theme,
    selected_index: usize,
) -> Vec<Line<'static>> {
    group_lines_at_width(snapshot, theme, selected_index, 54)
}

fn group_lines_at_width(
    snapshot: &SidebarSnapshot,
    theme: &Theme,
    selected_index: usize,
    width: usize,
) -> Vec<Line<'static>> {
    let mut row_index = 0;
    let mut lines = Vec::new();
    let mut map = Vec::new();
    let mut more_hits = Vec::new();
    let cost_rolls = CostRolls::default();
    let ctx = test_row_ctx(snapshot, theme, width, selected_index, 0, &cost_rolls);
    worktree_group_lines(
        &ctx,
        &snapshot.worktree_groups[0],
        false,
        &mut row_index,
        None,
        &mut lines,
        &mut map,
        &mut more_hits,
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

fn bottom_chrome_texts(
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
) -> (Vec<String>, Vec<ProviderTabHit>) {
    bottom_chrome_texts_with_ui(snapshot, alert, &UiState::default())
}

fn bottom_chrome_texts_with_ui(
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    ui: &UiState,
) -> (Vec<String>, Vec<ProviderTabHit>) {
    let theme = Theme::fixed(true);
    let (lines, hits) = build_bottom_chrome(snapshot, alert, &theme, 40, ui);
    (line_texts(&lines), hits)
}

fn bottom_tally() -> crate::SpendTally {
    crate::SpendTally {
        week: crate::SpendWindow {
            usd: 12.34,
            tokens: 120_000,
            input: 90_000,
            output: 30_000,
            cache_read: 20_000,
            sessions: 4,
            ..Default::default()
        },
        month: crate::SpendWindow {
            usd: 56.78,
            tokens: 560_000,
            input: 420_000,
            output: 140_000,
            cache_read: 80_000,
            sessions: 19,
            ..Default::default()
        },
        year: crate::SpendWindow {
            usd: 56.78,
            tokens: 560_000,
            ..Default::default()
        },
        ..Default::default()
    }
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
) -> crate::SidebarProviderPanel {
    let now = fixed_now();
    let window = |used: u8, mins: u32, resets_in: Duration| RateLimitWindow {
        used_percentage: Some(used),
        resets_at: Some(now + resets_in),
        duration_mins: Some(mins),
        ..Default::default()
    };
    crate::SidebarProviderPanel {
        kind: kind.to_owned(),
        account_scope: Default::default(),
        product_name: product_name.to_owned(),
        art: vec![
            " ▐▛███▜▌".to_owned(),
            "▝▜█████▛▘".to_owned(),
            "  ▘▘ ▝▝".to_owned(),
        ],
        color,
        color_rgb: None,
        color_role: None,
        version: Some("2.1.158".to_owned()),
        plan: Some("Claude Max".to_owned()),
        metered,
        remote_control: if remote_control {
            crate::RemoteControlBadge::Healthy
        } else {
            crate::RemoteControlBadge::Hidden
        },
        active_sessions: 12,
        spending: Some(crate::SpendTally {
            headline: crate::SpendWindow {
                usd: 3.5,
                tokens: 498_000,
                input: 434_000,
                output: 64_000,
                cache_write: 12_000,
                cache_read: 68_000,
                sessions: 12,
            },
            ..Default::default()
        }),
        day_budget: None,
        extra_credits: None,
        reset_credits: None,
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

/// The metered bar rows of one panel (5h then 7d), rendered narrow so the art
/// column drops and each row's first span is its label. Filters to the lines
/// carrying bar glyphs.
fn metered_bar_rows(theme: &Theme, panel: &crate::SidebarProviderPanel) -> Vec<Line<'static>> {
    provider_panel_lines(
        theme,
        std::slice::from_ref(panel),
        None,
        false,
        30,
        &crate::config::BudgetBarConfig::default(),
        fixed_now(),
    )
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
/// carries a `↻` reset countdown — the key color/shape facts for budget rows.
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

/// The foreground color of the reset marker.
fn reset_marker_fg(line: &Line<'static>) -> Option<Color> {
    line.spans
        .iter()
        .find(|span| span.content.contains('↻'))
        .and_then(|span| span.style.fg)
}

/// The full style of the reset marker.
fn reset_marker_style(line: &Line<'static>) -> Option<Style> {
    line.spans
        .iter()
        .find(|span| span.content.contains('↻'))
        .map(|span| span.style)
}

/// The full style of the reset countdown time immediately after the marker.
fn reset_time_style(line: &Line<'static>) -> Option<Style> {
    line.spans
        .iter()
        .position(|span| span.content.contains('↻'))
        .and_then(|index| line.spans.get(index + 1))
        .map(|span| span.style)
}

/// The full provider stats line (all spans joined) of one rendered panel.
fn stats_line(theme: &Theme, panel: &crate::SidebarProviderPanel) -> String {
    provider_panel_lines(
        theme,
        std::slice::from_ref(panel),
        None,
        false,
        52,
        &crate::config::BudgetBarConfig::default(),
        fixed_now(),
    )
    .0
    .into_iter()
    .flat_map(|line| line.spans)
    .map(|span| span.content.into_owned())
    .collect()
}

/// Two providers on the dashboard fixture: the metered Claude (rc flag on,
/// 5h/7d windows) and the unmetered Codex with a plan, version, and today's
/// spending. Shared by the tabbed-dashboard tests.
fn two_provider_panels() -> Vec<crate::SidebarProviderPanel> {
    vec![
        provider_panel("claude", "Claude", 173, true, true, Some((25, 40))),
        {
            let mut codex = provider_panel("codex", "Codex", 33, false, false, None);
            codex.plan = Some("ChatGPT Pro".to_owned());
            codex.version = Some("0.135.0".to_owned());
            codex.spending = Some(crate::SpendTally {
                headline: crate::SpendWindow {
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

/// The tab rail's text, flattened from its first line's spans.
fn rail_text(lines: &[Line<'static>]) -> String {
    lines[0]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

/// One failed and one running agent across two worktrees — the make-up
/// click-to-filter tests' fixture: two non-zero buckets, one in each cluster.
fn make_up_snapshot() -> SidebarSnapshot {
    let mut failed = agent(
        "claude-1",
        "claude",
        AgentStatus::Failed,
        Some("/home/me/query-engine"),
        Some("main"),
        Some("db migrate"),
    );
    failed.last_activity = fixed_now() - Duration::from_secs(12 * 60);
    let mut running = agent(
        "codex-1",
        "codex",
        AgentStatus::Running,
        Some("/home/me/query-engine-wt/feature-migration"),
        Some("feature-migration"),
        Some("add tests"),
    );
    running.last_activity = fixed_now() - Duration::from_secs(8);
    snapshot_with(vec![failed, running])
}

/// The make-up line's text, flattened from its single line's spans.
fn make_up_text(lines: &[Line<'static>]) -> String {
    lines[0]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn text_cell_range(text: &str, start: u16, end: u16) -> String {
    let start = byte_index_at_cell(text, usize::from(start));
    let end = byte_index_at_cell(text, usize::from(end));
    text[start..end].to_owned()
}

fn byte_index_at_cell(text: &str, target: usize) -> usize {
    let mut cells = 0;
    for (index, ch) in text.char_indices() {
        let width = ratatui::text::Span::raw(ch.to_string()).width();
        if cells >= target && width > 0 {
            return index;
        }
        cells += width;
    }
    text.len()
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
    snapshot_with(agents)
}

/// One lead actionable unread row plus enough calm rows in a single worktree to
/// overflow the small sidebar frames. The lead sits at visible row 0, so a
/// scroll-to-top reveals it.
fn overflowing_fleet_with_unread_lead() -> SidebarSnapshot {
    let now = fixed_now();
    let mut lead = agent(
        "lead",
        "claude",
        AgentStatus::Waiting,
        Some("/repo/main"),
        Some("main"),
        Some("answer me"),
    );
    lead.last_activity = now - Duration::from_secs(10 * 60);
    let mut agents = vec![lead];
    for i in 0..8 {
        let mut codex = agent(
            &format!("codex-{i}"),
            "codex",
            AgentStatus::Running,
            Some("/repo/main"),
            Some("main"),
            Some(&format!("task-{i}")),
        );
        codex.last_activity = now - Duration::from_secs(8);
        agents.push(codex);
    }
    let mut snapshot = snapshot_with(agents);
    snapshot.worktree_groups[0].rows[0].unread = true;
    snapshot
}

/// A fade in the mid-scroll state — its last draw resolved `offset` with the
/// move stamped at `phase` — built through the real observe path, since the
/// fade's fields are the scrollbar module's own.
fn scrolled_fade(offset: usize, phase: u64) -> ScrollbarFade {
    let mut fade = ScrollbarFade::default();
    fade.observe(offset + 1, phase);
    fade.observe(offset, phase);
    fade
}

fn truecolor_sidebar_theme() -> Theme {
    Theme::fixed_for_theme(
        false,
        &crate::config::ThemeConfig {
            mode: crate::config::ThemeMode::Truecolor,
            ..Default::default()
        },
    )
}

#[test]
fn lead_unread_picks_the_oldest_actionable_row() {
    let now = fixed_now();
    let mut older = agent(
        "a",
        "claude",
        AgentStatus::Waiting,
        Some("/repo/main"),
        Some("main"),
        Some("a"),
    );
    older.last_activity = now - Duration::from_secs(20 * 60);
    let mut newer = agent(
        "b",
        "claude",
        AgentStatus::Failed,
        Some("/repo/main"),
        Some("main"),
        Some("b"),
    );
    newer.last_activity = now - Duration::from_secs(5 * 60);
    // A still-older unread *result* must never win the continuous signal.
    let mut result = agent(
        "c",
        "claude",
        AgentStatus::Success,
        Some("/repo/main"),
        Some("main"),
        Some("c"),
    );
    result.last_activity = now - Duration::from_secs(60 * 60);
    let mut snapshot = snapshot_with(vec![newer, older, result]);
    for row in snapshot
        .worktree_groups
        .iter_mut()
        .flat_map(|group| group.rows.iter_mut())
    {
        row.unread = true;
    }
    assert_eq!(
        lead_unread(&snapshot.worktree_groups).map(|(id, status)| (id.to_owned(), status)),
        Some(("a".to_owned(), AgentStatus::Waiting)),
        "the oldest unread row that needs an answer leads; an older unread result never does",
    );
}

#[test]
fn lead_unread_is_none_without_an_actionable_unread_row() {
    let result = agent(
        "c",
        "claude",
        AgentStatus::Success,
        Some("/repo/main"),
        Some("main"),
        Some("c"),
    );
    let mut snapshot = snapshot_with(vec![result]);
    snapshot.worktree_groups[0].rows[0].unread = true;
    assert_eq!(
        lead_unread(&snapshot.worktree_groups),
        None,
        "an unread result alone reserves nothing — it settles bright, never leads",
    );
}

#[test]
fn unread_jump_banner_tracks_lead_visibility_and_maps_inert() {
    let snapshot = overflowing_fleet_with_unread_lead();
    let ui = UiState {
        scroll_offset: 99,
        manual_scroll: Some(ManualScroll {
            selection_at_start: None,
        }),
        ..UiState::default()
    };
    let theme = Theme::for_sidebar(&snapshot.theme);
    let composed = compose_lines(&snapshot, None, &ui, &theme, 54, 20);
    let banner = composed
        .banner_line
        .expect("the unread banner renders once the lead scrolls out of view");
    assert!(
        line_texts(std::slice::from_ref(&composed.lines[banner]))[0].contains("need you"),
        "banner line carries the unread jump text",
    );
    assert_eq!(
        composed.line_map[banner], None,
        "banner is structural; its click is handled through banner_line",
    );

    let default_ui = UiState::default();
    let composed = compose_lines(&snapshot, None, &default_ui, &theme, 54, 20);
    assert_eq!(
        composed.banner_line, None,
        "lead visible at the top makes the banner redundant",
    );
    assert!(
        !composed
            .lines
            .iter()
            .any(|line| line_texts(std::slice::from_ref(line))[0].contains("need you")),
        "no banner text while the lead is already visible",
    );

    let overflow = overflowing_fleet();
    let overflow_theme = Theme::for_sidebar(&overflow.theme);
    let default_ui = UiState::default();
    let composed = compose_lines(&overflow, None, &default_ui, &overflow_theme, 54, 20);
    assert_eq!(
        composed.banner_line, None,
        "no banner without an actionable unread",
    );
}
