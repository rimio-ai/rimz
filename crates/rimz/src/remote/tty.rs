//! Pure terminal-state hygiene for remote SSH attaches.

use nix::sys::termios::{InputFlags, LocalFlags, OutputFlags};

/// Marks the host's handoff to its multiplexer client on a supervised attach.
pub const ROOM_OPEN_SENTINEL: &str = "\x1b]7717;rimz-room-open\x07";

/// Device Status Report query used to fence replies from an earlier client.
pub const STATUS_QUERY: &str = "\x1b[5n";

/// Recognizes a Device Status Report reply one byte at a time.
///
/// DA1 cannot be the fence because an attaching tmux client's own outstanding
/// DA1 reply is indistinguishable from the fence reply. DSR 5 is not sent by
/// the client, and its reply shares no shape with DA1, DA2, XTVERSION, or a
/// terminfo key sequence. Both ready (`ESC[0n`) and malfunction (`ESC[3n`)
/// replies match; private-parameter forms such as `ESC[?6n` do not. A reply
/// arriving after the settle cap remains the documented bounded residual.
#[derive(Debug, Default)]
pub struct StatusReplyScanner {
    state: StatusReplyState,
}

#[derive(Debug, Default)]
enum StatusReplyState {
    #[default]
    Ground,
    Escape,
    Csi,
    Parameters,
}

impl StatusReplyScanner {
    /// Returns `true` only for the byte completing a status reply.
    pub fn feed(&mut self, byte: u8) -> bool {
        use StatusReplyState::{Csi, Escape, Ground, Parameters};

        if matches!((&self.state, byte), (Parameters, b'n')) {
            self.state = Ground;
            return true;
        }
        self.state = match (&self.state, byte) {
            (_, b'\x1b') => Escape,
            (Escape, b'[') => Csi,
            (Csi, b'0'..=b'9') => Parameters,
            (Parameters, b'0'..=b'9') => Parameters,
            _ => Ground,
        };
        false
    }
}

/// A significant boundary in a supervised host's output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelayEvent {
    /// Ordinary output, or an incomplete escape sequence.
    None,
    /// The first pre-room text, including an overflowing escape hold.
    Speech,
    /// The host is about to run its multiplexer client.
    RoomOpen,
}

/// The host stream's current screen, independent of the local panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamScreen {
    /// The main screen is known to be active.
    Main,
    /// An alternate screen is active.
    Alternate,
    /// A non-saving alternate-screen exit or output failure lost the state.
    Unknown,
}

/// Bounded, byte-stream parser for supervised SSH output.
#[derive(Debug)]
pub struct RelayScanner {
    state: EscapeState,
    sequence: Vec<u8>,
    held: Vec<u8>,
    spilled: bool,
    watching: bool,
    room_opened: bool,
    screen: StreamScreen,
    entered_alternate: bool,
    column_zero: Option<bool>,
    saved_column_zero: Option<bool>,
    tail: Vec<u8>,
    csi: CsiMode,
}

const RELAY_HOLD_BYTES: usize = 16 * 1024;
const RELAY_TAIL_BYTES: usize = 8 * 1024;

#[derive(Debug, PartialEq, Eq)]
enum EscapeState {
    Ground,
    Escape,
    Intermediate,
    Csi,
    String,
    StringEscape,
}

#[derive(Debug, Default)]
struct CsiMode {
    private: bool,
    first: bool,
    valid: bool,
    value: u16,
    mode: Option<u16>,
}

impl CsiMode {
    fn feed(&mut self, byte: u8) {
        match byte {
            b'?' if self.first => self.private = true,
            b'0'..=b'9' => {
                self.value = self
                    .value
                    .saturating_mul(10)
                    .saturating_add(u16::from(byte - b'0'))
            }
            b';' | b'h' | b'l' => {
                if matches!(self.value, 47 | 1047 | 1049) {
                    self.mode = Some(self.value);
                }
                self.value = 0;
            }
            _ => self.valid = false,
        }
        self.first = false;
    }
}

impl Default for RelayScanner {
    fn default() -> Self {
        Self {
            state: EscapeState::Ground,
            sequence: Vec::new(),
            held: Vec::new(),
            spilled: false,
            watching: true,
            room_opened: false,
            screen: StreamScreen::Main,
            entered_alternate: false,
            column_zero: Some(true),
            saved_column_zero: None,
            tail: Vec::new(),
            csi: CsiMode::default(),
        }
    }
}

