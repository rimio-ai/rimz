//! Named key presses for pane input.
//!
//! Literal text and control keys travel on separate channels. `send_keys`
//! types bytes as text; `send_key` asks the backend to press a terminal key.

use std::str::FromStr;

/// Small named-key vocabulary Rimz exposes for pane automation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamedKey {
    Enter,
    Escape,
    Tab,
    Backspace,
    Up,
    Down,
    Left,
    Right,
    CtrlC,
    CtrlD,
    CtrlU,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "unknown key `{0}`; expected enter, escape, tab, backspace, up, down, left, right, ctrl-c, ctrl-d, or ctrl-u"
)]
pub struct UnknownKey(pub String);

impl NamedKey {
    pub fn tmux_name(self) -> &'static str {
        match self {
            Self::Enter => "Enter",
            Self::Escape => "Escape",
            Self::Tab => "Tab",
            Self::Backspace => "BSpace",
            Self::Up => "Up",
            Self::Down => "Down",
            Self::Left => "Left",
            Self::Right => "Right",
            Self::CtrlC => "C-c",
            Self::CtrlD => "C-d",
            Self::CtrlU => "C-u",
        }
    }

    pub fn write_bytes(self) -> &'static [u8] {
        match self {
            Self::Enter => b"\r",
            Self::Escape => b"\x1b",
            Self::Tab => b"\t",
            Self::Backspace => b"\x7f",
            Self::Up => b"\x1b[A",
            Self::Down => b"\x1b[B",
            Self::Right => b"\x1b[C",
            Self::Left => b"\x1b[D",
            Self::CtrlC => b"\x03",
            Self::CtrlD => b"\x04",
            Self::CtrlU => b"\x15",
        }
    }
}

impl FromStr for NamedKey {
    type Err = UnknownKey;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let normalized = raw
            .trim()
            .to_ascii_lowercase()
            .replace(['_', '+', ' '], "-");
        match normalized.as_str() {
            "enter" | "return" => Ok(Self::Enter),
            "escape" | "esc" => Ok(Self::Escape),
            "tab" => Ok(Self::Tab),
            "backspace" | "bspace" | "bs" => Ok(Self::Backspace),
            "up" => Ok(Self::Up),
            "down" => Ok(Self::Down),
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            "ctrl-c" | "control-c" | "c-c" => Ok(Self::CtrlC),
            "ctrl-d" | "control-d" | "c-d" => Ok(Self::CtrlD),
            "ctrl-u" | "control-u" | "c-u" => Ok(Self::CtrlU),
            _ => Err(UnknownKey(raw.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_aliases_case_insensitively() {
        assert_eq!("Esc".parse::<NamedKey>().unwrap(), NamedKey::Escape);
        assert_eq!("ctrl_c".parse::<NamedKey>().unwrap(), NamedKey::CtrlC);
        assert_eq!("control+d".parse::<NamedKey>().unwrap(), NamedKey::CtrlD);
        assert!("page-down".parse::<NamedKey>().is_err());
    }

    #[test]
    fn maps_to_backend_shapes() {
        assert_eq!(NamedKey::Up.tmux_name(), "Up");
        assert_eq!(NamedKey::CtrlC.tmux_name(), "C-c");
        assert_eq!(NamedKey::Enter.write_bytes(), b"\r");
        assert_eq!(NamedKey::Up.write_bytes(), b"\x1b[A");
        assert_eq!(NamedKey::Backspace.write_bytes(), b"\x7f");
    }
}
