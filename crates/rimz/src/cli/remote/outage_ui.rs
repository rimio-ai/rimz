//! Alternate-screen recovery panel and plain-line fallback.

use std::io::{self, IsTerminal, Write};
use std::time::Duration;

use ratatui::crossterm::cursor::MoveTo;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::crossterm::style::{
    Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor,
};
use ratatui::crossterm::terminal::{self, Clear, ClearType};
use ratatui::crossterm::{execute, queue};
use rimz::remote::recovery::{RecoveryFrame, RecoveryPanel, StageStatus};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::cli::spinner::{SPINNER_FRAMES, SPINNER_TICK, animation_allowed};
use rimz::tui::{MouseCapture, Screen, TerminalModeGuard, no_color};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UiEvent {
    Continue,
    Interrupted,
}

pub(super) struct OutageUi {
    host: String,
    state: UiState,
}

enum UiState {
    PendingPanel,
    Panel(OutagePanel),
    PlainLines,
}

impl OutageUi {
    pub(super) fn auto(host: impl Into<String>) -> Self {
        let panel = panel_allowed(
            std::io::stdout().is_terminal(),
            std::env::var("RIMZ_NO_PROGRESS").ok().as_deref(),
            std::env::var(rimz::harness::run::ENV_AGENT_KIND)
                .ok()
                .as_deref(),
            std::env::var("TERM").ok().as_deref(),
        );
        Self {
            host: host.into(),
            state: if panel {
                UiState::PendingPanel
            } else {
                UiState::PlainLines
            },
        }
    }

    pub(super) fn plain_lines(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            state: UiState::PlainLines,
        }
    }

    pub(super) fn host(&self) -> &str {
        &self.host
    }

    pub(super) fn is_plain(&self) -> bool {
        matches!(self.state, UiState::PlainLines)
    }

    pub(super) fn report_unreachable(&self) {
        if self.is_plain() {
            let _ = writeln!(
                std::io::stderr().lock(),
                "rimz: {} unreachable — holding reconnect until the network returns; Ctrl-C stops",
                self.host,
            );
        }
    }

    pub(super) fn tick(
        &mut self,
        recovery: &mut RecoveryPanel,
        elapsed: Duration,
    ) -> io::Result<UiEvent> {
        if matches!(self.state, UiState::PlainLines) || !recovery.visible(elapsed) {
            return Ok(UiEvent::Continue);
        }
        if matches!(self.state, UiState::PendingPanel) {
            match OutagePanel::new() {
                Ok(panel) => {
                    recovery.note_shown(elapsed);
                    self.state = UiState::Panel(panel);
                }
                Err(err) => {
                    tracing::debug!(error = %err, "remote recovery panel unavailable");
                    self.state = UiState::PlainLines;
                    return Ok(UiEvent::Continue);
                }
            }
        }
        let UiState::Panel(panel) = &mut self.state else {
            return Ok(UiEvent::Continue);
        };
        panel.draw(&recovery.frame())?;
        panel.poll_interrupt()
    }

    pub(super) fn release(&mut self, establishing: bool) -> io::Result<()> {
        let UiState::Panel(panel) = std::mem::replace(&mut self.state, UiState::PlainLines) else {
            return Ok(());
        };
        panel.release(&self.host, establishing)
    }
}

fn panel_allowed(
    stdout_is_terminal: bool,
    no_progress: Option<&str>,
    agent_kind: Option<&str>,
    term: Option<&str>,
) -> bool {
    stdout_is_terminal && animation_allowed(no_progress, agent_kind, term)
}

struct OutagePanel {
    guard: Option<TerminalModeGuard>,
    frame_index: usize,
}

impl OutagePanel {
    fn new() -> io::Result<Self> {
        Ok(Self {
            guard: Some(TerminalModeGuard::enable(
                MouseCapture::Off,
                Screen::Alternate,
            )?),
            frame_index: 0,
        })
    }

