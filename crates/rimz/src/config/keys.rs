//! Backend-neutral room keys, action vocabulary, and machine-only overlays.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{MachineConfig, SidebarConfig, mux::RoomStyle};

pub mod sidebar;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct KeysConfig {
    keymap: Value,
    /// Null when unset, so the selected keymap's own prefix applies.
    #[serde(skip_serializing_if = "Value::is_null")]
    prefix: Value,
    bind: Value,
    pub(super) sidebar: Value,
}

impl Default for KeysConfig {
    fn default() -> Self {
        Self {
            keymap: Value::String("rimz".into()),
            prefix: Value::Null,
            bind: Value::Object(Default::default()),
            sidebar: Value::Object(Default::default()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum KeyChord {
    Character(char),
    Alt(char),
    Ctrl(char),
    Esc,
    Enter,
    Arrow(KeyDirection),
    AltArrow(KeyDirection),
    CtrlArrow(KeyDirection),
}

const ARROW_KEYS: [(&str, KeyDirection); 4] = [
    ("Left", KeyDirection::Left),
    ("Right", KeyDirection::Right),
    ("Up", KeyDirection::Up),
    ("Down", KeyDirection::Down),
];

fn arrow(raw: &str) -> Option<KeyDirection> {
    ARROW_KEYS
        .iter()
        .find(|(name, _)| *name == raw)
        .map(|&(_, direction)| direction)
}

impl KeyChord {
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        match raw {
            "Esc" => return Some(Self::Esc),
            "Enter" => return Some(Self::Enter),
            _ => {}
        }
        if let Some(key) = graphic(raw) {
            return Some(Self::Character(key));
        }
        if let Some(direction) = arrow(raw) {
            return Some(Self::Arrow(direction));
        }
        let (modifier, key) = raw.split_once(['+', '-'])?;
        let modifier = Modifier::parse(modifier)?;
        let key = key.trim();
        if let Some(direction) = arrow(key) {
            return Some(match modifier {
                Modifier::Alt => Self::AltArrow(direction),
                Modifier::Ctrl => Self::CtrlArrow(direction),
            });
        }
        let key = graphic(key)?;
        match modifier {
            Modifier::Alt => Some(Self::Alt(key)),
            Modifier::Ctrl => Some(Self::Ctrl(key)),
        }
    }

    pub(crate) fn is_modified(self) -> bool {
        matches!(
            self,
            Self::Alt(_) | Self::Ctrl(_) | Self::AltArrow(_) | Self::CtrlArrow(_)
        )
    }

    /// A room prefix is `Alt` or `Ctrl` with one ASCII graphic character.
    pub(super) fn parse_prefix(raw: &str) -> Option<Self> {
        Self::parse(raw)
            .filter(|key| matches!(key, Self::Alt(_) | Self::Ctrl(_)) && !key.is_terminal_control())
    }

    fn is_terminal_control(self) -> bool {
        matches!(self, Self::Ctrl(key) if is_terminal_control(key))
    }
}

/// The modifier names both key grammars accept, in any case.
#[derive(Clone, Copy)]
enum Modifier {
    Alt,
    Ctrl,
}

impl Modifier {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "alt" | "meta" | "m" => Some(Self::Alt),
            "ctrl" | "control" | "c" => Some(Self::Ctrl),
            _ => None,
        }
    }
}

/// Whether `Ctrl` with this key reaches a program as the terminal's Enter, Esc,
/// Tab, or Backspace byte, so the chord cannot be told apart from that key.
fn is_terminal_control(key: char) -> bool {
    matches!(
        key.to_ascii_lowercase(),
        'h' | 'i' | 'j' | 'm' | '[' | '{' | '?' | '3' | '8'
    )
}

const TERMINAL_CONTROL_REFUSAL: &str =
    "Ctrl aliases for terminal Enter, Esc, Tab, and Backspace cannot be bound here";

fn graphic(raw: &str) -> Option<char> {
    let mut chars = raw.chars();
    let key = chars.next()?;
    (chars.next().is_none() && key.is_ascii_graphic()).then_some(key)
}

impl fmt::Display for KeyChord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Character(key) => write!(formatter, "{key}"),
            Self::Alt(key) => write!(formatter, "Alt+{key}"),
            Self::Ctrl(key) => write!(formatter, "Ctrl+{key}"),
            Self::Esc => formatter.write_str("Esc"),
            Self::Enter => formatter.write_str("Enter"),
            Self::Arrow(direction) => formatter.write_str(arrow_name(*direction)),
            Self::AltArrow(direction) => write!(formatter, "Alt+{}", arrow_name(*direction)),
            Self::CtrlArrow(direction) => write!(formatter, "Ctrl+{}", arrow_name(*direction)),
        }
    }
}

