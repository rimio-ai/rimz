//! Ratatui rendering for the sidebar snapshot model.
//!
//! `draw` is the entry point a Ratatui frame calls; `render_fixed` is the
//! offscreen variant used by the vt100-backed snapshot tests. Section
//! composition lives in [`sections`]; vocabulary labels in [`labels`];
//! pure formatting helpers in [`fmt`].

mod fmt;
mod labels;
mod sections;

use std::io::{self, Write};

use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::{Frame, Terminal, TerminalOptions, Viewport};
use rimz::SidebarSnapshot;

use self::fmt::time_ago;
use self::sections::{
    MAX_ROWS_PER_GROUP, SectionMode, activity_section, agent_line, feed_section, section_title,
};

pub fn draw(frame: &mut Frame<'_>, snapshot: &SidebarSnapshot) {
    let area = frame.area();
    let title = format!(" Rimz | {} ", snapshot.workspace_id);
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = snapshot_lines(snapshot);
    let paragraph = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: true });
    frame.render_widget(paragraph, inner);
}

pub fn draw_to_terminal<B: Backend>(
    terminal: &mut Terminal<B>,
    snapshot: &SidebarSnapshot,
) -> Result<(), B::Error> {
    terminal.draw(|frame| draw(frame, snapshot)).map(|_| ())
}

pub fn render_fixed<W: Write>(
    writer: W,
    snapshot: &SidebarSnapshot,
    width: u16,
    height: u16,
) -> io::Result<()> {
    let backend = CrosstermBackend::new(writer);
    let viewport = Viewport::Fixed(Rect::new(0, 0, width, height));
    let mut terminal = Terminal::with_options(backend, TerminalOptions { viewport })?;
    terminal.clear()?;
    draw_to_terminal(&mut terminal, snapshot)?;
    Ok(())
}

