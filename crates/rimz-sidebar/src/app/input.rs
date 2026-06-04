//! Wakeup codec for the sidebar input socket: the pure mapping between
//! crossterm events, the wire strings the input thread sends over the
//! `UnixDatagram`, and the [`Wakeup`] the serve loop dispatches. No
//! `UiState` here — selection and focus handling stay in [`super`].

use std::io;
use std::os::unix::net::UnixDatagram;

use ratatui::crossterm::event::{KeyCode, MouseButton, MouseEventKind};
use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Wakeup {
    Tick,
    /// A ledger mutation posted a `ledger_delta` datagram. Distinct from `Tick`
    /// (the poll timeout) so the loop can coalesce a burst of deltas from one
    /// mutation into a single refetch instead of one refetch per event.
    Ledger {
        fresh_panes: bool,
    },
    /// The background fetch worker finished a snapshot and posted
    /// [`SNAPSHOT_WAKEUP`]; the loop folds the result waiting on its result
    /// channel. Keeps the fetch subprocess off the render thread.
    Snapshot,
    /// A resize-triggered sibling-count probe finished. This carries only
    /// pane-list metadata for the self-close latch, never a rendered snapshot.
    SelfCloseProbe,
    /// The tmux control-mode presence watcher saw pane topology change — a
    /// window or split opened/closed. A latency fast path only: the loop pulls
    /// a fresh pane list now instead of waiting out the poll, and a dead
    /// watcher degrades to exactly that poll.
    PanesChanged,
    Resize,
    /// `rimz reload` asks the renderer to re-exec its own binary in place so a
    /// freshly-installed build takes effect without a session rebirth.
    Reload,
    Key(KeyAction),
    MouseClick {
        column: u16,
        row: u16,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum KeyAction {
    Up,
    Down,
    Enter,
    Space,
    Help,
    Dismiss,
    Digit(u8),
}

/// The control word the background fetch worker sends to the loop's wakeup
/// socket once a snapshot is ready to fold. Riding the same socket every other
/// wakeup uses keeps the loop blocking in exactly one place.
pub(super) const SNAPSHOT_WAKEUP: &[u8] = b"snapshot";
pub(super) const SELF_CLOSE_WAKEUP: &[u8] = b"self_close_probe";
/// The control word the tmux presence watcher posts on a topology change.
pub(super) const PANES_CHANGED_WAKEUP: &[u8] = b"panes_changed";

pub(super) fn encode_key(code: KeyCode) -> Option<String> {
    let wire = match code {
        KeyCode::Up => "key:up",
        KeyCode::Down => "key:down",
        KeyCode::Enter => "key:enter",
        KeyCode::Char(' ') => "key:space",
        KeyCode::Char('?') => "key:help",
        KeyCode::Char('x') => "key:dismiss",
        KeyCode::Char(c @ '1'..='9') => return Some(format!("key:digit:{c}")),
        // `r` rides the very `reload` control word `rimz reload` posts, so a
        // keypress and the CLI converge on the one re-exec path in `super`.
        KeyCode::Char('r') => "reload",
        _ => return None,
    };
    Some(wire.to_owned())
}

pub(super) fn encode_mouse(kind: MouseEventKind, column: u16, row: u16) -> Option<String> {
    match kind {
        // Only the press fires a click — never the release. A press and its
        // release report the same cell, so encoding both made one physical click
        // select twice; because selecting a row expands it (compact → full)
        // between the two events, the second landed on a now-shifted row and the
        // highlight flashed to the wrong card. One event per click fixes it.
        MouseEventKind::Down(MouseButton::Left) => Some(format!("mouse:left:{column}:{row}")),
        MouseEventKind::ScrollUp => Some("key:up".to_owned()),
        MouseEventKind::ScrollDown => Some("key:down".to_owned()),
        _ => None,
    }
}

pub(super) fn decode_wakeup(bytes: &[u8]) -> Wakeup {
    // The ledger posts a JSON `ledger_delta` envelope; no control or input wire
    // word starts with `{` (asserted by `control_words_never_start_with_brace`),
    // so the leading brace is an unambiguous, allocation-free discriminator.
    if bytes.first() == Some(&b'{') {
        return decode_ledger_wakeup(bytes);
    }
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
        "snapshot" => Wakeup::Snapshot,
        "self_close_probe" => Wakeup::SelfCloseProbe,
        "panes_changed" => Wakeup::PanesChanged,
        "resize" => Wakeup::Resize,
        "reload" => Wakeup::Reload,
        "key:up" => Wakeup::Key(KeyAction::Up),
        "key:down" => Wakeup::Key(KeyAction::Down),
        "key:enter" => Wakeup::Key(KeyAction::Enter),
        "key:space" => Wakeup::Key(KeyAction::Space),
        "key:help" => Wakeup::Key(KeyAction::Help),
        "key:dismiss" => Wakeup::Key(KeyAction::Dismiss),
        _ => Wakeup::Tick,
    }
}

#[derive(Deserialize)]
struct LedgerWakeup<'a> {
    #[serde(borrow)]
    kind: Option<&'a str>,
    event_method: Option<&'a str>,
    agent_event_name: Option<&'a str>,
}

fn decode_ledger_wakeup(bytes: &[u8]) -> Wakeup {
    let fresh_panes = serde_json::from_slice::<LedgerWakeup<'_>>(bytes)
        .ok()
        .is_some_and(|frame| frame.needs_fresh_panes());
    Wakeup::Ledger { fresh_panes }
}

