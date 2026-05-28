//! Wakeup codec for the sidebar input socket: the pure mapping between
//! crossterm events, the wire strings the input thread sends over the
//! `UnixDatagram`, and the [`Wakeup`] the serve loop dispatches. No
//! `UiState` here — selection and focus handling stay in [`super`].

use std::io;
use std::os::unix::net::UnixDatagram;

use ratatui::crossterm::event::{KeyCode, MouseButton, MouseEventKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Wakeup {
    Tick,
    Resize,
    Key(KeyAction),
    MouseClick { column: u16, row: u16 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum KeyAction {
    Up,
    Down,
    Enter,
    Space,
    Help,
    Digit(u8),
}

pub(super) fn encode_key(code: KeyCode) -> Option<String> {
    let wire = match code {
        KeyCode::Up => "key:up",
        KeyCode::Down => "key:down",
        KeyCode::Enter => "key:enter",
        KeyCode::Char(' ') => "key:space",
        KeyCode::Char('?') => "key:help",
        KeyCode::Char(c @ '1'..='9') => return Some(format!("key:digit:{c}")),
        _ => return None,
    };
    Some(wire.to_owned())
}

pub(super) fn encode_mouse(kind: MouseEventKind, column: u16, row: u16) -> Option<String> {
    match kind {
        MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left) => {
            Some(format!("mouse:left:{column}:{row}"))
        }
        _ => None,
    }
}

pub(super) fn decode_wakeup(bytes: &[u8]) -> Wakeup {
    let raw = std::str::from_utf8(bytes).unwrap_or_default();
    if let Some(mouse) = decode_mouse_click(raw) {
        return mouse;
    }
    if let Some(digit) = raw.strip_prefix("key:digit:")
        && let Ok(n @ 1..=9) = digit.parse::<u8>()
    {
        return Wakeup::Key(KeyAction::Digit(n));
    }
    match raw {
        "resize" => Wakeup::Resize,
        "key:up" => Wakeup::Key(KeyAction::Up),
        "key:down" => Wakeup::Key(KeyAction::Down),
        "key:enter" => Wakeup::Key(KeyAction::Enter),
        "key:space" => Wakeup::Key(KeyAction::Space),
        "key:help" => Wakeup::Key(KeyAction::Help),
        _ => Wakeup::Tick,
    }
}

fn decode_mouse_click(raw: &str) -> Option<Wakeup> {
    let mut parts = raw.split(':');
    match (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) {
        (Some("mouse"), Some("left"), Some(column), Some(row), None) => Some(Wakeup::MouseClick {
            column: column.parse().ok()?,
            row: row.parse().ok()?,
        }),
        _ => None,
    }
}

pub(super) fn wait_for_wakeup(socket: &UnixDatagram) -> io::Result<Wakeup> {
    let mut buf = [0_u8; 4096];
    match socket.recv(&mut buf) {
        Ok(n) => Ok(decode_wakeup(&buf[..n])),
        // Timeout (the tick), or a signal (the resize watcher's SIGWINCH handler
        // interrupts this blocking recv): all are just "redraw now", never fatal.
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(Wakeup::Tick)
        }
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_click_codec_round_trips_left_button_down() {
        let encoded = encode_mouse(MouseEventKind::Down(MouseButton::Left), 4, 7)
            .expect("left button down is encoded");

        assert_eq!(
            decode_wakeup(encoded.as_bytes()),
            Wakeup::MouseClick { column: 4, row: 7 }
        );
        let release = encode_mouse(MouseEventKind::Up(MouseButton::Left), 4, 7)
            .expect("left button release is also a click");
        assert_eq!(
            decode_wakeup(release.as_bytes()),
            Wakeup::MouseClick { column: 4, row: 7 }
        );
    }

    #[test]
    fn digit_keys_round_trip_one_through_nine() {
        for c in '1'..='9' {
            let encoded = encode_key(KeyCode::Char(c)).expect("digit is encoded");
            let n = c.to_digit(10).unwrap() as u8;
            assert_eq!(
                decode_wakeup(encoded.as_bytes()),
                Wakeup::Key(KeyAction::Digit(n))
            );
        }
        // '0' and out-of-range digit wire strings are not selectable rows.
        assert_eq!(encode_key(KeyCode::Char('0')), None);
        assert_eq!(decode_wakeup(b"key:digit:0"), Wakeup::Tick);
    }
}
