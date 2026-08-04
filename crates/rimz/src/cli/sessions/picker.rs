//! Interactive live-room picker shared by terminal and browser sessions.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, Paragraph};
use ratatui::{Frame, Terminal};
use rimz::config::{GlyphRole, MachineConfig, ThemeConfig, ThemeProviderStyle};
use rimz::ids::{AgentKind, MuxName};
use rimz::room::session::LiveRoom;
use rimz::sidebar::consumer::PublishedSnapshotReader;
use rimz::theme::{Identity, Palette, Tone, resolve_provider_brand, theme_glyphs};
use rimz::tui::{MouseCapture, Screen, TerminalModeGuard};
use rimz::workspace::KnownWorkspace;
use rimz::{RuntimePaths, SpendWindow, StatePaths};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::cli::GlobalFlags;

const EVENT_POLL: Duration = Duration::from_millis(250);
const PROBE_INTERVAL: Duration = Duration::from_secs(2);
const CARD_HEIGHT: usize = 3;
const PROMPT_RECENCY_WINDOW_SECS: i64 = 24 * 60 * 60;
const PANEL_MIN_WIDTH: u16 = 58;
const PANEL_MAX_WIDTH: u16 = 84;
const PANEL_TARGET_HEIGHT: u16 = 24;
const BANNER_GAP: u16 = 1;
const HELP_HEIGHT: u16 = 1;
const MIN_BLOCK_HEIGHT: u16 = 5;
const BANNER: [&str; 6] = [
    "██████╗ ██╗███╗   ███╗███████╗",
    "██╔══██╗██║████╗ ████║╚══███╔╝",
    "██████╔╝██║██╔████╔██║  ███╔╝ ",
    "██╔══██╗██║██║╚██╔╝██║ ███╔╝  ",
    "██║  ██║██║██║ ╚═╝ ██║███████╗",
    "╚═╝  ╚═╝╚═╝╚═╝     ╚═╝╚══════╝",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    Terminal,
    Web,
}

