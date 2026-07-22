//! Interactive live-room picker for ttyd browser sessions.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::{self, IsTerminal};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, Paragraph};
use ratatui::{Frame, Terminal};
use rimz::config::{MachineConfig, ThemeConfig, ThemeProviderStyle};
use rimz::ids::{AgentKind, MuxName};
use rimz::sidebar::consumer::PublishedSnapshotReader;
use rimz::theme::{Palette, Tone, resolve_provider_brand};
use rimz::tui::{MouseCapture, Screen, TerminalModeGuard};
use rimz::{RuntimePaths, StatePaths};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const EVENT_POLL: Duration = Duration::from_millis(250);
const PROBE_INTERVAL: Duration = Duration::from_secs(2);

pub(super) fn available() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

/// `false` means terminal setup failed before the picker painted, so the caller
/// can preserve the plain invalid-session error as a useful fallback.
pub(super) fn run(rejected_session: Option<&str>) -> Result<bool> {
    let guard = match TerminalModeGuard::enable(MouseCapture::Stdout, Screen::Alternate) {
        Ok(guard) => guard,
        Err(err) => {
            tracing::debug!(error = %err, "web session picker terminal mode unavailable");
            return Ok(false);
        }
    };
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(err) => {
            tracing::debug!(error = %err, "web session picker terminal unavailable");
            drop(guard);
            return Ok(false);
        }
    };
    if let Err(err) = terminal.clear() {
        tracing::debug!(error = %err, "web session picker screen unavailable");
        drop(guard);
        return Ok(false);
    }

    let theme = PickerTheme::load();
    let mut guard = Some(guard);
    let mut picker = Picker::new(rejected_session);
    let mut readers = BTreeMap::new();
    let mut next_probe = Instant::now();

    loop {
        if Instant::now() >= next_probe {
            match probe_rows(&mut readers) {
                Ok(rows) => picker.apply_probe(rows),
                Err(err) => picker.notice = Some(format!("could not read live rooms: {err}")),
            }
            next_probe = Instant::now() + PROBE_INTERVAL;
        }

        terminal.draw(|frame| render(frame, &mut picker, &theme))?;
        if !event::poll(EVENT_POLL)? {
            continue;
        }
        let Some(action) = picker.handle_event(event::read()?) else {
            continue;
        };
        match action {
            Action::Quit => return Ok(true),
            Action::Attach(session, mux) => {
                guard
                    .take()
                    .context("web session picker lost its terminal guard")?
                    .release_keep_screen()
                    .context("releasing the web session picker")?;
                let spec = rimz::mux::backend_for(mux).attach_existing_command(&session);
                let outcome = spec.to_command().spawn().and_then(|mut child| child.wait());
                guard = Some(
                    TerminalModeGuard::enable(MouseCapture::Stdout, Screen::Alternate)
                        .context("restoring the web session picker")?,
                );
                terminal.clear()?;
                match outcome {
                    Ok(status) if status.success() => picker.notice = None,
                    Ok(status) => {
                        picker.notice = Some(format!(
                            "session `{session}` attach exited with {}",
                            exit_status_label(status)
                        ));
                    }
                    Err(err) => {
                        picker.notice =
                            Some(format!("could not attach session `{session}`: {err}"));
                    }
                }
                next_probe = Instant::now();
            }
        }
    }
}

fn exit_status_label(status: std::process::ExitStatus) -> String {
    status
        .code()
        .map_or_else(|| status.to_string(), |code| format!("status {code}"))
}

fn probe_rows(
    readers: &mut BTreeMap<String, PublishedSnapshotReader>,
) -> rimz::web::Result<Vec<RoomRow>> {
    let rooms = rimz::web::live_rooms()?;
    let live_names = rooms
        .iter()
        .map(|room| room.session_name.clone())
        .collect::<BTreeSet<_>>();
    readers.retain(|session, _| live_names.contains(session));
    Ok(rooms
        .into_iter()
        .map(|room| {
            let agents = agents_for_room(&room, readers);
            RoomRow { room, agents }
        })
        .collect())
}

