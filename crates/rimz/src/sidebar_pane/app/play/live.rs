//! Main-screen columns and playback controls; every seek rebuilds and replays the column.

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::text::Line;

use crate::sidebar_pane::pixel::{BEGIN_SYNC, END_SYNC, PixelLease, probe};
use crate::tui::{MouseCapture, Screen, TerminalModeGuard};

use super::*;

struct PaintBatch;

impl PaintBatch {
    fn begin() -> io::Result<Self> {
        io::stdout().write_all(BEGIN_SYNC)?;
        Ok(Self)
    }
}

impl Drop for PaintBatch {
    fn drop(&mut self) {
        let _ = io::stdout()
            .write_all(END_SYNC)
            .and_then(|()| io::stdout().flush());
    }
}

struct LiveColumn {
    scenario: Scenario,
    column: Column,
    source: Option<String>,
    error: Option<String>,
    _pixel_lease: Option<PixelLease>,
    warned: bool,
    pass: u128,
}

impl Drop for LiveColumn {
    fn drop(&mut self) {
        self.column.state.clear_pixel(&mut self.column.terminal);
    }
}

impl LiveColumn {
    fn advance(&mut self, at: Duration, height: u16) -> Result<()> {
        let local = local_time(at, self.scenario.length);
        if self.pass != at.as_nanos() / self.scenario.length.as_nanos()
            || self.column.at > local
            || self.column.terminal.size()?.height != height
        {
            self.seek(at, height)?;
        } else {
            self.column.run_until(&self.scenario, local, |_| Ok(()))?;
        }
        Ok(())
    }

    fn reload(&mut self, path: &Path, at: Duration, height: u16) -> Result<()> {
        match std::fs::read_to_string(path) {
            Ok(source) if self.source.as_ref() != Some(&source) => {
                let name = path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or("scenario");
                match Scenario::parse(name, &source) {
                    Ok(scenario) => {
                        self.scenario = scenario;
                        self.error = None;
                        self.seek(at, height)?;
                    }
                    Err(error) => self.error = Some(format!("{}: {error}", path.display())),
                }
                self.source = Some(source);
            }
            Err(error) => {
                self.source = None;
                self.error = Some(format!("{}: {error}", path.display()));
            }
            _ => {}
        }
        Ok(())
    }

    fn seek(&mut self, at: Duration, height: u16) -> Result<()> {
        self.column.motion = Some(Duration::ZERO..self.scenario.length);
        if self.column.terminal.size()?.height != height {
            self.column = Column::build(
                &self.scenario,
                Some(height),
                self.column.motion.clone(),
                self.column.surface,
            )?;
        }
        self.column
            .seek(&self.scenario, local_time(at, self.scenario.length))?;
        self.pass = at.as_nanos() / self.scenario.length.as_nanos();
        Ok(())
    }
}

fn local_time(at: Duration, length: Duration) -> Duration {
    let nanos = at.as_nanos() % length.as_nanos();
    Duration::new(
        (nanos / 1_000_000_000) as u64,
        (nanos % 1_000_000_000) as u32,
    )
}

fn required_width(widths: impl Iterator<Item = u16>) -> u32 {
    widths
        .enumerate()
        .map(|(index, width)| u32::from(width) + u32::from(index > 0))
        .sum()
}

