//! The sidebar's in-pane keymap: `[keys.sidebar]` maps key sequences to named
//! in-pane actions over a default table, under both room styles. The renderer,
//! its pending-prefix hint, and its help legend all read one resolved table.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Modifier, TERMINAL_CONTROL_REFUSAL, graphic, is_terminal_control};

/// One key of an in-pane chord. Case lives in the character, so Shift is never
/// a modifier here; `Space` is the character `' '`.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum SidebarKey {
    Char(char),
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Enter,
    Esc,
    Tab,
    Backspace,
}

const NAMED_KEYS: &[(&str, SidebarKey)] = &[
    ("Up", SidebarKey::Up),
    ("Down", SidebarKey::Down),
    ("Left", SidebarKey::Left),
    ("Right", SidebarKey::Right),
    ("Home", SidebarKey::Home),
    ("End", SidebarKey::End),
    ("PageUp", SidebarKey::PageUp),
    ("PageDown", SidebarKey::PageDown),
    ("Enter", SidebarKey::Enter),
    ("Esc", SidebarKey::Esc),
    ("Tab", SidebarKey::Tab),
    ("Space", SidebarKey::Char(' ')),
    ("Backspace", SidebarKey::Backspace),
];

impl SidebarKey {
    fn parse(raw: &str) -> Option<Self> {
        graphic(raw).map(Self::Char).or_else(|| {
            NAMED_KEYS
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(raw))
                .map(|&(_, key)| key)
        })
    }
}

/// `[Ctrl+][Alt+]key`, as pressed in the sidebar pane.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SidebarChord {
    pub key: SidebarKey,
    pub ctrl: bool,
    pub alt: bool,
}

impl SidebarChord {
    fn parse(raw: &str) -> Result<Self, String> {
        let invalid = || {
            format!(
                "`{raw}` is not a key; use a character or Up, Down, Left, Right, Home, End, PageUp, PageDown, Enter, Esc, Tab, Space, or Backspace, after optional Ctrl+ and Alt+"
            )
        };
        let mut chord = Self {
            key: SidebarKey::Esc,
            ctrl: false,
            alt: false,
        };
        let mut rest = raw;
        loop {
            if let Some(key) = SidebarKey::parse(rest) {
                chord.key = key;
                break;
            }
            let (modifier, tail) = rest.split_once(['+', '-']).ok_or_else(invalid)?;
            match Modifier::parse(modifier).ok_or_else(invalid)? {
                Modifier::Alt => chord.alt = true,
                Modifier::Ctrl => chord.ctrl = true,
            }
            rest = tail.trim();
        }
        if chord.ctrl && matches!(chord.key, SidebarKey::Char(key) if is_terminal_control(key)) {
            return Err(TERMINAL_CONTROL_REFUSAL.to_owned());
        }
        // A terminal sends Ctrl with a letter as one control byte, which the
        // pane reads back as the lowercase letter whatever Shift was.
        if let (true, SidebarKey::Char(key)) = (chord.ctrl, chord.key) {
            chord.key = SidebarKey::Char(key.to_ascii_lowercase());
        }
        Ok(chord)
    }
}

impl fmt::Display for SidebarChord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            formatter.write_str("Ctrl+")?;
        }
        if self.alt {
            formatter.write_str("Alt+")?;
        }
        match self.key {
            SidebarKey::Char(key) if key != ' ' => write!(formatter, "{key}"),
            key => formatter.write_str(
                NAMED_KEYS
                    .iter()
                    .find(|(_, named)| *named == key)
                    .map_or("?", |(name, _)| name),
            ),
        }
    }
}

/// A named in-pane action. Where the room keymap has the same meaning, the
/// name matches it (`card.slot.N`, `card.last`, `card.close`).
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum SidebarAction {
    MoveDown,
    MoveUp,
    MoveWorktreeDown,
    MoveWorktreeUp,
    MoveTop,
    MoveBottom,
    MoveScreenTop,
    MoveScreenMiddle,
    MoveScreenBottom,
    MoveHalfPageDown,
    MoveHalfPageUp,
    MovePageDown,
    MovePageUp,
    CardFocus,
    CardSlot(u8),
    CardLast,
    CardClose,
    ReadToggle,
    ReadAll,
    PaletteCommands,
    PaneReturn,
    SearchStart,
    SearchNext,
    SearchPrev,
    FilterAsks,
    FilterFailures,
    FilterUnread,
    FilterWorking,
    FilterPaused,
    FilterDone,
    FilterSleeping,
    FilterIdle,
    FilterNeedsYou,
    FilterClear,
    FoldToggle,
    FoldOpenAll,
    FoldCloseAll,
    FoldInactive,
    WidthNarrower,
    WidthWider,
    WidthReset,
    ViewRedraw,
    ViewReload,
    ViewCancel,
    Help,
    DashboardPrev,
    DashboardNext,
}