fn arrow_name(direction: KeyDirection) -> &'static str {
    ARROW_KEYS
        .iter()
        .find(|(_, candidate)| *candidate == direction)
        .map_or("?", |(name, _)| name)
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum KeyMode {
    Normal,
    Prefix,
    Pane,
    Tab,
    Resize,
    Move,
}

impl KeyMode {
    const ALL: [Self; 6] = [
        Self::Normal,
        Self::Prefix,
        Self::Pane,
        Self::Tab,
        Self::Resize,
        Self::Move,
    ];

    fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.to_string() == name)
    }
}

impl fmt::Display for KeyMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Normal => "normal",
            Self::Prefix => "prefix",
            Self::Pane => "pane",
            Self::Tab => "tab",
            Self::Resize => "resize",
            Self::Move => "move",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum KeyDirection {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeySplitDirection {
    Right,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyPaletteMode {
    Cards,
    Commands,
    NewAgent,
}

impl KeyPaletteMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cards => "cards",
            Self::Commands => "commands",
            Self::NewAgent => "new",
        }
    }

    pub fn command(self) -> [&'static str; 4] {
        ["palette", "--popup", "--mode", self.as_str()]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyActionName {
    PaneFocus(KeyDirection),
    PaneFocusNext,
    PaneSwap(KeyDirection),
    PaneSplit(KeySplitDirection),
    PaneResize(KeyDirection),
    PaneClose,
    PaneSendPrefix,
    TabNew,
    TabNext,
    TabPrev,
    Mode(KeyMode),
    Scrollback,
    SessionDetach,
    KeysHelp,
    Command {
        name: &'static str,
        args: &'static [&'static str],
    },
    Palette(KeyPaletteMode),
}

use KeyActionName::{
    KeysHelp, Mode, Palette, PaneClose, PaneFocus, PaneFocusNext, PaneResize, PaneSendPrefix,
    PaneSplit, PaneSwap, Scrollback, SessionDetach, TabNew, TabNext, TabPrev,
};
use KeyDirection::{Down, Left, Right, Up};

const TYPED_ACTIONS: &[(&str, KeyActionName)] = &[
    ("pane.focus.left", PaneFocus(Left)),
    ("pane.focus.right", PaneFocus(Right)),
    ("pane.focus.up", PaneFocus(Up)),
    ("pane.focus.down", PaneFocus(Down)),
    ("pane.focus.next", PaneFocusNext),
    ("pane.swap.left", PaneSwap(Left)),
    ("pane.swap.right", PaneSwap(Right)),
    ("pane.swap.up", PaneSwap(Up)),
    ("pane.swap.down", PaneSwap(Down)),
    ("pane.split.right", PaneSplit(KeySplitDirection::Right)),
    ("pane.split.down", PaneSplit(KeySplitDirection::Down)),
    ("pane.resize.left", PaneResize(Left)),
    ("pane.resize.right", PaneResize(Right)),
    ("pane.resize.up", PaneResize(Up)),
    ("pane.resize.down", PaneResize(Down)),
    ("pane.close", PaneClose),
    ("pane.send_prefix", PaneSendPrefix),
    ("tab.new", TabNew),
    ("tab.next", TabNext),
    ("tab.prev", TabPrev),
    ("mode.prefix", Mode(KeyMode::Prefix)),
    ("mode.pane", Mode(KeyMode::Pane)),
    ("mode.tab", Mode(KeyMode::Tab)),
    ("mode.resize", Mode(KeyMode::Resize)),
    ("mode.move", Mode(KeyMode::Move)),
    ("mode.scrollback", Scrollback),
    ("mode.exit", Mode(KeyMode::Normal)),
    ("session.detach", SessionDetach),
    ("keys.help", KeysHelp),
    ("palette.cards", Palette(KeyPaletteMode::Cards)),
    ("palette.commands", Palette(KeyPaletteMode::Commands)),
    ("palette.new_agent", Palette(KeyPaletteMode::NewAgent)),
];

