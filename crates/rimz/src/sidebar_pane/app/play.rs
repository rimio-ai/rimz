//! Scenario playback through the attachment loop, with disposable per-pass runtime state.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use ratatui::Terminal;
use serde::Serialize;
use unicode_width::UnicodeWidthStr;

use crate::sidebar::scenario::{Action, Scenario, ScenarioError, UiAction};
use crate::sidebar::unread::reconcile_snapshot;
use crate::sidebar_pane::pixel::PixelRenderCaps;
use crate::sidebar_pane::pixel::PixelSlot;
use crate::sidebar_pane::render::SectionLabel;
use crate::{RuntimePaths, SidebarInstanceId};

use super::clock::LoopClock;
use super::fetch::{FetchDispatcher, FetchPhase, FetchRequest, FetchUpdate, SnapshotSource};
use super::inbox::WakeupInbox;
use super::input::{KeyAction, Wakeup};
use super::loop_state::LoopState;
use super::{PaneBackend, ServeConfig};

mod live;
#[cfg(test)]
mod tests;

#[derive(Debug, Default)]
pub struct PlayOptions {
    pub at: Option<Duration>,
    pub frames: Option<PathBuf>,
    pub motion: Option<std::ops::Range<Duration>>,
    pub ansi: bool,
    pub section: Option<String>,
    pub height: Option<u16>,
    pub width: Option<u16>,
    pub theme_mode: Option<crate::config::ThemeMode>,
    pub theme_scheme: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum PlayError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Scenario(#[from] ScenarioError),
    #[error(transparent)]
    Loop(#[from] super::SidebarAppErr),
    #[error("{0}")]
    Invalid(String),
}

type Result<T> = std::result::Result<T, PlayError>;

fn load(path: &Path) -> Result<(Scenario, String)> {
    let source = std::fs::read_to_string(path)?;
    let name = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("scenario");
    Ok((Scenario::parse(name, &source)?, source))
}

pub fn play(files: &[PathBuf], options: PlayOptions) -> Result<()> {
    if options.at.is_none() && options.frames.is_none() && !io::stdout().is_terminal() {
        return Err(PlayError::Invalid(
            "sidebar play needs a terminal; use --frames <dir> for headless playback".into(),
        ));
    }
    let scenarios = files
        .iter()
        .map(|path| {
            let (mut scenario, source) = load(path)?;
            scenario.configure_rendering(
                options.width,
                options.theme_mode,
                options.theme_scheme.as_deref(),
            );
            Ok((scenario, source))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut names = BTreeSet::new();
    for (scenario, _) in &scenarios {
        let mut components = Path::new(&scenario.name).components();
        let directory_name = matches!(components.next(), Some(std::path::Component::Normal(_)))
            && components.next().is_none();
        if options.frames.is_some() && (!names.insert(&scenario.name) || !directory_name) {
            return Err(PlayError::Invalid(format!(
                "scenario name {:?} must be unique and a single directory name",
                scenario.name
            )));
        }
    }
    if let Some(at) = options.at {
        let mut out = io::stdout().lock();
        for (scenario, _) in &scenarios {
            let mut column = Column::new(scenario, options.height, options.motion.clone())?;
            column.run_until(scenario, at, |_| Ok(()))?;
            column.warn_pet(scenario)?;
            let frame = column.capture()?;
            if options.ansi {
                frame.write_ansi(&mut out, options.section.as_deref())?;
            } else {
                writeln!(out, "== {} @ {}s ==", scenario.name, at.as_secs_f64())?;
                out.write_all(frame.text(options.section.as_deref()).as_bytes())?;
            }
        }
        return Ok(());
    }
    if let Some(root) = &options.frames {
        for (scenario, _) in &scenarios {
            let dir = root.join(&scenario.name);
            std::fs::create_dir_all(&dir)?;
            let mut writer = Filmstrip::new(&dir)?;
            let mut column = Column::new(scenario, options.height, options.motion.clone())?;
            column.warn_pet(scenario)?;
            column.run_until(
                scenario,
                scenario.length.saturating_sub(Duration::from_millis(1)),
                |frame| writer.push(frame),
            )?;
        }
        return Ok(());
    }
    live::serve(files, scenarios)
}

struct RuntimeDir(PathBuf);

impl RuntimeDir {
    fn new() -> io::Result<Self> {
        let path = std::env::temp_dir().join(format!("rzplay-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
}

impl Drop for RuntimeDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Column {
    _dir: RuntimeDir,
    runtime: RuntimePaths,
    clock: LoopClock,
    state: LoopState,
    terminal: Terminal<PaneBackend>,
    requests: Receiver<FetchRequest>,
    fetch: FetchDispatcher,
    inbox: WakeupInbox,
    at: Duration,
    next: Duration,
    next_tick: Duration,
    next_beat: usize,
    motion: Option<std::ops::Range<Duration>>,
    label: String,
    pending_key: bool,
    pending_ui: Vec<usize>,
    surface: Surface,
    pet_ready: bool,
}

#[derive(Clone, Copy)]
enum Surface {
    Text,
    Live {
        caps: PixelRenderCaps,
        slot: Option<PixelSlot>,
    },
}

impl Column {
    fn seek(&mut self, scenario: &Scenario, at: Duration) -> Result<()> {
        *self = Self::build(
            scenario,
            Some(self.terminal.size()?.height),
            self.motion.clone(),
            self.surface,
        )?;
        self.run_until(scenario, at, |_| Ok(()))
    }

    fn warn_pet(&self, scenario: &Scenario) -> Result<()> {
        if !self.pet_ready {
            writeln!(
                io::stderr(),
                "pet {} unavailable; playing without its body",
                scenario.at(Duration::ZERO).theme.pets.pet
            )?;
        }
        Ok(())
    }

    fn new(
        scenario: &Scenario,
        height: Option<u16>,
        motion: Option<std::ops::Range<Duration>>,
    ) -> Result<Self> {
        Self::build(scenario, height, motion, Surface::Text)
    }

    fn build(
        scenario: &Scenario,
        height: Option<u16>,
        motion: Option<std::ops::Range<Duration>>,
        surface: Surface,
    ) -> Result<Self> {
        let dir = RuntimeDir::new()?;
        let mut snapshot = scenario.at(Duration::ZERO);
        if matches!(surface, Surface::Text) {
            snapshot.theme.display.pixel = crate::config::PixelMode::Off;
        }
        let runtime = RuntimePaths::under(snapshot.workspace_id.clone(), &dir.0)
            .map_err(super::SidebarAppErr::from)?;
        let clock = LoopClock::play(snapshot.now);
        let (tx, requests) = mpsc::channel();
        let height = height.unwrap_or(scenario.height);
        let config = ServeConfig {
            workspace_id: snapshot.workspace_id.clone(),
            mux: crate::MuxName::Zellij,
            session_name: "scenario".into(),
            instance_id: SidebarInstanceId::new(),
            tick_seconds: 1,
            refresh_ms_override: None,
            timezone: jiff::tz::TimeZone::UTC,
            notification_prefs: Default::default(),
            own_pane: None,
        };
        let (caps, slot) = match surface {
            Surface::Text => (PixelRenderCaps::default(), None),
            Surface::Live { caps, slot } => (caps, slot),
        };
        let mut state = LoopState::new(
            clock.clone(),
            config,
            None,
            runtime.clone(),
            runtime.sidebar_host_wake_socket_path(crate::MuxName::Zellij, "scenario"),
            crate::diag::DiagSink::disabled(),
            Default::default(),
            Some(scenario.width),
            mpsc::sync_channel(64).0,
            caps,
            slot,
        );
        let pet_ready = state.prepare_play_pet(&snapshot);
        let mut backend = PaneBackend::captured(scenario.width, height);
        if matches!(surface, Surface::Live { .. }) {
            backend.play_output(io::stdout());
        }
        let terminal = Terminal::new(backend)?;
        Ok(Self {
            _dir: dir,
            runtime,
            clock,
            state,
            terminal,
            requests,
            fetch: FetchDispatcher::new(tx),
            inbox: WakeupInbox::new(mpsc::channel().1),
            at: Duration::ZERO,
            next: Duration::ZERO,
            next_tick: Duration::ZERO,
            next_beat: 0,
            motion,
            label: "opening".into(),
            pending_key: true,
            pending_ui: Vec::new(),
            surface,
            pet_ready,
        })
    }

    fn send(&mut self, scenario: &Scenario) {
        let mut snapshot = scenario.at(self.at);
        if matches!(self.surface, Surface::Text) {
            snapshot.theme.display.pixel = crate::config::PixelMode::Off;
        }
        reconcile_snapshot(&self.runtime, &mut snapshot);
        let mut views = self
            .state
            .views
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        views.publish(FetchUpdate::Snapshot {
            snapshot: Box::new(snapshot),
            phase: FetchPhase::Final,
            source: SnapshotSource::Produced,
        });
        self.state.completion.store(true, Ordering::Relaxed);
    }

    fn advance(&mut self, scenario: &Scenario) -> Result<bool> {
        let frozen = self.state.inspect_view(|ui, _| ui.freeze.is_some());
        let (mut painted, mut deadline) =
            self.state
                .advance(&mut self.inbox, &mut self.fetch, &mut self.terminal)?;
        if self.requests.try_iter().count() > 0 {
            self.send(scenario);
            let result =
                self.state
                    .advance(&mut self.inbox, &mut self.fetch, &mut self.terminal)?;
            painted |= result.0;
            deadline = result.1;
        }
        self.pending_key |= frozen && self.state.inspect_view(|ui, _| ui.freeze.is_none());
        self.next = Duration::from_millis(self.at.as_millis() as u64)
            + deadline
                .saturating_duration_since(self.clock.now())
                .max(Duration::from_millis(1));
        Ok(painted)
    }

    fn run_until(
        &mut self,
        scenario: &Scenario,
        target: Duration,
        mut emit: impl FnMut(PaintedFrame) -> Result<()>,
    ) -> Result<()> {
        loop {
            let next_beat = scenario
                .beats
                .get(self.next_beat)
                .map_or(target, |beat| beat.at);
            let next = self.next.min(self.next_tick).min(next_beat).min(target);
            self.clock.advance(Duration::from_millis(
                (next.as_millis() - self.at.as_millis()) as u64,
            ));
            self.at = next;
            self.state.presentation.cosmetic_phase = (!self
                .motion
                .as_ref()
                .is_some_and(|range| range.contains(&self.at)))
            .then_some(0);
            let mut changed = self.at == self.next_tick;
            if changed {
                self.next_tick += Duration::from_secs(1);
            }
            let first = self.next_beat;
            while scenario
                .beats
                .get(self.next_beat)
                .is_some_and(|beat| beat.at <= self.at)
            {
                self.label.clone_from(&scenario.beats[self.next_beat].label);
                self.next_beat += 1;
                self.pending_key = true;
                changed = true;
            }
            self.pending_ui.extend(
                (first..self.next_beat)
                    .filter(|index| matches!(scenario.beats[*index].action, Action::Ui(_))),
            );
            if changed {
                self.send(scenario);
            }
            let mut painted = self.advance(scenario)?;
            if !painted && !self.pending_ui.is_empty() {
                // Clock-only delivery can leave no paint due; input still needs fresh geometry.
                self.input(Wakeup::Resize)?;
                painted = true;
            }
            if painted {
                if !self.pending_ui.is_empty() {
                    for index in std::mem::take(&mut self.pending_ui) {
                        let Action::Ui(action) = &scenario.beats[index].action else {
                            continue;
                        };
                        self.ui_action(action)?;
                    }
                    self.advance(scenario)?;
                }
                let frame = self.capture()?;
                let key = frame.key;
                emit(frame)?;
                self.pending_key &= !key;
            }
            if self.at >= target {
                return Ok(());
            }
        }
    }

    fn input(&mut self, wakeup: Wakeup) -> Result<()> {
        self.state
            .on_input(wakeup, &mut self.terminal, &mut self.fetch)?;
        Ok(())
    }

    fn set_delegation(&mut self, row: String, open: bool) -> Result<bool> {
        let target = crate::sidebar_pane::render::HitTarget::ToggleDelegation { row, open };
        let Some((column, row)) = self.state.ui.interactions.line_for_target(&target) else {
            return Ok(false);
        };
        self.input(Wakeup::MouseClick { column, row })?;
        Ok(true)
    }

    fn ui_action(&mut self, action: &UiAction) -> Result<()> {
        match action {
            UiAction::Key(key) => {
                let (mods, key) = key
                    .strip_prefix("ctrl-")
                    .map_or(("-", key.as_str()), |key| ("c", key));
                let code = if key == "space" {
                    "char: ".into()
                } else if key.chars().count() == 1 {
                    format!("char:{key}")
                } else {
                    key.to_owned()
                };
                self.input(super::input::decode_wakeup(
                    format!("press:{mods}:{code}").as_bytes(),
                ))
            }
            UiAction::Scroll { up, count } => {
                for _ in 0..*count {
                    self.input(Wakeup::Scroll { down: !up })?;
                }
                Ok(())
            }
            UiAction::Select(handle)
            | UiAction::Expand(handle)
            | UiAction::Collapse(handle)
            | UiAction::Click(handle) => {
                let target = self.state.inspect_view(|ui, view| {
                    let roster = ui.visible_roster(&view.current);
                    let ordinal = roster.rows().iter().position(|row| {
                        row.as_agent()
                            .is_some_and(|card| card.handle.as_deref() == Some(handle))
                    })?;
                    Some((
                        ordinal,
                        ui.selected_index == Some(ordinal),
                        roster.rows()[ordinal].id.clone(),
                        roster.len(),
                    ))
                });
                let Some((ordinal, selected, row, count)) = target else {
                    self.label = format!("{}: @{handle} not on screen", self.label);
                    return Ok(());
                };
                let line = self.state.ui.interactions.line_for_row(ordinal);
                let Some(line) = line else {
                    self.label = format!("{}: @{handle} not on screen", self.label);
                    return Ok(());
                };
                if matches!(action, UiAction::Click(_)) {
                    return self.input(Wakeup::MouseClick {
                        column: 2,
                        row: line as u16,
                    });
                }
                if matches!(action, UiAction::Collapse(_)) {
                    if self.set_delegation(row, false)? {
                        return Ok(());
                    }
                    if selected {
                        if count > 1 {
                            self.input(Wakeup::Key(if ordinal == 0 {
                                KeyAction::Down
                            } else {
                                KeyAction::Up
                            }))?;
                        } else {
                            self.label =
                                format!("{}: @{handle} is the only selectable row", self.label);
                        }
                    }
                    return Ok(());
                }
                if !selected {
                    self.input(Wakeup::Key(KeyAction::Top))?;
                    for _ in 0..ordinal {
                        self.input(Wakeup::Key(KeyAction::Down))?;
                    }
                }
                if matches!(action, UiAction::Expand(_)) {
                    self.set_delegation(row, true)?;
                }
                Ok(())
            }
        }
    }

    fn capture(&self) -> Result<PaintedFrame> {
        let mut rows = Vec::new();
        for row in self.state.ui.frame_rows(self.terminal.backend().buffer()) {
            let mut ansi = Vec::new();
            row.write_ansi(&mut ansi)?;
            rows.push(PaintedRow {
                label: row.label.clone(),
                plain: row.plain_text(),
                ansi,
            });
        }
        Ok(PaintedFrame {
            t_ms: self.at.as_millis() as u64,
            beat: self.next_beat.saturating_sub(1),
            label: self.label.clone(),
            key: self.pending_key && self.state.play_frame_settled(),
            motion: self
                .motion
                .as_ref()
                .is_some_and(|range| range.contains(&self.at)),
            rows,
        })
    }
}

struct PaintedFrame {
    t_ms: u64,
    beat: usize,
    label: String,
    key: bool,
    motion: bool,
    rows: Vec<PaintedRow>,
}

struct PaintedRow {
    label: SectionLabel,
    plain: String,
    ansi: Vec<u8>,
}

impl PaintedFrame {
    fn text(&self, section: Option<&str>) -> String {
        let width = self
            .rows
            .iter()
            .map(|row| row.label.as_str().width())
            .max()
            .unwrap_or(0);
        self.rows
            .iter()
            .filter(|row| section.is_none_or(|section| row.label.matches(section)))
            .map(|row| {
                format!(
                    "{}{} │ {}\n",
                    row.label.as_str(),
                    " ".repeat(width - row.label.as_str().width()),
                    row.plain
                )
            })
            .collect()
    }

    fn write_ansi(&self, writer: &mut impl Write, section: Option<&str>) -> io::Result<()> {
        for row in self
            .rows
            .iter()
            .filter(|row| section.is_none_or(|section| row.label.matches(section)))
        {
            writer.write_all(&row.ansi)?;
        }
        Ok(())
    }

    fn sections(&self) -> BTreeMap<&str, Vec<(usize, &str)>> {
        let mut sections = BTreeMap::<_, Vec<_>>::new();
        for (index, row) in self.rows.iter().enumerate() {
            sections
                .entry(row.label.as_str())
                .or_default()
                .push((index, row.plain.as_str()));
        }
        sections
    }
}

struct Filmstrip<'a> {
    dir: &'a Path,
    index: std::fs::File,
    number: usize,
    previous: Option<PaintedFrame>,
    pending_key: bool,
}

impl<'a> Filmstrip<'a> {
    fn new(dir: &'a Path) -> Result<Self> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path
                .extension()
                .is_some_and(|ext| ext == "txt" || ext == "ansi" || ext == "png")
                && path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .is_some_and(|stem| {
                        stem.len() >= 4 && stem.bytes().all(|byte| byte.is_ascii_digit())
                    })
            {
                std::fs::remove_file(path)?;
            }
        }
        Ok(Self {
            dir,
            index: std::fs::File::create(dir.join("index.jsonl"))?,
            number: 0,
            previous: None,
            pending_key: false,
        })
    }

    fn push(&mut self, frame: PaintedFrame) -> Result<()> {
        self.pending_key |= frame.key;
        let current = frame.sections();
        let previous = self
            .previous
            .as_ref()
            .map(PaintedFrame::sections)
            .unwrap_or_default();
        let changed: Vec<_> = current
            .keys()
            .chain(previous.keys())
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|label| current.get(label) != previous.get(label))
            .collect();
        if changed.is_empty() && !frame.motion && !frame.key {
            return Ok(());
        }
        let key = self.pending_key;
        self.pending_key = false;
        let text = format!("{:04}.txt", self.number);
        let ansi = key.then(|| format!("{:04}.ansi", self.number));
        std::fs::write(self.dir.join(&text), frame.text(None))?;
        if let Some(ansi) = &ansi {
            frame.write_ansi(&mut std::fs::File::create(self.dir.join(ansi))?, None)?;
        }
        #[derive(Serialize)]
        struct Entry<'a> {
            frame: usize,
            t_ms: u64,
            beat: usize,
            label: &'a str,
            changed: Vec<&'a str>,
            key: bool,
            text: String,
            ansi: Option<String>,
        }
        let entry = Entry {
            frame: self.number,
            t_ms: frame.t_ms,
            beat: frame.beat,
            label: &frame.label,
            changed,
            key,
            text,
            ansi,
        };
        serde_json::to_writer(&mut self.index, &entry).map_err(io::Error::other)?;
        writeln!(self.index)?;
        self.number += 1;
        self.previous = Some(frame);
        Ok(())
    }
}
