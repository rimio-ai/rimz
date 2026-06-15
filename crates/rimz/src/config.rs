//! Per-machine settings, loaded from `~/.config/rimz/config.toml`.
//!
//! This is the personal, never-committed tier. The project-committed tier is
//! `<root>/.rimz/config.toml`, parsed for the executable-surface hash in
//! [`crate::trust`]. Settings here are machine-wide preferences that tune how
//! Rimz drives *your* box or link *your* accounts, so they live outside the
//! repo and outside the trust hash — a clone never inherits them.
//!
//! A missing file is the default config, and unknown keys are ignored so an
//! older binary tolerates a newer file. Invalid migrated launch config fails at
//! load time; background readers may still choose defaults for best-effort
//! rendering.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ledger::paths::config_home;

mod accounts;
mod agents;
mod animation;
mod autoping;
mod color;
mod mux;
mod notifications;
mod remote_control;
mod resume;
mod sentry;
mod sidebar;
mod worktree;

pub use accounts::{AccountsConfig, UsageLimitUsd};
pub use agents::{AgentsConfig, Alias, AliasesConfig, LayoutsConfig, TabPlacement};
pub use animation::{
    AnimationColor, AnimationEffect, AnimationFrames, AnimationSpec, AnimationSpeed,
    SidebarAnimationsConfig, UnreadEffect,
};
pub use autoping::{AutoPingConfig, ScheduleEntry, Schedules};
pub(crate) use color::xterm_rgb;
pub use color::{ColorDepth, Semantic, ThemeColor, ThemeMode, nearest_xterm_index, parse_hex};
pub use mux::{
    MultiplexerConfig, TmuxConfig, TmuxExtendedKeysFormat, TmuxPaneBorderLines,
    TmuxPaneBorderStatus, TmuxSetClipboard, ZellijClipboard, ZellijConfig, ZellijForceClose,
};
pub use notifications::{
    DesktopNotificationMode, NotificationSoundMode, NotificationTrigger, NotificationsPrefs,
};
pub use remote_control::RemoteControlConfig;
pub use resume::ResumeConfig;
pub use sentry::SentryConfig;
pub use sidebar::{
    AttentionConfig, BudgetPaceConfig, BudgetZonesConfig, CardDensityMode, ContextBand,
    ContextSeverityConfig, GlowMode, ProviderTabsMode, ScrollbarMode, SidebarConfig,
    SidebarProviderStyle, SidebarThemeConfig,
};
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
    #[error(
        "per-machine config at {path} uses [tab]; rename it to [agents] with [agents.aliases] and [agents.layouts]"
    )]
    LegacyTab { path: PathBuf },
    #[error("invalid per-machine agents config at {path}: {source}")]
    Agents {
        path: PathBuf,
        #[source]
        source: crate::agents_spec::LayoutErr,
    },
}

pub type Result<T> = std::result::Result<T, ConfigErr>;

/// Per-machine configuration. Lenient on unknown keys so a newer config never
/// breaks an older binary, and every field defaults so the smallest useful file
/// is a single section.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct MachineConfig {
    pub accounts: AccountsConfig,
    pub worktree: WorktreeConfig,
    pub agents: AgentsConfig,
    pub autoping: AutoPingConfig,
    pub remote_control: RemoteControlConfig,
    pub notifications: NotificationsPrefs,
    pub sidebar: SidebarConfig,
    pub zellij: ZellijConfig,
    pub tmux: TmuxConfig,
    pub resume: ResumeConfig,
    pub sentry: SentryConfig,
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
            Ok(text) => Self::parse_text(path, &text),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigErr::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    pub fn parse_text(path: &Path, text: &str) -> Result<Self> {
        let value = toml::from_str::<toml::Value>(text).map_err(|source| ConfigErr::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        if value
            .as_table()
            .is_some_and(|table| table.contains_key("tab"))
        {
            return Err(ConfigErr::LegacyTab {
                path: path.to_path_buf(),
            });
        }
        let mut config: Self = toml::from_str(text).map_err(|source| ConfigErr::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        let config_dir = path.parent().unwrap_or_else(|| Path::new("."));
        crate::agents_spec::resolve_alias_prompt_paths(&mut config.agents.aliases, config_dir);
        crate::agents_spec::validate_config(&config.agents.aliases, &config.agents.layouts)
            .map_err(|source| ConfigErr::Agents {
                path: path.to_path_buf(),
                source,
            })?;
        Ok(config)
    }
}

#[cfg(test)]
#[path = "config/template_tests.rs"]
mod template_tests;

#[cfg(test)]
#[path = "config/tests.rs"]
mod tests;
