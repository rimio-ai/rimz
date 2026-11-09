//! Wakeup codec: the pure mapping between crossterm events, input words, room event envelopes, and the [`Wakeup`] an attachment dispatches. Selection and focus handling stay in [`super`].

use crate::sidebar_pane::view::BodyFilter;
use crate::wakeup::events::SidebarEventEnvelope;
use ratatui::crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};

use crate::config::keys::sidebar::{
    SequenceMatch, SidebarAction, SidebarChord, SidebarKey, SidebarKeymap,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::sidebar_pane) enum Wakeup {
    Tick,
    Card(std::sync::Arc<super::card_commands::CardInput>),
    /// A typed sidebar event posted by the store, presence CLI, reload path,
    /// or the host's own notification delivery.
    Event(SidebarEventEnvelope),
    /// The background fetch worker finished a snapshot; fold the waiting result.
    Snapshot,
    FocusFailed {
        pane: crate::ids::PaneId,
        error: String,
    },
    FocusSuperseded {
        input: crate::ids::FocusNonce,
    },
    FocusDispatched {
        input: crate::ids::FocusNonce,
        nonce: crate::ids::FocusNonce,
    },
    Resize,
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
    /// selection; the view then stays anchored to the cards it shows.
    Scroll {
        down: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::sidebar_pane) enum KeyAction {
    WidthNarrower,
    WidthWider,
    Up,
    Down,
    WorktreeUp,
    WorktreeDown,
    /// Move the selection to the first / last visible row (browse, no focus),
    /// the Vim top/bottom jump.
    Top,
    Bottom,
    /// Move the selection up one painted screenful.
    PageUp,
    /// Move the selection down one painted screenful.
    PageDown,
    /// Move the selection by half a painted screenful.
    HalfPageUp,
    HalfPageDown,
    /// Move the selection to the top, middle or bottom row painted on screen.
    ScreenTop,
    ScreenMiddle,
    ScreenBottom,
    Enter,
    Search,
    QueryChar(char),
    Backspace,
    /// End typing and keep the committed search the draft started from.
    CancelSearch,
    /// Peel the innermost layer: the committed search, then a pane notice,
    /// then a recovered alert.
    Cancel,
    /// Walk the filtered roster under a committed search, else the inbox of
    /// rows that need you, and focus the target. `SearchPrev` walks back.
    SearchNext,
    SearchPrev,
    /// Toggle the selected row read/unread without jumping.
    MarkToggle,
    /// Mark every row read without jumping.
    MarkAllRead,
    Help,
    ToggleInactive,
    /// Flip the selected card's subagents and waits section.
    FoldToggle,
    /// Unfold or fold every worktree group.
    FoldOpenAll,
    FoldCloseAll,
    Reload,
    Filter(Option<BodyFilter>),
    /// The room card reducer's Slot, Last, and needs-you toggle.
    Slot(u8),
    LastCard,
    NeedsYou,
    /// Cycle the provider dashboard's tab.
    TabPrev,
    TabNext,
    /// Otherwise-unbound keypress, a pending prefix, or a cancelled one.
    /// Closes the help overlay; no-op when it is already closed.
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum InputMode {
    Normal,
    Help,
    Typing,
}

/// Resolve a press against the mode and the keys `pending` so far. A press
/// that continues a bound prefix waits in `pending`; any other press after a
/// prefix cancels it and is swallowed.
pub(super) fn resolve_key(
    keymap: &SidebarKeymap,
    mode: InputMode,
    pending: &mut Vec<SidebarChord>,
    code: KeyCode,
    mods: KeyModifiers,
) -> KeyAction {
    if mode == InputMode::Help {
        pending.clear();
        return KeyAction::Other;
    }
    if mode == InputMode::Typing {
        pending.clear();
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
    let Some(chord) = chord(code, mods) else {
        pending.clear();
        return KeyAction::Other;
    };
    pending.push(chord);
    match keymap.lookup(pending) {
        SequenceMatch::Pending => KeyAction::Other,
        SequenceMatch::Action(action) => {
            pending.clear();
            key_action(action)
        }
        SequenceMatch::Unbound => {
            pending.clear();
            KeyAction::Other
        }
    }
}

/// Resolve one press with no prefix pending, for tests of single keys.
#[cfg(test)]
pub(super) fn resolve_one(
    keymap: &SidebarKeymap,
    mode: InputMode,
    code: KeyCode,
    mods: KeyModifiers,
) -> KeyAction {
    resolve_key(keymap, mode, &mut Vec::new(), code, mods)
}

/// The table chord a press spells. Case lives in the character, so Shift is
/// dropped; keys the table cannot name have no chord.
fn chord(code: KeyCode, mods: KeyModifiers) -> Option<SidebarChord> {
    let key = match code {
        KeyCode::Char(c) => SidebarKey::Char(c),
        KeyCode::Up => SidebarKey::Up,
        KeyCode::Down => SidebarKey::Down,
        KeyCode::Left => SidebarKey::Left,
        KeyCode::Right => SidebarKey::Right,
        KeyCode::Home => SidebarKey::Home,
        KeyCode::End => SidebarKey::End,
        KeyCode::PageUp => SidebarKey::PageUp,
        KeyCode::PageDown => SidebarKey::PageDown,
        KeyCode::Enter => SidebarKey::Enter,
        KeyCode::Esc => SidebarKey::Esc,
        KeyCode::Tab => SidebarKey::Tab,
        KeyCode::Backspace => SidebarKey::Backspace,
        _ => return None,
    };
    Some(SidebarChord {
        key,
        ctrl: mods.contains(KeyModifiers::CONTROL),
        alt: mods.contains(KeyModifiers::ALT),
    })
}

/// The resolver's action for a table action. The effects that spawn, write
/// the room width, or repaint have no action yet.
fn key_action(action: SidebarAction) -> KeyAction {
    use crate::agents::AgentStatus as S;
    let status = |status| KeyAction::Filter(Some(BodyFilter::Status(status)));
    match action {
        SidebarAction::MoveDown => KeyAction::Down,
        SidebarAction::MoveUp => KeyAction::Up,
        SidebarAction::MoveWorktreeDown => KeyAction::WorktreeDown,
        SidebarAction::MoveWorktreeUp => KeyAction::WorktreeUp,
        SidebarAction::MoveTop => KeyAction::Top,
        SidebarAction::MoveBottom => KeyAction::Bottom,
        SidebarAction::MoveScreenTop => KeyAction::ScreenTop,
        SidebarAction::MoveScreenMiddle => KeyAction::ScreenMiddle,
        SidebarAction::MoveScreenBottom => KeyAction::ScreenBottom,
        SidebarAction::MoveHalfPageDown => KeyAction::HalfPageDown,
        SidebarAction::MoveHalfPageUp => KeyAction::HalfPageUp,
        SidebarAction::MovePageDown => KeyAction::PageDown,
        SidebarAction::MovePageUp => KeyAction::PageUp,
        SidebarAction::CardFocus => KeyAction::Enter,
        SidebarAction::CardSlot(n) => KeyAction::Slot(n),
        SidebarAction::CardLast => KeyAction::LastCard,
        SidebarAction::ReadToggle => KeyAction::MarkToggle,
        SidebarAction::ReadAll => KeyAction::MarkAllRead,
        SidebarAction::SearchStart => KeyAction::Search,
        SidebarAction::SearchNext => KeyAction::SearchNext,
        SidebarAction::SearchPrev => KeyAction::SearchPrev,
        SidebarAction::FilterAsks => status(S::Waiting),
        SidebarAction::FilterFailures => status(S::Failed),
        SidebarAction::FilterUnread => KeyAction::Filter(Some(BodyFilter::Unread)),
        SidebarAction::FilterWorking => status(S::Running),
        SidebarAction::FilterPaused => status(S::Paused),
        SidebarAction::FilterDone => status(S::Success),
        SidebarAction::FilterSleeping => status(S::Sleeping),
        SidebarAction::FilterIdle => status(S::Idle),
        SidebarAction::FilterNeedsYou => KeyAction::NeedsYou,
        SidebarAction::FilterClear => KeyAction::Filter(None),
        SidebarAction::FoldToggle => KeyAction::FoldToggle,
        SidebarAction::FoldOpenAll => KeyAction::FoldOpenAll,
        SidebarAction::FoldCloseAll => KeyAction::FoldCloseAll,
        SidebarAction::FoldInactive => KeyAction::ToggleInactive,
        SidebarAction::WidthNarrower => KeyAction::WidthNarrower,
        SidebarAction::WidthWider => KeyAction::WidthWider,
        SidebarAction::ViewReload => KeyAction::Reload,
        SidebarAction::ViewCancel => KeyAction::Cancel,
        SidebarAction::Help => KeyAction::Help,
        SidebarAction::DashboardPrev => KeyAction::TabPrev,
        SidebarAction::DashboardNext => KeyAction::TabNext,
        SidebarAction::CardClose
        | SidebarAction::PaletteCommands
        | SidebarAction::PaneReturn
        | SidebarAction::WidthReset
        | SidebarAction::ViewRedraw => KeyAction::Other,
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

pub(in crate::sidebar_pane) fn decode_wakeup(bytes: &[u8]) -> Wakeup {
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
        "resize" => Wakeup::Resize,
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

#[cfg(test)]
mod tests;