/// `false` means terminal setup failed before the picker took over, so the
/// caller can preserve its direct-attach or invalid-session fallback.
pub(super) fn run(
    mode: Mode,
    rejected_session: Option<&str>,
    initial_attach: Option<(&str, &rimz::mux::CommandSpec)>,
    globals: &GlobalFlags,
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
    let mut initial_attach = initial_attach.map(|(session, spec)| {
        (
            session.to_owned(),
            session_display_name(session),
            spec.clone(),
        )
    });

    if initial_attach.is_none() {
        write_session_sync(mode, None)?;
    }

    loop {
        let launch = if let Some((session, display_name, spec)) = initial_attach.take() {
            Launch::Attach(session, display_name, spec)
        } else {
            if Instant::now() >= next_probe {
                match probe_inventory(&mut readers) {
                    Ok((rows, dormant)) => {
                        picker.apply_probe(rows, jiff::Timestamp::now());
                        picker.apply_dormant(dormant);
                    }
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
                Action::Attach(session, display_name, mux) => {
                    let spec = rimz::mux::backend_for(mux).attach_existing_command(&session);
                    Launch::Attach(session, display_name, spec)
                }
                Action::Create(path) => Launch::Create(path),
            }
        };

        guard
            .take()
            .context("session picker lost its terminal guard")?
            .handoff_keep_screen()
            .context("handing off the session picker")?;
        let (target, outcome) = match launch {
            Launch::Attach(session, display_name, spec) => {
                write_session_sync(mode, Some((&session, &display_name)))?;
                let outcome = spec
                    .to_command()
                    .spawn()
                    .and_then(|mut child| child.wait())
                    .map_err(anyhow::Error::from);
                (Some((session, display_name)), outcome)
            }
            Launch::Create(path) => {
                match crate::cli::room::ensure_workspace_room_detached(&path, globals, false, false)
                {
                    Ok(context) => {
                        let session = context.session_name().to_owned();
                        let display_name = session_display_name(&session);
                        let spec = rimz::mux::backend_for(context.mux_name())
                            .attach_existing_command(&session);
                        write_session_sync(mode, Some((&session, &display_name)))?;
                        let outcome = spec
                            .to_command()
                            .spawn()
                            .and_then(|mut child| child.wait())
                            .map_err(anyhow::Error::from);
                        (Some((session, display_name)), outcome)
                    }
                    Err(err) => {
                        let error = err.context(format!("creating a room for {}", path.display()));
                        (None, Err(error))
                    }
                }
            }
        };
        guard = Some(
            TerminalModeGuard::enable(MouseCapture::Stdout, Screen::Alternate)
                .context("restoring the session picker")?,
        );
        write_session_sync(mode, None)?;
        terminal.clear()?;
        match outcome {
            Ok(status) if status.success() => picker.notice = None,
            Ok(status) => {
                let session = target
                    .as_ref()
                    .map(|(session, _)| session.as_str())
                    .unwrap_or("new room");
                picker.notice = Some(format!(
                    "session `{session}` attach exited with {}",
                    exit_status_label(status)
                ));
            }
            Err(err) => {
                picker.notice = Some(match target {
                    Some((session, _)) => {
                        format!("could not attach session `{session}`: {err}")
                    }
                    None => format!("could not create session: {err:#}"),
                });
            }
        }
        next_probe = Instant::now();
    }
}

fn session_sync_enabled(mode: Mode) -> bool {
    mode == Mode::Web
}

fn write_session_sync(mode: Mode, target: Option<(&str, &str)>) -> Result<()> {
    if session_sync_enabled(mode) {
        rimz::web::write_session_sync(target)?;
    }
    Ok(())
}

fn exit_status_label(status: std::process::ExitStatus) -> String {
    status
        .code()
        .map_or_else(|| status.to_string(), |code| format!("status {code}"))
}

fn probe_inventory(
    readers: &mut BTreeMap<String, PublishedSnapshotReader>,
) -> rimz::room::LiveRoomResult<(Vec<RoomRow>, Vec<KnownWorkspace>)> {
    let inventory = rimz::room::session::room_inventory()?;
    let rooms = inventory.live;
    let live_names = rooms
        .iter()
        .map(|room| room.session_name.clone())
        .collect::<BTreeSet<_>>();
    readers.retain(|session, _| live_names.contains(session));
    let rows = rooms
        .into_iter()
        .map(|room| {
            let stats = stats_for_room(&room, readers);
            RoomRow { room, stats }
        })
        .collect();
    Ok((rows, inventory.dormant))
}

fn stats_for_room(
    room: &LiveRoom,
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
    last_prompt_at: Option<jiff::Timestamp>,
}

impl RoomStats {
    fn from_snapshot(snapshot: &rimz::store::snapshot::SidebarSnapshot) -> Self {
        Self {
            agents: RoomAgents::from_snapshot(snapshot),
            headline: snapshot
                .workspace_value_tally
                .as_ref()
                .map_or_else(SpendWindow::default, |tally| tally.headline.clone()),
            last_prompt_at: snapshot
                .agents
                .iter()
                .filter(|agent| !agent.is_provider_subagent())
                .filter_map(|agent| agent.turn_started_at)
                .max(),
        }
    }
}

impl RoomAgents {
    fn from_snapshot(snapshot: &rimz::store::snapshot::SidebarSnapshot) -> Self {
        let live_agent_ids = snapshot
            .agent_panes
            .iter()
            .filter_map(|pane_agent| pane_agent.agent_id.as_ref())
            .collect::<HashSet<_>>();
        let mut by_kind = BTreeMap::new();
        let mut attention = 0;
        for agent in snapshot
            .agents
            .iter()
            .filter(|agent| !agent.is_provider_subagent())
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
    room: LiveRoom,
    stats: Option<RoomStats>,
}

enum Launch {
    Attach(String, String, rimz::mux::CommandSpec),
    Create(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Action {
    Attach(String, String, MuxName),
    Create(PathBuf),
    Quit,
}

#[derive(Debug)]
enum View {
    Rooms,
    NewSession(NewSession),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum NewSessionRow {
    Recent { name: String, path: PathBuf },
    Current { path: PathBuf },
    Directory { name: String, path: PathBuf },
}

impl NewSessionRow {
    fn path(&self) -> &Path {
        match self {
            Self::Recent { path, .. } | Self::Current { path } | Self::Directory { path, .. } => {
                path
            }
        }
    }

    fn matches(&self, filter: &str) -> bool {
        if filter.is_empty() {
            return true;
        }
        let filter = filter.to_lowercase();
        let path = crate::cli::render::home_relative(&self.path().to_string_lossy());
        let name = match self {
            Self::Recent { name, .. } | Self::Directory { name, .. } => name.as_str(),
            Self::Current { .. } => ".",
        };
        name.to_lowercase().contains(&filter) || path.to_lowercase().contains(&filter)
    }
}

#[derive(Debug)]
struct NewSession {
    current_dir: PathBuf,
    input: String,
    selected: usize,
    dormant: Vec<KnownWorkspace>,
    directories: Vec<PathBuf>,
    notice: Option<String>,
}

impl NewSession {
    fn new(current_dir: PathBuf, dormant: Vec<KnownWorkspace>) -> Self {
        let mut session = Self {
            current_dir,
            input: String::new(),
            selected: 0,
            dormant,
            directories: Vec::new(),
            notice: None,
        };
        session.reload_directories();
        session
    }

    fn reload_directories(&mut self) {
        match std::fs::read_dir(&self.current_dir) {
            Ok(entries) => {
                self.directories = entries
                    .filter_map(|entry| entry.ok())
                    .map(|entry| entry.path())
                    .filter(|path| {
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| !name.starts_with('.'))
                            && path.is_dir()
                    })
                    .collect();
                self.directories.sort_by(|left, right| {
                    left.file_name()
                        .cmp(&right.file_name())
                        .then_with(|| left.cmp(right))
                });
                self.notice = None;
            }
            Err(err) => {
                self.directories.clear();
                self.notice = Some(format!(
                    "could not read {}: {err}",
                    crate::cli::render::home_relative(&self.current_dir.to_string_lossy())
                ));
            }
        }
        self.normalize_selection();
    }

    fn rows(&self) -> Vec<NewSessionRow> {
        self.dormant
            .iter()
            .map(|workspace| NewSessionRow::Recent {
                name: repo_display_name(&workspace.project_root)
                    .unwrap_or_else(|| workspace.session_name.clone()),
                path: workspace.project_root.clone(),
            })
            .chain(std::iter::once(NewSessionRow::Current {
                path: self.current_dir.clone(),
            }))
            .chain(self.directories.iter().map(|path| {
                NewSessionRow::Directory {
                    name: path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or_default()
                        .to_owned(),
                    path: path.clone(),
                }
            }))
            .filter(|row| row.matches(&self.input))
            .collect()
    }

    fn normalize_selection(&mut self) {
        self.selected = self.selected.min(self.rows().len().saturating_sub(1));
    }

    fn move_selection(&mut self, offset: isize) {
        let len = self.rows().len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        self.selected = self
            .selected
            .saturating_add_signed(offset)
            .min(len.saturating_sub(1));
    }

    fn selected_row(&self) -> Option<NewSessionRow> {
        self.rows().into_iter().nth(self.selected)
    }

    fn descend(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        let path = row.path();
        if path != self.current_dir && path.is_dir() {
            self.current_dir = path.to_path_buf();
            self.input.clear();
            self.selected = 0;
            self.reload_directories();
        }
    }

    fn ascend(&mut self) {
        let Some(parent) = self.current_dir.parent() else {
            return;
        };
        self.current_dir = parent.to_path_buf();
        self.input.clear();
        self.selected = 0;
        self.reload_directories();
    }

    fn selected_action(&self) -> Option<Action> {
        Some(Action::Create(self.selected_row()?.path().to_path_buf()))
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
        match code {
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::Right | KeyCode::Tab => self.descend(),
            KeyCode::Left => self.ascend(),
            KeyCode::Enter => return self.selected_action(),
            KeyCode::Backspace if self.input.is_empty() => self.ascend(),
            KeyCode::Backspace => {
                self.input.pop();
                self.normalize_selection();
            }
            KeyCode::Char(character)
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.input.push(character);
                self.normalize_selection();
            }
            _ => {}
        }
        None
    }
}

#[derive(Debug)]
struct Picker {
    rows: Vec<RoomRow>,
    selected: Option<String>,
    filter: String,
    notice: Option<String>,
    hit_rows: BTreeMap<u16, String>,
    dormant: Vec<KnownWorkspace>,
    view: View,
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
            dormant: Vec::new(),
            view: View::Rooms,
        }
    }

    fn apply_probe(&mut self, mut rows: Vec<RoomRow>, now: jiff::Timestamp) {
        rows.sort_by_cached_key(|row| {
            let recent = row
                .stats
                .as_ref()
                .and_then(|stats| stats.last_prompt_at)
                .filter(|at| now.duration_since(*at).as_secs() < PROMPT_RECENCY_WINDOW_SECS);
            (
                Reverse(recent),
                Reverse(row.room.updated_at),
                repo_name(&row.room),
                row.room.project_root.clone(),
            )
        });
        self.rows = rows;
        self.normalize_selection();
    }

    fn apply_dormant(&mut self, dormant: Vec<KnownWorkspace>) {
        self.dormant = dormant;
        if let View::NewSession(session) = &mut self.view {
            session.dormant = self.dormant.clone();
            session.normalize_selection();
        }
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
        Some(Action::Attach(
            row.room.session_name.clone(),
            repo_name(&row.room),
            row.room.mux,
        ))
    }

    fn open_new_session(&mut self) {
        let start = std::env::var_os("HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        self.view = View::NewSession(NewSession::new(start, self.dormant.clone()));
    }

    fn handle_event(&mut self, event: Event) -> Option<Action> {
        if let Event::Key(key) = &event
            && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'C'))
        {
            return Some(Action::Quit);
        }

        if matches!(self.view, View::NewSession(_)) {
            return match event {
                Event::Key(key)
                    if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                {
                    if key.code == KeyCode::Esc {
                        self.view = View::Rooms;
                        None
                    } else if let View::NewSession(session) = &mut self.view {
                        session.handle_key(key.code, key.modifiers)
                    } else {
                        None
                    }
                }
                Event::Mouse(mouse) => {
                    if let View::NewSession(session) = &mut self.view {
                        match mouse.kind {
                            MouseEventKind::ScrollUp => session.move_selection(-1),
                            MouseEventKind::ScrollDown => session.move_selection(1),
                            _ => {}
                        }
                    }
                    None
                }
                _ => None,
            };
        }

        match event {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
                    KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
                    KeyCode::Char('n')
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        self.open_new_session();
                    }
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

    fn money(&self) -> Style {
        self.style(self.palette.identity(Identity::Money))
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
    picker.hit_rows.clear();
    let panel_width = if area.width < PANEL_MIN_WIDTH {
        area.width
    } else {
        (area.width.saturating_mul(2) / 5)
            .clamp(PANEL_MIN_WIDTH, PANEL_MAX_WIDTH)
            .min(area.width)
    };
    let banner_width = BANNER
        .iter()
        .map(|line| UnicodeWidthStr::width(*line))
        .max()
        .unwrap_or_default();
    let banner_height = u16::try_from(BANNER.len()).unwrap_or_default();
    let banner_fits = usize::from(panel_width) >= banner_width
        && area.height >= banner_height + BANNER_GAP + MIN_BLOCK_HEIGHT + HELP_HEIGHT;
    if !banner_fits {
        render_picker_block(frame, picker, theme, area, true);
        return;
    }

    let max_block_height = area
        .height
        .saturating_sub(banner_height + BANNER_GAP + HELP_HEIGHT);
    let block_height = PANEL_TARGET_HEIGHT.clamp(MIN_BLOCK_HEIGHT, max_block_height);
    let stack_height = banner_height + BANNER_GAP + block_height + HELP_HEIGHT;
    let top = area.height.saturating_sub(stack_height) / 3;
    let panel_x = area.x + area.width.saturating_sub(panel_width) / 2;
    let banner_area = Rect::new(panel_x, area.y + top, panel_width, banner_height);
    let block_area = Rect::new(
        panel_x,
        banner_area.bottom() + BANNER_GAP,
        panel_width,
        block_height,
    );
    let help_area = Rect::new(panel_x, block_area.bottom(), panel_width, HELP_HEIGHT);

    let banner = BANNER
        .iter()
        .map(|line| banner_line(line, theme))
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(banner).alignment(Alignment::Center),
        banner_area,
    );
    render_picker_block(frame, picker, theme, block_area, false);
    frame.render_widget(
        Paragraph::new(help_text(picker))
            .style(theme.faint())
            .alignment(Alignment::Center),
        help_area,
    );
}

fn banner_line(line: &str, theme: &PickerTheme) -> Line<'static> {
    let mut spans = Vec::new();
    let mut run = String::new();
    let mut blocks = None;
    for character in line.chars() {
        let is_block = character == '█';
        if blocks.is_some_and(|current| current != is_block) {
            let style = if blocks == Some(true) {
                theme.accent()
            } else {
                theme.rule()
            };
            spans.push(Span::styled(std::mem::take(&mut run), style));
        }
        blocks = Some(is_block);
        run.push(character);
    }
    if !run.is_empty() {
        spans.push(Span::styled(
            run,
            if blocks == Some(true) {
                theme.accent()
            } else {
                theme.rule()
            },
        ));
    }
    Line::from(spans)
}