fn agents_for_room(
    room: &rimz::web::LiveRoom,
    readers: &mut BTreeMap<String, PublishedSnapshotReader>,
) -> Option<RoomAgents> {
    if !readers.contains_key(&room.session_name) {
        let runtime = RuntimePaths::for_workspace(room.workspace_id.clone()).ok()?;
        readers.insert(
            room.session_name.clone(),
            PublishedSnapshotReader::new(runtime, room.session_name.clone(), None),
        );
    }
    let state = StatePaths::for_workspace(room.workspace_id.clone()).ok()?;
    let snapshot = readers.get_mut(&room.session_name)?.read(&state).ok()?;
    Some(RoomAgents::from_snapshot(&snapshot))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RoomAgents {
    by_kind: Vec<(AgentKind, usize)>,
    attention: usize,
}

impl RoomAgents {
    fn from_snapshot(snapshot: &rimz::SidebarSnapshot) -> Self {
        let live_agent_ids = snapshot
            .agent_panes
            .iter()
            .filter_map(|pane_agent| pane_agent.agent_id.as_ref())
            .collect::<HashSet<_>>();
        let mut by_kind = BTreeMap::new();
        let mut attention = 0;
        for agent in snapshot
            .root_agents()
            .filter(|agent| live_agent_ids.contains(&agent.agent_id))
        {
            *by_kind.entry(agent.kind.clone()).or_insert(0) += 1;
            attention += usize::from(agent.effective_status().is_attention());
        }
        Self {
            by_kind: by_kind.into_iter().collect(),
            attention,
        }
    }

    fn is_empty(&self) -> bool {
        self.by_kind.is_empty() && self.attention == 0
    }

    fn width(&self) -> usize {
        if self.is_empty() {
            return 1;
        }
        let kinds = self
            .by_kind
            .iter()
            .map(|(kind, count)| UnicodeWidthStr::width(kind.as_str()) + 2 + digits(*count))
            .sum::<usize>();
        let gaps = self.by_kind.len().saturating_sub(1);
        let attention = if self.attention == 0 {
            0
        } else {
            3 + digits(self.attention)
        };
        kinds + gaps + attention
    }
}

fn digits(value: usize) -> usize {
    value.checked_ilog10().map_or(1, |power| power as usize + 1)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RoomRow {
    room: rimz::web::LiveRoom,
    agents: Option<RoomAgents>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Action {
    Attach(String, MuxName),
    Quit,
}

#[derive(Debug)]
struct Picker {
    rows: Vec<RoomRow>,
    selected: Option<String>,
    filter: String,
    notice: Option<String>,
    hit_rows: BTreeMap<u16, String>,
}

impl Picker {
    fn new(rejected_session: Option<&str>) -> Self {
        Self {
            rows: Vec::new(),
            selected: None,
            filter: String::new(),
            notice: rejected_session
                .filter(|session| !session.is_empty())
                .map(|session| format!("session `{session}` is not a live RimZ room")),
            hit_rows: BTreeMap::new(),
        }
    }

    fn apply_probe(&mut self, rows: Vec<RoomRow>) {
        self.rows = rows;
        self.normalize_selection();
    }

    fn visible(&self) -> Vec<&RoomRow> {
        let filter = self.filter.to_lowercase();
        self.rows
            .iter()
            .filter(|row| {
                filter.is_empty() || row.room.session_name.to_lowercase().contains(&filter)
            })
            .collect()
    }

    fn normalize_selection(&mut self) {
        let current_is_visible = self.selected.as_deref().is_some_and(|selected| {
            self.visible()
                .iter()
                .any(|row| row.room.session_name == selected)
        });
        if !current_is_visible {
            self.selected = self
                .visible()
                .first()
                .map(|row| row.room.session_name.clone());
        }
    }

    fn move_selection(&mut self, offset: isize) {
        let visible = self.visible();
        if visible.is_empty() {
            self.selected = None;
            return;
        }
        let current = self
            .selected
            .as_deref()
            .and_then(|selected| {
                visible
                    .iter()
                    .position(|row| row.room.session_name == selected)
            })
            .unwrap_or(0);
        let next = current
            .saturating_add_signed(offset)
            .min(visible.len().saturating_sub(1));
        self.selected = Some(visible[next].room.session_name.clone());
    }

    fn selected_action(&self) -> Option<Action> {
        let selected = self.selected.as_deref()?;
        let row = self
            .visible()
            .into_iter()
            .find(|row| row.room.session_name == selected)?;
        Some(Action::Attach(row.room.session_name.clone(), row.room.mux))
    }

    fn handle_event(&mut self, event: Event) -> Option<Action> {
        match event {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(key.code, KeyCode::Char('c' | 'C'))
                {
                    return Some(Action::Quit);
                }
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
                    KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
                    KeyCode::Enter => return self.selected_action(),
                    KeyCode::Backspace => {
                        self.filter.pop();
                        self.normalize_selection();
                    }
                    KeyCode::Esc if self.filter.is_empty() => return Some(Action::Quit),
                    KeyCode::Esc => {
                        self.filter.clear();
                        self.normalize_selection();
                    }
                    KeyCode::Char(character)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        self.filter.push(character);
                        self.normalize_selection();
                    }
                    _ => {}
                }
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollUp => self.move_selection(-1),
                MouseEventKind::ScrollDown => self.move_selection(1),
                MouseEventKind::Down(MouseButton::Left) => {
                    let session = self.hit_rows.get(&mouse.row)?.clone();
                    if self.selected.as_deref() == Some(session.as_str()) {
                        return self.selected_action();
                    }
                    self.selected = Some(session);
                }
                _ => {}
            },
            _ => {}
        }
        None
    }
}

