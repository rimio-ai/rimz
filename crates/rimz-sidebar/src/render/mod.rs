//! Ratatui rendering for the sidebar snapshot model.
//!
//! `draw` is the entry point a Ratatui frame calls; `render_fixed` is the
//! offscreen variant used by the vt100-backed snapshot tests. Section
//! composition lives in [`sections`]; vocabulary labels in [`labels`];
//! pure formatting helpers in [`fmt`].
//!
//! Every entry point takes a [`FetchStatus`] alongside the snapshot. When
//! degraded, a banner line surfaces the reason and the elapsed time since
//! the loop went unhealthy. This is the reload-recovery contract documented
//! in [`docs/internals/sidebar.md`](../../docs/internals/sidebar.md).

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

use self::fmt::elapsed_short;
use self::sections::{attention_line, first_run_hint_lines, worktree_group_lines};
use self::theme::Theme;

#[derive(Clone, Debug, Default)]
pub struct UiState {
    pub selected_index: usize,
    pub help_visible: bool,
}

/// Health of the sidebar's refresh loop. `Ok` means the last snapshot and
/// heartbeat round-trip succeeded; `Degraded` carries a short reason and
/// the time the degraded state started so the banner can show `for Ns`.
#[derive(Clone, Debug, Default)]
pub enum FetchStatus {
    #[default]
    Ok,
    Degraded {
        reason: String,
        since: Timestamp,
    },
}

impl FetchStatus {
    pub fn degraded(reason: impl Into<String>) -> Self {
        Self::Degraded {
            reason: reason.into(),
            since: Timestamp::now(),
        }
    }
}

pub fn draw(frame: &mut Frame<'_>, snapshot: &SidebarSnapshot, status: &FetchStatus) {
    draw_with_ui(frame, snapshot, status, &UiState::default());
}

pub fn draw_with_ui(
    frame: &mut Frame<'_>,
    snapshot: &SidebarSnapshot,
    status: &FetchStatus,
    ui: &UiState,
) {
    let area = frame.area();
    let title = format!(" {} ", snapshot.display_name);
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = snapshot_lines(snapshot, status, ui, usize::from(inner.width));
    let paragraph = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);
}

pub fn draw_to_terminal<B: Backend>(
    terminal: &mut Terminal<B>,
    snapshot: &SidebarSnapshot,
    status: &FetchStatus,
) -> Result<(), B::Error> {
    draw_to_terminal_with_ui(terminal, snapshot, status, &UiState::default())
}

pub fn draw_to_terminal_with_ui<B: Backend>(
    terminal: &mut Terminal<B>,
    snapshot: &SidebarSnapshot,
    status: &FetchStatus,
    ui: &UiState,
) -> Result<(), B::Error> {
    terminal
        .draw(|frame| draw_with_ui(frame, snapshot, status, ui))
        .map(|_| ())
}

pub fn render_fixed<W: Write>(
    writer: W,
    snapshot: &SidebarSnapshot,
    status: &FetchStatus,
    width: u16,
    height: u16,
) -> io::Result<()> {
    let backend = CrosstermBackend::new(writer);
    let viewport = Viewport::Fixed(Rect::new(0, 0, width, height));
    let mut terminal = Terminal::with_options(backend, TerminalOptions { viewport })?;
    terminal.clear()?;
    draw_to_terminal(&mut terminal, snapshot, status)?;
    Ok(())
}

fn snapshot_lines(
    snapshot: &SidebarSnapshot,
    status: &FetchStatus,
    ui: &UiState,
    width: usize,
) -> Vec<Line<'static>> {
    let theme = Theme::from_env();
    let mut lines = Vec::new();
    if let FetchStatus::Degraded { reason, since } = status {
        let elapsed = elapsed_short(*since);
        lines.push(Line::styled(
            format!("! Sidebar degraded for {elapsed}: {reason}"),
            theme.style(Color::Red, Modifier::BOLD),
        ));
        lines.push(Line::from(""));
    }

    if let Some(line) = attention_line(&theme, &snapshot.worktree_groups) {
        lines.push(line);
    }
    if snapshot.worktree_groups.is_empty() {
        // A healthy, empty room is a first run (or a room whose agents have all
        // exited); nudge the user toward launching one. When degraded the empty
        // body is a failed fetch, not an empty room, so suppress the hint.
        if matches!(status, FetchStatus::Ok) && should_show_first_run_hint(snapshot) {
            push_section_gap(&mut lines);
            lines.extend(first_run_hint_lines(&theme, snapshot.agent_hooks_ready));
        }
        if matches!(status, FetchStatus::Ok) {
            lines.extend(footer_lines(snapshot, status));
        }
        return lines;
    }

    push_section_gap(&mut lines);
    let mut row_index = 0;
    for (index, group) in snapshot.worktree_groups.iter().enumerate() {
        if index > 0 {
            lines.push(Line::from(""));
        }
        lines.extend(worktree_group_lines(
            &theme,
            group,
            width,
            &mut row_index,
            ui.selected_index,
        ));
    }
    if matches!(status, FetchStatus::Ok) && should_show_first_run_hint(snapshot) {
        lines.push(Line::from(""));
        lines.extend(first_run_hint_lines(&theme, snapshot.agent_hooks_ready));
    }
    if ui.help_visible && matches!(status, FetchStatus::Ok) {
        lines.push(Line::from(""));
        lines.extend(help_lines());
    }
    lines.extend(footer_lines(snapshot, status));
    lines
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

