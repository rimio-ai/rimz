use std::io::{self, IsTerminal, Write};
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::termios::{self, FlushArg, SetArg, Termios};

use rimz::remote::tty::{
    EMULATOR_RESET, STATUS_QUERY, StatusReplyScanner, sanitize_flags, termios_damaged,
};

/// Quiet window used when the terminal cannot be queried for a causal fence.
const SETTLE_QUIET: Duration = Duration::from_millis(250);
/// Hard cap for waiting on a terminal's causal fence reply.
const SETTLE_MAX: Duration = Duration::from_secs(2);

#[derive(Debug, Eq, PartialEq)]
enum Settled {
    Fenced,
    Quiet,
    Expired,
    Interrupted,
}

pub(super) struct TtyGuard {
    saved: Option<Termios>,
}

impl TtyGuard {
    pub(super) fn acquire() -> Self {
        Self {
            saved: sanitize_and_snapshot(),
        }
    }

    pub(super) fn restore(&self) {
        let Some(saved) = &self.saved else {
            return;
        };
        let stdin = io::stdin();
        if let Err(err) = termios::tcsetattr(&stdin, SetArg::TCSADRAIN, saved) {
            tracing::debug!(error = %err, "local tty restore failed");
        }
    }

    /// Fences terminal-protocol replies still arriving from a killed SSH
    /// generation with a DSR status round trip. Terminals answer queries in
    /// order, so the fence reply follows every earlier reply and precedes input
    /// belonging to the replacement. Reading one byte at a time stops exactly
    /// at that boundary. If the query cannot be written, a quiet-window drain
    /// remains as a fallback.
    ///
    /// Callers must only use this before a replacement attach: exit paths
    /// preserve pending input for the shell that regains the terminal.
    pub(super) fn settle_terminal_replies(&self) {
        let Some(saved) = &self.saved else {
            return;
        };
        let stdin = io::stdin();

        let mut raw = saved.clone();
        termios::cfmakeraw(&mut raw);
        raw.control_chars[termios::SpecialCharacterIndices::VMIN as usize] = 0;
        raw.control_chars[termios::SpecialCharacterIndices::VTIME as usize] = 0;
        if let Err(err) = termios::tcsetattr(&stdin, SetArg::TCSANOW, &raw) {
            tracing::debug!(error = %err, "local tty settle mode failed");
            if let Err(err) = termios::tcflush(&stdin, FlushArg::TCIFLUSH) {
                tracing::debug!(error = %err, "local tty input flush failed");
            }
            return;
        }

        let scanner = if io::stderr().is_terminal() {
            let mut stderr = io::stderr().lock();
            match stderr
                .write_all(STATUS_QUERY.as_bytes())
                .and_then(|()| stderr.flush())
            {
                Ok(()) => Some(StatusReplyScanner::default()),
                Err(err) => {
                    tracing::debug!(error = %err, "local tty status query failed");
                    None
                }
            }
        } else {
            None
        };

        let outcome = settle_input(&stdin, scanner, SETTLE_QUIET, SETTLE_MAX);

        if outcome != Settled::Fenced
            && let Err(err) = termios::tcflush(&stdin, FlushArg::TCIFLUSH)
        {
            tracing::debug!(error = %err, "local tty input flush failed");
        }
        if let Err(err) = termios::tcsetattr(&stdin, SetArg::TCSADRAIN, saved) {
            tracing::debug!(error = %err, "local tty restore after settle failed");
        }
        tracing::debug!(outcome = ?outcome, "local tty settled");
    }

    pub(super) fn reset_emulator(&self) {
        if self.saved.is_none() || !io::stderr().is_terminal() {
            return;
        }
        let mut stderr = io::stderr().lock();
        if let Err(err) = stderr
            .write_all(EMULATOR_RESET.as_bytes())
            .and_then(|()| stderr.flush())
        {
            tracing::debug!(error = %err, "local terminal emulator reset failed");
        }
    }

    pub(super) fn discard_pending_input(&self) {
        if self.saved.is_none() {
            return;
        }
        if let Err(err) = termios::tcflush(io::stdin(), FlushArg::TCIFLUSH) {
            tracing::debug!(error = %err, "local tty input flush before prompt failed");
        }
    }
}