const RIMZ_COMMANDS: &[(&str, &[&str])] = &[
    ("card.next", &["sidebar", "walk", "next"]),
    ("card.prev", &["sidebar", "walk", "prev"]),
    ("worktree.next", &["sidebar", "walk", "next", "--worktree"]),
    ("worktree.prev", &["sidebar", "walk", "prev", "--worktree"]),
    ("card.slot.1", &["sidebar", "slot", "1"]),
    ("card.slot.2", &["sidebar", "slot", "2"]),
    ("card.slot.3", &["sidebar", "slot", "3"]),
    ("card.slot.4", &["sidebar", "slot", "4"]),
    ("card.slot.5", &["sidebar", "slot", "5"]),
    ("card.slot.6", &["sidebar", "slot", "6"]),
    ("card.slot.7", &["sidebar", "slot", "7"]),
    ("card.slot.8", &["sidebar", "slot", "8"]),
    ("card.slot.9", &["sidebar", "slot", "9"]),
    ("card.last", &["sidebar", "last"]),
    (
        "filter.needs_you",
        &["sidebar", "filter", "needs-you", "--toggle"],
    ),
    ("card.close", &["sidebar", "close"]),
    ("card.reopen", &["sidebar", "reopen"]),
    ("sidebar.focus", &["sidebar", "focus", "--toggle"]),
    ("pane.zoom", &["pane", "zoom"]),
];

impl KeyActionName {
    #[cfg(test)]
    pub(crate) fn vocabulary() -> impl Iterator<Item = &'static str> {
        TYPED_ACTIONS
            .iter()
            .map(|(name, _)| *name)
            .chain(RIMZ_COMMANDS.iter().map(|(name, _)| *name))
    }

    pub fn parse(name: &str) -> Option<Self> {
        TYPED_ACTIONS
            .iter()
            .find(|(candidate, _)| *candidate == name)
            .map(|(_, action)| *action)
            .or_else(|| {
                RIMZ_COMMANDS
                    .iter()
                    .find(|(candidate, _)| *candidate == name)
                    .map(|&(name, args)| Self::Command { name, args })
            })
    }

    pub fn changes_mode(self) -> bool {
        matches!(self, Self::Mode(_) | Self::Scrollback)
    }

    /// Whether a pane, tab, resize, or move mode stays entered after this action.
    pub fn is_motion(self) -> bool {
        match self {
            Self::PaneFocus(_)
            | Self::PaneFocusNext
            | Self::PaneSwap(_)
            | Self::PaneResize(_)
            | Self::TabNext
            | Self::TabPrev => true,
            Self::Command { name, .. } => matches!(
                name,
                "card.next" | "card.prev" | "worktree.next" | "worktree.prev"
            ),
            _ => false,
        }
    }
}

impl fmt::Display for KeyDirection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Up => "up",
            Self::Down => "down",
        })
    }
}