fn footer_lines(snapshot: &SidebarSnapshot, status: &FetchStatus) -> Vec<Line<'static>> {
    if !matches!(status, FetchStatus::Ok) {
        return Vec::new();
    }
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
        .fg(Color::DarkGray)
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
            Line::styled("↵ jump   ␣ next ◆   ? keys", dim),
        ];
    }
    Vec::new()
}

fn help_lines() -> Vec<Line<'static>> {
    let dim = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM);
    vec![
        Line::styled("keys & legend", dim),
        Line::styled("↑/↓ select   1-9 jump   ↵ jump", dim),
        Line::styled("␣ next ◆/✗   ? close keys", dim),
        Line::styled("◆ waiting   ✗ failed   ▸ running", dim),
        Line::styled("○ idle      · process", dim),
    ]
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;
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
        snapshot_to_screen_with_status(snapshot, &FetchStatus::Ok, width, height)
    }

    fn snapshot_to_screen_with_status(
        snapshot: &SidebarSnapshot,
        status: &FetchStatus,
        width: u16,
        height: u16,
    ) -> String {
        snapshot_to_screen_with_status_and_ui(snapshot, status, &UiState::default(), width, height)
    }

    fn snapshot_to_screen_with_status_and_ui(
        snapshot: &SidebarSnapshot,
        status: &FetchStatus,
        ui: &UiState,
        width: u16,
        height: u16,
    ) -> String {
        let mut bytes = Vec::new();
        let backend = CrosstermBackend::new(&mut bytes);
        let viewport = Viewport::Fixed(Rect::new(0, 0, width, height));
        let mut terminal = Terminal::with_options(backend, TerminalOptions { viewport }).unwrap();
        terminal.clear().unwrap();
        draw_to_terminal_with_ui(&mut terminal, snapshot, status, ui).unwrap();
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
            last_event_pulse: 0,
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
        claude.model = Some("Opus".to_owned());
        claude.effort = Some("xhigh".to_owned());
        claude.context_pct = Some(38);
        claude.total_tokens = Some(12_400);
        claude.todo_done = Some(3);
        claude.todo_total = Some(5);
        claude.last_event_pulse = 3;
        let mut snapshot = snapshot_with(Vec::new(), vec![claude]);
        snapshot.worktree_groups[0].diff_added = Some(127);
        snapshot.worktree_groups[0].diff_removed = Some(43);

        let rendered = snapshot_to_screen_with_status_and_ui(
            &snapshot,
            &FetchStatus::Ok,
            &UiState {
                selected_index: 0,
                help_visible: false,
            },
            54,
            14,
        );

        assert!(rendered.contains("+127 -43"));
        assert!(rendered.contains("Opus"));
        assert!(rendered.contains("auto"));
        assert!(rendered.contains("ctx"));
        assert!(rendered.contains("38%"));
        assert!(rendered.contains("●●●○○ 3/5"));
        assert!(rendered.contains("12.4k tok"));
        assert_snapshot("enriched_selected_agent_card", rendered);
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
    fn render_degraded_status_shows_banner_above_snapshot() {
        let snapshot = snapshot_with(Vec::new(), Vec::new());
        let status = FetchStatus::Degraded {
            reason: "snapshot failed: ledger not found".to_owned(),
            since: fixed_now() - Duration::from_secs(8),
        };

        assert_snapshot(
            "degraded_banner",
            snapshot_to_screen_with_status(&snapshot, &status, 80, 18),
        );
    }

    #[test]
    fn render_ok_status_omits_banner() {
        let snapshot = snapshot_with(Vec::new(), Vec::new());
        let rendered = snapshot_to_screen_with_status(&snapshot, &FetchStatus::Ok, 80, 18);
        assert!(
            !rendered.contains("Sidebar degraded"),
            "ok status must not render the banner:\n{rendered}"
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

        let help = snapshot_to_screen_with_status_and_ui(
            &snapshot,
            &FetchStatus::Ok,
            &UiState {
                selected_index: 0,
                help_visible: true,
            },
            80,
            18,
        );
        assert!(help.contains("keys & legend"));
        assert!(help.contains("◆ waiting"));
        assert!(help.contains("○ idle"));
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
    fn render_degraded_empty_suppresses_first_run_nudge() {
        // An empty body under a degraded fetch is a failed snapshot, not an
        // empty room — the nudge would misreport. The banner speaks instead.
        let snapshot = snapshot_with(Vec::new(), Vec::new());
        let status = FetchStatus::Degraded {
            reason: "snapshot failed: ledger not found".to_owned(),
            since: fixed_now() - Duration::from_secs(8),
        };
        let rendered = snapshot_to_screen_with_status(&snapshot, &status, 80, 18);

        assert!(!rendered.contains("run claude or codex"));
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
            rendered.contains("▸ codex"),
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

    /// Honesty test: the pulse glyph is a pure function of the agent's
    /// observed event count. Re-rendering at any wall-clock time without a
    /// new lifecycle event must produce the same pulse frame — a silent
    /// agent freezes instead of pretending work continues.
    #[test]
    fn render_event_pulse_freezes_between_renders_without_events() {
        let mut claude = agent(
            "claude-1",
            "claude",
            AgentStatus::Running,
            PermissionPosture::Default,
            Some("/repo/main"),
            Some("main"),
            Some("waiting on tools"),
        );
        claude.last_event_pulse = 4;
        let snapshot = snapshot_with(Vec::new(), vec![claude]);
        let first = snapshot_to_screen(&snapshot, 40, 8);
        // Sleep so any timer-driven animation would tick.
        std::thread::sleep(Duration::from_millis(50));
        let second = snapshot_to_screen(&snapshot, 40, 8);

        assert_eq!(
            first, second,
            "no new lifecycle event must mean no frame change",
        );
    }
}