use SidebarAction as A;

/// Every action's config name and its short label for the hint and legend.
const ACTIONS: &[(&str, SidebarAction, &str)] = &[
    ("move.down", A::MoveDown, "down"),
    ("move.up", A::MoveUp, "up"),
    ("move.worktree_down", A::MoveWorktreeDown, "next worktree"),
    ("move.worktree_up", A::MoveWorktreeUp, "prev worktree"),
    ("move.top", A::MoveTop, "top"),
    ("move.bottom", A::MoveBottom, "bottom"),
    ("move.screen_top", A::MoveScreenTop, "screen top"),
    ("move.screen_middle", A::MoveScreenMiddle, "screen middle"),
    ("move.screen_bottom", A::MoveScreenBottom, "screen bottom"),
    ("move.half_page_down", A::MoveHalfPageDown, "half page down"),
    ("move.half_page_up", A::MoveHalfPageUp, "half page up"),
    ("move.page_down", A::MovePageDown, "page down"),
    ("move.page_up", A::MovePageUp, "page up"),
    ("card.focus", A::CardFocus, "focus"),
    ("card.slot.1", A::CardSlot(1), "card 1"),
    ("card.slot.2", A::CardSlot(2), "card 2"),
    ("card.slot.3", A::CardSlot(3), "card 3"),
    ("card.slot.4", A::CardSlot(4), "card 4"),
    ("card.slot.5", A::CardSlot(5), "card 5"),
    ("card.slot.6", A::CardSlot(6), "card 6"),
    ("card.slot.7", A::CardSlot(7), "card 7"),
    ("card.slot.8", A::CardSlot(8), "card 8"),
    ("card.slot.9", A::CardSlot(9), "card 9"),
    ("card.last", A::CardLast, "last card"),
    ("card.close", A::CardClose, "close"),
    ("read.toggle", A::ReadToggle, "read/unread"),
    ("read.all", A::ReadAll, "read all"),
    ("palette.commands", A::PaletteCommands, "commands"),
    ("pane.return", A::PaneReturn, "return"),
    ("search.start", A::SearchStart, "search"),
    ("search.next", A::SearchNext, "next"),
    ("search.prev", A::SearchPrev, "prev"),
    ("filter.asks", A::FilterAsks, "asks"),
    ("filter.failures", A::FilterFailures, "failures"),
    ("filter.unread", A::FilterUnread, "unread"),
    ("filter.working", A::FilterWorking, "working"),
    ("filter.paused", A::FilterPaused, "paused"),
    ("filter.done", A::FilterDone, "done"),
    ("filter.sleeping", A::FilterSleeping, "sleeping"),
    ("filter.idle", A::FilterIdle, "idle"),
    ("filter.needs_you", A::FilterNeedsYou, "needs you"),
    ("filter.clear", A::FilterClear, "clear filter"),
    ("fold.toggle", A::FoldToggle, "fold"),
    ("fold.open_all", A::FoldOpenAll, "open all"),
    ("fold.close_all", A::FoldCloseAll, "close all"),
    ("fold.inactive", A::FoldInactive, "inactive"),
    ("width.narrower", A::WidthNarrower, "narrower"),
    ("width.wider", A::WidthWider, "wider"),
    ("width.reset", A::WidthReset, "reset width"),
    ("view.redraw", A::ViewRedraw, "redraw"),
    ("view.reload", A::ViewReload, "reload"),
    ("view.cancel", A::ViewCancel, "cancel"),
    ("help", A::Help, "help"),
    ("dashboard.prev", A::DashboardPrev, "prev tab"),
    ("dashboard.next", A::DashboardNext, "next tab"),
];