pub(super) fn serve(files: &[PathBuf], scenarios: Vec<(Scenario, String)>) -> Result<()> {
    let (width, height) = ratatui::crossterm::terminal::size()?;
    let required = required_width(scenarios.iter().map(|(scenario, _)| scenario.width));
    if required > u32::from(width) {
        return Err(PlayError::Invalid(format!(
            "scenario columns need {required} columns; terminal has {width}"
        )));
    }
    let body_height = height.saturating_sub(1).max(1);
    let caps = probe::player_caps();
    let runtime = RuntimePaths::shared();
    let mut columns = scenarios
        .into_iter()
        .map(|(scenario, source)| {
            let pixel_lease = PixelLease::acquire(&runtime).ok().flatten();
            let surface = Surface::Live {
                caps,
                slot: pixel_lease.as_ref().map(|lease| lease.slot),
            };
            let column = Column::build(
                &scenario,
                Some(body_height),
                Some(Duration::ZERO..scenario.length),
                surface,
            )?;
            Ok(LiveColumn {
                scenario,
                column,
                source: Some(source),
                error: None,
                _pixel_lease: pixel_lease,
                warned: false,
                pass: 0,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    // Asset preparation can outlast a resize; check again before terminal writes.
    let (width, _) = ratatui::crossterm::terminal::size()?;
    if required > u32::from(width) {
        return Err(PlayError::Invalid(format!(
            "scenario columns need {required} columns; terminal has {width}"
        )));
    }
    let _mode = TerminalModeGuard::enable(MouseCapture::Stdout, Screen::Main)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    let mut controls = Controls {
        at: Duration::ZERO,
        paused: false,
        show_status: true,
        active: 0,
    };
    let mut previous = Instant::now();
    let mut reload_at = previous;
    loop {
        let now = Instant::now();
        if !controls.paused {
            controls.at += now.duration_since(previous);
        }
        previous = now;
        let size = terminal.size()?;
        let body_height = size.height.saturating_sub(1).max(1);
        let batch = PaintBatch::begin()?;
        for (index, entry) in columns.iter_mut().enumerate() {
            entry.advance(controls.at, body_height)?;
            if now >= reload_at {
                entry.reload(&files[index], controls.at, body_height)?;
            }
            if !entry.warned && !entry.column.pet_ready {
                entry.column.warn_pet(&entry.scenario)?;
                entry.warned = true;
            }
        }
        if now >= reload_at {
            reload_at = now + Duration::from_millis(200);
        }
        paint(&mut terminal, &mut columns, &controls, body_height)?;
        drop(batch);
        if event::poll(Duration::from_millis(16))?
            && controls.input(&mut columns, event::read()?, body_height)?
        {
            break;
        }
    }
    Ok(())
}

fn paint(
    terminal: &mut Terminal<impl ratatui::backend::Backend<Error = io::Error>>,
    columns: &mut [LiveColumn],
    controls: &Controls,
    body_height: u16,
) -> Result<()> {
    let required = required_width(columns.iter().map(|entry| entry.scenario.width));
    terminal.draw(|frame| {
        let area = frame.area();
        let mut x = 0u16;
        for (index, entry) in columns.iter().enumerate() {
            let buffer = entry.column.terminal.backend().buffer();
            for y in 0..body_height.min(area.height) {
                for col in 0..entry.scenario.width.min(area.width.saturating_sub(x)) {
                    frame.buffer_mut()[(x + col, y)] = buffer[(col, y)].clone();
                }
            }
            x = x.saturating_add(entry.scenario.width);
            if index + 1 < columns.len() && x < area.width {
                for y in 0..body_height.min(area.height) {
                    frame.buffer_mut()[(x, y)].set_symbol("│");
                }
                x = x.saturating_add(1);
            }
        }
        let mut line = Line::raw(controls.status(columns, area.width, required));
        if columns.iter().any(|entry| entry.error.is_some()) {
            let entry = &mut columns[0];
            line = line.style(
                entry
                    .column
                    .state
                    .ui
                    .theme(&entry.column.state.view.current.theme)
                    .alarm(ratatui::style::Modifier::empty()),
            );
        }
        frame.render_widget(
            line,
            ratatui::layout::Rect::new(0, area.height.saturating_sub(1), area.width, 1),
        );
    })?;
    Ok(())
}

struct Controls {
    at: Duration,
    paused: bool,
    show_status: bool,
    active: usize,
}

impl Controls {
    fn status(&self, columns: &[LiveColumn], width: u16, required: u32) -> String {
        let errors = columns
            .iter()
            .filter_map(|entry| entry.error.as_deref())
            .collect::<Vec<_>>()
            .join(" | ");
        if !errors.is_empty() {
            return errors;
        }
        if !self.show_status {
            return String::new();
        }
        let mut text = columns
            .iter()
            .map(|entry| {
                format!(
                    "{} {:.1}s {}",
                    entry.scenario.name,
                    local_time(self.at, entry.scenario.length).as_secs_f64(),
                    entry.column.label
                )
            })
            .collect::<Vec<_>>()
            .join(" │ ");
        if self.paused {
            text.insert_str(0, "paused │ ");
        }
        if required > u32::from(width) {
            text.insert_str(0, "rightmost columns clipped │ ");
        }
        text
    }

    fn input(
        &mut self,
        columns: &mut [LiveColumn],
        event: Event,
        body_height: u16,
    ) -> Result<bool> {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => return Ok(true),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(true);
                }
                KeyCode::Char(' ') => self.paused = !self.paused,
                KeyCode::Char('h') => self.show_status = !self.show_status,
                KeyCode::Char('r') => {
                    self.at = Duration::ZERO;
                    for entry in columns {
                        entry.seek(self.at, body_height)?;
                    }
                }
                KeyCode::Left | KeyCode::Right => {
                    self.paused = true;
                    let mut beats = Vec::new();
                    for entry in columns.iter() {
                        let base = self.at - local_time(self.at, entry.scenario.length);
                        for beat in &entry.scenario.beats {
                            beats.push(base + beat.at);
                            beats.push(base + entry.scenario.length + beat.at);
                            if base >= entry.scenario.length {
                                beats.push(base - entry.scenario.length + beat.at);
                            }
                        }
                    }
                    self.at = if key.code == KeyCode::Left {
                        beats
                            .into_iter()
                            .filter(|beat| *beat < self.at)
                            .max()
                            .unwrap_or_default()
                    } else {
                        beats
                            .into_iter()
                            .filter(|beat| *beat > self.at)
                            .min()
                            .unwrap_or(self.at)
                    };
                    for entry in columns {
                        entry.seek(self.at, body_height)?;
                    }
                }
                _ => columns[self.active].column.input(Wakeup::Press {
                    code: key.code,
                    mods: key.modifiers,
                })?,
            },
            Event::Mouse(mouse) => {
                let mut x = 0u16;
                for (index, entry) in columns.iter_mut().enumerate() {
                    if (x..x.saturating_add(entry.scenario.width)).contains(&mouse.column) {
                        self.active = index;
                        if mouse.row < body_height
                            && let Some(wire) = super::super::input::encode_mouse(
                                mouse.kind,
                                mouse.column - x,
                                mouse.row,
                            )
                        {
                            entry
                                .column
                                .input(super::super::input::decode_wakeup(wire.as_bytes()))?;
                        }
                        break;
                    }
                    x = x.saturating_add(entry.scenario.width).saturating_add(1);
                }
            }
            _ => {}
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stall_past_a_whole_pass_rebuilds_even_at_a_later_local_time() {
        let scenario = Scenario::parse(
            "wrap",
            "scenario wrap length 3s\nagent @a claude\n0s @a runs \"work\"",
        )
        .unwrap();
        let column = Column::new(&scenario, None, None).unwrap();
        let mut live = LiveColumn {
            scenario,
            column,
            source: None,
            error: None,
            _pixel_lease: None,
            warned: false,
            pass: 0,
        };
        live.advance(Duration::from_secs(1), 34).unwrap();
        live.column.state.ui.help_visible = true;
        live.advance(Duration::from_secs(5), 34).unwrap();
        assert!(
            !live.column.state.ui.help_visible,
            "wraps reset UI even when local time increases"
        );
        assert_eq!(live.column.at, Duration::from_secs(2));
    }

    #[test]
    fn divider_clips_to_a_frame_shrunk_since_the_size_read() {
        let mut columns = (0..2)
            .map(|_| {
                let scenario =
                    Scenario::parse("small", "scenario small width 10 height 8").unwrap();
                let mut column = Column::new(&scenario, None, None).unwrap();
                column
                    .run_until(&scenario, Duration::ZERO, |_| Ok(()))
                    .unwrap();
                LiveColumn {
                    scenario,
                    column,
                    source: None,
                    error: None,
                    _pixel_lease: None,
                    warned: false,
                    pass: 0,
                }
            })
            .collect::<Vec<_>>();
        let controls = Controls {
            at: Duration::ZERO,
            paused: false,
            show_status: true,
            active: 0,
        };
        let mut terminal = Terminal::new(PaneBackend::captured(21, 3)).unwrap();
        paint(&mut terminal, &mut columns, &controls, 8).unwrap();
        assert_eq!(terminal.backend().buffer()[(10, 0)].symbol(), "│");
    }
}
