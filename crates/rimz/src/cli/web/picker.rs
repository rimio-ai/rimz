//! Interactive live-room picker for ttyd browser sessions.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::{self, IsTerminal, Write};
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
use rimz::config::{GlyphRole, MachineConfig, ThemeConfig, ThemeProviderStyle};
use rimz::ids::{AgentKind, MuxName};
use rimz::sidebar::consumer::PublishedSnapshotReader;
use rimz::theme::{Palette, Tone, resolve_provider_brand, theme_glyphs};
use rimz::tui::{MouseCapture, Screen, TerminalModeGuard};
use rimz::{RuntimePaths, SpendWindow, StatePaths};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const EVENT_POLL: Duration = Duration::from_millis(250);
const PROBE_INTERVAL: Duration = Duration::from_secs(2);
const CARD_HEIGHT: usize = 3;

pub(super) fn available() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

/// `false` means terminal setup failed before the picker took over, so the
/// caller can preserve its direct-attach or invalid-session fallback.
pub(super) fn run(
    rejected_session: Option<&str>,
    initial_attach: Option<(&str, &rimz::mux::CommandSpec)>,
) -> Result<bool> {
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
    let mut initial_attach =
        initial_attach.map(|(session, spec)| (session.to_owned(), spec.clone()));

    if initial_attach.is_none() {
        write_session_sync(None)?;
    }

    loop {
        let (session, spec) = if let Some(initial_attach) = initial_attach.take() {
            initial_attach
        } else {
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
                    let spec = rimz::mux::backend_for(mux).attach_existing_command(&session);
                    (session, spec)
                }
            }
        };

        guard
            .take()
            .context("web session picker lost its terminal guard")?
            .release_keep_screen()
            .context("releasing the web session picker")?;
        write_session_sync(Some(&session))?;
        let outcome = spec.to_command().spawn().and_then(|mut child| child.wait());
        guard = Some(
            TerminalModeGuard::enable(MouseCapture::Stdout, Screen::Alternate)
                .context("restoring the web session picker")?,
        );
        write_session_sync(None)?;
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
                picker.notice = Some(format!("could not attach session `{session}`: {err}"));
            }
        }
        next_probe = Instant::now();
    }
}

fn session_sync_osc(session: Option<&str>) -> String {
    format!(
        "\x1b]{};rimz-session={}\x07",
        rimz::web::TTYD_SESSION_OSC,
        session.unwrap_or_default()
    )
}

fn write_session_sync(session: Option<&str>) -> Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(session_sync_osc(session).as_bytes())?;
    stdout.flush()?;
    Ok(())
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
            let stats = stats_for_room(&room, readers);
            RoomRow { room, stats }
        })
        .collect())
}