    fn draw(&mut self, frame: &RecoveryFrame) -> io::Result<()> {
        let (width, height) = terminal::size()?;
        let rows = display_rows(frame, self.frame_index);
        self.frame_index = self.frame_index.wrapping_add(1);
        let first_y = height.saturating_sub(u16::try_from(rows.len()).unwrap_or(u16::MAX)) / 2;
        let mut stdout = std::io::stdout().lock();
        queue!(stdout, Clear(ClearType::All))?;
        for (index, row) in rows.iter().enumerate() {
            let Ok(index) = u16::try_from(index) else {
                break;
            };
            let y = first_y.saturating_add(index);
            if y >= height {
                break;
            }
            let text = truncate_width(&row.text, usize::from(width));
            let text_width = u16::try_from(UnicodeWidthStr::width(text.as_str())).unwrap_or(width);
            let x = width.saturating_sub(text_width) / 2;
            queue!(stdout, MoveTo(x, y))?;
            if row.bold {
                queue!(stdout, SetAttribute(Attribute::Bold))?;
            }
            if !no_color() {
                queue!(stdout, SetForegroundColor(row.color))?;
            }
            queue!(
                stdout,
                Print(text),
                ResetColor,
                SetAttribute(Attribute::Reset)
            )?;
        }
        stdout.flush()
    }

    fn poll_interrupt(&self) -> io::Result<UiEvent> {
        while event::poll(Duration::ZERO)? {
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
                && key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('c' | 'C'))
            {
                return Ok(UiEvent::Interrupted);
            }
        }
        Ok(UiEvent::Continue)
    }

    fn release(mut self, host: &str, establishing: bool) -> io::Result<()> {
        drop(self.guard.take());
        let mut stdout = std::io::stdout().lock();
        execute!(stdout, MoveTo(0, 0), Clear(ClearType::All))?;
        if establishing {
            writeln!(stdout, "rimz: establishing session to {host}…")?;
        }
        stdout.flush()
    }
}

struct DisplayRow {
    text: String,
    color: Color,
    bold: bool,
}

fn display_rows(frame: &RecoveryFrame, frame_index: usize) -> Vec<DisplayRow> {
    let mut rows = Vec::with_capacity(frame.rows.len() + 4);
    rows.push(DisplayRow {
        text: format!("⚡ Connection to {} lost", frame.host),
        color: Color::Yellow,
        bold: true,
    });
    rows.push(DisplayRow {
        text: String::new(),
        color: Color::Reset,
        bold: false,
    });
    rows.extend(frame.rows.iter().map(|row| {
        let (symbol, color) = match row.status {
            StageStatus::Waiting => ('○', Color::DarkGrey),
            StageStatus::Checking => (
                SPINNER_FRAMES[frame_index % SPINNER_FRAMES.len()],
                Color::Yellow,
            ),
            StageStatus::Ok => ('✓', Color::Green),
            StageStatus::Down => ('✗', Color::Red),
        };
        DisplayRow {
            text: format!("{symbol}  {}", row.label),
            color,
            bold: false,
        }
    }));
    rows.push(DisplayRow {
        text: String::new(),
        color: Color::Reset,
        bold: false,
    });
    rows.push(DisplayRow {
        text: "retrying continuously — Ctrl-C stops".to_owned(),
        color: Color::DarkGrey,
        bold: false,
    });
    rows
}

fn truncate_width(text: &str, width: usize) -> String {
    let mut used = 0;
    text.chars()
        .take_while(|character| {
            let next = used + UnicodeWidthChar::width(*character).unwrap_or(0);
            if next > width {
                return false;
            }
            used = next;
            true
        })
        .collect()
}

pub(super) const PANEL_TICK: Duration = SPINNER_TICK;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_requires_stdout_tty_and_progress_permission() {
        assert!(panel_allowed(true, None, None, None));
        assert!(!panel_allowed(false, None, None, None));
        assert!(!panel_allowed(true, Some("1"), None, None));
        assert!(panel_allowed(true, Some("0"), None, None));
        assert!(!panel_allowed(true, None, Some("codex"), None));
        assert!(!panel_allowed(true, None, None, Some("dumb")));
    }

    #[test]
    fn tiny_width_truncates_on_unicode_cell_boundaries() {
        assert_eq!(truncate_width("⚡ abc", 3), "⚡ ");
        assert_eq!(truncate_width("abc", 0), "");
    }
}
