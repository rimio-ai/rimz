//! Per-machine settings, loaded from `~/.config/rimz/config.toml`.
//!
//! This is the personal, never-committed tier. The project-committed tier is
//! `<root>/.rimz/config.toml`, parsed for the executable-surface hash in
//! [`crate::trust`]. Settings here are machine-wide preferences that tune how
//! Rimz drives *your* box or link *your* accounts, so they live outside the
//! repo and outside the trust hash — a clone never inherits them.
//!
//! Loading is best-effort by contract: a missing file is the default config,
//! and unknown keys are ignored so an older binary tolerates a newer file.

use std::collections::BTreeMap;
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ledger::paths::config_home;

const CONFIG_FILE: &str = "config.toml";
const RIMZ_CONFIG_SUBDIR: &str = "rimz";

#[derive(Debug, thiserror::Error)]
pub enum ConfigErr {
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing per-machine config at {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

pub type Result<T> = std::result::Result<T, ConfigErr>;

/// Per-machine configuration. Lenient on unknown keys so a newer config never
/// breaks an older binary, and every field defaults so the smallest useful file
/// is a single section.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct MachineConfig {
    pub remote_control: RemoteControlConfig,
    pub sidebar: SidebarConfig,
    pub zellij: ZellijConfig,
    pub tmux: TmuxConfig,
    pub resume: ResumeConfig,
}

/// Resume-on-rebirth behavior. When a session is reborn — reboot, multiplexer
/// crash, or a Rimz-initiated rebirth of a stuck room — Rimz re-seeds the prior
/// agents from the durable rollup so the room comes up where the user left off
/// instead of empty. Backend-neutral product behavior the cli reads directly,
/// not a multiplexer preference.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ResumeConfig {
    /// Re-seed prior agents on any session birth. `--no-resume` overrides it
    /// per-invocation for a deliberately fresh start.
    pub on_rebirth: bool,
    /// Ceiling on agents auto-resumed into one reborn session, bounding the
    /// processes a long-lived workspace launches at birth. Overflow is reported,
    /// never silently dropped.
    pub max: usize,
}

impl Default for ResumeConfig {
    fn default() -> Self {
        Self {
            on_rebirth: true,
            max: crate::resume::DEFAULT_RESUME_MAX,
        }
    }
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

/// Rimz-owned Zellij room defaults. These are passed as `zellij attach …
/// options …` when a Rimz session is born or reattached, so they do not require
/// editing `~/.config/zellij/config.kdl`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ZellijConfig {
    pub mouse_mode: bool,
    pub mouse_click_through: bool,
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

/// Sidebar display preferences. A personal, machine-wide tuning of how the
/// renderer paints; it never affects ledger correctness.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct SidebarConfig {
    /// Per-provider styling for the bottom dashboard panel, keyed by agent kind
    /// (`claude`/`codex`/`pi`/…). Any field a user omits falls back to the
    /// built-in default for that kind, so overriding just the color leaves the
    /// shipped emblem intact.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub providers: BTreeMap<String, SidebarProviderStyle>,
    /// Most provider blocks the pinned dashboard shows before the rest are
    /// elided. Providers are few, so the cap rarely bites; it bounds the panel
    /// height on a box that links many accounts.
    pub max_provider_blocks: usize,
    /// Cap on the sidebar pane width in columns. Every sidebar pane targets the
    /// standard percentage of the view at this cap; on an ultra-wide terminal
    /// the percentage alone grows absurd, so a pane born above the cap is
    /// shrunk to it once, when it is created. Creation-time only: a manual
    /// resize afterwards sticks.
    pub max_cols: NonZeroU16,
    /// The context meter's severity bands — where the card's context read
    /// leaves calm blue for yellow, amber, and red. Display-only; it tunes the
    /// colour ramp, never the ledger.
    pub context: ContextSeverityConfig,
    /// Preferred comparison target for the worktree header's git stats (the
    /// `+/-` diff, the `⇡`/`⇣` commit delta, and the `≡` landed marker). Tried
    /// first in the trunk ladder, per repo: a repo where the branch doesn't
    /// resolve falls back to the `main` → `master` → remote-default detection,
    /// so one machine-wide value never breaks other projects. Unset means
    /// detection alone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trunk: Option<String>,
}

impl Default for SidebarConfig {
    fn default() -> Self {
        Self {
            providers: BTreeMap::new(),
            max_provider_blocks: default_max_provider_blocks(),
            max_cols: default_sidebar_max_cols(),
            context: ContextSeverityConfig::default(),
            trunk: None,
        }
    }
}

