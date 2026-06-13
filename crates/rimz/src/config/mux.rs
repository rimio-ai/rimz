use serde::{Deserialize, Serialize};

use super::MachineConfig;

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

/// Rimz-owned Zellij room defaults. These are passed as `zellij attach …
/// options …` when a Rimz session is born or reattached, so they do not require
/// editing `~/.config/zellij/config.kdl`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ZellijConfig {
    pub mouse_mode: bool,
    pub mouse_click_through: bool,
    pub advanced_mouse_actions: bool,
    pub mouse_hover_effects: bool,
    pub focus_follows_mouse: bool,
    pub pane_frames: bool,
    pub on_force_close: ZellijForceClose,
    pub scroll_buffer_size: u32,
    pub show_startup_tips: bool,
    pub show_release_notes: bool,
    pub copy_clipboard: ZellijClipboard,
    pub copy_on_select: bool,
    pub support_kitty_keyboard_protocol: bool,
    pub osc8_hyperlinks: bool,
    /// Whether Zellij applies swap layouts when panes open or close. Rimz keeps
    /// it on because explicit Rimz-opened agent layouts carry a swap layout that
    /// pins the fixed sidebar and rebalances the work area on no-direction
    /// `NewPane`.
    pub auto_layout: bool,
    /// Whether Zellij serializes this room to disk for later resurrection. Rimz
    /// keeps it off: a resurrected room comes back with every command pane
    /// `start_suspended` ("Waiting to run") and a dead mouse. Rimz owns rebirth,
    /// so a dead server leaves nothing to resurrect and the next start comes up
    /// clean and running. Passed as `--session-serialization false` on birth and
    /// attach.
    pub session_serialization: bool,
}

impl Default for ZellijConfig {
    fn default() -> Self {
        Self {
            mouse_mode: true,
            mouse_click_through: true,
            advanced_mouse_actions: false,
            mouse_hover_effects: false,
            focus_follows_mouse: false,
            pane_frames: false,
            on_force_close: ZellijForceClose::Detach,
            scroll_buffer_size: 100_000,
            show_startup_tips: false,
            show_release_notes: false,
            copy_clipboard: ZellijClipboard::System,
            copy_on_select: true,
            support_kitty_keyboard_protocol: true,
            osc8_hyperlinks: true,
            auto_layout: true,
            session_serialization: false,
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
/// Rimz session. tmux server options are runtime-global inside the tmux server;
/// Rimz sets them because clipboard and rich-key support are server-scoped.
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
    pub pane_border_status: TmuxPaneBorderStatus,
    pub pane_border_lines: TmuxPaneBorderLines,
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
            pane_border_status: TmuxPaneBorderStatus::Off,
            pane_border_lines: TmuxPaneBorderLines::Simple,
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
