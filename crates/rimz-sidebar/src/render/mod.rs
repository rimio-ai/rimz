//! Ratatui rendering for the sidebar snapshot model.
//!
//! `draw` is the entry point a Ratatui frame calls; `render_fixed` is the
//! offscreen variant used by the vt100-backed snapshot tests. Section
//! composition lives in [`sections`]; vocabulary labels in [`labels`];
//! pure formatting helpers in [`fmt`].
//!
//! Every entry point takes an optional [`Alert`] alongside the snapshot. The
//! alert is the sticky health line pinned to the bottom of the sidebar: while
//! the refresh loop is unhealthy it shows the reason and elapsed time, and
//! after recovery it lingers as a dismissable "last alert" notice. This is the
//! reload-recovery contract documented in
//! [`docs/internals/sidebar.md`](../../docs/internals/sidebar.md).

mod fmt;
mod labels;
mod sections;
mod theme;

use std::io::{self, Write};

use jiff::Timestamp;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::{Frame, Terminal, TerminalOptions, Viewport};
use rimz::{SidebarRowKind, SidebarSnapshot};

use self::fmt::age_short;
use self::sections::{attention_line, first_run_hint_lines, worktree_group_lines};
use self::theme::Theme;

#[derive(Clone, Debug, Default)]
pub struct UiState {
    pub selected_index: usize,
    pub help_visible: bool,
    /// Wall-clock animation frame counter, advanced by the serve loop's
    /// animation tick. The renderer derives the running-agent spin frame from
    /// it; freshness gating (per row) keeps a quiet agent frozen.
    pub animation_phase: u64,
}

/// A sticky health alert pinned to the bottom of the sidebar.
///
/// `since` is when the unhealthy episode began, so an active alert can show
/// `for Ns`. `recovered_at` is `None` while the loop is still unhealthy and
/// `Some(t)` once it healed — a recovered alert lingers as a dismissable
/// "last alert" notice rather than vanishing the instant a fetch succeeds.
#[derive(Clone, Debug)]
pub struct Alert {
    pub reason: String,
    pub since: Timestamp,
    pub recovered_at: Option<Timestamp>,
}

impl Alert {
    pub fn active(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            since: Timestamp::now(),
            recovered_at: None,
        }
    }

    pub fn is_active(&self) -> bool {
        self.recovered_at.is_none()
    }
}

pub fn draw(frame: &mut Frame<'_>, snapshot: &SidebarSnapshot, alert: Option<&Alert>) {
    draw_with_ui(frame, snapshot, alert, &UiState::default());
}

/// Whether any visible row is in an animated state — a running agent (working
/// or plan-mode thinking) or a resolver mid-flight. The serve loop uses this to
/// switch to the fast animation tick only while there is live motion to paint;
/// a calm sidebar (only idle/waiting/done/failed rows, all static) keeps idling
/// on the slow data tick. A stalled agent is projected to `failed` upstream, so
/// it reads as static `!` and never keeps the fast tick alive.
pub fn has_live_animation(snapshot: &SidebarSnapshot) -> bool {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .any(|row| {
            row.row_kind == SidebarRowKind::Agent
                && (row.resolver.is_some() || row.status == Some(rimz::feed::AgentStatus::Running))
        })
}

pub fn draw_with_ui(
    frame: &mut Frame<'_>,
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    ui: &UiState,
) {
    let area = frame.area();
    let title = format!(" {} ", snapshot.display_name);
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Indexed(244)));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = compose_lines(snapshot, alert, ui, inner.width, inner.height);
    let paragraph = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);
}

