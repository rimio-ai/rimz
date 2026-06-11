use std::io::{self, IsTerminal};

use anyhow::Result;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
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
pub(super) const CONSENT_BOUNDARY: &str =
    "These hooks only report events to Rimz. They never answer a prompt for you.";
pub(super) const CONSENT_REVERSIBLE: &str = "Reversible any time with `rimz hooks uninstall`.";

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
                ConsentAction::Finish => {
                    return Ok(state.selected_agents(&data));
                }
                ConsentAction::SkipAll => {
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

    fn max_diff_line_count(&self) -> usize {
        self.items
            .iter()
            .map(|item| item.diff_lines.len())
            .max()
            .unwrap_or(0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WizardStep {
    Welcome,
    Agent(usize),
}

impl WizardStep {
    fn previous(self) -> Self {
        match self {
            Self::Welcome | Self::Agent(0) => Self::Welcome,
            Self::Agent(idx) => Self::Agent(idx - 1),
        }
    }
}

#[derive(Clone, Debug)]
struct ConsentState {
    step: WizardStep,
    decisions: Vec<Option<bool>>,
    show_diff: bool,
    diff_scroll: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConsentAction {
    Continue,
    Finish,
    SkipAll,
}

impl ConsentState {
    fn new(previews: &[HookInstallPreview]) -> Self {
        Self {
            step: WizardStep::Welcome,
            decisions: vec![None; previews.len()],
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
        if is_ctrl_c(key) {
            return ConsentAction::SkipAll;
        }
        match self.step {
            WizardStep::Welcome => self.handle_welcome_key(key, data),
            WizardStep::Agent(idx) => self.handle_agent_key(idx, key, data, height),
        }
    }

    fn handle_welcome_key(&mut self, key: KeyEvent, data: &ConsentData<'_>) -> ConsentAction {
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                if data.len() == 0 {
                    ConsentAction::Finish
                } else {
                    self.step = WizardStep::Agent(0);
                    self.reset_diff();
                    ConsentAction::Continue
                }
            }
            KeyCode::Esc
            | KeyCode::Char('s')
            | KeyCode::Char('S')
            | KeyCode::Char('n')
            | KeyCode::Char('N') => ConsentAction::SkipAll,
            _ => ConsentAction::Continue,
        }
    }

    fn handle_agent_key(
        &mut self,
        idx: usize,
        key: KeyEvent,
        data: &ConsentData<'_>,
        height: usize,
    ) -> ConsentAction {
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.decide_and_advance(idx, true, data)
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('s') | KeyCode::Char('S') => {
                self.decide_and_advance(idx, false, data)
            }
            KeyCode::Esc => ConsentAction::Finish,
            KeyCode::Left | KeyCode::Char('b') | KeyCode::Char('B') => {
                self.step = self.step.previous();
                self.reset_diff();
                ConsentAction::Continue
            }
            KeyCode::Char('d') | KeyCode::Char('D') => {
                self.show_diff = !self.show_diff;
                self.clamp_diff_scroll(data, height);
                ConsentAction::Continue
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('K') => {
                if self.show_diff {
                    self.diff_scroll = self.diff_scroll.saturating_sub(1);
                }
                ConsentAction::Continue
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('J') => {
                if self.show_diff {
                    self.diff_scroll = self.diff_scroll.saturating_add(1);
                    self.clamp_diff_scroll(data, height);
                }
                ConsentAction::Continue
            }
            KeyCode::PageUp => {
                if self.show_diff {
                    self.diff_scroll = self.diff_scroll.saturating_sub(5);
                }
                ConsentAction::Continue
            }
            KeyCode::PageDown => {
                if self.show_diff {
                    self.diff_scroll = self.diff_scroll.saturating_add(5);
                    self.clamp_diff_scroll(data, height);
                }
                ConsentAction::Continue
            }
            _ => ConsentAction::Continue,
        }
    }

    fn decide_and_advance(
        &mut self,
        idx: usize,
        decision: bool,
        data: &ConsentData<'_>,
    ) -> ConsentAction {
        if let Some(slot) = self.decisions.get_mut(idx) {
            *slot = Some(decision);
        }
        self.reset_diff();
        if idx + 1 >= data.len() {
            ConsentAction::Finish
        } else {
            self.step = WizardStep::Agent(idx + 1);
            ConsentAction::Continue
        }
    }

    fn selected_agents(&self, data: &ConsentData<'_>) -> Vec<&'static str> {
        data.items
            .iter()
            .zip(&self.decisions)
            .filter_map(|(item, selected)| {
                selected
                    .is_some_and(|yes| yes)
                    .then_some(item.preview.agent)
            })
            .collect()
    }

    fn clamp_diff_scroll(&mut self, data: &ConsentData<'_>, height: usize) {
        let viewport = diff_view_capacity(data, self, height).max(1);
        let line_count = self.current_diff_line_count(data);
        self.diff_scroll = self.diff_scroll.min(line_count.saturating_sub(viewport));
    }

    fn current_diff_line_count(&self, data: &ConsentData<'_>) -> usize {
        match self.step {
            WizardStep::Welcome => 0,
            WizardStep::Agent(idx) => data
                .items
                .get(idx)
                .map(|item| item.diff_lines.len())
                .unwrap_or(0),
        }
    }

    fn reset_diff(&mut self) {
        self.show_diff = false;
        self.diff_scroll = 0;
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
        lines.extend(diff_section_lines(
            data,
            state,
            max_body_rows,
            lines.len(),
            no_color,
        ));
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
    match state.step {
        WizardStep::Welcome => welcome_lines(data, no_color),
        WizardStep::Agent(idx) => agent_lines(data, idx, no_color),
    }
}

fn welcome_lines(data: &ConsentData<'_>, no_color: bool) -> Vec<Line<'static>> {
    let agent_names = data
        .items
        .iter()
        .map(|item| item.preview.agent)
        .collect::<Vec<_>>()
        .join(", ");
    let agent_word = if data.len() == 1 { "agent" } else { "agents" };
    let question_line = if data.len() == 1 {
        "One quick question.".to_owned()
    } else {
        format!("{} quick questions - one per agent.", data.len())
    };
    vec![
        line(vec![styled(
            "rimz - first-run setup",
            Color::Cyan,
            Modifier::BOLD,
            no_color,
        )]),
        Line::from(""),
        Line::from(format!(
            "Rimz found {} coding {agent_word} on this machine: {agent_names}.",
            data.len()
        )),
        Line::from(CONSENT_INTRO),
        Line::from(
            "To show what an agent is doing, it adds reporting hooks to the agent's config.",
        ),
        Line::from(CONSENT_BOUNDARY),
        Line::from(""),
        Line::from(question_line),
    ]
}

fn agent_lines(data: &ConsentData<'_>, idx: usize, no_color: bool) -> Vec<Line<'static>> {
    let Some(item) = data.items.get(idx) else {
        return welcome_lines(data, no_color);
    };
    let preview = item.preview;
    let mut lines = vec![
        line(vec![styled(
            format!(
                "rimz - first-run setup - {} ({} of {})",
                preview.agent,
                idx + 1,
                data.len()
            ),
            Color::Cyan,
            Modifier::BOLD,
            no_color,
        )]),
        Line::from(""),
        Line::from(format!(
            "Add {} reporting hooks to {}?",
            preview.planned_events.len(),
            preview.agent
        )),
        Line::from(""),
        Line::from(format!("  config   {}", preview.config_path.display())),
        Line::from("  change   additive - your existing hooks are kept"),
    ];
    for summary in &item.status_summaries {
        lines.push(Line::from(format!("  also     {summary}")));
    }
    lines.push(Line::from(format!(
        "  undo     rimz hooks uninstall {}",
        preview.agent
    )));
    lines
}

fn footer_line(state: &ConsentState, no_color: bool) -> Line<'static> {
    match state.step {
        WizardStep::Welcome => Line::from(vec![
            styled("[Enter]", Color::Green, Modifier::BOLD, no_color),
            Span::raw(" set up   "),
            styled("[s/Esc]", Color::Red, Modifier::BOLD, no_color),
            Span::raw(" skip for now"),
        ]),
        WizardStep::Agent(_) => Line::from(vec![
            styled("[Enter]", Color::Green, Modifier::BOLD, no_color),
            Span::raw(" add   "),
            styled("[n]", Color::Red, Modifier::BOLD, no_color),
            Span::raw(" skip   "),
            styled("[d]", Color::Cyan, Modifier::BOLD, no_color),
            Span::raw(if state.show_diff {
                " hide diff   "
            } else {
                " view diff   "
            }),
            styled("[Left/b]", Color::Yellow, Modifier::BOLD, no_color),
            Span::raw(" back   "),
            styled("[Esc]", Color::Red, Modifier::BOLD, no_color),
            Span::raw(" skip rest"),
        ]),
    }
}

fn diff_section_lines(
    data: &ConsentData<'_>,
    state: &ConsentState,
    max_body_rows: usize,
    base_rows: usize,
    no_color: bool,
) -> Vec<Line<'static>> {
    let WizardStep::Agent(idx) = state.step else {
        return Vec::new();
    };
    let Some(item) = data.items.get(idx) else {
        return Vec::new();
    };
    let capacity = max_body_rows
        .saturating_sub(base_rows)
        .saturating_sub(DIFF_CHROME_ROWS)
        .max(1);
    let diff_lines = item
        .diff_lines
        .iter()
        .map(|line| diff_line(line, no_color))
        .collect::<Vec<_>>();
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
    let wanted = max_base_line_count(data)
        .saturating_add(FOOTER_ROWS)
        .saturating_add(DIFF_CHROME_ROWS)
        .saturating_add(usize::from(DIFF_VIEW_ROWS).min(data.max_diff_line_count()));
    let max_rows = usize::from(terminal_rows.max(1));
    wanted.min(max_rows).max(1) as u16
}

fn max_base_line_count(data: &ConsentData<'_>) -> usize {
    let welcome = welcome_lines(data, true).len();
    let agent = (0..data.len())
        .map(|idx| agent_lines(data, idx, true).len())
        .max()
        .unwrap_or(0);
    welcome.max(agent)
}

fn base_line_count(data: &ConsentData<'_>, state: &ConsentState) -> usize {
    base_lines(data, state, true).len()
}

fn diff_view_capacity(data: &ConsentData<'_>, state: &ConsentState, height: usize) -> usize {
    height
        .saturating_sub(FOOTER_ROWS)
        .saturating_sub(base_line_count(data, state))
        .saturating_sub(DIFF_CHROME_ROWS)
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
            summaries.push(format!(
                "sets your {key} to {purpose} (removed on uninstall)"
            ));
        }
        Some(StatusLineChange::Wrapping { original }) => {
            summaries.push(format!(
                "wraps your {key} command ({original}) - restored on uninstall"
            ));
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

fn is_ctrl_c(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
        && key.modifiers.contains(KeyModifiers::CONTROL)
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

    fn preview_for(
        agent: &'static str,
        original: Option<&str>,
        candidate: &str,
    ) -> HookInstallPreview {
        HookInstallPreview {
            agent,
            config_path: PathBuf::from(format!("/home/me/.{agent}/settings.json")),
            planned_events: vec!["SessionStart".to_owned(), "PreToolUse".to_owned()],
            original_config: original.map(str::to_owned),
            candidate_config: candidate.to_owned(),
            merged: original.is_some(),
            status_line_change: None,
            subagent_status_line_change: None,
        }
    }

    fn preview(original: Option<&str>, candidate: &str) -> HookInstallPreview {
        preview_for("claude", original, candidate)
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
    fn enter_through_wizard_selects_agent() {
        let previews = [preview(Some("{}\n"), "{\"hooks\": []}\n")];
        let data = ConsentData::new(&previews);
        let mut state = ConsentState::new(&previews);

        assert!(state.selected_agents(&data).is_empty());
        let action = state.handle_key(KeyEvent::from(KeyCode::Enter), &data, 20);
        assert_eq!(action, ConsentAction::Continue);
        assert_eq!(state.step, WizardStep::Agent(0));
        assert_eq!(
            state.handle_key(KeyEvent::from(KeyCode::Enter), &data, 20),
            ConsentAction::Finish
        );
        assert_eq!(state.selected_agents(&data), vec!["claude"]);
    }

    #[test]
    fn no_skips_one_agent_and_advances() {
        let previews = [
            preview_for("claude", Some("{}\n"), "{\"hooks\": []}\n"),
            preview_for("codex", Some("{}\n"), "{\"hooks\": []}\n"),
        ];
        let data = ConsentData::new(&previews);
        let mut state = ConsentState::new(&previews);

        assert_eq!(
            state.handle_key(KeyEvent::from(KeyCode::Enter), &data, 20),
            ConsentAction::Continue
        );
        assert_eq!(
            state.handle_key(KeyEvent::from(KeyCode::Char('n')), &data, 20),
            ConsentAction::Continue
        );
        assert_eq!(state.step, WizardStep::Agent(1));
        assert_eq!(
            state.handle_key(KeyEvent::from(KeyCode::Enter), &data, 20),
            ConsentAction::Finish
        );
        assert_eq!(state.selected_agents(&data), vec!["codex"]);
    }

    #[test]
    fn esc_on_agent_keeps_prior_yeses_and_skips_rest() {
        let previews = [
            preview_for("claude", Some("{}\n"), "{\"hooks\": []}\n"),
            preview_for("codex", Some("{}\n"), "{\"hooks\": []}\n"),
        ];
        let data = ConsentData::new(&previews);
        let mut state = ConsentState::new(&previews);

        state.handle_key(KeyEvent::from(KeyCode::Enter), &data, 20);
        state.handle_key(KeyEvent::from(KeyCode::Enter), &data, 20);

        assert_eq!(state.step, WizardStep::Agent(1));
        assert_eq!(
            state.handle_key(KeyEvent::from(KeyCode::Esc), &data, 20),
            ConsentAction::Finish
        );
        assert_eq!(state.selected_agents(&data), vec!["claude"]);
    }

    #[test]
    fn ctrl_c_skips_the_consent_gate() {
        let previews = [preview(Some("{}\n"), "{\"hooks\": []}\n")];
        let data = ConsentData::new(&previews);
        let mut state = ConsentState::new(&previews);
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);

        assert_eq!(state.handle_key(key, &data, 20), ConsentAction::SkipAll);
    }

    #[test]
    fn back_steps_to_previous_agent_and_resets_diff() {
        let previews = [
            preview_for("claude", Some("old\n"), "new\n"),
            preview_for("codex", Some("old\n"), "new\n"),
        ];
        let data = ConsentData::new(&previews);
        let mut state = ConsentState::new(&previews);

        state.handle_key(KeyEvent::from(KeyCode::Enter), &data, 20);
        state.handle_key(KeyEvent::from(KeyCode::Enter), &data, 20);
        state.handle_key(KeyEvent::from(KeyCode::Char('d')), &data, 20);
        assert!(state.show_diff);

        assert_eq!(
            state.handle_key(KeyEvent::from(KeyCode::Left), &data, 20),
            ConsentAction::Continue
        );
        assert_eq!(state.step, WizardStep::Agent(0));
        assert!(!state.show_diff);
    }

    #[test]
    fn frame_composition_renders_agents_and_diff() {
        let previews = [preview(Some("old\n"), "new\n")];
        let mut state = ConsentState::new(&previews);
        state.step = WizardStep::Agent(0);
        state.show_diff = true;

        let screen = render(&previews, &state, 90, 24);

        assert!(screen.contains("rimz - first-run setup - claude (1 of 1)"));
        assert!(screen.contains("Add 2 reporting hooks to claude?"));
        assert!(screen.contains("Diff"));
        assert!(screen.contains("-old"));
        assert!(screen.contains("+new"));
        assert!(screen.contains("[Enter] add"));
    }

    #[test]
    fn collapsed_view_pins_footer_to_bottom_of_reserved_viewport() {
        let previews = [preview(Some("old\n"), "new\n")];
        let state = ConsentState::new(&previews);

        let screen = render(&previews, &state, 90, 24);
        let lines = screen.lines().collect::<Vec<_>>();

        assert!(
            lines[23].contains("[Enter] set up"),
            "footer should own the last reserved row:\n{screen}"
        );
        assert!(
            !lines[22].contains("[Enter] set up"),
            "footer should not float above blank slack:\n{screen}"
        );
    }
}
