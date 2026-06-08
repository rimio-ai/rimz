use std::io::{self, IsTerminal};

use anyhow::Result;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::crossterm::terminal as crossterm_terminal;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal, TerminalOptions, Viewport};
use rimz::agents::{HookInstallPreview, StatusLineChange};
use rimz::tui::{MouseCapture, TerminalModeGuard};
use similar::TextDiff;

const DIFF_CONTEXT_LINES: usize = 3;
const DIFF_VIEW_ROWS: u16 = 14;
const DIFF_CHROME_ROWS: usize = 2;
const FOOTER_ROWS: usize = 1;

pub(super) const CONSENT_INTRO: &str =
    "Rimz routes attention across your coding agents into one sidebar.";
pub(super) const CONSENT_INSTALL_INTENT: &str =
    "To show what an agent is doing, it adds reporting hooks to the agents on this machine.";
pub(super) const CONSENT_BOUNDARY: &str =
    "These hooks only report events to Rimz. They never answer a prompt for you.";
pub(super) const CONSENT_CHANGE_SUMMARY: &str =
    "What changes: additive config edits; existing hooks are kept.";
pub(super) const CONSENT_TEXT_CHANGE_SUMMARY: &str =
    "Rimz will make an additive, reversible per-user config change so runs appear in the sidebar.";
pub(super) const CONSENT_REVERSIBLE: &str =
    "Reversible any time with `rimz hooks uninstall <agent>`.";

pub(super) fn run_consent_gate(previews: &[HookInstallPreview]) -> Result<Vec<&'static str>> {
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return Ok(Vec::new());
    }

    let data = ConsentData::new(previews);
    let no_color = rimz::tui::no_color();
    let (_, terminal_rows) = crossterm_terminal::size()?;
    let height = inline_height(&data, terminal_rows);
    let _mode = TerminalModeGuard::enable(MouseCapture::Off)?;
    let backend = CrosstermBackend::new(io::stderr());
    let viewport = Viewport::Inline(height);
    let mut terminal = Terminal::with_options(backend, TerminalOptions { viewport })?;
    terminal.clear()?;

    let mut state = ConsentState::new(previews);
    loop {
        draw_to_terminal(&mut terminal, &data, &state, no_color)?;
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match state.handle_key(key, &data, height as usize) {
                ConsentAction::Continue => {}
                ConsentAction::Install => {
                    return Ok(state.selected_agents(&data));
                }
                ConsentAction::Skip => {
                    return Ok(Vec::new());
                }
            }
        }
    }
}

pub(super) fn preview_diff(preview: &HookInstallPreview) -> String {
    let path = preview.config_path.display().to_string();
    match preview.original_config.as_deref() {
        Some(original) => {
            let diff = TextDiff::from_lines(original, &preview.candidate_config);
            let rendered = diff
                .unified_diff()
                .context_radius(DIFF_CONTEXT_LINES)
                .header(&path, &path)
                .to_string();
            if rendered.is_empty() {
                format!("--- {path}\n+++ {path}\n@@ no changes @@\n")
            } else {
                rendered
            }
        }
        None => {
            let mut out = format!("--- /dev/null\n+++ {path}\n@@ new file @@\n");
            for line in preview.candidate_config.lines() {
                out.push('+');
                out.push_str(line);
                out.push('\n');
            }
            out
        }
    }
}

#[derive(Debug)]
struct ConsentData<'a> {
    items: Vec<ConsentItem<'a>>,
}

#[derive(Debug)]
struct ConsentItem<'a> {
    preview: &'a HookInstallPreview,
    status_summaries: Vec<String>,
    diff_lines: Vec<String>,
}

impl<'a> ConsentData<'a> {
    fn new(previews: &'a [HookInstallPreview]) -> Self {
        Self {
            items: previews
                .iter()
                .map(|preview| ConsentItem {
                    preview,
                    status_summaries: status_line_summaries(preview),
                    diff_lines: preview_diff(preview).lines().map(str::to_owned).collect(),
                })
                .collect(),
        }
    }

    fn len(&self) -> usize {
        self.items.len()
    }

    fn diff_line_count(&self) -> usize {
        self.items
            .iter()
            .enumerate()
            .map(|(idx, item)| item.diff_lines.len() + usize::from(idx > 0))
            .sum()
    }
}