/// Lay out the body, then pin the health alert to the bottom edge of the
/// viewport like a status bar. Space for the alert is always reserved — the
/// body is truncated before the alert is ever clipped — so the sticky notice
/// can never scroll off the bottom of a full sidebar.
fn compose_lines(
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    ui: &UiState,
    width: u16,
    height: u16,
) -> Vec<Line<'static>> {
    // `NO_COLOR` can't change mid-process, so read the palette once per frame
    // and hand the same `Theme` to the body and the alert.
    let theme = Theme::from_env();
    let mut body = snapshot_lines(snapshot, alert, ui, usize::from(width), &theme);
    let Some(alert) = alert else {
        return body;
    };

    let alert_block = alert_lines(&theme, alert);
    let cells = usize::from(width.max(1));
    let height = usize::from(height);
    let alert_height = alert_block
        .iter()
        .map(|line| line.width().div_ceil(cells))
        .sum::<usize>()
        .min(height);

    let max_body = height.saturating_sub(alert_height);
    if body.len() > max_body {
        body.truncate(max_body);
    }
    let pad = height.saturating_sub(body.len() + alert_height);
    body.extend(std::iter::repeat_n(Line::from(""), pad));
    body.extend(alert_block);
    body
}

pub fn draw_to_terminal<B: Backend>(
    terminal: &mut Terminal<B>,
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
) -> Result<(), B::Error> {
    draw_to_terminal_with_ui(terminal, snapshot, alert, &UiState::default())
}

pub fn draw_to_terminal_with_ui<B: Backend>(
    terminal: &mut Terminal<B>,
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    ui: &UiState,
) -> Result<(), B::Error> {
    terminal
        .draw(|frame| draw_with_ui(frame, snapshot, alert, ui))
        .map(|_| ())
}

pub fn render_fixed<W: Write>(
    writer: W,
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    width: u16,
    height: u16,
) -> io::Result<()> {
    let backend = CrosstermBackend::new(writer);
    let viewport = Viewport::Fixed(Rect::new(0, 0, width, height));
    let mut terminal = Terminal::with_options(backend, TerminalOptions { viewport })?;
    terminal.clear()?;
    draw_to_terminal(&mut terminal, snapshot, alert)?;
    Ok(())
}

fn snapshot_lines(
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    ui: &UiState,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    // An *active* alert means the body is a stale/empty fetch, not a live room:
    // suppress the first-run hint, footer, and help so the alert speaks alone.
    // A recovered alert is just a lingering notice — the room below it is live.
    let active = alert.is_some_and(Alert::is_active);
    let mut lines = Vec::new();

    if let Some(line) = attention_line(theme, &snapshot.worktree_groups) {
        lines.push(line);
    }
    if snapshot.worktree_groups.is_empty() {
        if !active && should_show_first_run_hint(snapshot) {
            push_section_gap(&mut lines);
            lines.extend(first_run_hint_lines(theme, snapshot.agent_hooks_ready));
        }
        if !active {
            lines.extend(footer_lines(snapshot));
        }
    } else {
        push_section_gap(&mut lines);
        let mut row_index = 0;
        for (index, group) in snapshot.worktree_groups.iter().enumerate() {
            if index > 0 {
                lines.push(Line::from(""));
            }
            lines.extend(worktree_group_lines(
                theme,
                group,
                width,
                &mut row_index,
                ui.selected_index,
                ui.animation_phase,
            ));
        }
        if !active && should_show_first_run_hint(snapshot) {
            lines.push(Line::from(""));
            lines.extend(first_run_hint_lines(theme, snapshot.agent_hooks_ready));
        }
        if ui.help_visible && !active {
            lines.push(Line::from(""));
            lines.extend(help_lines());
        }
        if !active {
            lines.extend(footer_lines(snapshot));
        }
    }

    lines
}

fn alert_lines(theme: &Theme, alert: &Alert) -> Vec<Line<'static>> {
    if alert.is_active() {
        let elapsed = age_short(alert.since);
        vec![Line::styled(
            format!("! Sidebar degraded for {elapsed}: {}", alert.reason),
            theme.style(Color::Red, Modifier::BOLD),
        )]
    } else {
        let elapsed = alert
            .recovered_at
            .map(age_short)
            .unwrap_or_else(|| "0s".to_owned());
        vec![Line::styled(
            format!("⚠ last alert {elapsed} ago: {}  ·  x dismiss", alert.reason),
            theme.style(Color::Yellow, Modifier::DIM),
        )]
    }
}

fn push_section_gap(lines: &mut Vec<Line<'static>>) {
    if lines.last().is_some_and(|line| line.width() > 0) {
        lines.push(Line::from(""));
    }
}

