//! Pure input side of remote paste: the keyboard scanner that finds Ctrl+V and
//! bracketed pastes in the bytes typed into a supervised attach, and the
//! Ghostty drop format a pasted path list is read from and written in.
//!
//! The scanner holds bytes only while they can still become a trigger, and
//! hands back every byte it read, in order, as forwarded bytes, a Ctrl+V key,
//! or a whole held paste. The driver in `cli/remote` owns the clock: it calls
//! [`InputScanner::expire`] once [`InputScanner::hold_deadline`] passes with
//! no further input.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

const PASTE_OPEN: &[u8] = b"\x1b[200~";
const PASTE_CLOSE: &[u8] = b"\x1b[201~";
const CTRL_V: u8 = 0x16;
const ESC: u8 = 0x1b;
/// A held escape that would be an Esc key press is released after this pause.
const KEY_HOLD: Duration = Duration::from_millis(50);
/// A held paste that stops arriving for this long is no longer a drop.
const PASTE_HOLD: Duration = Duration::from_millis(200);
const PASTE_HOLD_BYTES: usize = 64 * 1024;
const CSI_HOLD_BYTES: usize = 32;
/// Bytes Ghostty backslash-escapes in a dropped path (`Shell::escape`, v1.4.0).
const DROP_ESCAPED: &[u8] = b"\\ ()[]{}<>\"'`!#$&;|*?\t";
/// Kitty keyboard modifier bits for Caps Lock and Num Lock, which a trigger ignores.
const LOCK_BITS: u32 = 64 | 128;
const CTRL_BIT: u32 = 4;

/// What one stretch of typed input amounts to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    /// Bytes to forward unchanged.
    Forward(Vec<u8>),
    /// One Ctrl+V key press or repeat outside a paste, in the bytes it came as.
    CtrlV(Vec<u8>),
    /// A whole bracketed paste whose payload may be a path list, markers included.
    Paste(Vec<u8>),
}

impl Input {
    /// The bytes this input arrived as, which forwarding it unchanged writes.
    pub fn bytes(&self) -> &[u8] {
        match self {
            Self::Forward(bytes) | Self::CtrlV(bytes) | Self::Paste(bytes) => bytes,
        }
    }
}

/// The payload of a held paste, between its markers.
pub fn paste_payload(paste: &[u8]) -> &[u8] {
    paste
        .strip_prefix(PASTE_OPEN)
        .and_then(|rest| rest.strip_suffix(PASTE_CLOSE))
        .unwrap_or(paste)
}

#[derive(Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    Escape,
    Csi,
    HeldPaste,
    StreamedPaste,
}

/// Bounded byte-state scanner over the keyboard input of a supervised attach.
#[derive(Debug, Default)]
pub struct InputScanner {
    state: State,
    held: Vec<u8>,
    close_matched: usize,
}

impl InputScanner {
    /// Scans `bytes`, appending what they amount to; partial triggers stay held.
    pub fn feed(&mut self, bytes: &[u8], out: &mut Vec<Input>) {
        for &byte in bytes {
            self.feed_byte(byte, out);
        }
    }

    /// Releases held bytes after their pause: an escape forwards as typed, and
    /// a paste forwards what it holds and streams from there on.
    pub fn expire(&mut self, out: &mut Vec<Input>) {
        match self.state {
            State::Escape | State::Csi => {
                self.spill(out);
                self.state = State::Ground;
            }
            State::HeldPaste => {
                self.spill(out);
                self.state = State::StreamedPaste;
            }
            State::Ground | State::StreamedPaste => {}
        }
    }

    /// How long held bytes may wait for the next byte before [`Self::expire`].
    pub fn hold_deadline(&self) -> Option<Duration> {
        match self.state {
            State::Escape | State::Csi => Some(KEY_HOLD),
            State::HeldPaste => Some(PASTE_HOLD),
            State::Ground | State::StreamedPaste => None,
        }
    }

    /// Whether a bracketed paste is open, so nothing may be written into it.
    pub fn in_paste(&self) -> bool {
        matches!(self.state, State::HeldPaste | State::StreamedPaste)
    }

    fn feed_byte(&mut self, byte: u8, out: &mut Vec<Input>) {
        match self.state {
            State::Ground => match byte {
                ESC => {
                    self.held.push(byte);
                    self.state = State::Escape;
                }
                CTRL_V => out.push(Input::CtrlV(vec![byte])),
                _ => forward(out, &[byte]),
            },
            // Alt+Ctrl+V in the legacy encoding forwards whole, as no trigger.
            State::Escape if byte == CTRL_V => {
                self.held.push(byte);
                self.spill(out);
                self.state = State::Ground;
            }
            State::Escape if byte == b'[' => {
                self.held.push(byte);
                self.state = State::Csi;
            }
            State::Csi if is_parameter(byte) && self.held.len() < CSI_HOLD_BYTES => {
                self.held.push(byte);
            }
            State::Csi if matches!(byte, b'u' | b'~') => {
                self.held.push(byte);
                self.finish_csi(out);
            }
            State::Escape | State::Csi => {
                self.spill(out);
                self.state = State::Ground;
                self.feed_byte(byte, out);
            }
            State::HeldPaste => self.hold_paste_byte(byte, out),
            State::StreamedPaste => {
                forward(out, &[byte]);
                if self.match_close(byte) {
                    self.state = State::Ground;
                }
            }
        }
    }