#[derive(Clone, Debug)]
struct ConsentState {
    focused: usize,
    selected: Vec<bool>,
    show_diff: bool,
    diff_scroll: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConsentAction {
    Continue,
    Install,
    Skip,
}

impl ConsentState {
    fn new(previews: &[HookInstallPreview]) -> Self {
        Self {
            focused: 0,
            selected: vec![true; previews.len()],
            show_diff: false,
            diff_scroll: 0,
        }
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        data: &ConsentData<'_>,
        height: usize,
    ) -> ConsentAction {
        match key.code {
            KeyCode::Enter => ConsentAction::Install,
            KeyCode::Esc | KeyCode::Char('s') | KeyCode::Char('S') => ConsentAction::Skip,
            KeyCode::Char('d') | KeyCode::Char('D') => {
                self.show_diff = !self.show_diff;
                self.clamp_diff_scroll(data, height);
                ConsentAction::Continue
            }
            KeyCode::Char(' ') => {
                if let Some(selected) = self.selected.get_mut(self.focused) {
                    *selected = !*selected;
                }
                ConsentAction::Continue
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('K') => {
                if self.show_diff {
                    self.diff_scroll = self.diff_scroll.saturating_sub(1);
                } else {
                    self.focused = self.focused.saturating_sub(1);
                }
                ConsentAction::Continue
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('J') => {
                if self.show_diff {
                    self.diff_scroll = self.diff_scroll.saturating_add(1);
                    self.clamp_diff_scroll(data, height);
                } else if self.focused + 1 < self.selected.len() {
                    self.focused += 1;
                }
                ConsentAction::Continue
            }
            KeyCode::PageUp => {
                self.diff_scroll = self.diff_scroll.saturating_sub(5);
                ConsentAction::Continue
            }
            KeyCode::PageDown => {
                self.diff_scroll = self.diff_scroll.saturating_add(5);
                self.clamp_diff_scroll(data, height);
                ConsentAction::Continue
            }
            _ => ConsentAction::Continue,
        }
    }

    fn selected_agents(&self, data: &ConsentData<'_>) -> Vec<&'static str> {
        data.items
            .iter()
            .zip(&self.selected)
            .filter_map(|(item, selected)| selected.then_some(item.preview.agent))
            .collect()
    }

    fn clamp_diff_scroll(&mut self, data: &ConsentData<'_>, height: usize) {
        let viewport = diff_view_capacity(data, height).max(1);
        self.diff_scroll = self
            .diff_scroll
            .min(data.diff_line_count().saturating_sub(viewport));
    }
}

fn draw_to_terminal<B: Backend>(
    terminal: &mut Terminal<B>,
    data: &ConsentData<'_>,
    state: &ConsentState,
    no_color: bool,
) -> std::result::Result<(), B::Error> {
    terminal
        .draw(|frame| draw_gate(frame, data, state, no_color))
        .map(|_| ())
}

fn draw_gate(frame: &mut Frame<'_>, data: &ConsentData<'_>, state: &ConsentState, no_color: bool) {
    let area = frame.area();
    let lines = gate_lines(data, state, area, no_color);
    frame.render_widget(Paragraph::new(lines), area);
}

fn gate_lines(
    data: &ConsentData<'_>,
    state: &ConsentState,
    area: Rect,
    no_color: bool,
) -> Vec<Line<'static>> {
    let height = area.height as usize;
    let mut lines = base_lines(data, state, no_color);

    let footer = footer_line(state, no_color);
    let max_body_rows = height.saturating_sub(FOOTER_ROWS);
    if state.show_diff {
        lines.extend(diff_section_lines(data, state, max_body_rows, no_color));
    }

    if lines.len() > max_body_rows {
        lines.truncate(max_body_rows);
    }
    while lines.len() < max_body_rows {
        lines.push(Line::from(""));
    }
    lines.push(footer);
    lines
}

fn base_lines(data: &ConsentData<'_>, state: &ConsentState, no_color: bool) -> Vec<Line<'static>> {
    let mut lines = vec![
        line(vec![styled(
            "rimz hook install",
            Color::Cyan,
            Modifier::BOLD,
            no_color,
        )]),
        Line::from(""),
        Line::from(CONSENT_INTRO),
        Line::from(CONSENT_INSTALL_INTENT),
        Line::from(CONSENT_BOUNDARY),
        Line::from(""),
        Line::from("Detected agents (space toggles):"),
    ];

    for (idx, item) in data.items.iter().enumerate() {
        let preview = item.preview;
        let marker = if state.focused == idx { ">" } else { " " };
        let checked = if state.selected[idx] { "[x]" } else { "[ ]" };
        let style = if state.focused == idx {
            style(Color::Yellow, Modifier::BOLD, no_color)
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{marker} {checked} "), style),
            Span::styled(preview.agent.to_owned(), style),
            Span::raw(format!(
                "  {} events  {}",
                preview.planned_events.len(),
                preview.config_path.display()
            )),
        ]));
        for summary in &item.status_summaries {
            lines.push(Line::from(vec![
                Span::raw("      "),
                styled(summary.to_owned(), Color::DarkGray, Modifier::DIM, no_color),
            ]));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(CONSENT_CHANGE_SUMMARY));
    for item in &data.items {
        let preview = item.preview;
        lines.push(Line::from(format!(
            "  + {}: {} events at {}",
            preview.agent,
            preview.planned_events.len(),
            preview.config_path.display()
        )));
    }
    lines
}

fn footer_line(state: &ConsentState, no_color: bool) -> Line<'static> {
    Line::from(vec![
        styled("[Enter]", Color::Green, Modifier::BOLD, no_color),
        Span::raw(" install selected   "),
        styled("[Space]", Color::Yellow, Modifier::BOLD, no_color),
        Span::raw(" toggle   "),
        styled("[d]", Color::Cyan, Modifier::BOLD, no_color),
        Span::raw(if state.show_diff {
            " hide diff   "
        } else {
            " show diff   "
        }),
        styled("[s/Esc]", Color::Red, Modifier::BOLD, no_color),
        Span::raw(" skip"),
    ])
}

fn diff_section_lines(
    data: &ConsentData<'_>,
    state: &ConsentState,
    max_body_rows: usize,
    no_color: bool,
) -> Vec<Line<'static>> {
    let capacity = max_body_rows
        .saturating_sub(base_line_count(data))
        .saturating_sub(DIFF_CHROME_ROWS)
        .max(1);
    let diff_lines = all_diff_lines(data, no_color);
    let max_scroll = diff_lines.len().saturating_sub(capacity);
    let scroll = state.diff_scroll.min(max_scroll);
    let mut lines = vec![
        Line::from(""),
        Line::from(vec![
            styled("Diff", Color::Cyan, Modifier::BOLD, no_color),
            Span::raw(format!(
                "  {}/{}",
                scroll.saturating_add(1).min(diff_lines.len().max(1)),
                diff_lines.len().max(1)
            )),
        ]),
    ];
    lines.extend(diff_lines.into_iter().skip(scroll).take(capacity));
    lines
}

fn inline_height(data: &ConsentData<'_>, terminal_rows: u16) -> u16 {
    let wanted = base_line_count(data)
        .saturating_add(FOOTER_ROWS)
        .saturating_add(DIFF_CHROME_ROWS)
        .saturating_add(usize::from(DIFF_VIEW_ROWS).min(data.diff_line_count()));
    let max_rows = usize::from(terminal_rows.max(1));
    wanted.min(max_rows).max(1) as u16
}

fn base_line_count(data: &ConsentData<'_>) -> usize {
    9 + data.len() * 2
        + data
            .items
            .iter()
            .map(|item| item.status_summaries.len())
            .sum::<usize>()
}

fn diff_view_capacity(data: &ConsentData<'_>, height: usize) -> usize {
    height
        .saturating_sub(FOOTER_ROWS)
        .saturating_sub(base_line_count(data))
        .saturating_sub(DIFF_CHROME_ROWS)
}

fn all_diff_lines(data: &ConsentData<'_>, no_color: bool) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (idx, item) in data.items.iter().enumerate() {
        if idx > 0 {
            lines.push(Line::from(""));
        }
        for line in &item.diff_lines {
            lines.push(diff_line(line, no_color));
        }
    }
    lines
}