fn should_show_first_run_hint(snapshot: &SidebarSnapshot) -> bool {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .all(|row| row.row_kind == SidebarRowKind::Process && !is_known_agent_process(row))
}

fn is_known_agent_process(row: &rimz::SidebarRow) -> bool {
    // tmux can expose Claude/Codex as the shared Node host before hook
    // enrichment claims the pane, so `node` is agent-like for the empty-room cue.
    row.row_kind == SidebarRowKind::Process
        && (rimz::agents::KNOWN_AGENTS.contains(&row.name.as_str()) || row.name == "node")
}

fn footer_lines(snapshot: &SidebarSnapshot) -> Vec<Line<'static>> {
    let attention = snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter(|row| {
            matches!(
                row.status,
                Some(rimz::feed::AgentStatus::Waiting | rimz::feed::AgentStatus::Failed)
            )
        })
        .collect::<Vec<_>>();
    let jumpable = snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter(|row| row.pane.is_some())
        .count();
    let dim = Style::default()
        .fg(Color::Indexed(244))
        .add_modifier(Modifier::DIM);
    if attention.len() == 1 {
        return vec![
            Line::from(""),
            Line::styled(format!("↵ jump to {}", attention[0].name), dim),
        ];
    }
    if attention.len() > 1 || jumpable > 0 {
        return vec![
            Line::from(""),
            Line::styled("↵ jump   ␣ next ?!   ? keys", dim),
        ];
    }
    Vec::new()
}

