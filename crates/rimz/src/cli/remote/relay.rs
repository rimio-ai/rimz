//! Per-attach output relay and screen-aware terminal handoff.

use std::io::{self, IsTerminal, PipeReader, PipeWriter, Read, Write};
use std::os::fd::AsFd;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process::{ChildStderr, ChildStdout};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use nix::errno::Errno;
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use ratatui::crossterm::terminal;
use rimz::child_process::StderrTail;
use rimz::remote::tty::{RelayEvent, RelayScanner, StreamScreen};
use rimz::tui::{TerminalModeGuard, no_color};
use unicode_width::UnicodeWidthStr;

use crate::cli::render::palette;

const DRAIN_TIMEOUT: Duration = Duration::from_millis(200);

pub(super) struct OutputRelay {
    stop: mpsc::Sender<Instant>,
    wake: PipeWriter,
    worker: JoinHandle<RelayOutput>,
}

impl OutputRelay {
    pub(super) fn start(
        stdout: Option<ChildStdout>,
        stderr: Option<ChildStderr>,
        mut output: RelayOutput,
    ) -> io::Result<Self> {
        let (stop, stopped) = mpsc::channel();
        let (reader, wake) = io::pipe()?;
        let worker = thread::Builder::new()
            .name("remote-output".to_owned())
            .spawn(move || {
                output.stderr = stderr.as_ref().map(|_| StderrTail::default());
                let result = catch_unwind(AssertUnwindSafe(|| {
                    pump(stdout, stderr, stopped, reader, &mut output)?;
                    let mut pending = Vec::new();
                    output.scanner.finish(&mut pending);
                    output.write(&pending)
                }));
                output.failure = match result {
                    Ok(Ok(())) => None,
                    Ok(Err(err)) => Some(err.to_string()),
                    Err(_) => Some("worker panicked".to_owned()),
                };
                output
            })?;
        Ok(Self { stop, wake, worker })
    }

    pub(super) fn is_finished(&self) -> bool {
        self.worker.is_finished()
    }

    pub(super) fn finish(self) -> Result<RelayOutput> {
        let _ = self.stop.send(Instant::now() + DRAIN_TIMEOUT);
        drop(self.wake);
        self.worker
            .join()
            .map_err(|_| anyhow::anyhow!("local output relay panicked"))
    }
}

pub(super) struct RelayOutput {
    scanner: RelayScanner,
    panel: Option<TerminalModeGuard>,
    log_capture: Option<TerminalModeGuard>,
    target: String,
    reconnected: bool,
    tty: bool,
    stderr: Option<StderrTail>,
    failure: Option<String>,
}

impl RelayOutput {
    pub(super) fn new(panel: Option<TerminalModeGuard>, target: &str, reconnected: bool) -> Self {
        Self {
            scanner: RelayScanner::default(),
            panel,
            log_capture: None,
            target: target.chars().filter(|ch| !ch.is_control()).collect(),
            reconnected,
            tty: io::stdout().is_terminal(),
            stderr: None,
            failure: None,
        }
    }

    fn feed(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut output = Vec::with_capacity(bytes.len());
        for &byte in bytes {
            let at_line_start = self.scanner.at_line_start();
            match self.scanner.feed(byte, &mut output) {
                RelayEvent::Speech => {
                    if let Some(mut panel) = self.panel.take() {
                        panel.restore_modes_keep_capture();
                        self.log_capture = Some(panel);
                    }
                    self.separator(at_line_start)?;
                }
                RelayEvent::RoomOpen => {
                    self.write(&output)?;
                    output.clear();
                    if self.panel.is_some() && !self.scanner.entered_alternate() && !no_color() {
                        self.write("\x1b[32m✓\x1b[39m".as_bytes())?;
                    }
                }
                RelayEvent::None => {}
            }
        }
        self.write(&output)
    }

    fn write(&self, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let mut stdout = io::stdout().lock();
        stdout.write_all(bytes)?;
        stdout.flush()
    }

    fn separator(&self, at_line_start: bool) -> io::Result<()> {
        if !self.tty {
            return Ok(());
        }
        let width = usize::from(terminal::size().map_or(48, |(width, _)| width));
        let time = jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::system());
        let time = time.strftime("%H:%M").to_string();
        let text = separator_label(&self.target, &time, self.reconnected, width);
        if !at_line_start {
            self.write(b"\r\n")?;
        }
        if no_color() {
            self.write(format!("{text}\r\n").as_bytes())
        } else {
            let style = palette::rule();
            self.write(format!("{style}{text}{style:#}\r\n").as_bytes())
        }
    }

    pub(super) fn complete(
        mut self,
        guard: &super::tty::TtyGuard,
    ) -> Result<(bool, Option<String>)> {
        let replay = self.panel.is_some() && !self.scanner.entered_alternate();
        if let Some(panel) = self.panel.as_mut()
            && !self.scanner.entered_alternate()
        {
            panel.restore_modes_keep_capture();
        }
        let capture = self.panel.take().or(self.log_capture.take());
        let unknown = self.failure.is_some() || self.scanner.screen() == StreamScreen::Unknown;
        if self.tty && (unknown || self.scanner.screen() == StreamScreen::Alternate) {
            let _ = self.write(b"\x1b[?1049l");
        }
        guard.reset_emulator();
        let cleanup: io::Result<()> = (|| {
            // A held panel never let host text reach the main screen.
            let mut at_line_start = replay || (!unknown && self.scanner.at_line_start());
            if let Some(capture) = capture {
                let bytes = capture.into_captured_logs();
                if !bytes.is_empty() {
                    let stderr = io::stderr();
                    let on_terminal = stderr.is_terminal();
                    let mut stderr = stderr.lock();
                    if on_terminal && !at_line_start {
                        stderr.write_all(b"\r\n")?;
                    }
                    stderr.write_all(&bytes)?;
                    stderr.flush()?;
                    if on_terminal {
                        at_line_start = bytes.ends_with(b"\n");
                    }
                }
            }
            if replay && !self.scanner.unpainted_tail().is_empty() {
                self.separator(at_line_start)?;
                self.write(String::from_utf8_lossy(self.scanner.unpainted_tail()).as_bytes())?;
                at_line_start = self.scanner.at_line_start();
            }
            if self.tty && !at_line_start {
                self.write(b"\r\n")?;
            }
            Ok(())
        })();
        if let Some(failure) = self.failure {
            anyhow::bail!("local output relay failed: {failure}");
        }
        cleanup.context("finishing local output relay")?;
        Ok((
            self.scanner.room_opened(),
            self.stderr.map(|tail| tail.text()),
        ))
    }
}