    fn finish_csi(&mut self, out: &mut Vec<Input>) {
        if self.held == PASTE_OPEN {
            self.state = State::HeldPaste;
            return;
        }
        let held = std::mem::take(&mut self.held);
        if is_ctrl_v(&held) {
            out.push(Input::CtrlV(held));
        } else {
            forward(out, &held);
        }
        self.state = State::Ground;
    }

    fn hold_paste_byte(&mut self, byte: u8, out: &mut Vec<Input>) {
        let first = self.held.len() == PASTE_OPEN.len();
        if self.close_matched > 0 || (byte == ESC && !first) {
            if PASTE_CLOSE[self.close_matched] == byte {
                self.held.push(byte);
                self.close_matched += 1;
                if self.close_matched == PASTE_CLOSE.len() {
                    self.close_matched = 0;
                    self.state = State::Ground;
                    out.push(Input::Paste(std::mem::take(&mut self.held)));
                }
                return;
            }
        } else if (!first || byte == b'/')
            && !is_control(byte)
            && self.held.len() < PASTE_HOLD_BYTES
        {
            self.held.push(byte);
            return;
        }
        self.close_matched = 0;
        self.spill(out);
        self.state = State::StreamedPaste;
        self.feed_byte(byte, out);
    }

    fn match_close(&mut self, byte: u8) -> bool {
        self.close_matched = if PASTE_CLOSE[self.close_matched] == byte {
            self.close_matched + 1
        } else {
            usize::from(byte == ESC)
        };
        if self.close_matched == PASTE_CLOSE.len() {
            self.close_matched = 0;
            return true;
        }
        false
    }

    fn spill(&mut self, out: &mut Vec<Input>) {
        if !self.held.is_empty() {
            forward(out, &std::mem::take(&mut self.held));
        }
    }
}

fn forward(out: &mut Vec<Input>, bytes: &[u8]) {
    if let Some(Input::Forward(last)) = out.last_mut() {
        last.extend_from_slice(bytes);
    } else {
        out.push(Input::Forward(bytes.to_vec()));
    }
}

fn is_parameter(byte: u8) -> bool {
    byte.is_ascii_digit() || matches!(byte, b';' | b':')
}

fn is_control(byte: u8) -> bool {
    byte < 0x20 || byte == 0x7f
}

/// Ctrl+V as a CSI-u key (`ESC[118;5u`, press or repeat) or an xterm
/// modifyOtherKeys key (`ESC[27;5;118~`), with Ctrl the only modifier held
/// apart from the lock keys.
fn is_ctrl_v(sequence: &[u8]) -> bool {
    let Some((&last, body)) = sequence[2..].split_last() else {
        return false;
    };
    let fields: Vec<Vec<Option<u32>>> = body
        .split(|byte| *byte == b';')
        .map(|field| field.split(|byte| *byte == b':').map(number).collect())
        .collect();
    let first = |index: usize, fallback: u32| {
        fields
            .get(index)
            .and_then(|field| field.first().copied().flatten())
            .unwrap_or(fallback)
    };
    match last {
        b'u' => {
            let event = fields
                .get(1)
                .and_then(|field| field.get(1).copied().flatten())
                .unwrap_or(1);
            first(0, 0) == 118 && ctrl_alone(first(1, 1)) && matches!(event, 1 | 2)
        }
        b'~' => {
            fields.len() == 3 && first(0, 0) == 27 && first(2, 0) == 118 && ctrl_alone(first(1, 1))
        }
        _ => false,
    }
}

fn number(digits: &[u8]) -> Option<u32> {
    std::str::from_utf8(digits).ok()?.parse().ok()
}

fn ctrl_alone(modifiers: u32) -> bool {
    modifiers
        .checked_sub(1)
        .is_some_and(|bits| bits & !LOCK_BITS == CTRL_BIT)
}

/// Reads a paste payload as a Ghostty file drop: absolute paths, backslash
/// escaped, one space between. Every path must pass `usable`; anything else
/// in the payload makes it not a drop.
pub fn parse_drop(payload: &[u8], usable: impl Fn(&Path) -> bool) -> Option<Vec<PathBuf>> {
    let mut paths = Vec::new();
    let mut token = Vec::new();
    let mut bytes = payload.iter().copied();
    loop {
        match bytes.next() {
            Some(b'\\') => token.push(bytes.next()?),
            Some(byte) if byte != b' ' && is_control(byte) => return None,
            Some(byte) if byte != b' ' => token.push(byte),
            end => {
                let path = Path::new(OsStr::from_bytes(&token));
                if !token.starts_with(b"/") || !usable(path) {
                    return None;
                }
                paths.push(path.to_path_buf());
                token.clear();
                if end.is_none() {
                    return Some(paths);
                }
            }
        }
    }
}

/// Writes paths in Ghostty's drop format.
pub fn encode_drop(paths: &[String]) -> String {
    let mut encoded = String::new();
    for (index, path) in paths.iter().enumerate() {
        if index > 0 {
            encoded.push(' ');
        }
        for ch in path.chars() {
            if ch.is_ascii() && DROP_ESCAPED.contains(&(ch as u8)) {
                encoded.push('\\');
            }
            encoded.push(ch);
        }
    }
    encoded
}

/// The bracketed paste that types `paths` into the session as a drop would.
pub fn drop_paste(paths: &[String]) -> Vec<u8> {
    [PASTE_OPEN, encode_drop(paths).as_bytes(), PASTE_CLOSE].concat()
}

#[cfg(test)]
#[path = "paste_tests.rs"]
mod tests;