/// The context meter's severity bands: each tier names the inclusive lower
/// bound where it begins, on both axes — the fill percentage and the absolute
/// tokens in the window. Severity is the worse of the two axes, so a
/// large-window model calm by percentage still warms by sheer volume. Below
/// `yellow` on both axes the meter rests calm blue.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextSeverityConfig {
    /// Where the meter leaves calm blue for yellow.
    pub yellow: ContextBand,
    /// Where yellow deepens to amber.
    pub amber: ContextBand,
    /// Where amber escalates to red.
    pub red: ContextBand,
}

impl Default for ContextSeverityConfig {
    fn default() -> Self {
        Self {
            yellow: ContextBand {
                percent: 60,
                tokens: 160_000,
            },
            // 258k matches Codex's effective GPT-5.5 window (272k catalog ×
            // 95%), so a Codex session deepens to amber as it crosses its own
            // ceiling.
            amber: ContextBand {
                percent: 80,
                tokens: 258_000,
            },
            red: ContextBand {
                percent: 95,
                tokens: 420_000,
            },
        }
    }
}

/// One severity tier's entry thresholds: the tier begins once *either* axis
/// reaches its value (`value >= threshold`, inclusive).
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ContextBand {
    /// Fill percentage (0–100) of the context window.
    pub percent: u8,
    /// Absolute tokens occupying the window.
    pub tokens: u64,
}

/// Default column cap on the sidebar pane width: comfortably past the widest
/// card tier while keeping a 30% split from swallowing an ultra-wide terminal.
fn default_sidebar_max_cols() -> NonZeroU16 {
    // Provably non-zero literal.
    NonZeroU16::new(72).expect("non-zero literal")
}

/// Default cap on provider blocks in the bottom dashboard.
fn default_max_provider_blocks() -> usize {
    3
}

/// Per-provider styling: the ASCII emblem and brand color for the bottom
/// dashboard. Every field is optional; an omitted field uses the built-in
/// default for the provider kind, so a user overrides just the art or just the
/// color without restating both.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct SidebarProviderStyle {
    /// Display name for the panel header (`Claude`, `Codex`, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_name: Option<String>,
    /// Multi-line ASCII emblem painted at the left of the provider block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ascii_art: Option<String>,
    /// 256-color index for the emblem (the provider's brand color).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<u8>,
}

/// Remote-control auto-launch policy, per agent. Off unless explicitly enabled
/// — Rimz never links your account or starts a remote-control host without
/// opt-in, so the absence of this section reads as "do nothing". Each agent has
/// its own toggle because each links a different account and is detected
/// independently — Claude on PATH, Codex by its managed standalone install.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RemoteControlConfig {
    /// Auto-launch `claude remote-control` (the worktree spawn mode) in the
    /// managed background view when Claude is on PATH and a workspace starts.
    pub claude: bool,
    /// Ensure the per-user Codex app-server daemon by spawning `codex
    /// remote-control start` detached on workspace start — a per-user singleton
    /// (one control socket), not a pane. `remote-control start` boots its daemon
    /// from the managed standalone install, so when this is on that install must
    /// be present (a `codex` on PATH alone won't do); otherwise `rimz start`
    /// refuses fail-fast with the fix. The daemon it brings up is the one Codex
    /// enrichment re-uses over the control socket.
    pub codex: bool,
}

impl MachineConfig {
    /// The per-machine config path: `$XDG_CONFIG_HOME/rimz/config.toml`
    /// (`~/.config/rimz/config.toml`).
    pub fn path() -> PathBuf {
        config_home().join(RIMZ_CONFIG_SUBDIR).join(CONFIG_FILE)
    }

    /// Load from the default per-machine path. A missing file is the default
    /// config — never an error.
    pub fn load() -> Result<Self> {
        Self::load_from(&Self::path())
    }

