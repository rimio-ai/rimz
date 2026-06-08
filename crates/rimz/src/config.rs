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
use std::num::{NonZeroU16, NonZeroU32};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::feed::AgentStatus;
use crate::ledger::paths::config_home;
use crate::sidebar::timing::{DEFAULT_REFRESH_MS, MAX_REFRESH_MS, MIN_REFRESH_MS};

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
    pub worktree: WorktreeConfig,
    pub agents: AgentsConfig,
    pub remote_control: RemoteControlConfig,
    pub notifications: NotificationsPrefs,
    pub sidebar: SidebarConfig,
    pub zellij: ZellijConfig,
    pub tmux: TmuxConfig,
    pub resume: ResumeConfig,
}

/// Best-effort attention delivery preferences. These are per-machine because
/// they describe how this terminal or host should reach this user; a clone never
/// inherits them and they do not enter project trust.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct NotificationsPrefs {
    pub enabled: bool,
    pub triggers: Vec<NotificationTrigger>,
    pub desktop: DesktopNotificationMode,
    pub sound: NotificationSoundMode,
    pub suppress_focused: bool,
    pub debounce_ms: u64,
    pub coalesce_ms: u64,
    #[serde(default)]
    pub command: Option<String>,
}

impl Default for NotificationsPrefs {
    fn default() -> Self {
        Self {
            enabled: true,
            triggers: NotificationTrigger::all().to_vec(),
            desktop: DesktopNotificationMode::Auto,
            sound: NotificationSoundMode::Bell,
            suppress_focused: true,
            debounce_ms: 5_000,
            coalesce_ms: 1_000,
            command: None,
        }
    }
}

impl NotificationsPrefs {
    pub fn command(&self) -> Option<&str> {
        self.command
            .as_deref()
            .map(str::trim)
            .filter(|command| !command.is_empty())
    }