fn pump(
    mut stdout: Option<ChildStdout>,
    mut stderr: Option<ChildStderr>,
    stopped: mpsc::Receiver<Instant>,
    wake: PipeReader,
    output: &mut RelayOutput,
) -> io::Result<()> {
    if let Some(pipe) = &stdout {
        fcntl(pipe, FcntlArg::F_SETFL(OFlag::O_NONBLOCK))?;
    }
    if let Some(pipe) = &stderr {
        fcntl(pipe, FcntlArg::F_SETFL(OFlag::O_NONBLOCK))?;
    }
    let mut wake = Some(wake);
    let mut deadline: Option<Instant> = None;
    let mut buffer = [0_u8; 8192];
    loop {
        if deadline.is_some_and(|until| Instant::now() >= until)
            || (deadline.is_some() && stdout.is_none() && stderr.is_none())
        {
            return Ok(());
        }
        let wait = deadline.map_or(PollTimeout::NONE, |until| {
            PollTimeout::try_from(until.saturating_duration_since(Instant::now()))
                .unwrap_or(PollTimeout::MAX)
        });
        let mut fds: Vec<_> = [
            stdout
                .as_ref()
                .map(|pipe| PollFd::new(pipe.as_fd(), PollFlags::POLLIN)),
            stderr
                .as_ref()
                .map(|pipe| PollFd::new(pipe.as_fd(), PollFlags::POLLIN)),
            wake.as_ref()
                .map(|pipe| PollFd::new(pipe.as_fd(), PollFlags::POLLIN)),
        ]
        .into_iter()
        .flatten()
        .collect();
        match poll(&mut fds, wait) {
            Ok(_) => {}
            Err(Errno::EINTR) => continue,
            Err(err) => return Err(err.into()),
        }
        if wake.is_some()
            && fds
                .last()
                .is_some_and(|fd| fd.revents().is_some_and(|events| !events.is_empty()))
        {
            deadline = Some(
                stopped
                    .try_recv()
                    .unwrap_or_else(|_| Instant::now() + DRAIN_TIMEOUT),
            );
            wake.take();
        }
        if deadline.is_some_and(|until| Instant::now() >= until) {
            return Ok(());
        }
        let count = read_pipe(&mut stdout, &mut buffer)?;
        output.feed(&buffer[..count])?;
        let count = read_pipe(&mut stderr, &mut buffer)?;
        if let Some(tail) = &mut output.stderr {
            tail.append(&buffer[..count]);
        }
    }
}

fn read_pipe(pipe: &mut Option<impl Read>, buffer: &mut [u8]) -> io::Result<usize> {
    let Some(reader) = pipe.as_mut() else {
        return Ok(0);
    };
    match reader.read(buffer) {
        Ok(0) => {
            pipe.take();
            Ok(0)
        }
        Ok(count) => Ok(count),
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(0)
        }
        Err(err) => Err(err),
    }
}

/// `── <target> · <time>[ · reconnected] `, ruled out to 48 cells. A target too
/// wide for the terminal is cut with `…` so the time always shows.
fn separator_label(target: &str, time: &str, reconnected: bool, width: usize) -> String {
    use super::outage_ui::truncate_width;

    let suffix = if reconnected { " · reconnected" } else { "" };
    let tail = format!(" · {time}{suffix} ");
    let room = width.saturating_sub(UnicodeWidthStr::width(format!("── {tail}").as_str()));
    let target = if UnicodeWidthStr::width(target) <= room {
        target.to_owned()
    } else {
        format!("{}…", truncate_width(target, room.saturating_sub(1)))
    };
    let mut label = truncate_width(&format!("── {target}{tail}"), width);
    let rule = width
        .min(48)
        .saturating_sub(UnicodeWidthStr::width(label.as_str()));
    label.push_str(&"─".repeat(rule));
    label
}

#[cfg(test)]
mod tests {
    use super::separator_label;

    #[test]
    fn separator_label_keeps_the_time_whatever_the_target_width() {
        let short = separator_label("box:repo", "09:41", false, 80);
        assert_eq!(short, format!("── box:repo · 09:41 {}", "─".repeat(28)));

        let long = "relaybox:/tmp/relaylive.aeGn/remote-home/plain-zellij";
        let wide = separator_label(long, "09:41", true, 120);
        assert_eq!(wide, format!("── {long} · 09:41 · reconnected "));

        let narrow = separator_label(long, "09:41", true, 48);
        assert_eq!(narrow, "── relaybox:/tmp/relayli… · 09:41 · reconnected ");
    }
}