fn diff_line(text: &str, no_color: bool) -> Line<'static> {
    let (color, modifier) = if text.starts_with("+++") || text.starts_with("---") {
        (Color::Cyan, Modifier::BOLD)
    } else if text.starts_with('+') {
        (Color::Green, Modifier::empty())
    } else if text.starts_with('-') {
        (Color::Red, Modifier::empty())
    } else if text.starts_with("@@") {
        (Color::Yellow, Modifier::BOLD)
    } else {
        (Color::DarkGray, Modifier::DIM)
    };
    Line::from(styled(text.to_owned(), color, modifier, no_color))
}

fn status_line_summaries(preview: &HookInstallPreview) -> Vec<String> {
    let mut summaries = Vec::new();
    push_status_line_summary(
        &mut summaries,
        "statusLine",
        "report context to Rimz",
        &preview.status_line_change,
    );
    push_status_line_summary(
        &mut summaries,
        "subagentStatusLine",
        "report subagent activity to Rimz",
        &preview.subagent_status_line_change,
    );
    summaries
}

fn push_status_line_summary(
    summaries: &mut Vec<String>,
    key: &str,
    purpose: &str,
    change: &Option<StatusLineChange>,
) {
    match change {
        Some(StatusLineChange::Added) => {
            summaries.push(format!("also sets {key} to {purpose}"));
        }
        Some(StatusLineChange::Wrapping { original }) => {
            summaries.push(format!("also wraps {key} command ({original})"));
        }
        Some(StatusLineChange::Unchanged) | None => {}
    }
}