/// The shipped in-pane bindings: a sequence of space-separated chords and the
/// action it runs. No sequence here is a prefix of another.
const DEFAULTS: &[(&str, &str)] = &[
    ("j", "move.down"),
    ("Down", "move.down"),
    ("k", "move.up"),
    ("Up", "move.up"),
    ("J", "move.worktree_down"),
    ("}", "move.worktree_down"),
    ("K", "move.worktree_up"),
    ("{", "move.worktree_up"),
    ("g g", "move.top"),
    ("G", "move.bottom"),
    ("H", "move.screen_top"),
    ("M", "move.screen_middle"),
    ("L", "move.screen_bottom"),
    ("Ctrl+d", "move.half_page_down"),
    ("Ctrl+u", "move.half_page_up"),
    ("Ctrl+f", "move.page_down"),
    ("PageDown", "move.page_down"),
    ("Ctrl+b", "move.page_up"),
    ("PageUp", "move.page_up"),
    ("Enter", "card.focus"),
    ("l", "card.focus"),
    ("1", "card.slot.1"),
    ("2", "card.slot.2"),
    ("3", "card.slot.3"),
    ("4", "card.slot.4"),
    ("5", "card.slot.5"),
    ("6", "card.slot.6"),
    ("7", "card.slot.7"),
    ("8", "card.slot.8"),
    ("9", "card.slot.9"),
    ("Tab", "card.last"),
    ("d d", "card.close"),
    ("m", "read.toggle"),
    ("U", "read.all"),
    (":", "palette.commands"),
    ("q", "pane.return"),
    ("/", "search.start"),
    ("n", "search.next"),
    ("N", "search.prev"),
    ("f ?", "filter.asks"),
    ("f !", "filter.failures"),
    ("f u", "filter.unread"),
    ("f w", "filter.working"),
    ("f p", "filter.paused"),
    ("f d", "filter.done"),
    ("f z", "filter.sleeping"),
    ("f i", "filter.idle"),
    ("f n", "filter.needs_you"),
    ("F", "filter.clear"),
    ("z a", "fold.toggle"),
    ("z R", "fold.open_all"),
    ("z M", "fold.close_all"),
    ("z i", "fold.inactive"),
    ("<", "width.narrower"),
    (">", "width.wider"),
    ("=", "width.reset"),
    ("Ctrl+l", "view.redraw"),
    ("R", "view.reload"),
    ("Esc", "view.cancel"),
    ("?", "help"),
    ("Left", "dashboard.prev"),
    ("Right", "dashboard.next"),
];

impl SidebarAction {
    fn parse(name: &str) -> Option<Self> {
        ACTIONS
            .iter()
            .find(|(candidate, ..)| *candidate == name)
            .map(|&(_, action, _)| action)
    }

    fn entry(self) -> &'static (&'static str, SidebarAction, &'static str) {
        ACTIONS
            .iter()
            .find(|(_, action, _)| *action == self)
            .expect("ACTIONS names every SidebarAction, pinned by tests")
    }

    /// The `[keys.sidebar]` value naming this action.
    pub fn name(self) -> &'static str {
        self.entry().0
    }

    /// The short label the pending-prefix hint and the help legend show.
    pub fn label(self) -> &'static str {
        self.entry().2
    }
}

/// How a key sequence pressed so far meets the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SequenceMatch {
    /// The sequence is bound to this action.
    Action(SidebarAction),
    /// The sequence is a strict prefix of at least one bound sequence.
    Pending,
    /// Nothing is bound to the sequence or starts with it.
    Unbound,
}

/// The resolved in-pane keymap: the defaults with `[keys.sidebar]` applied.
/// No bound sequence is a prefix of another, so a full match never also
/// waits for more keys.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SidebarKeymap {
    bindings: Vec<(Vec<SidebarChord>, SidebarAction)>,
}

impl Default for SidebarKeymap {
    fn default() -> Self {
        Self {
            bindings: DEFAULTS
                .iter()
                .map(|(sequence, action)| {
                    (
                        parse_sequence(sequence).expect("default sequences are pinned by tests"),
                        SidebarAction::parse(action).expect("default actions are pinned by tests"),
                    )
                })
                .collect(),
        }
    }
}