fn render_picker_block(
    frame: &mut Frame<'_>,
    picker: &mut Picker,
    theme: &PickerTheme,
    area: Rect,
    help_inside: bool,
) {
    let view_title = match &picker.view {
        View::Rooms => "sessions",
        View::NewSession(_) => "new session",
    };
    let title = if help_inside {
        Line::from(vec![
            Span::styled(" RimZ ", theme.accent().add_modifier(Modifier::BOLD)),
            Span::styled(format!("── {view_title} "), theme.muted()),
        ])
    } else {
        Line::from(Span::styled(format!(" {view_title} "), theme.muted()))
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.rule())
        .title(title);
    let mut inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    if inner.width > 2 {
        inner.x += 1;
        inner.width -= 2;
    }

    if let View::NewSession(session) = &mut picker.view {
        render_new_session(frame, session, theme, inner, help_inside);
        return;
    }

    let notice_height = u16::from(picker.notice.is_some());
    if let Some(notice) = picker.notice.as_deref() {
        frame.render_widget(
            Paragraph::new(crate::cli::render::clip_to_width(
                notice,
                usize::from(inner.width),
            ))
            .style(theme.alarm()),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
    }

    let controls_height = inner
        .height
        .saturating_sub(notice_height)
        .min(if help_inside { 2 } else { 1 });
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
    if help_inside && controls_height == 2 {
        frame.render_widget(
            Paragraph::new(help_text(picker)).style(theme.faint()),
            Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1),
        );
    }
}