impl fmt::Display for KeyActionName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PaneFocus(direction) => write!(formatter, "pane.focus.{direction}"),
            Self::PaneFocusNext => formatter.write_str("pane.focus.next"),
            Self::PaneSendPrefix => formatter.write_str("pane.send_prefix"),
            Self::PaneSwap(direction) => write!(formatter, "pane.swap.{direction}"),
            Self::PaneResize(direction) => write!(formatter, "pane.resize.{direction}"),
            Self::PaneSplit(KeySplitDirection::Right) => formatter.write_str("pane.split.right"),
            Self::PaneSplit(KeySplitDirection::Down) => formatter.write_str("pane.split.down"),
            Self::PaneClose => formatter.write_str("pane.close"),
            Self::TabNew => formatter.write_str("tab.new"),
            Self::TabNext => formatter.write_str("tab.next"),
            Self::TabPrev => formatter.write_str("tab.prev"),
            Self::Mode(KeyMode::Normal) => formatter.write_str("mode.exit"),
            Self::Mode(mode) => write!(formatter, "mode.{mode}"),
            Self::Scrollback => formatter.write_str("mode.scrollback"),
            Self::SessionDetach => formatter.write_str("session.detach"),
            Self::KeysHelp => formatter.write_str("keys.help"),
            Self::Command { name, .. } => formatter.write_str(name),
            Self::Palette(KeyPaletteMode::Cards) => formatter.write_str("palette.cards"),
            Self::Palette(KeyPaletteMode::Commands) => formatter.write_str("palette.commands"),
            Self::Palette(KeyPaletteMode::NewAgent) => formatter.write_str("palette.new_agent"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyAction {
    Named(KeyActionName),
    Run { args: Vec<String> },
}

impl fmt::Display for KeyAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Named(action) => action.fmt(formatter),
            Self::Run { args } => formatter.write_str(
                &shlex::try_join(std::iter::once("rimz").chain(args.iter().map(String::as_str)))
                    .map_err(|_| fmt::Error)?,
            ),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoomKeymap {
    pub modes: BTreeMap<KeyMode, BTreeMap<KeyChord, KeyAction>>,
    /// The RimZ-style facts the backends compile from; `None` in native style.
    pub style: Option<RoomKeyStyle>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoomKeyStyle {
    pub prefix: KeyChord,
    pub scrollback: KeyScrollback,
}

/// Which keys a RimZ-style room's scrollback answers to.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KeyScrollback {
    /// RimZ's vi-style table on both backends.
    Vi,
    /// The multiplexer's own stock keys, as the user configured them.
    Native,
}

impl MachineConfig {
    /// Resolve machine-only room keys without consulting a running multiplexer.
    pub fn room_keymap(&self) -> RoomKeymap {
        self.resolve_room_keymap(&mut BTreeMap::new())
    }

    /// Resolve the sidebar's in-pane keymap: the defaults with `[keys.sidebar]`
    /// applied, under either room style.
    pub fn sidebar_keymap(&self) -> sidebar::SidebarKeymap {
        sidebar::SidebarKeymap::resolve(&self.keys.sidebar, &mut BTreeMap::new())
    }

    pub(super) fn resolve_room_keymap(&self, notices: &mut BTreeMap<String, String>) -> RoomKeymap {
        if self.room.style == RoomStyle::Native {
            let mut normal = BTreeMap::new();
            for (key, action) in [
                (&self.sidebar.focus_key, "sidebar.focus"),
                (&self.sidebar.zoom_key, "pane.zoom"),
            ] {
                if let Some(chord) = SidebarConfig::key_label(key)
                    .and_then(KeyChord::parse)
                    .filter(|chord| chord.is_modified())
                {
                    normal.insert(
                        chord,
                        KeyAction::Named(
                            KeyActionName::parse(action)
                                .expect("the command table names both sidebar actions"),
                        ),
                    );
                }
            }
            return RoomKeymap {
                modes: BTreeMap::from([(KeyMode::Normal, normal)]),
                style: None,
            };
        }

        let keymap = self
            .keys
            .keymap
            .as_str()
            .and_then(bundled_keymap)
            .unwrap_or_else(|| {
                notices.insert(
                    "keys.keymap".into(),
                    format!(
                        "keys.keymap: expected {}; using the rimz keymap",
                        bundled_keymap_names()
                    ),
                );
                bundled_keymap("rimz").expect("the rimz keymap is bundled")
            });
        let prefix = if self.keys.prefix.is_null() {
            keymap.default_prefix
        } else {
            self.keys
                .prefix
                .as_str()
                .and_then(KeyChord::parse_prefix)
                .unwrap_or_else(|| {
                    notices.insert(
                        "keys.prefix".into(),
                        format!(
                            "keys.prefix: expected a modified chord such as Alt+p, not a terminal \
                             key alias; using {}",
                            keymap.default_prefix
                        ),
                    );
                    keymap.default_prefix
                })
        };

        let mut map = RoomKeymap {
            modes: BTreeMap::new(),
            style: Some(RoomKeyStyle {
                prefix,
                scrollback: keymap.scrollback,
            }),
        };
        for (&mode, bindings) in &keymap.modes {
            let resolved = map.modes.entry(mode).or_default();
            for (key, action) in bindings {
                if key != "<prefix>" {
                    resolved.insert(
                        KeyChord::parse(key).expect("embedded key grammar is pinned by tests"),
                        KeyAction::Named(
                            KeyActionName::parse(action)
                                .expect("embedded actions are pinned by tests"),
                        ),
                    );
                }
            }
        }
        for mode in KeyMode::ALL {
            map.modes.entry(mode).or_default();
        }
        overlay(&mut map, &self.keys.bind, notices);
        for (&mode, bindings) in &keymap.modes {
            if let Some(action) = bindings.get("<prefix>") {
                map.modes.entry(mode).or_default().insert(
                    prefix,
                    KeyAction::Named(
                        KeyActionName::parse(action)
                            .expect("embedded prefix actions are pinned by tests"),
                    ),
                );
            }
        }
        map
    }
}

/// One bundled keymap file: its default prefix (`default_prefix`, since `prefix`
/// names the prefix-mode table), its scrollback flavour, and its
/// mode tables, where `<prefix>` stands for the resolved prefix chord.
#[derive(Deserialize)]
struct BundledKeymap {
    #[serde(deserialize_with = "deserialize_prefix")]
    default_prefix: KeyChord,
    scrollback: KeyScrollback,
    #[serde(flatten)]
    modes: BTreeMap<KeyMode, BTreeMap<String, String>>,
}

fn deserialize_prefix<'de, D: serde::Deserializer<'de>>(raw: D) -> Result<KeyChord, D::Error> {
    let raw = String::deserialize(raw)?;
    KeyChord::parse_prefix(&raw)
        .ok_or_else(|| serde::de::Error::custom(format!("invalid keymap prefix {raw:?}")))
}

const BUNDLED_KEYMAP_FILES: &[(&str, &str)] = &[
    ("rimz", include_str!("../../keymaps/rimz.toml")),
    ("tmux", include_str!("../../keymaps/tmux.toml")),
    ("zellij", include_str!("../../keymaps/zellij.toml")),
    ("vim", include_str!("../../keymaps/vim.toml")),
];

static BUNDLED_KEYMAPS: LazyLock<Vec<(&str, BundledKeymap)>> = LazyLock::new(|| {
    BUNDLED_KEYMAP_FILES
        .iter()
        .map(|&(name, text)| {
            (
                name,
                toml::from_str(text).expect("bundled keymap TOML is pinned by tests"),
            )
        })
        .collect()
});

fn bundled_keymap(name: &str) -> Option<&'static BundledKeymap> {
    BUNDLED_KEYMAPS
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, keymap)| keymap)
}