fn snapshot_lines(snapshot: &SidebarSnapshot) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("Generated ", Style::default().fg(Color::DarkGray)),
        Span::raw(time_ago(snapshot.generated_at)),
    ]));
    lines.push(Line::from(""));

    if !snapshot.agents.is_empty() {
        section_title(&mut lines, "Agents");
        for agent in snapshot.agents.iter().take(MAX_ROWS_PER_GROUP) {
            lines.push(agent_line(agent));
        }
        lines.push(Line::from(""));
    }

    feed_section(
        &mut lines,
        "Needs your attention",
        &snapshot.needs_attention,
        SectionMode::NeedsAttention,
        true,
    );
    feed_section(
        &mut lines,
        "Resolver is working",
        &snapshot.resolver_working,
        SectionMode::ResolverWorking,
        false,
    );
    feed_section(
        &mut lines,
        "Recently answered",
        &snapshot.recently_answered,
        SectionMode::RecentlyAnswered,
        false,
    );
    activity_section(&mut lines, "Recent activity", &snapshot.recent_activity);
    lines
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;
    use rimz::feed::{AgentMode, AgentState, AgentStatus, FeedKind, Resolution};
    use rimz::{
        EventEnvelope, FeedItem, FeedStatus, ResolutionMethod, SidebarActivity, SidebarSnapshot,
        Surface, WorkspaceId,
    };
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
        let mut bytes = Vec::new();
        render_fixed(&mut bytes, snapshot, width, height).unwrap();
        let mut parser = vt100::Parser::new(height, width, 0);
        parser.process(&bytes);
        parser.screen().contents()
    }

    fn assert_snapshot(name: &str, screen: String) {
        // Two transients escape any `Timestamp::now()` call inside the
        // renderer: the `Generated Ns ago` header and the per-event/answered
        // timestamps. Both render as `\d+[smhd] ago`.
        insta::with_settings!({
            filters => vec![
                (r"\d+[smhd] ago", "<elapsed> ago"),
                (r"\bjust now\b", "<elapsed> ago"),
            ],
        }, {
            insta::assert_snapshot!(name, screen);
        });
    }

    #[test]
    fn render_includes_four_sidebar_groups_and_native_actions() {
        let workspace = fixed_workspace();
        let mut native = FeedItem::new(
            workspace.clone(),
            Surface::NativeUi,
            FeedKind::Permission,
            "psql DROP TABLE invoices",
            "claude",
            "agent-hook",
        );
        native.worktree_path = Some("/home/me/billing-service".to_owned());
        let mut script = FeedItem::new(
            workspace.clone(),
            Surface::Script,
            FeedKind::Question,
            "Deploy staging?",
            "deploy.sh",
            "cli",
        );
        script.options = vec!["yes".to_owned(), "no".to_owned()];
        let mut answered = FeedItem::new(
            workspace.clone(),
            Surface::Script,
            FeedKind::Question,
            "Deploy staging?",
            "deploy.sh",
            "cli",
        );
        answered.status = FeedStatus::Resolved;
        answered.resolution = Some(Resolution::new(
            json!({ "choice": "yes" }),
            ResolutionMethod::Sidebar,
        ));
        let snapshot = SidebarSnapshot {
            workspace_id: workspace,
            generated_at: fixed_now() - Duration::from_secs(2),
            needs_attention: vec![native, script],
            resolver_working: Vec::new(),
            recently_answered: vec![answered],
            recent_activity: Vec::new(),
            agents: Vec::new(),
        };

        assert_snapshot(
            "four_groups_and_native_actions",
            snapshot_to_screen(&snapshot, 96, 24),
        );
    }

    #[test]
    fn render_includes_agent_rollup_when_present() {
        let snapshot = SidebarSnapshot {
            workspace_id: fixed_workspace(),
            generated_at: fixed_now(),
            needs_attention: Vec::new(),
            resolver_working: Vec::new(),
            recently_answered: Vec::new(),
            recent_activity: Vec::new(),
            agents: vec![AgentState {
                agent_id: "agent-1".to_owned(),
                kind: "codex".to_owned(),
                status: AgentStatus::Waiting,
                mode: AgentMode::Interactive,
                pane: None,
                worktree_path: None,
                worktree_branch: Some("feature-migration".to_owned()),
                last_seen: fixed_now(),
            }],
        };

        assert_snapshot("agent_rollup", snapshot_to_screen(&snapshot, 80, 18));
    }

    #[test]
    fn render_includes_event_activity() {
        let workspace = fixed_workspace();
        let event = EventEnvelope::new(
            workspace.clone(),
            "rimz-test",
            "rimz",
            "cli",
            "event.emit",
            json!({ "kind": "build.started", "title": "Building web" }),
        );
        let snapshot = SidebarSnapshot {
            workspace_id: workspace,
            generated_at: fixed_now(),
            needs_attention: Vec::new(),
            resolver_working: Vec::new(),
            recently_answered: Vec::new(),
            recent_activity: vec![SidebarActivity::Event {
                event: Box::new(event),
            }],
            agents: Vec::new(),
        };

        assert_snapshot("event_activity", snapshot_to_screen(&snapshot, 80, 18));
    }

    #[test]
    fn render_empty_snapshot_shows_dashes() {
        let snapshot = SidebarSnapshot {
            workspace_id: fixed_workspace(),
            generated_at: fixed_now(),
            needs_attention: Vec::new(),
            resolver_working: Vec::new(),
            recently_answered: Vec::new(),
            recent_activity: Vec::new(),
            agents: Vec::new(),
        };

        assert_snapshot("empty_snapshot", snapshot_to_screen(&snapshot, 80, 18));
    }

    #[test]
    fn render_exactly_max_rows_shows_no_overflow() {
        let workspace = fixed_workspace();
        let items: Vec<FeedItem> = (0..MAX_ROWS_PER_GROUP)
            .map(|i| {
                let mut item = FeedItem::new(
                    workspace.clone(),
                    Surface::NativeUi,
                    FeedKind::Permission,
                    format!("decision-{i}"),
                    "claude",
                    "agent-hook",
                );
                item.worktree_path = Some("/repo/main".to_owned());
                item
            })
            .collect();
        let snapshot = SidebarSnapshot {
            workspace_id: workspace,
            generated_at: fixed_now(),
            needs_attention: items,
            resolver_working: Vec::new(),
            recently_answered: Vec::new(),
            recent_activity: Vec::new(),
            agents: Vec::new(),
        };

        assert_snapshot(
            "max_rows_no_overflow",
            snapshot_to_screen(&snapshot, 80, 24),
        );
    }

    #[test]
    fn render_above_max_rows_shows_overflow_indicator() {
        let workspace = fixed_workspace();
        let items: Vec<FeedItem> = (0..MAX_ROWS_PER_GROUP + 3)
            .map(|i| {
                let mut item = FeedItem::new(
                    workspace.clone(),
                    Surface::NativeUi,
                    FeedKind::Permission,
                    format!("decision-{i}"),
                    "claude",
                    "agent-hook",
                );
                item.worktree_path = Some("/repo/main".to_owned());
                item
            })
            .collect();
        let snapshot = SidebarSnapshot {
            workspace_id: workspace,
            generated_at: fixed_now(),
            needs_attention: items,
            resolver_working: Vec::new(),
            recently_answered: Vec::new(),
            recent_activity: Vec::new(),
            agents: Vec::new(),
        };

        assert_snapshot(
            "max_rows_with_overflow",
            snapshot_to_screen(&snapshot, 80, 24),
        );
    }
}
