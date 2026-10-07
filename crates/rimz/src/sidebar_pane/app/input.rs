//! Wakeup codec for the sidebar input socket: the pure mapping between
//! crossterm events, the wire strings the input thread sends over the
//! `UnixDatagram`, and the [`Wakeup`] the serve loop dispatches. No
//! `UiState` here — selection and focus handling stay in [`super`].

use std::io;
use std::os::unix::net::UnixDatagram;

use crate::agents::AgentStatus;
use crate::sidebar_pane::view::BodyFilter;
use crate::wakeup::events::{
    RELOAD_CONTROL_WORD, SUPERVISOR_HANDOFF_CONTROL_WORD, SidebarEventEnvelope,
};
use ratatui::crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};

use super::NavKeymap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Wakeup {
    Tick,
    /// A typed sidebar event posted by the store, presence CLI, reload path,
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
    /// The supervisor has proven a replacement worker and needs this worker to
    /// release the terminal before the supervisor replaces its own image.
    SupervisorHandoff,
    Press {
        code: KeyCode,
        mods: KeyModifiers,
    },
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
    WidthNarrower,
    WidthWider,
    Up,
    Down,
    WorktreeUp,
    WorktreeDown,
    /// `g`/`G` — move the selection to the first / last visible row (browse,
    /// no focus), the Vim top/bottom jump.
    Top,
    Bottom,
    /// Move the selection up one painted screenful.
    PageUp,
    /// Move the selection down one painted screenful.
    PageDown,
    /// Move the selection to the top row currently painted on screen.
    ScreenTop,
    /// Move the selection to the bottom row currently painted on screen.
    ScreenBottom,
    Enter,
    Search,
    QueryChar(char),
    Backspace,
    CancelSearch,
    /// `n`/`Space` — jump to the next item that needs you and focus it; `N`
    /// walks the same inbox in reverse. The fleet-scale triage keys.
    InboxNext,
    InboxPrev,
    /// `m` — toggle the selected row read/unread without jumping.
    MarkToggle,
    /// `M` — mark every row read without jumping.
    MarkAllRead,
    Help,
    Reload,
    Dismiss,
    Filter(Option<BodyFilter>),
    Digit(u8),
    /// `←`/`→` — cycle the provider dashboard's tab.
    TabPrev,
    TabNext,
    /// Otherwise-unbound keypress. Closes the help overlay; no-op when it is
    /// already closed.
    Other,
}

/// The control word the background fetch worker sends to the loop's wakeup
/// socket once a snapshot is ready to fold. Riding the same socket every other
/// wakeup uses keeps the loop blocking in exactly one place.
pub(super) const SNAPSHOT_WAKEUP: &[u8] = b"snapshot";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum InputMode {
    Normal,
    Help,
    Typing,
}

pub(super) fn resolve_key(
    keymap: &NavKeymap,
    mode: InputMode,
    code: KeyCode,
    mods: KeyModifiers,
) -> KeyAction {
    if mode == InputMode::Help {
        return KeyAction::Other;
    }
    if mode == InputMode::Typing {
        let chord_mods = mods & (KeyModifiers::CONTROL | KeyModifiers::ALT);
        return match (code, chord_mods) {
            (KeyCode::Char('n'), KeyModifiers::CONTROL) => KeyAction::Down,
            (KeyCode::Char('p'), KeyModifiers::CONTROL) => KeyAction::Up,
            (KeyCode::Up, KeyModifiers::NONE) => KeyAction::Up,
            (KeyCode::Down, KeyModifiers::NONE) => KeyAction::Down,
            (KeyCode::Enter, KeyModifiers::NONE) => KeyAction::Enter,
            (KeyCode::Esc, KeyModifiers::NONE) => KeyAction::CancelSearch,
            (KeyCode::Backspace, KeyModifiers::NONE) => KeyAction::Backspace,
            (KeyCode::Char(ch), KeyModifiers::NONE) if !ch.is_control() => KeyAction::QueryChar(ch),
            _ => KeyAction::Other,
        };
    }
    if let Some(action) = keymap.action_for(code, mods) {
        return action;
    }
    if mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
        return KeyAction::Other;
    }
    match code {
        KeyCode::Left => KeyAction::TabPrev,
        KeyCode::Right => KeyAction::TabNext,
        KeyCode::Enter | KeyCode::Char('l') => KeyAction::Enter,
        // `n` and `Space` are one key: walk forward through the inbox and read.
        // `N` walks it in reverse.
        KeyCode::Char('n') | KeyCode::Char(' ') => KeyAction::InboxNext,
        KeyCode::Char('N') => KeyAction::InboxPrev,
        KeyCode::Char('m') => KeyAction::MarkToggle,
        KeyCode::Char('M') => KeyAction::MarkAllRead,
        KeyCode::Char('?') => KeyAction::Help,
        KeyCode::Char('/') => KeyAction::Search,
        KeyCode::Char('A') => KeyAction::Filter(None),
        KeyCode::Char('u') => KeyAction::Filter(Some(BodyFilter::Unread)),
        KeyCode::Char('q') => KeyAction::Filter(Some(BodyFilter::Status(AgentStatus::Waiting))),
        KeyCode::Char('!') | KeyCode::Char('e') => {
            KeyAction::Filter(Some(BodyFilter::Status(AgentStatus::Failed)))
        }
        KeyCode::Char('o') => KeyAction::Filter(Some(BodyFilter::Status(AgentStatus::Idle))),
        KeyCode::Char('p') => KeyAction::Filter(Some(BodyFilter::Status(AgentStatus::Paused))),
        KeyCode::Char('w') => KeyAction::Filter(Some(BodyFilter::Status(AgentStatus::Running))),
        KeyCode::Char('s') => KeyAction::Filter(Some(BodyFilter::Status(AgentStatus::Success))),
        KeyCode::Char('z') => KeyAction::Filter(Some(BodyFilter::Status(AgentStatus::Sleeping))),
        KeyCode::Char('x') => KeyAction::Dismiss,
        KeyCode::Char(c @ '1'..='9') => KeyAction::Digit(c as u8 - b'0'),
        KeyCode::Char('r') => KeyAction::Reload,
        _ => KeyAction::Other,
    }
}