impl RelayScanner {
    /// Appends terminal bytes, stripping the room sentinel and holding pre-room escapes.
    pub fn feed(&mut self, byte: u8, output: &mut Vec<u8>) -> RelayEvent {
        use EscapeState::{Csi, Escape, Ground, Intermediate, String, StringEscape};

        if self.state == Ground && byte != b'\x1b' {
            let speech = byte >= b' ' && byte != b'\x7f' || matches!(byte, b'\r' | b'\n' | b'\t');
            if self.screen != StreamScreen::Alternate && speech {
                self.column_zero = Some(matches!(byte, b'\r' | b'\n'));
            }
            if self.room_opened && !self.entered_alternate && speech {
                if self.tail.len() == RELAY_TAIL_BYTES {
                    let mut cut = RELAY_TAIL_BYTES / 2;
                    while cut < self.tail.len() && self.tail[cut] & 0xc0 == 0x80 {
                        cut += 1;
                    }
                    self.tail.drain(..cut);
                }
                self.tail.push(byte);
            }
            let event = if self.watching && speech {
                self.watching = false;
                RelayEvent::Speech
            } else {
                RelayEvent::None
            };
            self.sequence.push(byte);
            return self.release(output, event);
        }

        let mut complete = false;
        self.state = match (&self.state, byte) {
            (String | StringEscape, b'\x07') | (StringEscape, b'\\') => {
                complete = true;
                Ground
            }
            (String | StringEscape, b'\x1b') => StringEscape,
            (String | StringEscape, _) => String,
            (_, b'\x1b') => {
                self.release(output, RelayEvent::None);
                self.spilled = false;
                Escape
            }
            (Escape, b'[') => {
                self.csi = CsiMode {
                    first: true,
                    valid: true,
                    ..CsiMode::default()
                };
                Csi
            }
            (Escape, b']' | b'P' | b'_' | b'^' | b'X') => String,
            (Escape | Intermediate, b'\x20'..=b'\x2f') => Intermediate,
            (Csi, byte) => {
                self.csi.feed(byte);
                if (b'\x40'..=b'\x7e').contains(&byte) {
                    if self.csi.private
                        && self.csi.valid
                        && let Some(mode) = self.csi.mode
                    {
                        self.select_screen(mode, byte == b'h');
                    }
                    complete = true;
                    Ground
                } else {
                    Csi
                }
            }
            _ => {
                complete = true;
                Ground
            }
        };
        self.sequence.push(byte);
        if complete {
            let event = if !self.spilled && self.sequence == ROOM_OPEN_SENTINEL.as_bytes() {
                self.sequence.clear();
                self.watching = false;
                self.room_opened = true;
                RelayEvent::RoomOpen
            } else {
                RelayEvent::None
            };
            self.spilled = false;
            return self.release(output, event);
        }
        if self.spilled {
            output.append(&mut self.sequence);
            return RelayEvent::None;
        }
        if self.held.len() + self.sequence.len() >= RELAY_HOLD_BYTES {
            let event = if self.watching {
                RelayEvent::Speech
            } else {
                RelayEvent::None
            };
            self.watching = false;
            self.spilled = true;
            return self.release(output, event);
        }
        RelayEvent::None
    }

    /// Releases any incomplete escape and pre-room hold at the end of the stream.
    pub fn finish(&mut self, output: &mut Vec<u8>) {
        self.watching = false;
        self.release(output, RelayEvent::None);
    }

    fn release(&mut self, output: &mut Vec<u8>, event: RelayEvent) -> RelayEvent {
        if self.watching {
            self.held.append(&mut self.sequence);
            if self.held.len() < RELAY_HOLD_BYTES {
                return event;
            }
            self.watching = false;
            output.append(&mut self.held);
            return RelayEvent::Speech;
        }
        output.append(&mut self.held);
        output.append(&mut self.sequence);
        event
    }

    fn select_screen(&mut self, mode: u16, enter: bool) {
        if enter {
            if mode == 1049 && self.screen != StreamScreen::Alternate {
                self.saved_column_zero = self.column_zero;
            }
            if mode != 1049 {
                self.saved_column_zero = None;
                self.column_zero = None;
            }
            self.screen = StreamScreen::Alternate;
            self.entered_alternate = true;
            self.watching = false;
            self.tail.clear();
            return;
        }
        self.screen = if mode == 1049 {
            StreamScreen::Main
        } else {
            StreamScreen::Unknown
        };
        self.column_zero = if mode == 1049 {
            self.saved_column_zero
        } else {
            None
        };
    }

    /// The last screen selected by the host.
    pub fn screen(&self) -> StreamScreen {
        self.screen
    }

    /// Whether the host ever selected an alternate screen.
    pub fn entered_alternate(&self) -> bool {
        self.entered_alternate
    }

    /// Whether the sentinel or a first alternate-screen entry ended the pre-room watch.
    pub fn room_opened(&self) -> bool {
        self.room_opened || self.entered_alternate
    }

    /// Whether the main screen's cursor is known to be at column zero.
    pub fn at_line_start(&self) -> bool {
        self.column_zero == Some(true)
    }

