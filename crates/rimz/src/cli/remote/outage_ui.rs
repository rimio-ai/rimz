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
            std::env::var(rimz::harness::run::ENV_AGENT_KIND)
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

    pub(super) fn handoff(&mut self, frame: &RecoveryFrame) -> io::Result<bool> {
        match std::mem::replace(&mut self.state, UiState::Released) {
            UiState::Panel(panel) => {
                panel.handoff(frame)?;
                Ok(true)
            }
            UiState::PlainLines => {
                self.state = UiState::PlainLines;
                Ok(false)
            }
            UiState::PendingPanel | UiState::Released => Ok(false),
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

    fn release(mut self) -> io::Result<()> {
        drop(self.guard.take());
        Ok(())
    }

    fn handoff(mut self, frame: &RecoveryFrame) -> io::Result<()> {
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
        if let Some(guard) = self.guard.take() {
            guard.release_keep_screen()?;
        }
        Ok(())
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

pub(super) fn leave_alternate_screen() {
    let _ = execute!(std::io::stdout(), terminal::LeaveAlternateScreen);
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
    use rimz::remote::reachability::FooterPhase;
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

    fn stage(stage: RecoveryStage, status: StageStatus, label: &str, detail: &str) -> StageFrame {
        StageFrame {
            stage,
            status,
            label: label.to_owned(),
            detail: detail.to_owned(),
        }
    }

    fn spinner_count(rows: &[DisplayRow]) -> usize {
        rows.iter()
            .filter(|row| {
                row.text
                    .chars()
                    .next()
                    .is_some_and(|glyph| SPINNER_FRAMES.contains(&glyph))
            })
            .count()
    }

    #[test]
    fn display_rows_align_stage_columns_and_show_outage_context() {
        let frame = RecoveryFrame {
            connect_stage: ConnectStage::Recovery,
            host: "dev-box".to_owned(),
            outage_for: Duration::from_secs(133),
            attempt: 7,
            phase: FooterPhase::NextAttemptIn(Duration::from_millis(11_100)),
            last_error: Some("Permission denied (publickey).".to_owned()),
            attaching: false,
            rows: vec![
                stage(
                    RecoveryStage::Internet,
                    StageStatus::Ok,
                    "Internet",
                    "cp.cloudflare.com",
                ),
                stage(
                    RecoveryStage::Server,
                    StageStatus::Suspect,
                    "Server",
                    "dev-box:22 · answers TCP · SSH failing",
                ),
                stage(
                    RecoveryStage::Session,
                    StageStatus::Down,
                    "SSH session",
                    "Permission denied (publickey).",
                ),
                stage(
                    RecoveryStage::Multiplexer,
                    StageStatus::Waiting,
                    "Multiplexer",
                    "waiting",
                ),
            ],
        };

        let rows = display_rows(&frame, 2);
        let text = rows.iter().map(|row| row.text.as_str()).collect::<Vec<_>>();

        assert_eq!(text[1], "down 2m 13s · attempt 7 · Ctrl-C stops");
        assert_eq!(text[3], "✓  Internet     cp.cloudflare.com");
        assert_eq!(
            text[4],
            "!  Server       dev-box:22 · answers TCP · SSH failing"
        );
        assert_eq!(
            text[5],
            "⠹  SSH session  Permission denied (publickey). · retry in 12s"
        );
        assert_eq!(text[6], "○  Multiplexer  waiting");
        assert!(rows[6].dim);
        assert_eq!(rows.len(), 7);
    }

    #[test]
    fn display_rows_distinguish_network_wait_from_fast_reconnect() {
        let mut frame = RecoveryFrame {
            connect_stage: ConnectStage::Recovery,
            host: "dev-box".to_owned(),
            outage_for: Duration::from_secs(2),
            attempt: 3,
            phase: FooterPhase::WaitingForNetwork,
            last_error: None,
            attaching: false,
            rows: vec![
                stage(
                    RecoveryStage::Internet,
                    StageStatus::Checking,
                    "Internet",
                    "cp.cloudflare.com",
                ),
                stage(
                    RecoveryStage::Session,
                    StageStatus::Down,
                    "SSH session",
                    "failed",
                ),
                stage(
                    RecoveryStage::Multiplexer,
                    StageStatus::Waiting,
                    "Multiplexer",
                    "waiting",
                ),
            ],
        };
        let waiting = display_rows(&frame, 0);
        assert_eq!(
            waiting[3].text,
            "⠋  Internet     cp.cloudflare.com · waiting for network"
        );
        assert_eq!(waiting[4].text, "○  SSH session  waiting");
        assert!(waiting[4].dim);
        assert_eq!(waiting[5].text, "○  Multiplexer  waiting");
        assert!(waiting[5].dim);

        frame.phase = FooterPhase::Connecting;
        let connecting = display_rows(&frame, 0);
        assert_eq!(connecting[3].text, "○  Internet     cp.cloudflare.com");
        assert_eq!(connecting[4].text, "⠋  SSH session  reconnecting…");
        assert!(!connecting[4].dim);
        assert_eq!(connecting[5].text, "○  Multiplexer  waiting");
    }

    #[test]
    fn display_rows_present_the_initial_connection_stage() {
        let frame = RecoveryFrame {
            connect_stage: ConnectStage::Initial,
            host: "dev-box".to_owned(),
            outage_for: Duration::from_secs(2),
            attempt: 1,
            phase: FooterPhase::Connecting,
            last_error: None,
            attaching: false,
            rows: vec![
                stage(
                    RecoveryStage::Session,
                    StageStatus::Checking,
                    "SSH session",
                    "starting…",
                ),
                stage(
                    RecoveryStage::Multiplexer,
                    StageStatus::Waiting,
                    "Multiplexer",
                    "waiting",
                ),
            ],
        };

        let rows = display_rows(&frame, 0);
        assert_eq!(rows[0].text, "⚡ Connecting to dev-box");
        assert_eq!(rows[1].text, "attempt 1 · 2s · Ctrl-C stops");
        assert_eq!(rows[3].text, "⠋  SSH session  connecting…");
    }

    #[test]
    fn display_rows_reserve_the_spinner_for_the_phase_target() {
        let mut frame = RecoveryFrame {
            connect_stage: ConnectStage::Recovery,
            host: "dev-box".to_owned(),
            outage_for: Duration::from_secs(2),
            attempt: 3,
            phase: FooterPhase::WaitingForNetwork,
            last_error: Some("timed out".to_owned()),
            attaching: false,
            rows: vec![
                stage(
                    RecoveryStage::Internet,
                    StageStatus::Checking,
                    "Internet",
                    "cp.cloudflare.com",
                ),
                stage(
                    RecoveryStage::Server,
                    StageStatus::Checking,
                    "Server",
                    "dev-box:22",
                ),
                stage(
                    RecoveryStage::Session,
                    StageStatus::Checking,
                    "SSH session",
                    "starting…",
                ),
                stage(
                    RecoveryStage::Multiplexer,
                    StageStatus::Waiting,
                    "Multiplexer",
                    "waiting",
                ),
            ],
        };

        for phase in [
            FooterPhase::WaitingForNetwork,
            FooterPhase::Connecting,
            FooterPhase::NextAttemptIn(Duration::from_secs(2)),
        ] {
            frame.phase = phase;
            assert_eq!(spinner_count(&display_rows(&frame, 0)), 1);
        }
        frame.phase = FooterPhase::NextAttemptIn(Duration::from_secs(2));
        frame.last_error = None;
        assert_eq!(
            display_rows(&frame, 0)[5].text,
            "⠋  SSH session  retry in 2s"
        );
    }

    #[test]
    fn network_wait_spinner_falls_back_to_the_session_without_an_internet_row() {
        let frame = RecoveryFrame {
            connect_stage: ConnectStage::Recovery,
            host: "proxy-box".to_owned(),
            outage_for: Duration::from_secs(2),
            attempt: 3,
            phase: FooterPhase::WaitingForNetwork,
            last_error: None,
            attaching: false,
            rows: vec![
                stage(
                    RecoveryStage::Session,
                    StageStatus::Waiting,
                    "SSH session",
                    "waiting",
                ),
                stage(
                    RecoveryStage::Multiplexer,
                    StageStatus::Waiting,
                    "Multiplexer",
                    "waiting",
                ),
            ],
        };

        let rows = display_rows(&frame, 0);

        assert_eq!(rows[3].text, "⠋  SSH session  waiting for network");
        assert_eq!(spinner_count(&rows), 1);
    }

    #[test]
    fn attaching_rows_animate_then_freeze_the_handoff_stage() {
        let frame = RecoveryFrame {
            connect_stage: ConnectStage::Initial,
            host: "dev-box".to_owned(),
            outage_for: Duration::from_secs(2),
            attempt: 1,
            phase: FooterPhase::Connecting,
            last_error: None,
            attaching: true,
            rows: vec![
                stage(
                    RecoveryStage::Internet,
                    StageStatus::Down,
                    "Internet",
                    "cp.cloudflare.com",
                ),
                stage(
                    RecoveryStage::Server,
                    StageStatus::Suspect,
                    "Server",
                    "dev-box:22 · answers TCP · SSH failing",
                ),
                stage(
                    RecoveryStage::Session,
                    StageStatus::Checking,
                    "SSH session",
                    "starting…",
                ),
                stage(
                    RecoveryStage::Multiplexer,
                    StageStatus::Checking,
                    "Multiplexer",
                    "attaching…",
                ),
            ],
        };

        let animated = display_rows(&frame, 0);
        assert_eq!(animated[0].text, "⚡ Connected to dev-box");
        assert_eq!(animated[6].text, "⠋  Multiplexer  attaching…");
        assert_eq!(spinner_count(&animated), 1);

        let rows = attaching_rows(&frame, '→');
        let text = rows.iter().map(|row| row.text.as_str()).collect::<Vec<_>>();

        assert_eq!(text[0], "⚡ Connected to dev-box");
        assert!(rows[0].bold);
        assert_eq!(text[1], "opening session… · this can take a few seconds");
        assert!(rows[1].dim);
        assert_eq!(text[3], "✓  Internet     cp.cloudflare.com");
        assert_eq!(text[4], "✓  Server       dev-box:22");
        assert_eq!(text[5], "✓  SSH session  connected");
        assert_eq!(text[6], "→  Multiplexer  attaching…");
        // Settled checkpoints read green; the one stage still running reads
        // yellow, so the eye lands on the row the wait belongs to.
        assert!(rows[3..6].iter().all(|row| row.color == Color::Green));
        assert_eq!(rows[6].color, Color::Yellow);

        let mut web_frame = frame.clone();
        let web_handoff = web_frame.rows.last_mut().expect("handoff row");
        web_handoff.label = "Web tunnel".to_owned();
        web_handoff.detail = "opening…".to_owned();
        assert_eq!(
            attaching_rows(&web_frame, '→')[6].text,
            "→  Web tunnel   opening…"
        );
        assert_eq!(
            success_detail(&stage(
                RecoveryStage::Server,
                StageStatus::Suspect,
                "Server",
                "dev-box:22 · via TUN utun9 · SSH failing",
            )),
            "dev-box:22 · via TUN utun9 · TCP check skipped"
        );
    }
}
