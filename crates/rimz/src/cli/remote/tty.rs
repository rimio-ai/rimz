use std::io::{self, IsTerminal, Write};

use nix::sys::termios::{self, FlushArg, SetArg, Termios};

use rimz::remote::tty::{EMULATOR_RESET, sanitize_flags, termios_damaged};

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

    /// Discards terminal replies addressed to a dead SSH generation.
    ///
    /// Callers must only use this while reconnecting: exit paths preserve
    /// pending input for the shell that regains the terminal.
    pub(super) fn discard_pending_input(&self) {
        if self.saved.is_none() {
            return;
        }
        let stdin = io::stdin();
        if let Err(err) = termios::tcflush(&stdin, FlushArg::TCIFLUSH) {
            tracing::debug!(error = %err, "local tty input flush failed");
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