fn help_text(picker: &Picker) -> &'static str {
    match &picker.view {
        View::Rooms => "↑↓ select · ⏎ attach · n new · type to filter · esc quit",
        View::NewSession(_) => "↑↓ select · →/tab open · ← back · ⏎ create · esc cancel",
    }
}

fn render_new_session(
    frame: &mut Frame<'_>,
    session: &mut NewSession,
    theme: &PickerTheme,
    inner: Rect,
    help_inside: bool,
) {
    let notice_height = u16::from(session.notice.is_some());
    if let Some(notice) = session.notice.as_deref() {
        frame.render_widget(
            Paragraph::new(crate::cli::render::clip_to_width(
                notice,
                usize::from(inner.width),
            ))
            .style(theme.alarm()),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
    }

    let path_y = inner.y.saturating_add(notice_height);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("path: ", theme.meta()),
            Span::styled(
                crate::cli::render::home_relative(&session.current_dir.to_string_lossy()),
                theme.body(),
            ),
        ])),
        Rect::new(inner.x, path_y, inner.width, 1),
    );

    let controls_height = if help_inside { 2 } else { 1 };
    let list_y = path_y.saturating_add(1);
    let list_height = inner
        .bottom()
        .saturating_sub(list_y)
        .saturating_sub(controls_height);
    render_new_session_rows(
        frame,
        session,
        theme,
        Rect::new(inner.x, list_y, inner.width, list_height),
    );

    let filter_y = inner.y.saturating_add(inner.height - controls_height);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("filter: ", theme.meta()),
            Span::styled(session.input.clone(), theme.body()),
            Span::styled("_", theme.accent()),
        ])),
        Rect::new(inner.x, filter_y, inner.width, 1),
    );
    if help_inside {
        frame.render_widget(
            Paragraph::new("↑↓ select · →/tab open · ← back · ⏎ create · esc cancel")
                .style(theme.faint()),
            Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1),
        );
    }
}

