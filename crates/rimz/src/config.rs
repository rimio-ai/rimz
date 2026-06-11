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

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ledger::paths::config_home;

mod animation;
mod mux;
mod notifications;
mod remote_control;
mod resume;
mod sidebar;
mod tab;
mod worktree;

pub use animation::{
    AnimationColor, AnimationEffect, AnimationFrames, AnimationSpec, AnimationSpeed,
    SidebarAnimationsConfig,
};
pub use mux::{
    MultiplexerConfig, TmuxConfig, TmuxExtendedKeysFormat, TmuxPaneBorderLines,
    TmuxPaneBorderStatus, TmuxSetClipboard, ZellijClipboard, ZellijConfig, ZellijForceClose,
};
pub use notifications::{
    DesktopNotificationMode, NotificationSoundMode, NotificationTrigger, NotificationsPrefs,
};
pub use remote_control::RemoteControlConfig;
pub use resume::ResumeConfig;
pub use sidebar::{
    AttentionConfig, BudgetPaceConfig, BudgetZonesConfig, CardDensityMode, ContextBand,
    ContextSeverityConfig, GlowMode, ProviderTabsMode, ScrollbarMode, SidebarConfig,
    SidebarProviderStyle, SidebarThemeConfig,
};
pub use tab::{Keyword, KeywordsConfig, LayoutsConfig, TabConfig};
pub use worktree::{WorktreeBase, WorktreeConfig};

const CONFIG_FILE: &str = "config.toml";
const RIMZ_CONFIG_SUBDIR: &str = "rimz";
pub const MACHINE_CONFIG_TEMPLATE: &str = include_str!("config.template.toml");

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
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct MachineConfig {
    pub worktree: WorktreeConfig,
    pub tab: TabConfig,
    pub remote_control: RemoteControlConfig,
    pub notifications: NotificationsPrefs,
    pub sidebar: SidebarConfig,
    pub zellij: ZellijConfig,
    pub tmux: TmuxConfig,
    pub resume: ResumeConfig,
}

impl MachineConfig {
    /// The generated per-machine config reference: every persisted section and
    /// default scalar lives here as commented TOML.
    pub fn template() -> &'static str {
        MACHINE_CONFIG_TEMPLATE
    }

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
#[path = "config/template_tests.rs"]
mod template_tests;

#[cfg(test)]
#[path = "config/tests.rs"]
mod tests;
