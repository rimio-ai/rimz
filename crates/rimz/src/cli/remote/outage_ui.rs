//! Alternate-screen recovery panel and plain-line fallback.

use std::io::{self, IsTerminal, Write};
use std::time::Duration;

use ratatui::crossterm::cursor::MoveTo;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::crossterm::queue;
use ratatui::crossterm::style::{
    Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor,
};
use ratatui::crossterm::terminal::{self, Clear, ClearType};
use rimz::remote::reachability::FooterPhase;
use rimz::remote::recovery::{
    ConnectStage, RecoveryFrame, RecoveryPanel, RecoveryStage, StageStatus,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::cli::spinner::{SPINNER_FRAMES, SPINNER_TICK, animation_allowed, format_elapsed};
use rimz::tui::{MouseCapture, Screen, TerminalModeGuard, no_color};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UiEvent {
    Continue,
    Interrupted,
}

pub(super) struct OutageUi {
    connect_stage: ConnectStage,
    host: String,
    state: UiState,
}

enum UiState {
    PendingPanel,
    Panel(OutagePanel),
    PlainLines,
    Released,
}

impl OutageUi {
    pub(super) fn auto(connect_stage: ConnectStage, host: impl Into<String>) -> Self {
        let panel = panel_allowed(
            std::io::stdout().is_terminal(),
            std::env::var("RIMZ_NO_PROGRESS").ok().as_deref(),
            std::env::var(rimz::harness::launch::ENV_AGENT_KIND)
                .ok()
                .as_deref(),
            std::env::var("TERM").ok().as_deref(),
        );
        Self {
            connect_stage,
            host: host.into(),
            state: if panel {
                UiState::PendingPanel
            } else {
                UiState::PlainLines
            },
        }
    }

    #[cfg(test)]
    pub(super) fn plain_lines(connect_stage: ConnectStage, host: impl Into<String>) -> Self {
        Self {
            connect_stage,
            host: host.into(),
            state: UiState::PlainLines,
        }
    }

    pub(super) fn is_plain(&self) -> bool {
        matches!(self.state, UiState::PlainLines)
    }

    pub(super) fn report_connecting(&self) {
        if self.is_plain() && self.connect_stage == ConnectStage::Initial {
            let _ = writeln!(
                std::io::stderr().lock(),
                "rimz: connecting to {}…",
                self.host,
            );
        }
    }

    pub(super) fn report_unreachable(&self) {
        if self.is_plain() {
            let state = match self.connect_stage {
                ConnectStage::Initial => "unavailable",
                ConnectStage::Recovery => "lost",
            };
            let _ = writeln!(
                std::io::stderr().lock(),
                "rimz: network to {} {state} — waiting for network; Ctrl-C stops",
                self.host,
            );
        }
    }

    pub(super) fn report_network_restored(&self) {
        if self.is_plain() {
            let (state, action) = match self.connect_stage {
                ConnectStage::Initial => ("available", "connecting"),
                ConnectStage::Recovery => ("restored", "reconnecting"),
            };
            let _ = writeln!(
                std::io::stderr().lock(),
                "rimz: network to {} {state} — {action} now",
                self.host,
            );
        }
    }

    pub(super) fn report_attempt_failed(&self, error: Option<&str>) {
        if self.is_plain() {
            let detail = error.unwrap_or("SSH attempt failed");
            let action = match self.connect_stage {
                ConnectStage::Initial => "connect",
                ConnectStage::Recovery => "reconnect",
            };
            let _ = writeln!(
                std::io::stderr().lock(),
                "rimz: {action} to {} failed — {detail}",
                self.host,
            );
        }
    }

    pub(super) fn report_server_tun(&self, ifname: &str) {
        if self.is_plain() {
            let _ = writeln!(
                std::io::stderr().lock(),
                "rimz: server route to {} uses TUN {ifname} — TCP check skipped",
                self.host,
            );
        }
    }

    pub(super) fn report_reattached(&self) {
        if self.is_plain() {
            let action = match self.connect_stage {
                ConnectStage::Initial => "connected",
                ConnectStage::Recovery => "reattached",
            };
            let _ = writeln!(std::io::stderr().lock(), "rimz: {action} to {}", self.host,);
        }
    }

    pub(super) fn tick(
        &mut self,
        recovery: &mut RecoveryPanel,
        wait_elapsed: Duration,
        outage_for: Duration,
        phase: FooterPhase,
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
        panel.draw(&recovery.frame(outage_for, phase))?;
        panel.poll_interrupt()
    }

    pub(super) fn release(&mut self) -> io::Result<()> {
        match std::mem::replace(&mut self.state, UiState::Released) {
            UiState::Panel(panel) => panel.release(),
            UiState::PlainLines => {
                self.state = UiState::PlainLines;
                Ok(())
            }
            UiState::PendingPanel | UiState::Released => Ok(()),
        }
    }

    /// Hold a final failure frame long enough to read it before restoring the
    /// main screen, where the caller will report the same error to scrollback.
    pub(super) fn fail_hold(&mut self, headline: &str, details: &[String]) -> io::Result<()> {
        let UiState::Panel(panel) = &mut self.state else {
            return Ok(());
        };
        let rows = failure_rows(headline, details, &rimz::tui::captured_log_lines());
        panel.draw_rows(&rows)?;
        wait_for_keypress()?;
        self.release()
    }

    pub(super) fn handoff(
        &mut self,
        frame: &RecoveryFrame,
    ) -> io::Result<Option<TerminalModeGuard>> {
        match std::mem::replace(&mut self.state, UiState::Released) {
            UiState::Panel(panel) => panel.handoff(frame).map(Some),
            UiState::PlainLines => {
                self.state = UiState::PlainLines;
                Ok(None)
            }
            UiState::PendingPanel | UiState::Released => Ok(None),
        }
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
    guard: TerminalModeGuard,
    frame_index: usize,
    last_layout: Option<PanelLayout>,
}

impl OutagePanel {
    fn new() -> io::Result<Self> {
        Ok(Self {
            guard: TerminalModeGuard::enable(
                // Ghostty and xterm.js both translate wheel motion into arrow
                // keys on an alternate screen without mouse reporting. Keep
                // reports enabled while the panel drains input and through the
                // attach handoff so scroll momentum cannot reach the new pane.
                MouseCapture::Stdout,
                Screen::Alternate,
            )?,
            frame_index: 0,
            last_layout: None,
        })
    }

    fn draw(&mut self, frame: &RecoveryFrame) -> io::Result<()> {
        let rows = display_rows(frame, self.frame_index);
        self.frame_index = self.frame_index.wrapping_add(1);
        self.draw_rows(&rows)
    }

    fn draw_rows(&mut self, rows: &[DisplayRow]) -> io::Result<()> {
        let (width, height) = terminal::size()?;
        let layout = panel_layout(width, height, rows);
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

    fn release(self) -> io::Result<()> {
        drop(self);
        Ok(())
    }

    fn handoff(mut self, frame: &RecoveryFrame) -> io::Result<TerminalModeGuard> {
        let rows = attaching_rows(frame, '→');
        self.draw_rows(&rows)?;
        if let Some(layout) = self.last_layout {
            let row_offset = u16::try_from(rows.len().saturating_sub(1)).unwrap_or(u16::MAX);
            let mut stdout = std::io::stdout().lock();
            queue!(
                stdout,
                MoveTo(layout.x0, layout.first_y.saturating_add(row_offset))
            )?;
            stdout.flush()?;
        }
        self.guard.handoff_keep_screen()
    }
}

fn wait_for_keypress() -> io::Result<()> {
    loop {
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Press {
            return Ok(());
        }
    }
}

struct DisplayRow {
    text: String,
    color: Color,
    bold: bool,
    dim: bool,
}

fn failure_rows(headline: &str, details: &[String], captured_logs: &[String]) -> Vec<DisplayRow> {
    let mut rows = Vec::with_capacity(details.len() + captured_logs.len().min(5) + 4);
    rows.push(DisplayRow {
        text: format!("✗ {headline}"),
        color: Color::Red,
        bold: true,
        dim: false,
    });
    rows.extend(details.iter().map(|detail| DisplayRow {
        text: detail.clone(),
        color: Color::Reset,
        bold: false,
        dim: false,
    }));
    rows.extend(
        captured_logs
            .iter()
            .rev()
            .take(5)
            .rev()
            .map(|line| DisplayRow {
                text: format!("⚠  {line}"),
                color: Color::DarkGrey,
                bold: false,
                dim: true,
            }),
    );
    rows.push(DisplayRow {
        text: String::new(),
        color: Color::Reset,
        bold: false,
        dim: false,
    });
    rows.push(DisplayRow {
        text: "press any key to exit".to_owned(),
        color: Color::DarkGrey,
        bold: false,
        dim: true,
    });
    rows
}

fn display_rows(frame: &RecoveryFrame, frame_index: usize) -> Vec<DisplayRow> {
    let spinner = SPINNER_FRAMES[frame_index % SPINNER_FRAMES.len()];
    if frame.attaching {
        return attaching_rows(frame, spinner);
    }
    let spinner_stage = match frame.phase {
        FooterPhase::WaitingForNetwork
            if frame
                .rows
                .iter()
                .any(|row| row.stage == RecoveryStage::Internet) =>
        {
            RecoveryStage::Internet
        }
        FooterPhase::WaitingForNetwork
        | FooterPhase::Connecting
        | FooterPhase::NextAttemptIn(_) => RecoveryStage::Session,
    };
    let mut rows = Vec::with_capacity(frame.rows.len() + 3);
    rows.push(DisplayRow {
        text: match frame.connect_stage {
            ConnectStage::Initial => format!("⚡ Connecting to {}", frame.host),
            ConnectStage::Recovery => format!("⚡ Connection to {} lost", frame.host),
        },
        color: Color::Yellow,
        bold: true,
        dim: false,
    });
    rows.push(DisplayRow {
        text: match frame.connect_stage {
            ConnectStage::Initial => format!(
                "attempt {} · {} · Ctrl-C stops",
                frame.attempt,
                format_elapsed(frame.outage_for).replace('m', "m ")
            ),
            ConnectStage::Recovery => format!(
                "down {} · attempt {} · Ctrl-C stops",
                format_elapsed(frame.outage_for).replace('m', "m "),
                frame.attempt
            ),
        },
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
        let spinner_target = row.stage == spinner_stage;
        let waiting_stage = row.stage == RecoveryStage::Multiplexer
            || (frame.phase == FooterPhase::WaitingForNetwork
                && row.stage == RecoveryStage::Session
                && !spinner_target);
        let (symbol, color) = if spinner_target {
            (spinner, Color::Yellow)
        } else if waiting_stage {
            ('○', Color::DarkGrey)
        } else {
            match row.status {
                StageStatus::Waiting | StageStatus::Checking => ('○', Color::DarkGrey),
                StageStatus::Ok => ('✓', Color::Green),
                StageStatus::Down => ('✗', Color::Red),
                StageStatus::Suspect => ('!', Color::Yellow),
            }
        };
        let detail = match (row.stage, frame.phase) {
            (RecoveryStage::Internet, FooterPhase::WaitingForNetwork) => {
                format!("{} · waiting for network", row.detail)
            }
            (RecoveryStage::Session, FooterPhase::Connecting) => match frame.connect_stage {
                ConnectStage::Initial => "connecting…".to_owned(),
                ConnectStage::Recovery => "reconnecting…".to_owned(),
            },
            (RecoveryStage::Session, FooterPhase::NextAttemptIn(remaining)) => {
                let retry = format!("retry in {}s", countdown_seconds(remaining));
                match &frame.last_error {
                    Some(error) => format!("{error} · {retry}"),
                    None => retry,
                }
            }
            (RecoveryStage::Session, FooterPhase::WaitingForNetwork) if spinner_target => {
                "waiting for network".to_owned()
            }
            (RecoveryStage::Session, FooterPhase::WaitingForNetwork) => "waiting".to_owned(),
            _ => row.detail.clone(),
        };
        DisplayRow {
            text: format!("{symbol}  {:<12} {detail}", row.label),
            color,
            bold: false,
            dim: waiting_stage,
        }
    }));
    rows
}

fn attaching_rows(frame: &RecoveryFrame, symbol: char) -> Vec<DisplayRow> {
    let mut rows = Vec::with_capacity(frame.rows.len() + 3);
    rows.push(DisplayRow {
        text: format!("⚡ Connected to {}", frame.host),
        color: Color::Green,
        bold: true,
        dim: false,
    });
    rows.push(DisplayRow {
        text: "opening session… · this can take a few seconds".to_owned(),
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
        let attaching = row.stage == RecoveryStage::Multiplexer;
        DisplayRow {
            text: format!(
                "{}  {:<12} {}",
                if attaching { symbol } else { '✓' },
                row.label,
                if attaching {
                    row.detail.clone()
                } else {
                    success_detail(row)
                }
            ),
            color: if attaching {
                Color::Yellow
            } else {
                Color::Green
            },
            bold: false,
            dim: false,
        }
    }));
    rows
}

fn success_detail(row: &rimz::remote::recovery::StageFrame) -> String {
    if row.stage == RecoveryStage::Session {
        return "connected".to_owned();
    }
    if row.stage == RecoveryStage::Server {
        if let Some(endpoint) = row.detail.strip_suffix(" · answers TCP · SSH failing") {
            return endpoint.to_owned();
        }
        if let Some(route) = row.detail.strip_suffix(" · SSH failing") {
            return format!("{route} · TCP check skipped");
        }
    }
    row.detail.clone()
}

pub(super) struct HandoffScreen {
    guard: Option<TerminalModeGuard>,
}

impl HandoffScreen {
    pub(super) fn take(held_screen: &mut Option<TerminalModeGuard>) -> Self {
        Self {
            guard: held_screen.take(),
        }
    }

    pub(super) fn release(&mut self) {
        drop(self.guard.take());
    }

    pub(super) fn hold_failure(&mut self, headline: &str) {
        let Some(guard) = self.guard.take() else {
            return;
        };
        if write_handoff_failure(headline).is_err() {
            drop(guard);
            return;
        }
        let Ok(guard) = guard.resume_handoff() else {
            return;
        };
        let _ = wait_for_keypress();
        drop(guard);
    }
}

fn write_handoff_failure(headline: &str) -> io::Result<()> {
    let mut stdout = std::io::stdout().lock();
    queue!(stdout, Print("\n"), SetAttribute(Attribute::Bold))?;
    if !no_color() {
        queue!(stdout, SetForegroundColor(Color::Red))?;
    }
    queue!(
        stdout,
        Print(format!("✗ {headline}")),
        ResetColor,
        SetAttribute(Attribute::Reset),
        Print("\n\n"),
        SetAttribute(Attribute::Dim),
        Print("press any key to exit"),
        SetAttribute(Attribute::Reset),
        Print("\n")
    )?;
    stdout.flush()
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
#[path = "outage_ui_tests.rs"]
mod tests;