    /// Text after the sentinel, discarded as soon as a multiplexer paints.
    pub fn unpainted_tail(&self) -> &[u8] {
        &self.tail
    }
}

/// Restores emulator modes that an interrupted remote TUI can leave behind.
///
/// - `ESC[<9999u` drains kitty keyboard enhancement from the main-screen stack.
/// - `ESC[?2004l` disables bracketed paste.
/// - `ESC[?1004l` disables focus reporting.
/// - `ESC[?1006l`, `ESC[?1003l`, `ESC[?1002l`, and `ESC[?1000l` disable mouse reporting.
/// - `ESC[?7h` enables autowrap.
/// - `ESC[?25h` makes the cursor visible.
/// - `ESC7 ESC[r ESC8` resets the scroll region without moving the cursor.
/// - `ESC[0m` resets SGR attributes after DECRC restores the saved attributes.
/// - `ESC>` selects the normal keypad.
pub const EMULATOR_RESET: &str = concat!(
    "\x1b[<9999u",
    "\x1b[?2004l",
    "\x1b[?1004l",
    "\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l",
    "\x1b[?7h",
    "\x1b[?25h",
    "\x1b7\x1b[r\x1b8",
    "\x1b[0m",
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

    #[test]
    fn room_open_sentinel_uses_private_osc() {
        assert_eq!(ROOM_OPEN_SENTINEL, "\x1b]7717;rimz-room-open\x07");
    }

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
            "\x1b[<9999u\x1b[?2004l\x1b[?1004l\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?7h\x1b[?25h\x1b7\x1b[r\x1b8\x1b[0m\x1b>"
        );
    }

    fn scan(scanner: &mut RelayScanner, bytes: &[u8], output: &mut Vec<u8>) -> Vec<RelayEvent> {
        bytes
            .iter()
            .filter_map(|byte| {
                let event = scanner.feed(*byte, output);
                (event != RelayEvent::None).then_some(event)
            })
            .collect()
    }

    #[test]
    fn relay_strips_the_sentinel_at_every_split_and_holds_escapes() {
        let bytes = format!("\x1b[31m{ROOM_OPEN_SENTINEL}\x1b[?1000;1049hpaint");
        for split in 0..=bytes.len() {
            let mut scanner = RelayScanner::default();
            let mut output = Vec::new();
            let mut events = scan(&mut scanner, &bytes.as_bytes()[..split], &mut output);
            events.extend(scan(&mut scanner, &bytes.as_bytes()[split..], &mut output));
            scanner.finish(&mut output);
            assert_eq!(events, [RelayEvent::RoomOpen], "split {split}");
            assert_eq!(output, b"\x1b[31m\x1b[?1000;1049hpaint");
            assert!(scanner.room_opened());
            assert!(scanner.entered_alternate());
            assert_eq!(scanner.screen(), StreamScreen::Alternate);
        }
    }

    #[test]
    fn relay_only_calls_ground_text_speech_and_preserves_byte_order() {
        for escape in [
            "\x1b]title words\x07",
            "\x1bPwords\x1b\\",
            "\x1b_words\x07",
            "\x1b^words\x1b\\",
            "\x1bXwords\x07",
            "\x1b(B",
        ] {
            let mut scanner = RelayScanner::default();
            let mut output = Vec::new();
            assert!(scan(&mut scanner, escape.as_bytes(), &mut output).is_empty());
            assert!(output.is_empty());
            assert_eq!(
                scan(&mut scanner, "é".as_bytes(), &mut output),
                [RelayEvent::Speech]
            );
            assert_eq!(output, format!("{escape}é").as_bytes());
        }
        for byte in *b"\r\n\t ~" {
            assert_eq!(
                RelayScanner::default().feed(byte, &mut Vec::new()),
                RelayEvent::Speech
            );
        }
    }

    #[test]
    fn relay_escape_hold_is_bounded_even_inside_an_unterminated_string() {
        let mut scanner = RelayScanner::default();
        let mut output = Vec::new();
        let bytes = [b"\x1b]".as_slice(), &vec![b'x'; 64 * 1024]].concat();
        assert_eq!(
            scan(&mut scanner, &bytes, &mut output),
            [RelayEvent::Speech]
        );
        scanner.finish(&mut output);
        assert_eq!(output, bytes);
    }

    #[test]
    fn relay_tracks_saving_and_non_saving_screen_modes_and_last_parameter() {
        for mode in [47, 1047, 1049] {
            let mut scanner = RelayScanner::default();
            let mut output = Vec::new();
            scan(&mut scanner, b"command", &mut output);
            scan(
                &mut scanner,
                format!("\x1b[?1000;{mode}hpaint\r\n\x1b[?{mode}l").as_bytes(),
                &mut output,
            );
            assert!(scanner.entered_alternate());
            assert_eq!(
                scanner.screen(),
                if mode == 1049 {
                    StreamScreen::Main
                } else {
                    StreamScreen::Unknown
                }
            );
            assert!(!scanner.at_line_start());
            scan(&mut scanner, b"\r\n", &mut output);
            assert!(scanner.at_line_start());
        }
        let mut scanner = RelayScanner::default();
        scan(&mut scanner, b"\x1b[?1049h\x1b[?47;1049l", &mut Vec::new());
        assert_eq!(scanner.screen(), StreamScreen::Main);
        assert!(scanner.at_line_start());
    }

    #[test]
    fn relay_non_saving_entry_invalidates_the_main_column_and_earlier_save() {
        for mode in [47, 1047] {
            let mut scanner = RelayScanner::default();
            let mut output = Vec::new();
            scan(&mut scanner, b"\x1b[?1049h\x1b[?1049l", &mut output);
            assert!(scanner.at_line_start());
            scan(
                &mut scanner,
                format!("\x1b[?{mode}h").as_bytes(),
                &mut output,
            );
            assert!(
                !scanner.at_line_start(),
                "mode {mode} cannot save the main cursor"
            );
            scan(&mut scanner, b"\x1b[?1049l", &mut output);
            assert!(
                !scanner.at_line_start(),
                "an older save cannot establish the cursor"
            );
        }
    }

    #[test]
    fn relay_alt_entry_without_sentinel_ends_watching_and_tail_drops_escapes() {
        let mut scanner = RelayScanner::default();
        let mut output = Vec::new();
        assert!(scan(&mut scanner, b"\x1b[?1049hpaint", &mut output).is_empty());
        assert_eq!(output, b"\x1b[?1049hpaint");
        let mut scanner = RelayScanner::default();
        scan(
            &mut scanner,
            format!("{ROOM_OPEN_SENTINEL}\x1b[31merror\x1b]hidden\x07\r\n").as_bytes(),
            &mut output,
        );
        assert_eq!(scanner.unpainted_tail(), b"error\r\n");
        scan(&mut scanner, &vec![b'x'; 32 * 1024], &mut output);
        assert!(scanner.unpainted_tail().len() <= 8192);
        scan(&mut scanner, b"\x1b[?1049h", &mut output);
        assert!(scanner.unpainted_tail().is_empty());
    }

    #[test]
    fn relay_unpainted_tail_evicts_only_at_utf8_boundaries() {
        let text = "€".repeat(RELAY_TAIL_BYTES);
        let mut scanner = RelayScanner::default();
        let mut output = Vec::new();
        scan(&mut scanner, ROOM_OPEN_SENTINEL.as_bytes(), &mut output);
        scan(&mut scanner, text.as_bytes(), &mut output);

        let tail = scanner.unpainted_tail();
        assert!(tail.len() <= RELAY_TAIL_BYTES);
        assert!(
            std::str::from_utf8(tail).is_ok(),
            "tail eviction must not leave a partial UTF-8 character: {:?}",
            &tail[..3]
        );
        assert!(text.as_bytes().ends_with(tail));
        assert_eq!(output, text.as_bytes());
    }

    #[test]
    fn status_scanner_accepts_ready_and_malfunction_replies() {
        for reply in [b"\x1b[0n".as_slice(), b"\x1b[3n"] {
            let mut scanner = StatusReplyScanner::default();
            let completions = completion_indices(&mut scanner, reply);

            assert_eq!(completions, [reply.len() - 1]);
        }
    }

    #[test]
    fn status_scanner_rejects_mux_replies_private_status_and_user_bytes() {
        let mut scanner = StatusReplyScanner::default();
        for bytes in [
            b"\x1b[?62;22;52c".as_slice(),
            b"\x1b[>1;10;0c".as_slice(),
            b"\x1bP>|ghostty 1.3.1\x1b\\",
            b"\x1b[?6n",
            b"\x1b[24;1R",
            b"RIMZ-USER-MARKER",
        ] {
            assert!(!bytes.iter().any(|byte| scanner.feed(*byte)));
        }
    }

    #[test]
    fn status_scanner_completes_after_stale_reply_trio_on_exact_last_byte() {
        let bytes = b"\x1b[?62;22;52c\x1b[>1;10;0c\x1bP>|ghostty 1.3.1\x1b\\\x1b[0n";
        let mut scanner = StatusReplyScanner::default();
        let completions = completion_indices(&mut scanner, bytes);

        assert_eq!(completions, [bytes.len() - 1]);
    }

    fn completion_indices(scanner: &mut StatusReplyScanner, bytes: &[u8]) -> Vec<usize> {
        bytes
            .iter()
            .enumerate()
            .filter_map(|(index, byte)| scanner.feed(*byte).then_some(index))
            .collect()
    }
}