pub(super) fn is_bundled_keymap(name: &str) -> bool {
    BUNDLED_KEYMAP_FILES
        .iter()
        .any(|(candidate, _)| *candidate == name)
}

/// The bundled keymap names as a sentence list: `rimz, tmux, zellij, or vim`.
pub(super) fn bundled_keymap_names() -> String {
    let names: Vec<_> = BUNDLED_KEYMAP_FILES.iter().map(|(name, _)| *name).collect();
    match names.split_last() {
        Some((last, [])) => (*last).to_owned(),
        Some((last, [only])) => format!("{only} or {last}"),
        Some((last, rest)) => format!("{}, or {last}", rest.join(", ")),
        None => String::new(),
    }
}

fn overlay(map: &mut RoomKeymap, user: &Value, notices: &mut BTreeMap<String, String>) {
    let Some(modes) = user.as_object() else {
        notices.insert(
            "keys.bind".into(),
            "keys.bind: expected normal, prefix, pane, tab, resize, or move tables; bindings ignored"
                .into(),
        );
        return;
    };
    for (name, entries) in modes {
        let path = format!("keys.bind.{name}");
        let Some(mode) = KeyMode::parse(name) else {
            notices.insert(
                path.clone(),
                format!(
                    "{path}: expected normal, prefix, pane, tab, resize, or move; bindings ignored"
                ),
            );
            continue;
        };
        let Some(entries) = entries.as_object() else {
            notices.insert(
                path.clone(),
                format!("{path}: expected a table of key bindings; bindings ignored"),
            );
            continue;
        };
        for (key, value) in entries {
            let path = format!("{path}.{key:?}");
            match binding(mode, key, value) {
                Ok((key, Some(action))) => {
                    map.modes.entry(mode).or_default().insert(key, action);
                }
                Ok((key, None)) => {
                    map.modes.entry(mode).or_default().remove(&key);
                }
                Err(reason) => {
                    notices.insert(path.clone(), format!("{path}: {reason}; binding ignored"));
                }
            }
        }
    }
}