fn render_new_session_rows(
    frame: &mut Frame<'_>,
    session: &NewSession,
    theme: &PickerTheme,
    area: Rect,
) {
    if area.height == 0 {
        return;
    }
    let rows = session.rows();
    let mut items = Vec::new();
    let mut selected_item = None;
    let mut row_index = 0;
    for (title, recent) in [("recent", true), ("directories", false)] {
        items.push(ListItem::new(Line::from(Span::styled(
            title.to_owned(),
            theme.meta().add_modifier(Modifier::BOLD),
        ))));
        let start_len = items.len();
        for row in rows
            .iter()
            .filter(|row| matches!(row, NewSessionRow::Recent { .. }) == recent)
        {
            let selected = row_index == session.selected;
            if selected {
                selected_item = Some(items.len());
            }
            let prefix = if selected { "▸ " } else { "  " };
            let content = match row {
                NewSessionRow::Recent { name, path } => format!(
                    "{prefix}{name}  {}",
                    crate::cli::render::home_relative(&path.to_string_lossy())
                ),
                NewSessionRow::Current { path } => format!(
                    "{prefix}.  {}",
                    crate::cli::render::home_relative(&path.to_string_lossy())
                ),
                NewSessionRow::Directory { name, .. } => format!("{prefix}{name}/"),
            };
            let mut item = ListItem::new(crate::cli::render::clip_to_width(
                &content,
                usize::from(area.width),
            ));
            if selected {
                item = item.style(theme.selected());
            }
            items.push(item);
            row_index += 1;
        }
        if items.len() == start_len {
            items.push(ListItem::new(Span::styled("  (none)", theme.muted())));
        }
    }

    let capacity = usize::from(area.height);
    let selected_item = selected_item.unwrap_or(0);
    let start = selected_item
        .saturating_add(1)
        .saturating_sub(capacity)
        .min(items.len().saturating_sub(capacity));
    let shown = items
        .into_iter()
        .skip(start)
        .take(capacity)
        .collect::<Vec<_>>();
    frame.render_widget(List::new(shown).style(theme.body()), area);
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
    let capacity = (usize::from(area.height).saturating_add(1) / CARD_HEIGHT).max(1);
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
        (
            crate::cli::render::clip_to_width(&repo, available),
            0,
            String::new(),
        )
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
        spans.push(Span::styled(theme.sessions_glyph.clone(), theme.accent()));
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
        theme.money(),
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

fn repo_name(room: &LiveRoom) -> String {
    repo_display_name(&room.project_root).unwrap_or_else(|| room.session_name.clone())
}

fn repo_display_name(project_root: &Path) -> Option<String> {
    rimz::worktree::normalize_path_lexical(project_root)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

pub(super) fn session_display_name(session: &str) -> String {
    rimz::room::session::workspace_record_for_session(session)
        .ok()
        .flatten()
        .and_then(|record| repo_display_name(&record.project_root))
        .unwrap_or_else(|| session.to_owned())
}

fn room_path(room: &LiveRoom) -> String {
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
                crate::cli::render::clip_to_width(span.content.as_ref(), remaining),
                span.style,
            ));
        }
        break;
    }
    truncated
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
