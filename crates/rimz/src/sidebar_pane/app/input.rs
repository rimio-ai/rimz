//! Wakeup codec for the sidebar input socket: the pure mapping between
//! crossterm events, the wire strings the input thread sends over the
//! `UnixDatagram`, and the [`Wakeup`] the serve loop dispatches. No
//! `UiState` here — selection and focus handling stay in [`super`].

use std::io;
use std::os::unix::net::UnixDatagram;

use crate::feed::AgentStatus;
use crate::schema::sidebar_event::{RELOAD_CONTROL_WORD, SidebarEventEnvelope};
use ratatui::crossterm::event::{KeyCode, MouseButton, MouseEventKind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Wakeup {
    Tick,
    /// A typed sidebar event posted by the ledger, presence CLI, reload path,
    /// or pane-frame publisher.
    Event(SidebarEventEnvelope),
    /// The background fetch worker finished a snapshot and posted
    /// [`SNAPSHOT_WAKEUP`]; the loop folds the result waiting on its result
    /// channel. Keeps the fetch subprocess off the render thread.
    Snapshot,
    Resize,
    /// `rimz reload` asks the renderer to re-exec its own binary in place so a
    /// freshly-installed build takes effect without a session rebirth.
    Reload,
    Key(KeyAction),
    MouseClick {
        column: u16,
        row: u16,
    },
    /// A mouse wheel tick. Scrolls the agent-cards viewport without moving the
    /// selection; the next selection change snaps the viewport back to it.
    Scroll {
        down: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum KeyAction {
    Up,
    Down,
    WorktreeUp,
    WorktreeDown,
    /// `g`/`G` — move the selection to the first / last visible row (browse,
    /// no focus), the Vim top/bottom jump.
    Top,
    Bottom,
    Enter,
    /// `n`/`Space` — jump to the next item that needs you and focus it; `N`
    /// walks the same inbox in reverse. The fleet-scale triage keys.
    InboxNext,
    InboxPrev,
    /// `m`/`M` — mark the selected row read / unread without jumping, the
    /// email-inbox hygiene keys.
    MarkRead,
    MarkUnread,
    Help,
    Dismiss,
    Filter(FilterAction),
    Digit(u8),
    /// `←`/`→` — cycle the provider dashboard's tab.
    TabPrev,
    TabNext,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FilterAction {
    All,
    Status(AgentStatus),
    Unread,
}

/// The control word the background fetch worker sends to the loop's wakeup
/// socket once a snapshot is ready to fold. Riding the same socket every other
/// wakeup uses keeps the loop blocking in exactly one place.
pub(super) const SNAPSHOT_WAKEUP: &[u8] = b"snapshot";
pub(super) fn encode_key(code: KeyCode) -> Option<String> {
    let wire = match code {
        KeyCode::Up => "key:up",
        KeyCode::Down => "key:down",
        KeyCode::Left => "key:tab_prev",
        KeyCode::Right => "key:tab_next",
        KeyCode::Enter => "key:enter",
        KeyCode::Char('k') => "key:up",
        KeyCode::Char('j') => "key:down",
        KeyCode::Char('K') => "key:worktree_up",
        KeyCode::Char('J') => "key:worktree_down",
        KeyCode::Char('g') => "key:top",
        KeyCode::Char('G') => "key:bottom",
        KeyCode::Char('l') => "key:enter",
        // `n` and `Space` are one key: walk forward through the inbox and read.
        // `N` walks it in reverse.
        KeyCode::Char('n') | KeyCode::Char(' ') => "key:inbox_next",
        KeyCode::Char('N') => "key:inbox_prev",
        KeyCode::Char('m') => "key:mark_read",
        KeyCode::Char('M') => "key:mark_unread",
        KeyCode::Char('?') => "key:help",
        KeyCode::Char('a') => "key:filter:all",
        KeyCode::Char('u') => "key:filter:unread",
        KeyCode::Char('q') => "key:filter:waiting",
        KeyCode::Char('!') => "key:filter:failed",
        KeyCode::Char('e') => "key:filter:failed",
        KeyCode::Char('o') => "key:filter:idle",
        KeyCode::Char('p') => "key:filter:paused",
        KeyCode::Char('w') => "key:filter:running",
        KeyCode::Char('d') => "key:filter:success",
        KeyCode::Char('x') => "key:dismiss",
        KeyCode::Char(c @ '1'..='9') => return Some(format!("key:digit:{c}")),
        // `r` rides the very reload control word `rimz reload` posts, so a
        // keypress and the CLI converge on the one re-exec path in `super`.
        KeyCode::Char('r') => RELOAD_CONTROL_WORD,
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
        // The wheel scrolls the viewport, never the selection — ↑/↓ own the
        // selection browse, so a wheel peek past the fold moves no highlight.
        MouseEventKind::ScrollUp => Some("scroll:up".to_owned()),
        MouseEventKind::ScrollDown => Some("scroll:down".to_owned()),
        _ => None,
    }
}

pub(super) fn decode_wakeup(bytes: &[u8]) -> Wakeup {
    // External wakeups post JSON sidebar event envelopes; no control or input wire
    // word starts with `{` (asserted by `control_words_never_start_with_brace`),
    // so the leading brace is an unambiguous, allocation-free discriminator.
    if bytes.first() == Some(&b'{') {
        return decode_event_wakeup(bytes);
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
        "resize" => Wakeup::Resize,
        RELOAD_CONTROL_WORD => Wakeup::Reload,
        "key:up" => Wakeup::Key(KeyAction::Up),
        "key:down" => Wakeup::Key(KeyAction::Down),
        "key:worktree_up" => Wakeup::Key(KeyAction::WorktreeUp),
        "key:worktree_down" => Wakeup::Key(KeyAction::WorktreeDown),
        "key:top" => Wakeup::Key(KeyAction::Top),
        "key:bottom" => Wakeup::Key(KeyAction::Bottom),
        "key:tab_prev" => Wakeup::Key(KeyAction::TabPrev),
        "key:tab_next" => Wakeup::Key(KeyAction::TabNext),
        "key:enter" => Wakeup::Key(KeyAction::Enter),
        "key:inbox_next" => Wakeup::Key(KeyAction::InboxNext),
        "key:inbox_prev" => Wakeup::Key(KeyAction::InboxPrev),
        "key:mark_read" => Wakeup::Key(KeyAction::MarkRead),
        "key:mark_unread" => Wakeup::Key(KeyAction::MarkUnread),
        "key:help" => Wakeup::Key(KeyAction::Help),
        "key:filter:all" => Wakeup::Key(KeyAction::Filter(FilterAction::All)),
        "key:filter:unread" => Wakeup::Key(KeyAction::Filter(FilterAction::Unread)),
        "key:filter:waiting" => Wakeup::Key(KeyAction::Filter(FilterAction::Status(
            AgentStatus::Waiting,
        ))),
        "key:filter:failed" => {
            Wakeup::Key(KeyAction::Filter(FilterAction::Status(AgentStatus::Failed)))
        }
        "key:filter:idle" => {
            Wakeup::Key(KeyAction::Filter(FilterAction::Status(AgentStatus::Idle)))
        }
        "key:filter:paused" => {
            Wakeup::Key(KeyAction::Filter(FilterAction::Status(AgentStatus::Paused)))
        }
        "key:filter:running" => Wakeup::Key(KeyAction::Filter(FilterAction::Status(
            AgentStatus::Running,
        ))),
        "key:filter:success" => Wakeup::Key(KeyAction::Filter(FilterAction::Status(
            AgentStatus::Success,
        ))),
        "key:dismiss" => Wakeup::Key(KeyAction::Dismiss),
        "scroll:up" => Wakeup::Scroll { down: false },
        "scroll:down" => Wakeup::Scroll { down: true },
        _ => Wakeup::Tick,
    }
}

fn decode_event_wakeup(bytes: &[u8]) -> Wakeup {
    serde_json::from_slice::<SidebarEventEnvelope>(bytes)
        .ok()
        .filter(SidebarEventEnvelope::is_current_version)
        .map(Wakeup::Event)
        .unwrap_or(Wakeup::Tick)
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
    let mut buf = [0_u8; 16 * 1024];
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
        // The wheel scrolls the viewport, never the selection: it must round-
        // trip to the scroll wakeup, not an arrow key.
        assert_eq!(
            decode_wakeup(
                encode_mouse(MouseEventKind::ScrollUp, 4, 7)
                    .unwrap()
                    .as_bytes()
            ),
            Wakeup::Scroll { down: false }
        );
        assert_eq!(
            decode_wakeup(
                encode_mouse(MouseEventKind::ScrollDown, 4, 7)
                    .unwrap()
                    .as_bytes()
            ),
            Wakeup::Scroll { down: true }
        );
    }

    #[test]
    fn r_key_triggers_a_reload() {
        // Pressing `r` re-execs the renderer in place through the local input
        // control word; external reloads arrive as typed sidebar events.
        let encoded = encode_key(KeyCode::Char('r')).expect("r is bound");
        assert_eq!(decode_wakeup(encoded.as_bytes()), Wakeup::Reload);
    }

    #[test]
    fn sidebar_event_envelope_decodes_to_event() {
        let envelope = SidebarEventEnvelope::new(
            crate::WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
            Some("rimz-test".to_owned()),
            42,
            crate::schema::sidebar_event::SidebarEvent::LedgerDelta {
                event_method: None,
                agent_signal: None,
            },
        );
        let encoded = serde_json::to_vec(&envelope).unwrap();
        assert_eq!(decode_wakeup(&encoded), Wakeup::Event(envelope));
        assert_eq!(decode_wakeup(b"{}"), Wakeup::Tick);
    }

    #[test]
    fn agent_session_boundary_event_requests_fresh_panes() {
        let start = crate::schema::sidebar_event::SidebarEvent::LedgerDelta {
            event_method: Some("agent.lifecycle".to_owned()),
            agent_signal: Some(crate::agents::LifecycleSignal::Registered.tag().to_owned()),
        };
        assert!(start.requests_producer_verification());

        let status = crate::schema::sidebar_event::SidebarEvent::LedgerDelta {
            event_method: Some("agent.lifecycle".to_owned()),
            agent_signal: Some(crate::agents::LifecycleSignal::TurnStarted.tag().to_owned()),
        };
        assert!(!status.requests_producer_verification());
    }

    #[test]
    fn keys_round_trip_through_the_wire() {
        // Every keycode encodes to its wire word and decodes back to the wakeup
        // the serve loop dispatches: vim row/focus keys, the J/K worktree jumps,
        // and the full filter key set. (The `r` reload keypress is covered by
        // `r_key_triggers_a_reload`; the literal reload word is checked below.)
        let cases = [
            // vim row and focus keys
            ("j → down", KeyCode::Char('j'), Wakeup::Key(KeyAction::Down)),
            ("k → up", KeyCode::Char('k'), Wakeup::Key(KeyAction::Up)),
            (
                "l → enter",
                KeyCode::Char('l'),
                Wakeup::Key(KeyAction::Enter),
            ),
            // worktree-jump keys
            (
                "J → worktree down",
                KeyCode::Char('J'),
                Wakeup::Key(KeyAction::WorktreeDown),
            ),
            (
                "K → worktree up",
                KeyCode::Char('K'),
                Wakeup::Key(KeyAction::WorktreeUp),
            ),
            // top/bottom jumps
            ("g → top", KeyCode::Char('g'), Wakeup::Key(KeyAction::Top)),
            (
                "G → bottom",
                KeyCode::Char('G'),
                Wakeup::Key(KeyAction::Bottom),
            ),
            // inbox triage: n and Space step forward, N steps back
            (
                "n → inbox next",
                KeyCode::Char('n'),
                Wakeup::Key(KeyAction::InboxNext),
            ),
            (
                "space → inbox next",
                KeyCode::Char(' '),
                Wakeup::Key(KeyAction::InboxNext),
            ),
            (
                "N → inbox prev",
                KeyCode::Char('N'),
                Wakeup::Key(KeyAction::InboxPrev),
            ),
            // mark read / unread without jumping
            (
                "m → mark read",
                KeyCode::Char('m'),
                Wakeup::Key(KeyAction::MarkRead),
            ),
            (
                "M → mark unread",
                KeyCode::Char('M'),
                Wakeup::Key(KeyAction::MarkUnread),
            ),
            // filter keys
            (
                "a → all",
                KeyCode::Char('a'),
                Wakeup::Key(KeyAction::Filter(FilterAction::All)),
            ),
            (
                "u → unread",
                KeyCode::Char('u'),
                Wakeup::Key(KeyAction::Filter(FilterAction::Unread)),
            ),
            (
                "q → waiting",
                KeyCode::Char('q'),
                Wakeup::Key(KeyAction::Filter(FilterAction::Status(
                    AgentStatus::Waiting,
                ))),
            ),
            (
                "! → failed",
                KeyCode::Char('!'),
                Wakeup::Key(KeyAction::Filter(FilterAction::Status(AgentStatus::Failed))),
            ),
            (
                "e → failed",
                KeyCode::Char('e'),
                Wakeup::Key(KeyAction::Filter(FilterAction::Status(AgentStatus::Failed))),
            ),
            (
                "o → idle",
                KeyCode::Char('o'),
                Wakeup::Key(KeyAction::Filter(FilterAction::Status(AgentStatus::Idle))),
            ),
            (
                "p → paused",
                KeyCode::Char('p'),
                Wakeup::Key(KeyAction::Filter(FilterAction::Status(AgentStatus::Paused))),
            ),
            (
                "w → running",
                KeyCode::Char('w'),
                Wakeup::Key(KeyAction::Filter(FilterAction::Status(
                    AgentStatus::Running,
                ))),
            ),
            (
                "d → success",
                KeyCode::Char('d'),
                Wakeup::Key(KeyAction::Filter(FilterAction::Status(
                    AgentStatus::Success,
                ))),
            ),
        ];
        for (label, key, wakeup) in cases {
            let encoded = encode_key(key).expect("key is encoded");
            assert_eq!(decode_wakeup(encoded.as_bytes()), wakeup, "{label}");
        }
        // The literal reload control word also decodes to a reload on its own.
        assert_eq!(decode_wakeup(b"reload"), Wakeup::Reload);
    }

    #[test]
    fn control_words_never_start_with_brace() {
        // The leading-brace discriminator (ledger delta vs control/input) holds
        // only while no control or input wire word can begin with `{`.
        let mut words = vec![
            "resize".to_owned(),
            RELOAD_CONTROL_WORD.to_owned(),
            String::from_utf8(SNAPSHOT_WAKEUP.to_vec()).unwrap(),
        ];
        for code in [
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Enter,
            KeyCode::Char('j'),
            KeyCode::Char('k'),
            KeyCode::Char('J'),
            KeyCode::Char('K'),
            KeyCode::Char('g'),
            KeyCode::Char('G'),
            KeyCode::Char('l'),
            KeyCode::Char('n'),
            KeyCode::Char('N'),
            KeyCode::Char(' '),
            KeyCode::Char('m'),
            KeyCode::Char('M'),
            KeyCode::Char('?'),
            KeyCode::Char('a'),
            KeyCode::Char('q'),
            KeyCode::Char('!'),
            KeyCode::Char('e'),
            KeyCode::Char('o'),
            KeyCode::Char('p'),
            KeyCode::Char('w'),
            KeyCode::Char('d'),
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
