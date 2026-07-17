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

use crate::cli::spinner::{SPINNER_FRAMES, SPINNER_TICK, animation_allowed, format_elapsed};
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
        wait_elapsed: Duration,
        outage_for: Duration,
        next_attempt_in: Option<Duration>,
    ) -> io::Result<UiEvent> {
        if matches!(self.state, UiState::PlainLines) || !recovery.visible(wait_elapsed) {
            return Ok(UiEvent::Continue);
        }
        if matches!(self.state, UiState::PendingPanel) {
            match OutagePanel::new() {
                Ok(panel) => {
                    recovery.note_shown(wait_elapsed);
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
        panel.draw(&recovery.frame(outage_for, next_attempt_in))?;
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
    last_layout: Option<PanelLayout>,
}

impl OutagePanel {
    fn new() -> io::Result<Self> {
        Ok(Self {
            guard: Some(TerminalModeGuard::enable(
                MouseCapture::Off,
                Screen::Alternate,
            )?),
            frame_index: 0,
            last_layout: None,
        })
    }

    fn draw(&mut self, frame: &RecoveryFrame) -> io::Result<()> {
        let (width, height) = terminal::size()?;
        let rows = display_rows(frame, self.frame_index);
        self.frame_index = self.frame_index.wrapping_add(1);
        let layout = panel_layout(width, height, &rows);
        let mut stdout = std::io::stdout().lock();
        if self.last_layout != Some(layout) {
            queue!(stdout, Clear(ClearType::All))?;
            self.last_layout = Some(layout);
        }
        for (index, row) in rows.iter().enumerate() {
            let Ok(index) = u16::try_from(index) else {
                break;
            };
            let y = layout.first_y.saturating_add(index);
            if y >= height {
                break;
            }
            let available_width = usize::from(width.saturating_sub(layout.x0));
            let text = truncate_width(&row.text, available_width);
            queue!(stdout, MoveTo(layout.x0, y))?;
            if row.bold {
                queue!(stdout, SetAttribute(Attribute::Bold))?;
            }
            if row.dim {
                queue!(stdout, SetAttribute(Attribute::Dim))?;
            }
            if !no_color() {
                queue!(stdout, SetForegroundColor(row.color))?;
            }
            queue!(
                stdout,
                Print(text),
                ResetColor,
                SetAttribute(Attribute::Reset),
                Clear(ClearType::UntilNewLine)
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
    dim: bool,
}

fn display_rows(frame: &RecoveryFrame, frame_index: usize) -> Vec<DisplayRow> {
    let spinner = SPINNER_FRAMES[frame_index % SPINNER_FRAMES.len()];
    let mut rows = Vec::with_capacity(frame.rows.len() + 6);
    rows.push(DisplayRow {
        text: format!("⚡ Connection to {} lost", frame.host),
        color: Color::Yellow,
        bold: true,
        dim: false,
    });
    rows.push(DisplayRow {
        text: format!(
            "down {} · attempt {}",
            format_elapsed(frame.outage_for).replace('m', "m "),
            frame.attempt
        ),
        color: Color::DarkGrey,
        bold: false,
        dim: true,
    });
    rows.push(DisplayRow {
        text: String::new(),
        color: Color::Reset,
        bold: false,
        dim: false,
    });
    rows.extend(frame.rows.iter().map(|row| {
        let (symbol, color) = match row.status {
            StageStatus::Waiting => ('○', Color::DarkGrey),
            StageStatus::Checking => (spinner, Color::Yellow),
            StageStatus::Ok => ('✓', Color::Green),
            StageStatus::Down => ('✗', Color::Red),
            StageStatus::Suspect => ('!', Color::Yellow),
        };
        DisplayRow {
            text: format!("{symbol}  {:<12} {}", row.label, row.detail),
            color,
            bold: false,
            dim: false,
        }
    }));
    rows.push(DisplayRow {
        text: String::new(),
        color: Color::Reset,
        bold: false,
        dim: false,
    });
    rows.push(DisplayRow {
        text: if frame.connecting {
            format!("{spinner} connecting…")
        } else {
            format!(
                "{spinner} next attempt in {}s",
                countdown_seconds(frame.next_attempt_in.unwrap_or_default())
            )
        },
        color: Color::Yellow,
        bold: false,
        dim: false,
    });
    rows.push(DisplayRow {
        text: "retrying until it returns · Ctrl-C stops".to_owned(),
        color: Color::DarkGrey,
        bold: false,
        dim: true,
    });
    rows
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PanelLayout {
    width: u16,
    height: u16,
    x0: u16,
    first_y: u16,
    row_count: usize,
}

fn panel_layout(width: u16, height: u16, rows: &[DisplayRow]) -> PanelLayout {
    let block_width = rows
        .iter()
        .map(|row| UnicodeWidthStr::width(row.text.as_str()))
        .max()
        .unwrap_or_default()
        .min(usize::from(width));
    let block_width = u16::try_from(block_width).unwrap_or(width);
    PanelLayout {
        width,
        height,
        x0: width.saturating_sub(block_width) / 2,
        first_y: height.saturating_sub(u16::try_from(rows.len()).unwrap_or(u16::MAX)) / 2,
        row_count: rows.len(),
    }
}

fn countdown_seconds(duration: Duration) -> u64 {
    duration.as_secs() + u64::from(duration.subsec_nanos() > 0)
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
    use rimz::remote::recovery::{RecoveryStage, StageFrame};

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

    #[test]
    fn rows_share_one_centered_block_left_edge() {
        let rows = vec![
            DisplayRow {
                text: "short".to_owned(),
                color: Color::Reset,
                bold: false,
                dim: false,
            },
            DisplayRow {
                text: "twelve cells".to_owned(),
                color: Color::Reset,
                bold: false,
                dim: false,
            },
        ];

        let layout = panel_layout(40, 20, &rows);

        assert_eq!(layout.x0, 14);
        assert_eq!(layout.first_y, 9);
        assert_eq!(layout.row_count, 2);
    }

    #[test]
    fn display_rows_align_stage_columns_and_show_outage_context() {
        let frame = RecoveryFrame {
            host: "dev-box".to_owned(),
            outage_for: Duration::from_secs(133),
            attempt: 7,
            next_attempt_in: Some(Duration::from_millis(11_100)),
            connecting: false,
            rows: vec![
                StageFrame {
                    stage: RecoveryStage::Internet,
                    status: StageStatus::Ok,
                    label: "Internet".to_owned(),
                    detail: "1.1.1.1:443".to_owned(),
                },
                StageFrame {
                    stage: RecoveryStage::Server,
                    status: StageStatus::Suspect,
                    label: "Server".to_owned(),
                    detail: "dev-box:22 · answers TCP · SSH failing".to_owned(),
                },
            ],
        };

        let rows = display_rows(&frame, 2);
        let text = rows.iter().map(|row| row.text.as_str()).collect::<Vec<_>>();

        assert_eq!(text[1], "down 2m 13s · attempt 7");
        assert_eq!(text[3], "✓  Internet     1.1.1.1:443");
        assert_eq!(
            text[4],
            "!  Server       dev-box:22 · answers TCP · SSH failing"
        );
        assert_eq!(text[6], "⠹ next attempt in 12s");
        assert_eq!(text[7], "retrying until it returns · Ctrl-C stops");
    }
}