struct PickerTheme {
    palette: Palette,
    providers: BTreeMap<String, ThemeProviderStyle>,
    no_color: bool,
}

impl PickerTheme {
    fn load() -> Self {
        let config = MachineConfig::load_lenient();
        Self::resolve(&config.theme, rimz::tui::truecolor(), rimz::tui::no_color())
    }

    fn resolve(theme: &ThemeConfig, truecolor: bool, no_color: bool) -> Self {
        let depth = theme.effective_theme_mode().depth(truecolor);
        Self {
            palette: Palette::resolve(theme, depth),
            providers: theme.providers.clone(),
            no_color,
        }
    }

    fn style(&self, tone: Tone) -> Style {
        if self.no_color {
            Style::default()
        } else {
            Style::default().fg(tone_color(tone))
        }
    }

    fn body(&self) -> Style {
        self.style(self.palette.body())
    }

    fn muted(&self) -> Style {
        self.style(self.palette.muted())
    }

    fn faint(&self) -> Style {
        self.style(self.palette.faint())
    }

    fn meta(&self) -> Style {
        self.style(self.palette.meta())
    }

    fn accent(&self) -> Style {
        self.style(self.palette.accent())
    }

    fn alarm(&self) -> Style {
        self.style(self.palette.alarm())
    }

    fn rule(&self) -> Style {
        self.style(self.palette.rule())
    }

    fn selected(&self) -> Style {
        let style = Style::default().add_modifier(Modifier::BOLD);
        if self.no_color {
            style
        } else {
            style
                .fg(tone_color(self.palette.selection()))
                .bg(tone_color(self.palette.selection_bg()))
        }
    }

    fn provider(&self, kind: &str) -> Style {
        self.style(resolve_provider_brand(kind, &self.providers).tone(&self.palette))
    }
}

fn tone_color(tone: Tone) -> Color {
    match tone {
        Tone::Indexed(index) => Color::Indexed(index),
        Tone::Rgb(red, green, blue) => Color::Rgb(red, green, blue),
    }
}

fn render(frame: &mut Frame<'_>, picker: &mut Picker, theme: &PickerTheme) {
    let area = frame.area();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.rule())
        .title(Line::from(vec![
            Span::styled(" RimZ ", theme.accent().add_modifier(Modifier::BOLD)),
            Span::styled("── sessions ", theme.muted()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    picker.hit_rows.clear();
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let notice_height = u16::from(picker.notice.is_some());
    if let Some(notice) = picker.notice.as_deref() {
        frame.render_widget(
            Paragraph::new(truncate_width(notice, usize::from(inner.width))).style(theme.alarm()),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
    }

    let controls_height = inner.height.saturating_sub(notice_height).min(2);
    let list_height = inner
        .height
        .saturating_sub(notice_height)
        .saturating_sub(controls_height);
    let list_area = Rect::new(
        inner.x,
        inner.y.saturating_add(notice_height),
        inner.width,
        list_height,
    );
    render_rooms(frame, picker, theme, list_area);

    if controls_height >= 1 {
        let filter_y = inner.y.saturating_add(inner.height - controls_height);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("filter: ", theme.meta()),
                Span::styled(picker.filter.clone(), theme.body()),
                Span::styled("_", theme.accent()),
            ])),
            Rect::new(inner.x, filter_y, inner.width, 1),
        );
    }
    if controls_height == 2 {
        frame.render_widget(
            Paragraph::new("↑↓ select · ⏎ attach · type to filter · esc quit").style(theme.faint()),
            Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1),
        );
    }
}