fn settle_input(
    stdin: &impl AsFd,
    mut scanner: Option<StatusReplyScanner>,
    quiet: Duration,
    max: Duration,
) -> Settled {
    let started = Instant::now();
    let deadline = started + max;
    let mut quiet_deadline = started + quiet;
    let mut buffer = [0_u8; 1];
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Settled::Expired;
        }
        if scanner.is_none() && now >= quiet_deadline {
            return Settled::Quiet;
        }
        let wait_until = scanner
            .as_ref()
            .map_or_else(|| quiet_deadline.min(deadline), |_| deadline);
        let timeout = PollTimeout::try_from(wait_until.saturating_duration_since(now))
            .unwrap_or(PollTimeout::MAX);
        let mut fds = [PollFd::new(stdin.as_fd(), PollFlags::POLLIN)];
        match poll(&mut fds, timeout) {
            Ok(0) => {}
            Ok(_)
                if fds[0]
                    .revents()
                    .is_some_and(|events| events.contains(PollFlags::POLLIN)) =>
            {
                match nix::unistd::read(stdin, &mut buffer) {
                    Ok(0) => return Settled::Expired,
                    Ok(_) => {
                        if let Some(scanner) = &mut scanner {
                            if scanner.feed(buffer[0]) {
                                return Settled::Fenced;
                            }
                        } else if buffer[0] == 0x03 {
                            return Settled::Interrupted;
                        } else {
                            quiet_deadline = Instant::now() + quiet;
                        }
                    }
                    Err(Errno::EINTR) => {}
                    Err(err) => {
                        tracing::debug!(error = %err, "local tty settle read failed");
                        return Settled::Expired;
                    }
                }
            }
            Ok(_) => return Settled::Expired,
            Err(Errno::EINTR) => {}
            Err(err) => {
                tracing::debug!(error = %err, "local tty settle poll failed");
                return Settled::Expired;
            }
        }
    }
}

impl Drop for TtyGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

pub(super) fn sanitize_local_tty() {
    let _ = sanitize_and_snapshot();
}

fn sanitize_and_snapshot() -> Option<Termios> {
    let stdin = io::stdin();
    if !stdin.is_terminal() {
        return None;
    }
    let mut saved = match termios::tcgetattr(&stdin) {
        Ok(saved) => saved,
        Err(err) => {
            tracing::debug!(error = %err, "local tty snapshot failed");
            return None;
        }
    };
    if !termios_damaged(saved.input_flags, saved.output_flags, saved.local_flags) {
        return Some(saved);
    }
    (saved.input_flags, saved.output_flags, saved.local_flags) =
        sanitize_flags(saved.input_flags, saved.output_flags, saved.local_flags);
    if let Err(err) = termios::tcsetattr(&stdin, SetArg::TCSANOW, &saved) {
        tracing::debug!(error = %err, "local tty sanitation failed");
        return None;
    }
    Some(saved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_c_ends_the_drain_without_waiting_for_quiet() {
        let (reader, writer) = nix::unistd::pipe().expect("open test pipe");
        nix::unistd::write(&writer, b"\x03").expect("write Ctrl-C");
        let quiet = Duration::from_secs(1);
        let started = Instant::now();

        let outcome = settle_input(&reader, None, quiet, Duration::from_secs(2));

        assert_eq!(outcome, Settled::Interrupted);
        assert!(
            started.elapsed() < quiet / 2,
            "Ctrl-C should end the drain immediately, not after {quiet:?}"
        );
    }

    #[test]
    fn fenced_settle_leaves_bytes_after_status_reply_unread() {
        let (reader, writer) = nix::unistd::pipe().expect("open test pipe");
        nix::unistd::write(
            &writer,
            b"\x1b[?62;22;52c\x1b[>1;10;0c\x1bP>|ghostty 1.3.1\x1b\\\x1b[0nuser",
        )
        .expect("write stale replies, fence reply, and user bytes");

        let outcome = settle_input(
            &reader,
            Some(StatusReplyScanner::default()),
            Duration::from_millis(100),
            Duration::from_secs(1),
        );
        let mut remaining = [0_u8; 4];
        let read = nix::unistd::read(&reader, &mut remaining).expect("read remaining bytes");

        assert_eq!(outcome, Settled::Fenced);
        assert_eq!(&remaining[..read], b"user");
    }

    #[test]
    fn ctrl_c_does_not_abandon_a_fenced_settle() {
        let (reader, writer) = nix::unistd::pipe().expect("open test pipe");
        nix::unistd::write(&writer, b"\x03\x1b[0n").expect("write Ctrl-C and status reply");

        let outcome = settle_input(
            &reader,
            Some(StatusReplyScanner::default()),
            Duration::from_millis(100),
            Duration::from_secs(1),
        );

        assert_eq!(outcome, Settled::Fenced);
    }

    #[test]
    fn scanner_waits_until_max_instead_of_quiet_timeout() {
        let (reader, writer) = nix::unistd::pipe().expect("open test pipe");
        nix::unistd::write(
            &writer,
            b"\x1b[?62;22;52c\x1b[>1;10;0c\x1bP>|ghostty 1.3.1\x1b\\",
        )
        .expect("write stale replies");

        let outcome = settle_input(
            &reader,
            Some(StatusReplyScanner::default()),
            Duration::from_millis(1),
            Duration::from_millis(20),
        );

        assert_eq!(outcome, Settled::Expired);
    }

    #[test]
    fn settle_without_scanner_uses_quiet_timeout() {
        let (reader, _writer) = nix::unistd::pipe().expect("open test pipe");

        let outcome = settle_input(
            &reader,
            None,
            Duration::from_millis(100),
            Duration::from_secs(1),
        );

        assert_eq!(outcome, Settled::Quiet);
    }
}