impl LedgerWakeup<'_> {
    fn needs_fresh_panes(&self) -> bool {
        self.kind == Some("ledger_delta")
            && self.event_method == Some("agent.lifecycle")
            && matches!(self.agent_event_name, Some("SessionStart" | "SessionEnd"))
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
        // Timeout (a frame boundary or the idle backstop interval), or a signal
        // (the resize watcher's SIGWINCH handler interrupts this blocking recv):
        // all decode to `Wakeup::Tick`, a bare wake the serve loop's frame phase
        // turns into the spin advance, the paint decision, and the backstop poll.
        // Never fatal.
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
    fn mouse_events_encode_clicks_and_scrolls() {
        let encoded = encode_mouse(MouseEventKind::Down(MouseButton::Left), 4, 7)
            .expect("left button down is encoded");
        assert_eq!(
            decode_wakeup(encoded.as_bytes()),
            Wakeup::MouseClick { column: 4, row: 7 }
        );
        // The release must NOT also encode a click: one physical click is one
        // selection event, so the card's compact→full expansion between press
        // and release can't relocate the highlight.
        assert_eq!(
            encode_mouse(MouseEventKind::Up(MouseButton::Left), 4, 7),
            None
        );
        assert_eq!(
            decode_wakeup(
                encode_mouse(MouseEventKind::ScrollUp, 4, 7)
                    .unwrap()
                    .as_bytes()
            ),
            Wakeup::Key(KeyAction::Up)
        );
        assert_eq!(
            decode_wakeup(
                encode_mouse(MouseEventKind::ScrollDown, 4, 7)
                    .unwrap()
                    .as_bytes()
            ),
            Wakeup::Key(KeyAction::Down)
        );
    }

    #[test]
    fn reload_control_word_decodes_to_reload() {
        assert_eq!(decode_wakeup(b"reload"), Wakeup::Reload);
        // The wakeup sender and decoder share one constant so they cannot drift.
        assert_eq!(
            decode_wakeup(rimz::ledger::wakeup::RELOAD_WAKEUP),
            Wakeup::Reload
        );
    }

    #[test]
    fn self_close_probe_control_word_decodes_to_probe() {
        assert_eq!(decode_wakeup(SELF_CLOSE_WAKEUP), Wakeup::SelfCloseProbe);
    }

    #[test]
    fn r_key_triggers_a_reload() {
        // Pressing `r` re-execs the renderer in place by riding the same
        // `reload` control word the CLI's wakeup posts.
        let encoded = encode_key(KeyCode::Char('r')).expect("r is bound");
        assert_eq!(decode_wakeup(encoded.as_bytes()), Wakeup::Reload);
    }

    #[test]
    fn ledger_delta_envelope_decodes_to_ledger() {
        // The real wire shape `wake_sidebars` posts is a JSON object.
        let envelope =
            br#"{"kind":"ledger_delta","workspace_id":"ws_x","protocol_version":"rimz.plugin.v3"}"#;
        assert_eq!(
            decode_wakeup(envelope),
            Wakeup::Ledger { fresh_panes: false }
        );
        assert_eq!(decode_wakeup(b"{}"), Wakeup::Ledger { fresh_panes: false });
    }

    #[test]
    fn agent_session_boundary_ledger_delta_requests_fresh_panes() {
        let start = br#"{"kind":"ledger_delta","event_method":"agent.lifecycle","agent_event_name":"SessionStart","protocol_version":"rimz.plugin.v3"}"#;
        assert_eq!(decode_wakeup(start), Wakeup::Ledger { fresh_panes: true });

        let status = br#"{"kind":"ledger_delta","event_method":"agent.lifecycle","agent_event_name":"UserPromptSubmit","protocol_version":"rimz.plugin.v3"}"#;
        assert_eq!(decode_wakeup(status), Wakeup::Ledger { fresh_panes: false });
    }

    #[test]
    fn panes_changed_decodes_to_its_wakeup() {
        assert_eq!(decode_wakeup(PANES_CHANGED_WAKEUP), Wakeup::PanesChanged);
    }

    #[test]
    fn control_words_never_start_with_brace() {
        // The leading-brace discriminator (ledger delta vs control/input) holds
        // only while no control or input wire word can begin with `{`.
        let mut words = vec![
            "resize".to_owned(),
            "reload".to_owned(),
            String::from_utf8(SNAPSHOT_WAKEUP.to_vec()).unwrap(),
            String::from_utf8(SELF_CLOSE_WAKEUP.to_vec()).unwrap(),
            String::from_utf8(PANES_CHANGED_WAKEUP.to_vec()).unwrap(),
            String::from_utf8(rimz::ledger::wakeup::RELOAD_WAKEUP.to_vec()).unwrap(),
        ];
        for code in [
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Enter,
            KeyCode::Char(' '),
            KeyCode::Char('?'),
            KeyCode::Char('x'),
            KeyCode::Char('r'),
            KeyCode::Char('1'),
        ] {
            if let Some(w) = encode_key(code) {
                words.push(w);
            }
        }
        words.push(encode_mouse(MouseEventKind::Down(MouseButton::Left), 1, 2).unwrap());
        for word in words {
            assert_ne!(
                word.as_bytes().first(),
                Some(&b'{'),
                "{word:?} must not collide with the ledger-delta discriminator"
            );
        }
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