pub(super) fn encode_key(code: KeyCode, mods: KeyModifiers) -> Option<String> {
    let mods = match (
        mods.contains(KeyModifiers::CONTROL),
        mods.contains(KeyModifiers::ALT),
    ) {
        (false, false) => "-",
        (true, false) => "c",
        (false, true) => "a",
        (true, true) => "ca",
    };
    let code = match code {
        KeyCode::Char(c) => return Some(format!("press:{mods}:char:{c}")),
        KeyCode::Up => "up",
        KeyCode::Down => "down",
        KeyCode::Left => "left",
        KeyCode::Right => "right",
        KeyCode::Home => "home",
        KeyCode::End => "end",
        KeyCode::PageUp => "pageup",
        KeyCode::PageDown => "pagedown",
        KeyCode::Enter => "enter",
        KeyCode::Esc => "esc",
        KeyCode::Backspace => "backspace",
        KeyCode::Tab => "tab",
        KeyCode::Delete => "delete",
        _ => return None,
    };
    Some(format!("press:{mods}:{code}"))
}

#[cfg(feature = "testkit")]
pub(super) fn encode_click(column: u16, row: u16) -> String {
    // A left-button press is always encoded by the ordinary mouse input path.
    encode_mouse(MouseEventKind::Down(MouseButton::Left), column, row)
        .expect("left-button press is always encoded")
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

fn decode_wakeup(bytes: &[u8]) -> Wakeup {
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
    if let Some(press) = decode_press(raw) {
        return press;
    }
    match raw {
        "snapshot" => Wakeup::Snapshot,
        "resize" => Wakeup::Resize,
        RELOAD_CONTROL_WORD => Wakeup::Reload,
        SUPERVISOR_HANDOFF_CONTROL_WORD => Wakeup::SupervisorHandoff,
        "scroll:up" => Wakeup::Scroll { down: false },
        "scroll:down" => Wakeup::Scroll { down: true },
        _ => Wakeup::Tick,
    }
}

fn decode_press(raw: &str) -> Option<Wakeup> {
    let (mods, code) = raw.strip_prefix("press:")?.split_once(':')?;
    let mods = match mods {
        "-" => KeyModifiers::NONE,
        "c" => KeyModifiers::CONTROL,
        "a" => KeyModifiers::ALT,
        "ca" => KeyModifiers::CONTROL | KeyModifiers::ALT,
        _ => return None,
    };
    let code = if let Some(raw) = code.strip_prefix("char:") {
        let mut chars = raw.chars();
        let c = chars.next()?;
        if chars.next().is_some() {
            return None;
        }
        KeyCode::Char(c)
    } else {
        match code {
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" => KeyCode::PageUp,
            "pagedown" => KeyCode::PageDown,
            "enter" => KeyCode::Enter,
            "esc" => KeyCode::Esc,
            "backspace" => KeyCode::Backspace,
            "tab" => KeyCode::Tab,
            "delete" => KeyCode::Delete,
            _ => return None,
        }
    };
    Some(Wakeup::Press { code, mods })
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
mod tests;