fn help_lines() -> Vec<Line<'static>> {
    let dim = Style::default()
        .fg(Color::Indexed(244))
        .add_modifier(Modifier::DIM);
    vec![
        Line::styled("keys & legend", dim),
        Line::styled("↑/↓ select   1-9 jump   ↵ jump", dim),
        Line::styled("␣ next ?!   x dismiss   r reload   ? close", dim),
        Line::styled("⢿ working   ✽ thinking   ? waiting", dim),
        Line::styled("! attention   ◌ idle   ✓ done   · process", dim),
    ]
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;
    use rimz::agents::{
        AgentContext, AgentCost, AgentCurrentUsage, AgentRateLimits, AgentTokenUsage,
        RateLimitWindow,
    };
    use rimz::feed::{AgentState, AgentStatus, FeedKind, PaneRef, PermissionPosture};
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
        draw_to_terminal_with_ui(&mut terminal, snapshot, alert, ui).unwrap();
        drop(terminal);
        let mut parser = vt100::Parser::new(height, width, 0);
        parser.process(&bytes);
        parser.screen().contents()
    }

    fn assert_snapshot(name: &str, screen: String) {
        // Row ages and degraded elapsed values are intentionally relative.
        insta::with_settings!({
            filters => vec![
                (r"degraded for \d+[smhd]", "degraded for <elapsed>"),
                // Usage-window resets are a live countdown; scrub the value but
                // keep the marker so the line shape stays asserted.
                (r"↻ [0-9dhms]+", "↻ <reset>"),
                (r"\b\d+[smhd]\b", "<t>"),
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

    fn snapshot_with(items: Vec<FeedItem>, agents: Vec<AgentState>) -> SidebarSnapshot {
        let mut snapshot =
            SidebarSnapshot::build_with_carryover(fixed_workspace(), items, Vec::new(), agents);
        snapshot.display_name = "query-engine".to_owned();
        snapshot
    }

    fn agent(
        id: &str,
        kind: &str,
        status: AgentStatus,
        permission_posture: PermissionPosture,
        worktree_path: Option<&str>,
        branch: Option<&str>,
        task: Option<&str>,
    ) -> AgentState {
        let now = fixed_now();
        AgentState {
            agent_id: id.to_owned(),
            kind: kind.to_owned(),
            status,
            permission_posture,
            plan_mode: false,
            pane: None,
            agent_pid: None,
            agent_process_start: None,
            runtime_owner: None,
            worktree_path: worktree_path.map(ToOwned::to_owned),
            worktree_branch: branch.map(ToOwned::to_owned),
            task: task.map(ToOwned::to_owned),
            model: None,
            effort: None,
            context_pct: None,
            total_tokens: None,
            todo_done: None,
            todo_total: None,
            context: None,
            last_seen: now,
            last_activity: now,
        }
    }

    fn pane(raw: &str, command: &str, cwd: &str) -> PaneRef {
        PaneRef {
            pane_id: PaneId::from_parts(MuxName::Tmux, raw),
            session_name: "rimz-test".to_owned(),
            view_id: Some("@0".to_owned()),
            view_kind: Some(ViewKind::Window),
            is_focused: false,
            command: Some(command.to_owned()),
            cwd: Some(cwd.to_owned()),
            pane_pid: None,
            pane_process_start: None,
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
                total_duration_ms: None,
                total_api_duration_ms: None,
                total_lines_added: None,
                total_lines_removed: None,
            }),
            tokens: Some(AgentTokenUsage {
                total_input_tokens: Some(64_200),
                total_output_tokens: Some(12_300),
                context_window_size: Some(200_000),
                used_percentage: Some(38),
                remaining_percentage: Some(62),
                current_usage: Some(AgentCurrentUsage {
                    input_tokens: Some(8_500),
                    output_tokens: Some(1_200),
                    cache_creation_input_tokens: Some(20_000),
                    cache_read_input_tokens: Some(48_000),
                }),
            }),
            rate_limits: Some(AgentRateLimits {
                five_hour: Some(RateLimitWindow {
                    used_percentage: Some(30),
                    resets_at: Some(now + Duration::from_secs(3 * 3_600 + 12 * 60)),
                }),
                seven_day: Some(RateLimitWindow {
                    used_percentage: Some(60),
                    resets_at: Some(now + Duration::from_secs(3 * 86_400 + 4 * 3_600)),
                }),
            }),
            pr: None,
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
            PermissionPosture::Default,
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
            snapshot_to_screen(&snapshot, 38, 18),
        );
    }

    #[test]
    fn render_agent_capability_and_posture() {
        let mut claude = agent(
            "claude-1",
            "claude",
            AgentStatus::Failed,
            PermissionPosture::Yolo,
            Some("/repo/feature-migration"),
            Some("feature-migration"),
            Some("db migrate"),
        );
        claude.model = Some("Opus".to_owned());
        claude.effort = Some("xhigh".to_owned());
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
            PermissionPosture::Auto,
            Some("/repo/feature-migration"),
            Some("feature-migration"),
            Some("db migrate"),
        );
        // Transcript scalars are the coarse fallback; the statusline context
        // below supersedes them (`Opus` → `Opus 4.8 (1M)`, `xhigh` → `high`).
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

        let rendered = snapshot_to_screen_with_alert_and_ui(
            &snapshot,
            None,
            &UiState {
                selected_index: 0,
                help_visible: false,
                animation_phase: 0,
            },
            54,
            14,
        );

        assert!(rendered.contains("+127 -43"));
        // Line 1 prefers the user's session name over the task; line 2 prefers
        // the statusline model/effort and pins the cost right.
        assert!(rendered.contains("ledger refactor"));
        // The model display name is shortened — `(1M context)` → `(1M)`.
        assert!(rendered.contains("Opus 4.8 (1M)"));
        assert!(!rendered.contains("context"));
        assert!(rendered.contains("high"));
        assert!(rendered.contains("auto"));
        assert!(rendered.contains("$1.3"));
        assert!(rendered.contains("●●●○○ 3/5"));
        // Selection reveals the token split above the usage windows (reset
        // marker). The window labels are scrubbed in the snapshot — they match
        // the age filter — so assert the `5h`/`7d` rename on the raw string.
        assert!(rendered.contains('↻'));
        assert!(rendered.contains("76.5k tok"));
        assert!(rendered.contains("↑64.2k"));
        assert!(rendered.contains("↓12.3k"));
        assert!(rendered.contains("5h "));
        assert!(rendered.contains("7d "));
        assert_snapshot("enriched_selected_agent_card", rendered);
    }

    #[test]
    fn line_one_prefers_session_name_over_task() {
        let mut claude = agent(
            "claude-1",
            "claude",
            AgentStatus::Running,
            PermissionPosture::Default,
            Some("/repo/main"),
            Some("main"),
            Some("db migrate"),
        );
        claude.context = Some(claude_context(fixed_now()));
        let snapshot = snapshot_with(Vec::new(), vec![claude]);
        let rendered = snapshot_to_screen(&snapshot, 44, 10);

        assert!(rendered.contains("ledger refactor"));
        assert!(!rendered.contains("db migrate"));
    }

    #[test]
    fn selected_agent_without_context_keeps_bare_token_total() {
        // An agent that publishes no statusline (today, Codex) degrades to the
        // simple selected-row token total — no cost, no usage windows.
        let mut codex = agent(
            "codex-1",
            "codex",
            AgentStatus::Running,
            PermissionPosture::Default,
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
            },
            44,
            12,
        );

        assert!(rendered.contains("5.0k tok"));
        assert!(!rendered.contains('↻'));
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
        let mut snapshot =
            SidebarSnapshot::build_with_carryover(workspace, vec![answered], vec![event], vec![]);
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

        assert!(rendered.contains("· zsh"));
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

        assert!(rendered.contains("· claude"));
        assert!(rendered.contains("· node"));
        assert!(!rendered.contains("no agents yet"));
        assert!(!rendered.contains("rimz hooks install"));
        assert!(!rendered.contains("run claude or codex"));
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
        assert!(rendered.contains("↵ jump to codex"));

        let help = snapshot_to_screen_with_alert_and_ui(
            &snapshot,
            None,
            &UiState {
                selected_index: 0,
                help_visible: true,
                animation_phase: 0,
            },
            80,
            18,
        );
        assert!(help.contains("keys & legend"));
        assert!(help.contains("? waiting"));
        assert!(help.contains("◌ idle"));
        assert!(help.contains("· process"));
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
                    PermissionPosture::Default,
                    Some("/repo/main"),
                    Some("main"),
                    Some(&format!("task-{i}")),
                );
                agent.last_activity = fixed_now() - Duration::from_secs(i);
                agent
            })
            .collect::<Vec<_>>();
        let snapshot = snapshot_with(Vec::new(), agents);

        assert_snapshot(
            "group_cap_with_overflow",
            snapshot_to_screen(&snapshot, 36, 16),
        );
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
            PermissionPosture::Default,
            Some("/repo/main"),
            Some("main"),
            Some("compile"),
        );
        codex.last_activity = fixed_now() - Duration::from_secs(3);
        let snapshot = snapshot_with(Vec::new(), vec![codex]);
        let rendered = snapshot_to_screen(&snapshot, 24, 8);

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
            !rendered.contains("auto") && !rendered.contains("yolo"),
            "default posture stays the omitted baseline:\n{rendered}"
        );
        assert_snapshot("l0_density_minimal_row", rendered);
    }

    fn ui_at_phase(phase: u64) -> UiState {
        UiState {
            selected_index: 0,
            help_visible: false,
            animation_phase: phase,
        }
    }

    /// Honesty test: a running agent silent past the stall window is projected
    /// to the attention bucket, so it reads as a static `!` and its cell does
    /// not animate — a wedged agent stops spinning and asks for a look.
    #[test]
    fn render_stalled_agent_reads_as_static_attention() {
        let mut claude = agent(
            "claude-1",
            "claude",
            AgentStatus::Running,
            PermissionPosture::Default,
            Some("/repo/main"),
            Some("main"),
            Some("waiting on tools"),
        );
        claude.last_activity =
            fixed_now() - Duration::from_secs(rimz::feed::STALL_WINDOW_SECS as u64 + 60);
        let snapshot = snapshot_with(Vec::new(), vec![claude]);
        let first = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(0), 40, 8);
        let second = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(2), 40, 8);

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
            PermissionPosture::Default,
            Some("/repo/main"),
            Some("main"),
            Some("compiling"),
        );
        claude.last_activity = fixed_now() - Duration::from_secs(30);
        let snapshot = snapshot_with(Vec::new(), vec![claude]);
        let first = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(0), 40, 8);
        let second = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(1), 40, 8);

        assert_ne!(
            first, second,
            "a running agent's head must advance with the phase"
        );
    }
}
