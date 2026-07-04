use serde::{Deserialize, Serialize};

use crate::ids::MuxName;

use super::MachineConfig;

/// Backend-selection preference. `default` is consulted after `--mux` and an
/// active mux env; unset resolves from installed binaries with tmux as the
/// tiebreak. A set-but-uninstalled backend fails fast at selection.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct MuxConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<MuxName>,
}

/// Multiplexer-only preferences, split out so CLI launch code can thread just
/// the settings a backend needs instead of the whole per-machine config.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MultiplexerConfig {
    pub zellij: ZellijConfig,
    pub tmux: TmuxConfig,
}

impl From<&MachineConfig> for MultiplexerConfig {
    fn from(config: &MachineConfig) -> Self {
        Self {
            zellij: config.zellij.clone(),
            tmux: config.tmux.clone(),
        }
    }
}

/// Zellij room options. Critical Rimz invariants are passed on every birth and
/// attach; optional fields are passed only when the user sets them here, so the
/// user's `~/.config/zellij/config.kdl` remains authoritative otherwise.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ZellijConfig {
    /// Runtime-only: `rimz web open` sets this on the mux config it threads
    /// into a web-born room so Zellij accepts browser clients. It is not a
    /// persisted user preference; normal rooms follow the user's Zellij config.
    #[serde(skip)]
    pub web_sharing: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mouse_mode: Option<bool>,
    pub mouse_click_through: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub advanced_mouse_actions: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mouse_hover_effects: Option<bool>,
    pub focus_follows_mouse: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_frames: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_force_close: Option<ZellijForceClose>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scroll_buffer_size: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_startup_tips: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_release_notes: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy_clipboard: Option<ZellijClipboard>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy_on_select: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub support_kitty_keyboard_protocol: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub osc8_hyperlinks: Option<bool>,
    /// Whether Zellij serializes this room to disk for later resurrection. Rimz
    /// keeps it off: a resurrected room comes back with every command pane
    /// `start_suspended` ("Waiting to run") and a dead mouse. Rimz owns rebirth,
    /// so a dead server leaves nothing to resurrect and the next start comes up
    /// clean and running. Passed as `--session-serialization false` on birth and
    /// attach.
    pub session_serialization: bool,
    /// Whether Zellij skips the per-second session metadata writer and command
    /// discovery loop. Rimz keeps it on because the loop rewrites
    /// `session-metadata.kdl` and forks `ps` even when session serialization is
    /// disabled.
    pub disable_session_metadata: bool,
}

impl Default for ZellijConfig {
    fn default() -> Self {
        Self {
            web_sharing: false,
            mouse_mode: None,
            mouse_click_through: true,
            advanced_mouse_actions: None,
            mouse_hover_effects: None,
            focus_follows_mouse: false,
            pane_frames: None,
            on_force_close: None,
            scroll_buffer_size: None,
            show_startup_tips: None,
            show_release_notes: None,
            copy_clipboard: None,
            copy_on_select: None,
            support_kitty_keyboard_protocol: None,
            osc8_hyperlinks: None,
            session_serialization: false,
            disable_session_metadata: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ZellijForceClose {
    #[default]
    Detach,
    Quit,
}

impl ZellijForceClose {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Detach => "detach",
            Self::Quit => "quit",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ZellijClipboard {
    #[default]
    System,
    Primary,
}

impl ZellijClipboard {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Primary => "primary",
        }
    }
}

/// Rimz-owned tmux room defaults. Session/window options stay scoped to the
/// Rimz session. Optional pane-border fields are passed only when the user sets
/// them here, so the user's `~/.tmux.conf` remains authoritative otherwise.
/// tmux server options are runtime-global inside the tmux server; Rimz sets
/// them because clipboard and rich-key support are server-scoped.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct TmuxConfig {
    pub mouse: bool,
    pub focus_events: bool,
    pub history_limit: u32,
    pub allow_passthrough: bool,
    pub set_clipboard: TmuxSetClipboard,
    pub extended_keys: bool,
    pub extended_keys_format: TmuxExtendedKeysFormat,
    pub escape_time_ms: u32,
    pub renumber_windows: bool,
    pub aggressive_resize: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_border_status: Option<TmuxPaneBorderStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_border_lines: Option<TmuxPaneBorderLines>,
}

impl Default for TmuxConfig {
    fn default() -> Self {
        Self {
            mouse: true,
            focus_events: true,
            history_limit: 100_000,
            allow_passthrough: true,
            set_clipboard: TmuxSetClipboard::On,
            extended_keys: true,
            extended_keys_format: TmuxExtendedKeysFormat::CsiU,
            escape_time_ms: 0,
            renumber_windows: true,
            aggressive_resize: true,
            pane_border_status: None,
            pane_border_lines: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TmuxSetClipboard {
    Off,
    External,
    #[default]
    On,
}

impl TmuxSetClipboard {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::External => "external",
            Self::On => "on",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum TmuxExtendedKeysFormat {
    #[default]
    #[serde(rename = "csi-u")]
    CsiU,
    #[serde(rename = "xterm")]
    Xterm,
}

impl TmuxExtendedKeysFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CsiU => "csi-u",
            Self::Xterm => "xterm",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TmuxPaneBorderStatus {
    #[default]
    Off,
    Top,
    Bottom,
}

impl TmuxPaneBorderStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Top => "top",
            Self::Bottom => "bottom",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TmuxPaneBorderLines {
    Single,
    Double,
    Heavy,
    #[default]
    Simple,
}

impl TmuxPaneBorderLines {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Double => "double",
            Self::Heavy => "heavy",
            Self::Simple => "simple",
        }
    }
}