    /// Load from an explicit path — the test and tooling seam.
    pub fn load_from(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|source| ConfigErr::Parse {
                path: path.to_path_buf(),
                source,
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigErr::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(dir: &tempfile::TempDir, text: &str) -> PathBuf {
        let path = dir.path().join("config.toml");
        std::fs::write(&path, text).expect("write config");
        path
    }

    #[test]
    fn missing_file_is_default_off() {
        let dir = tempdir().expect("tempdir");
        let config = MachineConfig::load_from(&dir.path().join("absent.toml")).expect("load");
        assert_eq!(config, MachineConfig::default());
        assert!(!config.remote_control.claude);
        assert!(!config.remote_control.codex);
    }

    #[test]
    fn empty_file_keeps_remote_control_off() {
        let dir = tempdir().expect("tempdir");
        let config = MachineConfig::load_from(&write(&dir, "")).expect("load");
        assert!(!config.remote_control.claude);
        assert!(!config.remote_control.codex);
    }

    #[test]
    fn per_agent_toggles_parse_independently() {
        let dir = tempdir().expect("tempdir");
        let config = MachineConfig::load_from(&write(&dir, "[remote_control]\nclaude = true\n"))
            .expect("load");
        assert!(config.remote_control.claude);
        assert!(!config.remote_control.codex, "codex stays off when unset");

        let both = MachineConfig::load_from(&write(
            &dir,
            "[remote_control]\nclaude = true\ncodex = true\n",
        ))
        .expect("load");
        assert!(both.remote_control.claude);
        assert!(both.remote_control.codex);
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let dir = tempdir().expect("tempdir");
        let text = "sound_profile = \"chime\"\n\n[remote_control]\ncodex = true\ncapacity = 16\n";
        let config = MachineConfig::load_from(&write(&dir, text)).expect("load");
        assert!(config.remote_control.codex);
        assert!(!config.remote_control.claude);
    }

    #[test]
    fn sidebar_max_cols_defaults_parses_and_rejects_zero() {
        let dir = tempdir().expect("tempdir");
        let config =
            MachineConfig::load_from(&write(&dir, "[sidebar]\nmax_cols = 100\n")).expect("load");
        assert_eq!(
            config.sidebar.max_cols,
            NonZeroU16::new(100).expect("nonzero")
        );
        assert_eq!(
            MachineConfig::default().sidebar.max_cols.get(),
            72,
            "unset caps the percentage split at the 72-column default",
        );
        // A zero-width sidebar can never work: fail at config load, with the
        // parse error naming the field, rather than launching a broken pane.
        assert!(MachineConfig::load_from(&write(&dir, "[sidebar]\nmax_cols = 0\n")).is_err());
    }

    #[test]
    fn sidebar_trunk_parses_and_defaults_unset() {
        let dir = tempdir().expect("tempdir");
        let config = MachineConfig::load_from(&write(&dir, "[sidebar]\ntrunk = \"develop\"\n"))
            .expect("load");
        assert_eq!(config.sidebar.trunk.as_deref(), Some("develop"));
        assert_eq!(
            MachineConfig::default().sidebar.trunk,
            None,
            "unset leaves the trunk ladder to detection alone",
        );
    }

    #[test]
    fn zellij_room_defaults_are_agent_friendly() {
        let dir = tempdir().expect("tempdir");
        let config = MachineConfig::load_from(&write(&dir, "")).expect("load");
        assert!(config.zellij.mouse_mode);
        assert!(config.zellij.mouse_click_through);
        assert!(!config.zellij.focus_follows_mouse);
        assert!(!config.zellij.pane_frames);
        assert_eq!(config.zellij.on_force_close, ZellijForceClose::Detach);
        assert_eq!(config.zellij.scroll_buffer_size, 100_000);
        assert!(!config.zellij.show_startup_tips);
        assert!(!config.zellij.show_release_notes);
        assert_eq!(config.zellij.copy_clipboard, ZellijClipboard::System);
        assert!(config.zellij.copy_on_select);
        assert!(config.zellij.support_kitty_keyboard_protocol);
        assert!(config.zellij.osc8_hyperlinks);
        assert!(!config.zellij.session_serialization);
    }

    #[test]
    fn zellij_room_options_parse() {
        let dir = tempdir().expect("tempdir");
        let config = MachineConfig::load_from(&write(
            &dir,
            "[zellij]\n\
             pane_frames = true\n\
             focus_follows_mouse = false\n\
             copy_clipboard = \"primary\"\n\
             on_force_close = \"quit\"\n",
        ))
        .expect("load");
        assert!(config.zellij.pane_frames);
        assert!(!config.zellij.focus_follows_mouse);
        assert_eq!(config.zellij.copy_clipboard, ZellijClipboard::Primary);
        assert_eq!(config.zellij.on_force_close, ZellijForceClose::Quit);
    }

    #[test]
    fn tmux_room_defaults_are_agent_friendly() {
        let dir = tempdir().expect("tempdir");
        let config = MachineConfig::load_from(&write(&dir, "")).expect("load");
        assert!(config.tmux.mouse);
        assert!(config.tmux.focus_events);
        assert_eq!(config.tmux.history_limit, 100_000);
        assert!(config.tmux.allow_passthrough);
        assert_eq!(config.tmux.set_clipboard, TmuxSetClipboard::On);
        assert!(config.tmux.extended_keys);
        assert_eq!(
            config.tmux.extended_keys_format,
            TmuxExtendedKeysFormat::CsiU,
        );
        assert_eq!(config.tmux.escape_time_ms, 0);
        assert!(config.tmux.renumber_windows);
        assert!(config.tmux.aggressive_resize);
        assert_eq!(config.tmux.pane_border_status, TmuxPaneBorderStatus::Off);
        assert_eq!(config.tmux.pane_border_lines, TmuxPaneBorderLines::Simple);
    }

    #[test]
    fn tmux_room_options_parse() {
        let dir = tempdir().expect("tempdir");
        let config = MachineConfig::load_from(&write(
            &dir,
            "[tmux]\n\
             set_clipboard = \"external\"\n\
             extended_keys_format = \"xterm\"\n\
             pane_border_status = \"top\"\n\
             pane_border_lines = \"heavy\"\n",
        ))
        .expect("load");
        assert_eq!(config.tmux.set_clipboard, TmuxSetClipboard::External);
        assert_eq!(
            config.tmux.extended_keys_format,
            TmuxExtendedKeysFormat::Xterm,
        );
        assert_eq!(config.tmux.pane_border_status, TmuxPaneBorderStatus::Top);
        assert_eq!(config.tmux.pane_border_lines, TmuxPaneBorderLines::Heavy);
    }

    #[test]
    fn malformed_toml_surfaces_an_error() {
        let dir = tempdir().expect("tempdir");
        let err = MachineConfig::load_from(&write(&dir, "[remote_control]\nclaude = \"yes\"\n"))
            .expect_err("type mismatch should fail");
        assert!(matches!(err, ConfigErr::Parse { .. }));
    }

    #[test]
    fn provider_block_cap_defaults_to_three() {
        let dir = tempdir().expect("tempdir");
        let config = MachineConfig::load_from(&write(&dir, "")).expect("load");
        assert_eq!(config.sidebar.max_provider_blocks, 3);
        // Set just one sidebar field: the cap still falls back to its default.
        let partial =
            MachineConfig::load_from(&write(&dir, "[sidebar]\nmax_cols = 60\n")).expect("load");
        assert_eq!(partial.sidebar.max_provider_blocks, 3);
    }

    #[test]
    fn context_severity_bands_default_and_parse() {
        let dir = tempdir().expect("tempdir");
        // The shipped bands: yellow 60% / 160k, amber 80% / 258k (Codex's
        // effective GPT-5.5 window), red 95% / 420k.
        let config = MachineConfig::load_from(&write(&dir, "")).expect("load");
        let defaults = ContextSeverityConfig::default();
        assert_eq!(config.sidebar.context, defaults);
        assert_eq!(defaults.yellow.percent, 60);
        assert_eq!(defaults.yellow.tokens, 160_000);
        assert_eq!(defaults.amber.percent, 80);
        assert_eq!(defaults.amber.tokens, 258_000);
        assert_eq!(defaults.red.percent, 95);
        assert_eq!(defaults.red.tokens, 420_000);

        // A tuned tier states both axes together; an omitted tier keeps its
        // default.
        let tuned = MachineConfig::load_from(&write(
            &dir,
            "[sidebar.context]\nred = { percent = 50, tokens = 100000 }\n",
        ))
        .expect("load");
        assert_eq!(
            tuned.sidebar.context.red,
            ContextBand {
                percent: 50,
                tokens: 100_000
            }
        );
        assert_eq!(tuned.sidebar.context.yellow, defaults.yellow);
        assert_eq!(tuned.sidebar.context.amber, defaults.amber);
    }

    #[test]
    fn provider_style_parses_art_and_color() {
        let dir = tempdir().expect("tempdir");
        let text = "[sidebar.providers.claude]\ncolor = 173\nascii_art = \" ▐▛███▜▌\"\n";
        let config = MachineConfig::load_from(&write(&dir, text)).expect("load");
        let claude = config
            .sidebar
            .providers
            .get("claude")
            .expect("claude provider style");
        assert_eq!(claude.color, Some(173));
        assert_eq!(claude.ascii_art.as_deref(), Some(" ▐▛███▜▌"));
        // An unset color leaves room for the built-in default downstream.
        assert_eq!(claude.product_name, None);
    }
}