    pub fn triggers_status(&self, status: AgentStatus) -> bool {
        NotificationTrigger::from_status(status)
            .is_some_and(|trigger| self.triggers.contains(&trigger))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NotificationTrigger {
    Waiting,
    Failed,
    Paused,
    Success,
}

impl NotificationTrigger {
    pub const ALL: [Self; 4] = [Self::Waiting, Self::Failed, Self::Paused, Self::Success];

    pub const fn all() -> &'static [Self; 4] {
        &Self::ALL
    }

    pub const fn from_status(status: AgentStatus) -> Option<Self> {
        match status {
            AgentStatus::Waiting => Some(Self::Waiting),
            AgentStatus::Failed => Some(Self::Failed),
            AgentStatus::Paused => Some(Self::Paused),
            AgentStatus::Success => Some(Self::Success),
            AgentStatus::Running | AgentStatus::Idle => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Failed => "failed",
            Self::Paused => "paused",
            Self::Success => "success",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DesktopNotificationMode {
    #[default]
    Auto,
    Osc,
    Off,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NotificationSoundMode {
    #[default]
    Bell,
    Off,
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

/// Git-worktree launch defaults. Per-machine by design: it names where this
/// machine stores sibling worktrees and which base ref it prefers for new ones.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct WorktreeConfig {
    /// Directory template for Rimz-owned worktrees. Relative paths resolve from
    /// the repository root; `{repo}` expands to the root directory basename.
    pub dir: String,
    /// Base ref for new worktrees: local `HEAD`, remote `origin/HEAD`, or an
    /// explicit ref string.
    pub base: WorktreeBase,
}

impl Default for WorktreeConfig {
    fn default() -> Self {
        Self {
            dir: "../{repo}-worktrees".to_owned(),
            base: WorktreeBase::Head,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub enum WorktreeBase {
    #[default]
    Head,
    Fresh,
    Explicit(String),
}

impl WorktreeBase {
    pub fn as_refspec(&self) -> &str {
        match self {
            Self::Head => "HEAD",
            Self::Fresh => "origin/HEAD",
            Self::Explicit(value) => value,
        }
    }
}

impl<'de> Deserialize<'de> for WorktreeBase {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(serde::de::Error::custom("worktree base cannot be empty"));
        }
        Ok(match trimmed {
            "head" => Self::Head,
            "fresh" => Self::Fresh,
            other => Self::Explicit(other.to_owned()),
        })
    }
}

/// Agent-launch preferences. Layout strings name registry-backed agent kinds or
/// `term`; the parser lives in [`crate::tab_layout`].
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct AgentsConfig {
    pub layouts: LayoutsConfig,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct LayoutsConfig(pub BTreeMap<String, String>);

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

/// `[sidebar] scrollbar`: when the agent cards overflow their viewport, how
/// the right-margin scrollbar shows. Display-only.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ScrollbarMode {
    /// Show the bar only while the viewport is moving — a wheel scroll or the
    /// selection-driven auto-follow — then hide it about a second after the
    /// view settles.
    #[default]
    Auto,
    /// Keep the bar up whenever the cards overflow.
    Always,
    /// Never paint the bar.
    Never,
}

/// `[sidebar] glow`: whether the truecolor effects tier — the attention glow
/// and the brief transition flashes — runs over the composed frame.
/// Display-only; with the tier off the modifier-based attention breath alone
/// carries the cue.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GlowMode {
    /// Follow the terminal: run when `COLORTERM` advertises 24-bit color.
    #[default]
    Auto,
    /// Force the pass on. The lever for a truecolor terminal the environment
    /// under-advertises — an SSH hop forwards `TERM` but drops `COLORTERM`.
    /// `NO_COLOR` still wins.
    Always,
    /// Pin the plain 256-color render on any terminal.
    Never,
}

/// `[sidebar] provider_tabs`: how the bottom provider dashboard switches
/// between stacked account blocks and a tab rail. Display-only.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProviderTabsMode {
    /// Stack one or two providers so both accounts stay visible; tab three or
    /// more providers to keep the dashboard bounded.
    #[default]
    Auto,
    /// Tab whenever more than one provider is present.
    Always,
    /// Always stack provider blocks; no tab rail is painted.
    Never,
}

/// Sidebar display preferences. A personal, machine-wide tuning of how the
/// renderer paints; it never affects ledger correctness.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct SidebarConfig {
    /// Base render cadence in milliseconds. This controls animation and
    /// event-coalesced paint timing; data polling stays on `--tick-seconds`.
    pub refresh_ms: u16,
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
    /// How provider blocks lay out: `auto` stacks one or two providers and tabs
    /// three or more; `always` tabs whenever more than one provider is present;
    /// `never` always stacks. Resolved producer-side onto the snapshot like
    /// the rest of `[sidebar]`.
    pub provider_tabs: ProviderTabsMode,
    /// Provider kinds to show in the dashboard and their order. Empty means all
    /// discovered providers, still governed by `max_provider_blocks`; `"all"`
    /// expands to every remaining discovered provider at that position; without
    /// `"all"` this is a strict allowlist. An explicit list bypasses
    /// `max_provider_blocks`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub provider_list: Vec<String>,
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
    /// The provider dashboard's budget-bar color zones — where the draining
    /// mana bar leaves green for yellow, amber, and red as the remaining
    /// budget shrinks. Display-only; it tunes the colour ramp, never the
    /// ledger.
    pub budget: BudgetZonesConfig,
    /// Attention timing knobs. These decide when a silent running row becomes
    /// an actionable `!`; the renderer's heat colours remain separate display
    /// grammar.
    pub attention: AttentionConfig,
    /// Preferred comparison target for the worktree header's git stats (the
    /// `+/-` diff, the `⇡`/`⇣` commit delta, and the `≡`/`✓` landed markers).
    /// Tried
    /// first in the trunk ladder, per repo: a repo where the branch doesn't
    /// resolve falls back to the `main` → `master` → remote-default detection,
    /// so one machine-wide value never breaks other projects. Unset means
    /// detection alone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trunk: Option<String>,
    /// Palette overrides for the renderer's semantic color slots. Each slot is
    /// a 256-color index; an omitted slot keeps the built-in tone, so an
    /// absent section paints exactly the shipped palette. Resolved
    /// producer-side onto the snapshot like `providers`, so every renderer of
    /// the workspace paints the same tones.
    #[serde(skip_serializing_if = "SidebarThemeConfig::is_unset")]
    pub theme: SidebarThemeConfig,
    /// How the agent-cards scrollbar shows when the cards overflow. `auto`
    /// (default) paints it only while the viewport moves and hides it once the
    /// view settles; `always` keeps it up; `never` removes it. Resolved
    /// producer-side onto the snapshot like the rest of `[sidebar]`.
    pub scrollbar: ScrollbarMode,
    /// Whether the truecolor glow tier runs — the attention glow and the
    /// brief transition flashes the renderer layers over the composed frame.
    /// `auto` (default) follows the terminal's 24-bit advertisement
    /// (`COLORTERM`); `always` forces the pass where the advertisement is
    /// missing — an SSH hop forwards `TERM` but drops `COLORTERM`; `never`
    /// keeps the plain 256-color render with the modifier-based attention
    /// breath. `NO_COLOR` beats every mode. Resolved producer-side onto the
    /// snapshot like the rest of `[sidebar]`.
    pub glow: GlowMode,
}

impl Default for SidebarConfig {
    fn default() -> Self {
        Self {
            providers: BTreeMap::new(),
            refresh_ms: DEFAULT_REFRESH_MS,
            max_provider_blocks: default_max_provider_blocks(),
            provider_tabs: ProviderTabsMode::default(),
            provider_list: Vec::new(),
            max_cols: default_sidebar_max_cols(),
            context: ContextSeverityConfig::default(),
            budget: BudgetZonesConfig::default(),
            attention: AttentionConfig::default(),
            trunk: None,
            theme: SidebarThemeConfig::default(),
            scrollbar: ScrollbarMode::default(),
            glow: GlowMode::default(),
        }
    }
}

impl SidebarConfig {
    pub fn resolved_refresh_ms(&self) -> u16 {
        self.refresh_ms.clamp(MIN_REFRESH_MS, MAX_REFRESH_MS)
    }
}

/// `[sidebar.theme]`: per-machine overrides for the renderer's semantic palette
/// slots, each a 256-color index. Display-only — it tunes tones, never the
/// glyph grammar (shape still carries every state under `NO_COLOR`), and never
/// the ledger. Slot names follow the semantics, not the shipped hues, so a
/// user re-theming to light terminals reads `good`/`warn`/`alarm` rather than
/// `green`/`yellow`/`red`.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct SidebarThemeConfig {
    /// Calm/positive: running tallies, low gauges, `+` additions, cache reads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub good: Option<u8>,
    /// Caution: waiting glyphs at rest, mid gauges, cache writes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warn: Option<u8>,
    /// Alarm: failed glyphs, high gauges, `-` removals, fresh input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alarm: Option<u8>,
    /// Structure accent: worktree headers and the selected lane spine.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accent: Option<u8>,
    /// Cool informational: the `plan` posture pill, window tags.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cool: Option<u8>,
    /// Delegation/meta: the `⇅ rc` flag, the subagent `⧉` marker.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<u8>,
    /// Soft content text: stat figures, capability tokens, subagent lines —
    /// a step above `dim`, just below full-strength text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub soft: Option<u8>,
    /// Dim chrome: labels, ages, subordinate values.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dim: Option<u8>,
    /// Faintest chrome: bar tracks, `·` separators, dotted dividers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub faint: Option<u8>,
    /// The darkest chrome (the scrollbar track) — a step below `faint`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<u8>,
    /// The selected-row `▌` accent bar.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection: Option<u8>,
}

impl SidebarThemeConfig {
    /// Whether every slot is unset — the serialized config omits the section.
    pub fn is_unset(&self) -> bool {
        *self == Self::default()
    }
}

/// `[sidebar.attention]`: timing knobs for the attention projection. The
/// values are per-machine display/routing preferences, never ledger truth.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct AttentionConfig {
    /// Seconds a `running` agent may record no completed tool or turn activity
    /// before the sidebar projects it to the actionable `!` attention bucket.
    pub stalled_after_secs: NonZeroU32,
}

impl Default for AttentionConfig {
    fn default() -> Self {
        Self {
            stalled_after_secs: NonZeroU32::new(crate::feed::DEFAULT_STALL_AFTER_SECS)
                .expect("non-zero default stall window"),
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

/// The provider dashboard's budget-bar color zones: each tier names the
/// exclusive upper bound of *remaining* budget (in percent) where it applies,
/// so the draining bar crosses into the tier as the remaining figure drops
/// below the bound. At or above `yellow` the bar stays green. The mirror of
/// [`ContextSeverityConfig`], whose bands bound a *rising* fill from below —
/// here a *draining* figure is bounded from above. A fully spent window's
/// full-width red track is a shape rule independent of these zones.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct BudgetZonesConfig {
    /// Remaining % below which the bar leaves green for yellow.
    pub yellow: u8,
    /// Remaining % below which yellow deepens to amber.
    pub amber: u8,
    /// Remaining % below which the bar goes red.
    pub red: u8,
}

impl Default for BudgetZonesConfig {
    fn default() -> Self {
        Self {
            yellow: 50,
            amber: 25,
            red: 10,
        }
    }
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
    fn worktree_config_defaults_and_parses() {
        let dir = tempdir().expect("tempdir");
        let defaults = MachineConfig::load_from(&write(&dir, "")).expect("load");
        assert_eq!(defaults.worktree.dir, "../{repo}-worktrees");
        assert_eq!(defaults.worktree.base, WorktreeBase::Head);

        let config = MachineConfig::load_from(&write(
            &dir,
            "[worktree]\n\
             dir = \"../wt-{repo}\"\n\
             base = \"fresh\"\n",
        ))
        .expect("load");
        assert_eq!(config.worktree.dir, "../wt-{repo}");
        assert_eq!(config.worktree.base, WorktreeBase::Fresh);

        let explicit =
            MachineConfig::load_from(&write(&dir, "[worktree]\nbase = \"main\"\n")).expect("load");
        assert_eq!(
            explicit.worktree.base,
            WorktreeBase::Explicit("main".to_owned())
        );
        assert!(MachineConfig::load_from(&write(&dir, "[worktree]\nbase = \"\"\n")).is_err());
    }

    #[test]
    fn agents_layouts_parse_as_named_specs() {
        let dir = tempdir().expect("tempdir");
        let config = MachineConfig::load_from(&write(
            &dir,
            "[agents.layouts]\n\
             review = \"claude,codex+term\"\n",
        ))
        .expect("load");
        assert_eq!(
            config.agents.layouts.0.get("review").map(String::as_str),
            Some("claude,codex+term")
        );
        assert!(MachineConfig::default().agents.layouts.0.is_empty());
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
    fn notification_defaults_cover_attention_transitions() {
        let config = MachineConfig::default();
        assert!(config.notifications.enabled);
        assert_eq!(
            config.notifications.triggers,
            NotificationTrigger::all().to_vec()
        );
        assert_eq!(config.notifications.desktop, DesktopNotificationMode::Auto);
        assert_eq!(config.notifications.sound, NotificationSoundMode::Bell);
        assert!(config.notifications.suppress_focused);
        assert_eq!(config.notifications.debounce_ms, 5_000);
        assert_eq!(config.notifications.coalesce_ms, 1_000);
        assert!(config.notifications.command().is_none());
    }

    #[test]
    fn notifications_parse_per_machine_preferences() {
        let dir = tempdir().expect("tempdir");
        let config = MachineConfig::load_from(&write(
            &dir,
            "[notifications]\n\
             enabled = false\n\
             triggers = [\"waiting\", \"failed\"]\n\
             desktop = \"osc\"\n\
             sound = \"off\"\n\
             suppress_focused = false\n\
             debounce_ms = 2500\n\
             coalesce_ms = 0\n\
             command = \"ntfy publish rimz\"\n",
        ))
        .expect("load");
        assert!(!config.notifications.enabled);
        assert_eq!(
            config.notifications.triggers,
            vec![NotificationTrigger::Waiting, NotificationTrigger::Failed]
        );
        assert_eq!(config.notifications.desktop, DesktopNotificationMode::Osc);
        assert_eq!(config.notifications.sound, NotificationSoundMode::Off);
        assert!(!config.notifications.suppress_focused);
        assert_eq!(config.notifications.debounce_ms, 2_500);
        assert_eq!(config.notifications.coalesce_ms, 0);
        assert_eq!(config.notifications.command(), Some("ntfy publish rimz"));
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
    fn sidebar_refresh_ms_defaults_parses_and_clamps_at_use() {
        let dir = tempdir().expect("tempdir");
        let config =
            MachineConfig::load_from(&write(&dir, "[sidebar]\nrefresh_ms = 80\n")).expect("load");
        assert_eq!(config.sidebar.refresh_ms, 80);
        assert_eq!(config.sidebar.resolved_refresh_ms(), 80);
        assert_eq!(
            MachineConfig::default().sidebar.refresh_ms,
            crate::sidebar::timing::DEFAULT_REFRESH_MS
        );

        let too_low =
            MachineConfig::load_from(&write(&dir, "[sidebar]\nrefresh_ms = 1\n")).expect("load");
        assert_eq!(
            too_low.sidebar.resolved_refresh_ms(),
            crate::sidebar::timing::MIN_REFRESH_MS
        );

        let too_high =
            MachineConfig::load_from(&write(&dir, "[sidebar]\nrefresh_ms = 5000\n")).expect("load");
        assert_eq!(
            too_high.sidebar.resolved_refresh_ms(),
            crate::sidebar::timing::MAX_REFRESH_MS
        );
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
    fn sidebar_scrollbar_parses_and_defaults_auto() {
        let dir = tempdir().expect("tempdir");
        let config = MachineConfig::load_from(&write(&dir, "[sidebar]\nscrollbar = \"never\"\n"))
            .expect("load");
        assert_eq!(config.sidebar.scrollbar, ScrollbarMode::Never);
        assert_eq!(
            MachineConfig::default().sidebar.scrollbar,
            ScrollbarMode::Auto,
            "unset auto-hides: the bar shows only while the viewport moves",
        );
        // A typo'd mode fails at config load, with the parse error naming the
        // field, rather than silently painting the default.
        assert!(
            MachineConfig::load_from(&write(&dir, "[sidebar]\nscrollbar = \"bogus\"\n")).is_err()
        );
    }

    #[test]
    fn attention_config_defaults_parses_and_rejects_zero() {
        let dir = tempdir().expect("tempdir");
        let config = MachineConfig::load_from(&write(&dir, "")).expect("load");
        assert_eq!(
            config.sidebar.attention.stalled_after_secs.get(),
            crate::feed::DEFAULT_STALL_AFTER_SECS,
            "unset uses the shipped 30-minute stall window",
        );

        let tuned = MachineConfig::load_from(&write(
            &dir,
            "[sidebar.attention]\nstalled_after_secs = 2700\n",
        ))
        .expect("load");
        assert_eq!(tuned.sidebar.attention.stalled_after_secs.get(), 2700);

        let partial =
            MachineConfig::load_from(&write(&dir, "[sidebar.attention]\n")).expect("load");
        assert_eq!(partial.sidebar.attention, AttentionConfig::default());

        assert!(
            MachineConfig::load_from(&write(
                &dir,
                "[sidebar.attention]\nstalled_after_secs = 0\n",
            ))
            .is_err()
        );
    }

    #[test]
    fn sidebar_theme_parses_defaults_unset_and_rejects_out_of_range() {
        let dir = tempdir().expect("tempdir");
        let config =
            MachineConfig::load_from(&write(&dir, "[sidebar.theme]\ngood = 34\nselection = 25\n"))
                .expect("load");
        assert_eq!(config.sidebar.theme.good, Some(34));
        assert_eq!(config.sidebar.theme.selection, Some(25));
        assert_eq!(config.sidebar.theme.alarm, None, "unset slots stay builtin");
        assert!(MachineConfig::default().sidebar.theme.is_unset());
        // Slots are 256-color indices: a value past u8 fails at config load,
        // with the parse error naming the field, rather than rendering with a
        // silently-wrong palette.
        assert!(MachineConfig::load_from(&write(&dir, "[sidebar.theme]\ngood = 300\n")).is_err());
    }

    #[test]
    fn sidebar_glow_parses_and_defaults_auto() {
        let dir = tempdir().expect("tempdir");
        assert_eq!(
            MachineConfig::default().sidebar.glow,
            GlowMode::Auto,
            "the glow tier ships following the terminal's advertisement",
        );
        let config =
            MachineConfig::load_from(&write(&dir, "[sidebar]\nglow = \"always\"\n")).expect("load");
        assert_eq!(config.sidebar.glow, GlowMode::Always);
        let config =
            MachineConfig::load_from(&write(&dir, "[sidebar]\nglow = \"never\"\n")).expect("load");
        assert_eq!(config.sidebar.glow, GlowMode::Never);
        // The pre-mode boolean form fails at load with the parse error naming
        // the field, rather than silently rendering with a default.
        assert!(MachineConfig::load_from(&write(&dir, "[sidebar]\nglow = false\n")).is_err());
    }

    #[test]
    fn zellij_room_defaults_are_agent_friendly() {
        let dir = tempdir().expect("tempdir");
        let config = MachineConfig::load_from(&write(&dir, "")).expect("load");
        assert!(config.zellij.mouse_mode);
        assert!(config.zellij.mouse_click_through);
        assert!(!config.zellij.advanced_mouse_actions);
        assert!(!config.zellij.mouse_hover_effects);
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
             advanced_mouse_actions = true\n\
             mouse_hover_effects = true\n\
             focus_follows_mouse = false\n\
             copy_clipboard = \"primary\"\n\
             on_force_close = \"quit\"\n",
        ))
        .expect("load");
        assert!(config.zellij.pane_frames);
        assert!(config.zellij.advanced_mouse_actions);
        assert!(config.zellij.mouse_hover_effects);
        assert!(!config.zellij.focus_follows_mouse);
        assert_eq!(config.zellij.copy_clipboard, ZellijClipboard::Primary);
        assert_eq!(config.zellij.on_force_close, ZellijForceClose::Quit);
    }

    #[test]
    fn zellij_default_mode_config_is_legacy_noop() {
        let dir = tempdir().expect("tempdir");
        let config =
            MachineConfig::load_from(&write(&dir, "[zellij]\ndefault_mode = \"normal\"\n"))
                .expect("legacy default_mode key is ignored");
        assert_eq!(config.zellij, ZellijConfig::default());
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
        assert_eq!(config.sidebar.provider_tabs, ProviderTabsMode::Auto);
        assert!(config.sidebar.provider_list.is_empty());
        // Set just one sidebar field: the cap still falls back to its default.
        let partial =
            MachineConfig::load_from(&write(&dir, "[sidebar]\nmax_cols = 60\n")).expect("load");
        assert_eq!(partial.sidebar.max_provider_blocks, 3);
        assert_eq!(partial.sidebar.provider_tabs, ProviderTabsMode::Auto);
        assert!(partial.sidebar.provider_list.is_empty());
    }

    #[test]
    fn provider_dashboard_tabs_and_list_parse_and_round_trip() {
        let dir = tempdir().expect("tempdir");
        let config = MachineConfig::load_from(&write(
            &dir,
            "[sidebar]\nprovider_tabs = \"always\"\nprovider_list = [\"codex\", \"all\"]\n",
        ))
        .expect("load");
        assert_eq!(config.sidebar.provider_tabs, ProviderTabsMode::Always);
        assert_eq!(config.sidebar.provider_list, vec!["codex", "all"]);

        let encoded = toml::to_string(&config.sidebar).expect("serialize sidebar");
        let round_tripped: SidebarConfig = toml::from_str(&encoded).expect("parse sidebar");
        assert_eq!(round_tripped.provider_tabs, ProviderTabsMode::Always);
        assert_eq!(round_tripped.provider_list, vec!["codex", "all"]);
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
    fn budget_zones_default_and_parse() {
        let dir = tempdir().expect("tempdir");
        // The shipped zones: green at/above 50% remaining, yellow below 50,
        // amber below 25, red below 10.
        let config = MachineConfig::load_from(&write(&dir, "")).expect("load");
        let defaults = BudgetZonesConfig::default();
        assert_eq!(config.sidebar.budget, defaults);
        assert_eq!(defaults.yellow, 50);
        assert_eq!(defaults.amber, 25);
        assert_eq!(defaults.red, 10);

        // A tuned tier overrides just its bound; an omitted tier keeps its
        // default.
        let tuned =
            MachineConfig::load_from(&write(&dir, "[sidebar.budget]\nred = 20\n")).expect("load");
        assert_eq!(tuned.sidebar.budget.red, 20);
        assert_eq!(tuned.sidebar.budget.amber, defaults.amber);
        assert_eq!(tuned.sidebar.budget.yellow, defaults.yellow);
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