fn styled(
    text: impl Into<String>,
    color: Color,
    modifier: Modifier,
    no_color: bool,
) -> Span<'static> {
    Span::styled(text.into(), style(color, modifier, no_color))
}

fn line(spans: Vec<Span<'static>>) -> Line<'static> {
    Line::from(spans)
}

fn style(color: Color, modifier: Modifier, no_color: bool) -> Style {
    let style = Style::default().add_modifier(modifier);
    if no_color { style } else { style.fg(color) }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::backend::TestBackend;

    use super::*;

    fn preview(original: Option<&str>, candidate: &str) -> HookInstallPreview {
        HookInstallPreview {
            agent: "claude",
            config_path: PathBuf::from("/home/me/.claude/settings.json"),
            planned_events: vec!["SessionStart".to_owned(), "PreToolUse".to_owned()],
            original_config: original.map(str::to_owned),
            candidate_config: candidate.to_owned(),
            merged: original.is_some(),
            status_line_change: None,
            subagent_status_line_change: None,
        }
    }

    fn render(
        previews: &[HookInstallPreview],
        state: &ConsentState,
        width: u16,
        height: u16,
    ) -> String {
        let data = ConsentData::new(previews);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw_gate(frame, &data, state, false))
            .unwrap();
        terminal.backend().to_string()
    }

    #[test]
    fn diff_uses_unified_hunks_for_existing_file() {
        let preview = preview(
            Some("alpha\nkeep\nold\nomega\n"),
            "alpha\nkeep\nnew\nomega\n",
        );

        let diff = preview_diff(&preview);

        assert!(diff.contains("--- /home/me/.claude/settings.json"));
        assert!(diff.contains("+++ /home/me/.claude/settings.json"));
        assert!(diff.contains("@@"));
        assert!(diff.contains("-old"));
        assert!(diff.contains("+new"));
        assert!(!diff.contains("@@ original @@"));
        assert!(!diff.contains("@@ candidate @@"));
    }

    #[test]
    fn new_file_diff_is_framed_as_new_file() {
        let preview = preview(None, "one\ntwo\n");

        let diff = preview_diff(&preview);

        assert!(
            diff.starts_with("--- /dev/null\n+++ /home/me/.claude/settings.json\n@@ new file @@\n")
        );
        assert!(diff.contains("+one\n+two\n"));
    }

    #[test]
    fn space_toggles_focused_agent_and_enter_installs_selected() {
        let previews = [preview(Some("{}\n"), "{\"hooks\": []}\n")];
        let data = ConsentData::new(&previews);
        let mut state = ConsentState::new(&previews);

        assert_eq!(state.selected_agents(&data), vec!["claude"]);
        let action = state.handle_key(KeyEvent::from(KeyCode::Char(' ')), &data, 20);
        assert_eq!(action, ConsentAction::Continue);
        assert!(state.selected_agents(&data).is_empty());
        assert_eq!(
            state.handle_key(KeyEvent::from(KeyCode::Enter), &data, 20),
            ConsentAction::Install
        );
    }

    #[test]
    fn frame_composition_renders_agents_and_diff() {
        let previews = [preview(Some("old\n"), "new\n")];
        let mut state = ConsentState::new(&previews);
        state.show_diff = true;

        let screen = render(&previews, &state, 90, 24);

        assert!(screen.contains("rimz hook install"));
        assert!(screen.contains("[x] claude"));
        assert!(screen.contains("Diff"));
        assert!(screen.contains("-old"));
        assert!(screen.contains("+new"));
        assert!(screen.contains("[Enter] install selected"));
    }

    #[test]
    fn collapsed_view_pins_footer_to_bottom_of_reserved_viewport() {
        let previews = [preview(Some("old\n"), "new\n")];
        let state = ConsentState::new(&previews);

        let screen = render(&previews, &state, 90, 24);
        let lines = screen.lines().collect::<Vec<_>>();

        assert!(
            lines[23].contains("[Enter] install selected"),
            "footer should own the last reserved row:\n{screen}"
        );
        assert!(
            !lines[22].contains("[Enter] install selected"),
            "footer should not float above blank slack:\n{screen}"
        );
    }
}