fn stats_for_room(
    room: &rimz::web::LiveRoom,
    readers: &mut BTreeMap<String, PublishedSnapshotReader>,
) -> Option<RoomStats> {
    if !readers.contains_key(&room.session_name) {
        let runtime = RuntimePaths::for_workspace(room.workspace_id.clone()).ok()?;
        readers.insert(
            room.session_name.clone(),
            PublishedSnapshotReader::new(runtime, room.session_name.clone(), None),
        );
    }
    let state = StatePaths::for_workspace(room.workspace_id.clone()).ok()?;
    let snapshot = readers.get_mut(&room.session_name)?.read(&state).ok()?;
    Some(RoomStats::from_snapshot(&snapshot))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RoomAgents {
    by_kind: Vec<(AgentKind, usize)>,
    attention: usize,
}

#[derive(Clone, Debug, PartialEq)]
struct RoomStats {
    agents: RoomAgents,
    headline: SpendWindow,
}

impl RoomStats {
    fn from_snapshot(snapshot: &rimz::SidebarSnapshot) -> Self {
        Self {
            agents: RoomAgents::from_snapshot(snapshot),
            headline: snapshot
                .workspace_value_tally
                .as_ref()
                .map_or_else(SpendWindow::default, |tally| tally.headline),
        }
    }
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

#[derive(Clone, Debug, PartialEq)]
struct RoomRow {
    room: rimz::web::LiveRoom,
    stats: Option<RoomStats>,
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

    fn apply_probe(&mut self, mut rows: Vec<RoomRow>) {
        rows.sort_by_cached_key(|row| (repo_name(&row.room), row.room.project_root.clone()));
        self.rows = rows;
        self.normalize_selection();
    }

    fn visible(&self) -> Vec<&RoomRow> {
        let filter = self.filter.to_lowercase();
        self.rows
            .iter()
            .filter(|row| {
                filter.is_empty()
                    || repo_name(&row.room).to_lowercase().contains(&filter)
                    || room_path(&row.room).to_lowercase().contains(&filter)
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
    workspace_glyph: String,
    sessions_glyph: String,
    tokens_glyph: String,
    no_color: bool,
}

impl PickerTheme {
    fn load() -> Self {
        let config = MachineConfig::load_lenient();
        Self::resolve(&config.theme, rimz::tui::truecolor(), rimz::tui::no_color())
    }

    fn resolve(theme: &ThemeConfig, truecolor: bool, no_color: bool) -> Self {
        let depth = theme.effective_theme_mode().depth(truecolor);
        let glyph = theme_glyphs(theme);
        Self {
            palette: Palette::resolve(theme, depth),
            providers: theme.providers.clone(),
            workspace_glyph: glyph(GlyphRole::CockpitWorkspace),
            sessions_glyph: glyph(GlyphRole::CockpitSessions),
            tokens_glyph: glyph(GlyphRole::TokensTotal),
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
    let capacity = (usize::from(area.height) / CARD_HEIGHT).max(1);
    let start = selected_index.saturating_add(1).saturating_sub(capacity);
    let shown = &visible[start..visible.len().min(start + capacity)];
    let mut items = Vec::with_capacity(shown.len().saturating_mul(CARD_HEIGHT));
    for (index, row) in shown.iter().enumerate() {
        let selected = picker.selected.as_deref() == Some(row.room.session_name.as_str());
        let [identity, stats] = room_lines(row, selected, usize::from(area.width), theme);
        let mut item = ListItem::new(vec![identity, stats]);
        if selected {
            item = item.style(theme.selected());
        }
        items.push(item);
        if index + 1 < shown.len() {
            items.push(ListItem::new(""));
        }
    }
    frame.render_widget(List::new(items).style(theme.body()), area);
    let mut hit_rows = Vec::with_capacity(shown.len().saturating_mul(2));
    for (index, row) in shown.iter().enumerate() {
        let Ok(offset) = u16::try_from(index.saturating_mul(CARD_HEIGHT)) else {
            continue;
        };
        let first = area.y.saturating_add(offset);
        for hit in [first, first.saturating_add(1)] {
            if hit < area.bottom() {
                hit_rows.push((hit, row.room.session_name.clone()));
            }
        }
    }
    picker.hit_rows.extend(hit_rows);
}

fn room_lines(
    row: &RoomRow,
    selected: bool,
    total_width: usize,
    theme: &PickerTheme,
) -> [Line<'static>; 2] {
    let prefix = if selected { "▸ " } else { "  " };
    let workspace = format!("{} ", theme.workspace_glyph);
    let fixed_width = UnicodeWidthStr::width(prefix) + UnicodeWidthStr::width(workspace.as_str());
    let available = total_width.saturating_sub(fixed_width);
    let repo = repo_name(&row.room);
    let repo_width = UnicodeWidthStr::width(repo.as_str());
    let path = room_path(&row.room);
    let (repo, gap, path) = if repo_width >= available {
        (truncate_width(&repo, available), 0, String::new())
    } else {
        let path = truncate_left_width(&path, available.saturating_sub(repo_width + 1));
        let gap = available.saturating_sub(repo_width + UnicodeWidthStr::width(path.as_str()));
        (repo, gap, path)
    };
    let mut identity = vec![
        Span::styled(prefix.to_owned(), theme.accent()),
        Span::styled(workspace, theme.accent()),
        Span::styled(repo, theme.body().add_modifier(Modifier::BOLD)),
    ];
    if !path.is_empty() {
        identity.push(Span::raw(" ".repeat(gap)));
        identity.push(Span::styled(path, theme.muted()));
    }

    let mut stats = vec![Span::raw("  ")];
    let Some(room_stats) = row.stats.as_ref() else {
        stats.push(Span::styled("–", theme.muted()));
        return [Line::from(identity), Line::from(stats)];
    };
    let mut agents = Vec::new();
    push_agent_spans(&mut agents, Some(&room_stats.agents), theme);
    let agents_width = room_stats.agents.width();
    let metric_sets = [
        metric_spans(room_stats, true, true, theme),
        metric_spans(room_stats, true, false, theme),
        metric_spans(room_stats, false, false, theme),
    ];
    let metrics = metric_sets
        .into_iter()
        .find(|metrics| 2 + agents_width + 1 + spans_width(metrics) <= total_width)
        .unwrap_or_else(|| metric_spans(room_stats, false, false, theme));
    let metrics_width = spans_width(&metrics);
    let agents_max = total_width.saturating_sub(2 + metrics_width + 1);
    let agents = truncate_spans_width(agents, agents_max);
    let gap = total_width.saturating_sub(2 + spans_width(&agents) + metrics_width);
    stats.extend(agents);
    stats.push(Span::raw(" ".repeat(gap)));
    stats.extend(metrics);
    [Line::from(identity), Line::from(stats)]
}

fn metric_spans(
    stats: &RoomStats,
    show_sessions: bool,
    show_tokens: bool,
    theme: &PickerTheme,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    if show_sessions {
        spans.push(Span::styled(theme.sessions_glyph.clone(), theme.meta()));
        spans.push(Span::styled(
            format!(" {}", stats.headline.sessions),
            theme.body(),
        ));
    }
    if show_tokens {
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(theme.tokens_glyph.clone(), theme.meta()));
        spans.push(Span::styled(
            format!(
                " {}",
                rimz::theme::fmt::compact_count(stats.headline.tokens)
            ),
            theme.body(),
        ));
    }
    if !spans.is_empty() {
        spans.push(Span::raw("  "));
    }
    spans.push(Span::styled(
        rimz::theme::fmt::dollars2(stats.headline.usd),
        theme.body(),
    ));
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

fn repo_name(room: &rimz::web::LiveRoom) -> String {
    rimz::worktree::normalize_path_lexical(&room.project_root)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map_or_else(|| room.session_name.clone(), str::to_owned)
}

fn room_path(room: &rimz::web::LiveRoom) -> String {
    crate::cli::render::home_relative(&room.project_root.to_string_lossy())
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}

fn truncate_spans_width(spans: Vec<Span<'static>>, max_width: usize) -> Vec<Span<'static>> {
    let mut remaining = max_width;
    let mut truncated = Vec::new();
    for span in spans {
        let width = UnicodeWidthStr::width(span.content.as_ref());
        if width <= remaining {
            remaining -= width;
            truncated.push(span);
            continue;
        }
        if remaining > 0 {
            truncated.push(Span::styled(
                truncate_width(span.content.as_ref(), remaining),
                span.style,
            ));
        }
        break;
    }
    truncated
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

fn truncate_left_width(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_owned();
    }
    if max_width == 0 {
        return String::new();
    }
    let content_width = max_width.saturating_sub(1);
    let mut width = 0;
    let mut suffix = Vec::new();
    for character in text.chars().rev() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if width + character_width > content_width {
            break;
        }
        width += character_width;
        suffix.push(character);
    }
    let suffix = suffix.into_iter().rev().collect::<String>();
    format!("…{suffix}")
}

#[cfg(test)]
mod tests;
