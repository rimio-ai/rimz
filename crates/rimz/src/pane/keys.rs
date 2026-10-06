//! Named key presses for pane input.
//!
//! Literal text and control keys travel on separate channels. `send_keys`
//! types bytes as text; `send_key` asks the backend to press a terminal key.

use std::str::FromStr;

/// Bracketed-paste open marker (`ESC[200~`). Wraps injected text so an agent
/// composer takes it as one pasted block and a following Enter reads as a
/// submit keystroke, not a folded newline.
pub const BRACKET_PASTE_OPEN: &str = "\u{1b}[200~";
/// Bracketed-paste close marker (`ESC[201~`).
pub const BRACKET_PASTE_CLOSE: &str = "\u{1b}[201~";

/// Encode logical line endings the way a terminal carries them inside a
/// bracketed paste.
///
/// Agent composers normalize CR back to a newline but drop a bare LF. Real
/// terminal pastes (including tmux's `paste-buffer`) therefore carry CR, with
/// CRLF collapsed so one logical newline stays one composer newline.
pub(crate) fn paste_payload(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r")
}

macro_rules! named_keys {
    ($($key:ident: $name:literal $(| $alias:literal)* => $tmux:literal, $bytes:literal;)*) => {
        /// Small named-key vocabulary RimZ exposes for pane automation.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum NamedKey { $($key,)* }

        impl NamedKey {
            /// Canonical spellings shared by parsing, errors, and command help.
            pub const NAMES: &'static [&'static str] = &[$($name,)*];

            #[cfg(test)]
            const ALL: &'static [(Self, &'static str)] = &[$((Self::$key, $name),)*];

            pub(crate) fn tmux_name(self) -> &'static str {
                match self { $(Self::$key => $tmux,)* }
            }

            pub fn write_bytes(self) -> &'static [u8] {
                match self { $(Self::$key => $bytes,)* }
            }
        }

        impl FromStr for NamedKey {
            type Err = UnknownKey;

            fn from_str(raw: &str) -> Result<Self, Self::Err> {
                let normalized = raw.trim().to_ascii_lowercase().replace(['_', '+', ' '], "-");
                match normalized.as_str() {
                    $($name $(| $alias)* => Ok(Self::$key),)*
                    _ => Err(UnknownKey(raw.to_owned())),
                }
            }
        }
    };
}

named_keys! {
    Enter: "enter" | "return" => "Enter", b"\r";
    Escape: "escape" | "esc" => "Escape", b"\x1b";
    Tab: "tab" => "Tab", b"\t";
    ShiftTab: "shift-tab" | "backtab" | "btab" => "BTab", b"\x1b[Z";
    Backspace: "backspace" | "bspace" | "bs" => "BSpace", b"\x7f";
    Up: "up" => "Up", b"\x1b[A";
    Down: "down" => "Down", b"\x1b[B";
    Left: "left" => "Left", b"\x1b[D";
    Right: "right" => "Right", b"\x1b[C";
    CtrlC: "ctrl-c" | "control-c" | "c-c" => "C-c", b"\x03";
    CtrlD: "ctrl-d" | "control-d" | "c-d" => "C-d", b"\x04";
    CtrlU: "ctrl-u" | "control-u" | "c-u" => "C-u", b"\x15";
    Space: "space" => "Space", b" ";
    Delete: "delete" | "del" => "DC", b"\x1b[3~";
    Home: "home" => "Home", b"\x1b[H";
    End: "end" => "End", b"\x1b[F";
    PageUp: "page-up" | "pgup" => "PPage", b"\x1b[5~";
    PageDown: "page-down" | "pgdn" => "NPage", b"\x1b[6~";
    CtrlA: "ctrl-a" | "control-a" | "c-a" => "C-a", b"\x01";
    CtrlE: "ctrl-e" | "control-e" | "c-e" => "C-e", b"\x05";
    CtrlL: "ctrl-l" | "control-l" | "c-l" => "C-l", b"\x0c";
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("unknown key `{0}`; expected {names}", names = NamedKey::NAMES.join(", "))]
pub struct UnknownKey(pub String);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_aliases_and_maps_to_backend_shapes() {
        assert_eq!("Esc".parse::<NamedKey>().unwrap(), NamedKey::Escape);
        assert_eq!("ctrl_c".parse::<NamedKey>().unwrap(), NamedKey::CtrlC);
        assert_eq!("control+d".parse::<NamedKey>().unwrap(), NamedKey::CtrlD);

        assert_eq!(NamedKey::Up.tmux_name(), "Up");
        assert_eq!(NamedKey::CtrlC.tmux_name(), "C-c");
        assert_eq!(NamedKey::Enter.write_bytes(), b"\r");
        assert_eq!(NamedKey::Up.write_bytes(), b"\x1b[A");
        assert_eq!(NamedKey::ShiftTab.write_bytes(), b"\x1b[Z");
        assert_eq!(NamedKey::Backspace.write_bytes(), b"\x7f");
    }

    #[test]
    fn navigation_and_control_keys_have_terminal_mappings() {
        for (names, tmux, bytes) in [
            ("space", "Space", &b" "[..]),
            ("delete del", "DC", &b"\x1b[3~"[..]),
            ("home", "Home", &b"\x1b[H"[..]),
            ("end", "End", &b"\x1b[F"[..]),
            ("page-up pgup", "PPage", &b"\x1b[5~"[..]),
            ("page-down pgdn", "NPage", &b"\x1b[6~"[..]),
            ("ctrl-a control-a c-a", "C-a", &b"\x01"[..]),
            ("ctrl-e control-e c-e", "C-e", &b"\x05"[..]),
            ("ctrl-l control-l c-l", "C-l", &b"\x0c"[..]),
        ] {
            for name in names.split_whitespace() {
                let key = name.parse::<NamedKey>();
                assert!(key.is_ok(), "{name}: {key:?}");
                let key = key.unwrap();
                assert_eq!(key.tmux_name(), tmux, "{name}");
                assert_eq!(key.write_bytes(), bytes, "{name}");
            }
        }
    }

    #[test]
    fn unknown_key_lists_the_whole_vocabulary() {
        let error = "unknown".parse::<NamedKey>().unwrap_err().to_string();
        for &(key, name) in NamedKey::ALL {
            assert_eq!(name.parse::<NamedKey>().unwrap(), key);
            assert!(error.contains(name), "{name}: {error}");
            assert!(!key.tmux_name().is_empty() && !key.write_bytes().is_empty());
        }
        for name in "enter escape tab shift-tab backspace up down left right ctrl-c ctrl-d ctrl-u space delete home end page-up page-down ctrl-a ctrl-e ctrl-l".split_whitespace() {
            assert!(error.contains(name), "{name}: {error}");
        }
    }

    #[test]
    fn bracketed_paste_markers_are_the_csi_byte_sequences() {
        // A typo in either marker would break submit on every agent; pin the
        // exact bytes both backends emit.
        assert_eq!(BRACKET_PASTE_OPEN.as_bytes(), &[27, 91, 50, 48, 48, 126]);
        assert_eq!(BRACKET_PASTE_CLOSE.as_bytes(), &[27, 91, 50, 48, 49, 126]);
    }

    #[test]
    fn paste_payload_normalizes_logical_line_endings_to_carriage_return() {
        assert_eq!(paste_payload("a\nb\r\nc\rd"), "a\rb\rc\rd");
    }
}