fn render_rooms(frame: &mut Frame<'_>, picker: &mut Picker, theme: &PickerTheme, area: Rect) {
    if area.height == 0 {
        return;
    }
    let visible = picker.visible();
    if visible.is_empty() {
        let message = if picker.rows.is_empty() {
            "No live RimZ sessions — run `rimz start` in a project".to_owned()
        } else {
            format!("No sessions match `{}`", picker.filter)
        };
        frame.render_widget(Paragraph::new(message).style(theme.muted()), area);
        return;
    }

    let selected_index = picker
        .selected
        .as_deref()
        .and_then(|selected| {
            visible
                .iter()
                .position(|row| row.room.session_name == selected)
        })
        .unwrap_or(0);
    let capacity = usize::from(area.height);
    let start = selected_index.saturating_add(1).saturating_sub(capacity);
    let shown = &visible[start..visible.len().min(start + capacity)];
    let name_width = shown
        .iter()
        .map(|row| UnicodeWidthStr::width(row.room.session_name.as_str()))
        .max()
        .unwrap_or(0)
        .min(28);
    let items = shown
        .iter()
        .map(|row| {
            let selected = picker.selected.as_deref() == Some(row.room.session_name.as_str());
            let mut item = ListItem::new(Line::from(room_spans(
                row,
                selected,
                name_width,
                usize::from(area.width),
                theme,
            )));
            if selected {
                item = item.style(theme.selected());
            }
            item
        })
        .collect::<Vec<_>>();
    frame.render_widget(List::new(items).style(theme.body()), area);
    let hit_rows = shown
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {
            let index = u16::try_from(index).ok()?;
            Some((area.y.saturating_add(index), row.room.session_name.clone()))
        })
        .collect::<Vec<_>>();
    picker.hit_rows.extend(hit_rows);
}

fn room_spans(
    row: &RoomRow,
    selected: bool,
    name_width: usize,
    total_width: usize,
    theme: &PickerTheme,
) -> Vec<Span<'static>> {
    let prefix = if selected { "▸ " } else { "  " };
    let name = pad_right(
        &truncate_width(&row.room.session_name, name_width),
        name_width,
    );
    let mux = format!("{:<7}", row.room.mux.as_str());
    let agents_width = row.agents.as_ref().map_or(1, RoomAgents::width);
    let fixed_width = 2 + name_width + 2 + 7 + 2 + 2 + agents_width;
    let path_width = total_width.saturating_sub(fixed_width);
    let path = truncate_width(
        &crate::cli::render::home_relative(&row.room.project_root.to_string_lossy()),
        path_width,
    );
    let mut spans = vec![
        Span::styled(prefix.to_owned(), theme.accent()),
        Span::styled(name, theme.body().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled(mux, theme.meta()),
        Span::raw("  "),
        Span::styled(pad_right(&path, path_width), theme.muted()),
        Span::raw("  "),
    ];
    push_agent_spans(&mut spans, row.agents.as_ref(), theme);
    spans
}

fn push_agent_spans(
    spans: &mut Vec<Span<'static>>,
    agents: Option<&RoomAgents>,
    theme: &PickerTheme,
) {
    let Some(agents) = agents.filter(|agents| !agents.is_empty()) else {
        spans.push(Span::styled("–", theme.muted()));
        return;
    };
    for (index, (kind, count)) in agents.by_kind.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(
            format!("{} ×{count}", kind.as_str()),
            theme.provider(kind.as_str()),
        ));
    }
    if agents.attention > 0 {
        spans.push(Span::styled(
            format!(" ● {}", agents.attention),
            theme.alarm().add_modifier(Modifier::BOLD),
        ));
    }
}

fn pad_right(text: &str, width: usize) -> String {
    let padding = width.saturating_sub(UnicodeWidthStr::width(text));
    format!("{text}{}", " ".repeat(padding))
}

fn truncate_width(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_owned();
    }
    if max_width == 0 {
        return String::new();
    }
    let content_width = max_width.saturating_sub(1);
    let mut width = 0;
    let mut out = String::new();
    for character in text.chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if width + character_width > content_width {
            break;
        }
        width += character_width;
        out.push(character);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests;