fn binding(
    mode: KeyMode,
    raw: &str,
    value: &Value,
) -> Result<(KeyChord, Option<KeyAction>), &'static str> {
    let key = KeyChord::parse(raw)
        .ok_or("expected an ASCII key such as x, Alt+x, Ctrl+x, Esc, Enter, or Left")?;
    if mode == KeyMode::Normal && !key.is_modified() {
        return Err("normal-mode keys need an Alt or Ctrl modifier, such as Alt+x");
    }
    if matches!(mode, KeyMode::Normal | KeyMode::Prefix) && key.is_terminal_control() {
        return Err(TERMINAL_CONTROL_REFUSAL);
    }
    if let Some(name) = value.as_str() {
        return if name == "none" {
            Ok((key, None))
        } else {
            KeyActionName::parse(name)
                .map(|name| (key, Some(KeyAction::Named(name))))
                .ok_or("unknown action; use a named action such as card.close, or { run = \"rimz ...\" }")
        };
    }
    let run = value
        .as_object()
        .ok_or("expected an action name or { run = \"rimz ...\" }")?;
    if run.len() != 1 || !run.contains_key("run") {
        return Err("a command binding accepts only `run`; use { run = \"rimz ...\" }");
    }
    let run = run["run"]
        .as_str()
        .ok_or("run must be a string starting with `rimz`")?;
    let mut args =
        shlex::split(run).ok_or("run has unmatched quotes; quote each argument as in a shell")?;
    if args.first().map(String::as_str) != Some("rimz") {
        return Err("run must start with `rimz`; other programs are not room key actions");
    }
    if args.iter().any(|arg| arg.contains('\0')) {
        return Err("run arguments must not contain NUL");
    }
    args.remove(0);
    Ok((key, Some(KeyAction::Run { args })))
}

#[cfg(test)]
mod tests;
