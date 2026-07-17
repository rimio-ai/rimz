//! Pure terminal-state hygiene for remote SSH attaches.

use nix::sys::termios::{InputFlags, LocalFlags, OutputFlags};

/// Restores emulator modes that an interrupted remote TUI can leave behind.
///
/// - `ESC[<u` pops kitty keyboard enhancement.
/// - `ESC[?1049l` leaves the alternate screen before later bytes target the main screen.
/// - `ESC[?2004l` disables bracketed paste.
/// - `ESC[?1004l` disables focus reporting.
/// - `ESC[?1006l`, `ESC[?1003l`, `ESC[?1002l`, and `ESC[?1000l` disable mouse reporting.
/// - `ESC[?7h` enables autowrap.
/// - `ESC[?25h` makes the cursor visible.
/// - `ESC[0m` resets SGR attributes.
/// - `ESC[r` resets the scroll region.
/// - `ESC>` selects the normal keypad.
pub const EMULATOR_RESET: &str = concat!(
    "\x1b[<u",
    "\x1b[?1049l",
    "\x1b[?2004l",
    "\x1b[?1004l",
    "\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l",
    "\x1b[?7h",
    "\x1b[?25h",
    "\x1b[0m",
    "\x1b[r",
    "\x1b>",
);

/// Reports the raw-mode damage that cannot be present at a shell prompt.
pub fn termios_damaged(input: InputFlags, output: OutputFlags, local: LocalFlags) -> bool {
    !input.contains(InputFlags::ICRNL)
        || !output.contains(OutputFlags::OPOST)
        || !local.contains(LocalFlags::ICANON)
        || !local.contains(LocalFlags::ISIG)
        || !local.contains(LocalFlags::ECHO)
}

/// Repairs shell-critical flags while preserving every other terminal setting.
pub fn sanitize_flags(
    input: InputFlags,
    output: OutputFlags,
    local: LocalFlags,
) -> (InputFlags, OutputFlags, LocalFlags) {
    (
        input | InputFlags::ICRNL,
        output | OutputFlags::OPOST | OutputFlags::ONLCR,
        local
            | LocalFlags::ICANON
            | LocalFlags::ISIG
            | LocalFlags::ECHO
            | LocalFlags::ECHOE
            | LocalFlags::ECHOK
            | LocalFlags::IEXTEN,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SANE_INPUT: InputFlags = InputFlags::ICRNL;
    const SANE_OUTPUT: OutputFlags = OutputFlags::OPOST.union(OutputFlags::ONLCR);
    const SANE_LOCAL: LocalFlags = LocalFlags::ICANON
        .union(LocalFlags::ISIG)
        .union(LocalFlags::ECHO)
        .union(LocalFlags::ECHOE)
        .union(LocalFlags::ECHOK)
        .union(LocalFlags::IEXTEN);

    #[test]
    fn damage_detection_requires_each_shell_critical_flag() {
        assert!(!termios_damaged(SANE_INPUT, SANE_OUTPUT, SANE_LOCAL));
        assert!(termios_damaged(
            SANE_INPUT - InputFlags::ICRNL,
            SANE_OUTPUT,
            SANE_LOCAL
        ));
        assert!(termios_damaged(
            SANE_INPUT,
            SANE_OUTPUT - OutputFlags::OPOST,
            SANE_LOCAL
        ));
        for flag in [LocalFlags::ICANON, LocalFlags::ISIG, LocalFlags::ECHO] {
            assert!(termios_damaged(SANE_INPUT, SANE_OUTPUT, SANE_LOCAL - flag));
        }
    }

    #[test]
    fn sanitize_is_idempotent_and_repairs_raw_flags() {
        assert_eq!(
            sanitize_flags(SANE_INPUT, SANE_OUTPUT, SANE_LOCAL),
            (SANE_INPUT, SANE_OUTPUT, SANE_LOCAL)
        );
        assert_eq!(
            sanitize_flags(
                InputFlags::empty(),
                OutputFlags::empty(),
                LocalFlags::empty()
            ),
            (SANE_INPUT, SANE_OUTPUT, SANE_LOCAL)
        );
    }

    #[test]
    fn emulator_reset_bytes_cover_terminal_modes() {
        assert_eq!(
            EMULATOR_RESET,
            "\x1b[<u\x1b[?1049l\x1b[?2004l\x1b[?1004l\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?7h\x1b[?25h\x1b[0m\x1b[r\x1b>"
        );
    }
}
