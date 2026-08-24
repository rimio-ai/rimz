use std::io::{self, IsTerminal, Write};
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::termios::{self, FlushArg, SetArg, Termios};

use rimz::remote::tty::{EMULATOR_RESET, sanitize_flags, termios_damaged};

/// Quiet window the local tty must show before a replacement attach may own it.
const SETTLE_QUIET: Duration = Duration::from_millis(250);
/// Hard cap, so a terminal that never stops talking cannot stall a reconnect.
const SETTLE_MAX: Duration = Duration::from_secs(2);

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

    /// Absorbs terminal-protocol replies still arriving from a killed SSH
    /// generation, so the replacement attach cannot accept them as replies to
    /// its own capability queries and pass its real replies through as input.
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

        drain_until_quiet(&stdin, SETTLE_QUIET, SETTLE_MAX);

        if let Err(err) = termios::tcflush(&stdin, FlushArg::TCIFLUSH) {
            tracing::debug!(error = %err, "local tty input flush failed");
        }
        if let Err(err) = termios::tcsetattr(&stdin, SetArg::TCSADRAIN, saved) {
            tracing::debug!(error = %err, "local tty restore after settle failed");
        }
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
}

fn drain_until_quiet(stdin: &impl AsFd, quiet: Duration, max: Duration) {
    let started = Instant::now();
    let deadline = started + max;
    let mut quiet_deadline = started + quiet;
    let mut buffer = [0_u8; 256];
    loop {
        let now = Instant::now();
        let wait_until = quiet_deadline.min(deadline);
        if now >= wait_until {
            break;
        }
        let timeout = PollTimeout::try_from(wait_until.saturating_duration_since(now))
            .unwrap_or(PollTimeout::MAX);
        let mut fds = [PollFd::new(stdin.as_fd(), PollFlags::POLLIN)];
        match poll(&mut fds, timeout) {
            Ok(0) => break,
            Ok(_)
                if fds[0]
                    .revents()
                    .is_some_and(|events| events.contains(PollFlags::POLLIN)) =>
            {
                match nix::unistd::read(stdin, &mut buffer) {
                    Ok(0) => break,
                    Ok(read) if buffer[..read].contains(&0x03) => break,
                    Ok(_) => quiet_deadline = Instant::now() + quiet,
                    Err(Errno::EINTR) => {}
                    Err(err) => {
                        tracing::debug!(error = %err, "local tty settle read failed");
                        break;
                    }
                }
            }
            Ok(_) => break,
            Err(Errno::EINTR) => {}
            Err(err) => {
                tracing::debug!(error = %err, "local tty settle poll failed");
                break;
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

        drain_until_quiet(&reader, quiet, Duration::from_secs(2));

        assert!(
            started.elapsed() < quiet / 2,
            "Ctrl-C should end the drain immediately, not after {quiet:?}"
        );
    }
}