impl SidebarKeymap {
    /// Apply the raw `[keys.sidebar]` table over the defaults, recording each
    /// ignored entry under its dotted path.
    pub(in crate::config) fn resolve(user: &Value, notices: &mut BTreeMap<String, String>) -> Self {
        let mut map = Self::default();
        let Some(entries) = user.as_object() else {
            notices.insert(
                "keys.sidebar".into(),
                "keys.sidebar: expected a table of key sequences; in-pane bindings ignored".into(),
            );
            return map;
        };
        let mut bindings = Vec::new();
        for (raw, value) in entries {
            let path = format!("keys.sidebar.{raw:?}");
            match entry(raw, value) {
                Ok((sequence, None)) => map.bindings.retain(|(bound, _)| *bound != sequence),
                Ok((sequence, Some(action))) => bindings.push((path, sequence, action)),
                Err(reason) => {
                    notices.insert(path.clone(), format!("{path}: {reason}; binding ignored"));
                }
            }
        }
        for (path, sequence, action) in bindings {
            if let Some((other, _)) = map.bindings.iter().find(|(bound, _)| {
                bound.len() != sequence.len()
                    && bound[..bound.len().min(sequence.len())]
                        == sequence[..bound.len().min(sequence.len())]
            }) {
                let relation = if other.len() > sequence.len() {
                    "is a prefix of"
                } else {
                    "extends"
                };
                notices.insert(
                    path.clone(),
                    format!(
                        "{path}: \"{}\" {relation} the bound sequence \"{}\"; unbind one with \"none\"; binding ignored",
                        Sequence(&sequence),
                        Sequence(other),
                    ),
                );
                continue;
            }
            map.bindings.retain(|(bound, _)| *bound != sequence);
            map.bindings.push((sequence, action));
        }
        map
    }

    /// Resolve `[keys.sidebar]` entries given as sequence and action pairs, for
    /// tests that rebind the table.
    #[cfg(test)]
    pub(crate) fn rebound(entries: &[(&str, &str)]) -> Self {
        let table = entries
            .iter()
            .map(|&(key, action)| (key.to_owned(), Value::String(action.to_owned())))
            .collect();
        let mut notices = BTreeMap::new();
        let map = Self::resolve(&Value::Object(table), &mut notices);
        assert!(notices.is_empty(), "{notices:?}");
        map
    }

    /// How the keys pressed so far meet the table.
    pub fn lookup(&self, sequence: &[SidebarChord]) -> SequenceMatch {
        if let Some((_, action)) = self.bindings.iter().find(|(bound, _)| bound == sequence) {
            return SequenceMatch::Action(*action);
        }
        if self
            .bindings
            .iter()
            .any(|(bound, _)| bound.len() > sequence.len() && bound.starts_with(sequence))
        {
            return SequenceMatch::Pending;
        }
        SequenceMatch::Unbound
    }

    /// The bindings that continue `prefix`, as the chords after it and their
    /// action, in table order.
    pub fn continuations<'a>(
        &'a self,
        prefix: &'a [SidebarChord],
    ) -> impl Iterator<Item = (&'a [SidebarChord], SidebarAction)> + 'a {
        self.bindings
            .iter()
            .filter(move |(bound, _)| bound.len() > prefix.len() && bound.starts_with(prefix))
            .map(move |(bound, action)| (&bound[prefix.len()..], *action))
    }

    /// Every sequence bound to `action`, in table order: defaults first, then
    /// user entries.
    pub fn sequences(&self, action: SidebarAction) -> impl Iterator<Item = &[SidebarChord]> {
        self.bindings
            .iter()
            .filter(move |(_, bound)| *bound == action)
            .map(|(sequence, _)| sequence.as_slice())
    }
}

/// A sequence as `[keys.sidebar]` spells it: chords separated by spaces.
struct Sequence<'a>(&'a [SidebarChord]);

impl fmt::Display for Sequence<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, chord) in self.0.iter().enumerate() {
            if index > 0 {
                formatter.write_str(" ")?;
            }
            write!(formatter, "{chord}")?;
        }
        Ok(())
    }
}

fn parse_sequence(raw: &str) -> Result<Vec<SidebarChord>, String> {
    let sequence = raw
        .split_whitespace()
        .map(SidebarChord::parse)
        .collect::<Result<Vec<_>, _>>()?;
    if sequence.is_empty() {
        return Err("expected a key sequence such as g g or Ctrl+d".into());
    }
    Ok(sequence)
}

fn entry(raw: &str, value: &Value) -> Result<(Vec<SidebarChord>, Option<SidebarAction>), String> {
    let sequence = parse_sequence(raw)?;
    let name = value
        .as_str()
        .ok_or("expected an in-pane action name or \"none\"")?;
    if name == "none" {
        return Ok((sequence, None));
    }
    let action = SidebarAction::parse(name)
        .ok_or("unknown action; use an in-pane action such as move.top, or \"none\"")?;
    Ok((sequence, Some(action)))
}

#[cfg(test)]
mod tests;
