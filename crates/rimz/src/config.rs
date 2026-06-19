//! Per-machine settings, loaded from `~/.config/rimz/config.toml`,
//! `theme.toml`, and `agents.toml`.
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
mod attention;
mod autoping;
mod color;
pub mod effective;
mod glyphs;
mod loop_;
mod mux;
mod notifications;
mod pets;
mod remote_control;
mod resume;
mod sentry;
mod sidebar;
mod theme;
mod worktree;

pub use accounts::{AccountsConfig, UsageLimitUsd};
pub use agents::{
    AgentsConfig, CommandsConfig, LaunchPlacement, Profile, ProfilesConfig, RoleBinding, Team,
    TeamsConfig,
};
pub use animation::{
    AnimationColor, AnimationEffect, AnimationFrames, AnimationSpec, AnimationSpeed,
    ThemeAnimationsConfig, UnreadEffect, validate_glyph_cells, validate_single_cell,
};
pub use attention::AttentionConfig;
pub use autoping::{AutoPingConfig, ScheduleEntry, Schedules};
pub(crate) use color::xterm_rgb;
pub use color::{
    ColorDepth, PaletteRole, Semantic, ThemeColor, ThemeMode, nearest_xterm_index, parse_hex,
};
pub use glyphs::{GlyphGroup, GlyphNamespaces, GlyphRole, ThemeGlyphsConfig, is_named_glyph_set};
pub use loop_::LoopConfig;
pub use mux::{
    MultiplexerConfig, TmuxConfig, TmuxExtendedKeysFormat, TmuxPaneBorderLines,
    TmuxPaneBorderStatus, TmuxSetClipboard, ZellijClipboard, ZellijConfig, ZellijForceClose,
};
pub use notifications::{
    DesktopNotificationMode, NotificationSoundMode, NotificationTrigger, NotificationsPrefs,
};
pub use pets::{PetsConfig, PetsGlyphMode, PetsSize};
pub use remote_control::RemoteControlConfig;
pub use resume::{DEFAULT_OVERLOAD_BACKOFF_SECS, ResumeConfig};
pub use sentry::SentryConfig;
pub use sidebar::{
    BudgetPaceConfig, BudgetZonesConfig, CardDensityMode, ContextBand, ContextSeverityConfig,
    GlowMode, ProviderTabsMode, ScrollbarMode, SidebarConfig,
};
pub use theme::{
    InlineAnsiColors, InlinePalette, InlinePrimaryColors, InlineSelectionColors, ThemeConfig,
    ThemeProviderStyle, ThemeStyle,
};
pub use worktree::{WorktreeBase, WorktreeConfig};

const CONFIG_FILE: &str = "config.toml";
const THEME_FILE: &str = "theme.toml";
const AGENTS_FILE: &str = "agents.toml";
const RIMZ_CONFIG_SUBDIR: &str = "rimz";
pub const MACHINE_CONFIG_TEMPLATE: &str = include_str!("config/templates/config.template.toml");
pub const MACHINE_THEME_TEMPLATE: &str = include_str!("config/templates/theme.template.toml");
pub const MACHINE_AGENTS_TEMPLATE: &str = include_str!("config/templates/agents.template.toml");

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
        "per-machine config at {path} still uses moved monolithic sections ({sections}); split it with `rimz config init` and copy your values into config.toml, theme.toml, and agents.toml"
    )]
    LegacySplit { path: PathBuf, sections: String },
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
    pub remote_control: RemoteControlConfig,
    pub notifications: NotificationsPrefs,
    pub sidebar: SidebarConfig,
    pub zellij: ZellijConfig,
    pub tmux: TmuxConfig,
    pub resume: ResumeConfig,
    pub sentry: SentryConfig,
    #[serde(skip_serializing_if = "ThemeConfig::is_unset")]
    pub theme: ThemeConfig,
    pub agents: AgentsConfig,
}

impl MachineConfig {
    /// The generated core per-machine config reference.
    pub fn template_core() -> &'static str {
        MACHINE_CONFIG_TEMPLATE
    }

    /// The generated theme per-machine config reference.
    pub fn template_theme() -> &'static str {
        MACHINE_THEME_TEMPLATE
    }

    /// The generated agents per-machine config reference.
    pub fn template_agents() -> &'static str {
        MACHINE_AGENTS_TEMPLATE
    }

    /// The core per-machine config path: `$XDG_CONFIG_HOME/rimz/config.toml`.
    pub fn config_path() -> PathBuf {
        config_home().join(RIMZ_CONFIG_SUBDIR).join(CONFIG_FILE)
    }

    /// The theme per-machine config path: `$XDG_CONFIG_HOME/rimz/theme.toml`.
    pub fn theme_path() -> PathBuf {
        config_home().join(RIMZ_CONFIG_SUBDIR).join(THEME_FILE)
    }

    /// The agents per-machine config path: `$XDG_CONFIG_HOME/rimz/agents.toml`.
    pub fn agents_path() -> PathBuf {
        config_home().join(RIMZ_CONFIG_SUBDIR).join(AGENTS_FILE)
    }

    /// Load from the default per-machine paths. Missing files are defaults —
    /// never an error.
    pub fn load() -> Result<Self> {
        Self::load_from(&Self::config_path())
    }

    /// Load from an explicit config.toml path and its sibling theme.toml and
    /// agents.toml files — the test and tooling seam.
    pub fn load_from(config_path: &Path) -> Result<Self> {
        let dir = config_path.parent().unwrap_or_else(|| Path::new("."));
        let theme_path = dir.join(THEME_FILE);
        let agents_path = dir.join(AGENTS_FILE);

        let core = load_optional(config_path, parse_core_text)?.unwrap_or_default();
        let theme = load_optional(&theme_path, parse_theme_text)?.unwrap_or_default();
        let agents = load_optional(&agents_path, parse_agents_text)?.unwrap_or_default();

        let mut config = Self {
            accounts: core.accounts,
            remote_control: core.remote_control,
            notifications: core.notifications,
            sidebar: core.sidebar,
            zellij: core.zellij,
            tmux: core.tmux,
            resume: core.resume,
            sentry: core.sentry,
            theme,
            agents,
        };
        validate_agents_config(&mut config.agents, &agents_path)?;
        Ok(config)
    }

    pub fn parse_text(path: &Path, text: &str) -> Result<Self> {
        match path.file_name().and_then(|name| name.to_str()) {
            Some(THEME_FILE) => Ok(Self {
                theme: parse_theme_text(path, text)?,
                ..Self::default()
            }),
            Some(AGENTS_FILE) => {
                let mut agents = parse_agents_text(path, text)?;
                validate_agents_config(&mut agents, path)?;
                Ok(Self {
                    agents,
                    ..Self::default()
                })
            }
            _ => {
                let core = parse_core_text(path, text)?;
                Ok(Self {
                    accounts: core.accounts,
                    remote_control: core.remote_control,
                    notifications: core.notifications,
                    sidebar: core.sidebar,
                    zellij: core.zellij,
                    tmux: core.tmux,
                    resume: core.resume,
                    sentry: core.sentry,
                    ..Self::default()
                })
            }
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct CoreConfig {
    accounts: AccountsConfig,
    remote_control: RemoteControlConfig,
    notifications: NotificationsPrefs,
    sidebar: SidebarConfig,
    zellij: ZellijConfig,
    tmux: TmuxConfig,
    resume: ResumeConfig,
    sentry: SentryConfig,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ThemeFile {
    theme: ThemeConfig,
    colors: Option<InlinePalette>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct AgentsFile {
    agents: AgentsConfig,
}

fn load_optional<T>(path: &Path, parse: fn(&Path, &str) -> Result<T>) -> Result<Option<T>> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(path, &text).map(Some),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ConfigErr::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn parse_core_text(path: &Path, text: &str) -> Result<CoreConfig> {
    let value = toml::from_str::<toml::Value>(text).map_err(|source| ConfigErr::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    if let Some(sections) = legacy_split_sections(&value)
        && !sections.is_empty()
    {
        return Err(ConfigErr::LegacySplit {
            path: path.to_path_buf(),
            sections: sections.join(", "),
        });
    }
    toml::from_str(text).map_err(|source| ConfigErr::Parse {
        path: path.to_path_buf(),
        source,
    })
}

fn parse_theme_text(path: &Path, text: &str) -> Result<ThemeConfig> {
    let mut file: ThemeFile = toml::from_str(text).map_err(|source| ConfigErr::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    file.theme.colors = file.colors;
    Ok(file.theme)
}

fn parse_agents_text(path: &Path, text: &str) -> Result<AgentsConfig> {
    let file: AgentsFile = toml::from_str(text).map_err(|source| ConfigErr::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(file.agents)
}

fn validate_agents_config(agents: &mut AgentsConfig, path: &Path) -> Result<()> {
    let config_dir = path.parent().unwrap_or_else(|| Path::new("."));
    crate::agents_spec::resolve_profile_prompt_paths(&mut agents.profiles, config_dir);
    crate::agents_spec::resolve_team_prompt_paths(&mut agents.teams, config_dir);
    crate::agents_spec::validate_config(&agents.profiles, &agents.commands, &agents.teams).map_err(
        |source| ConfigErr::Agents {
            path: path.to_path_buf(),
            source,
        },
    )
}

fn legacy_split_sections(value: &toml::Value) -> Option<Vec<&'static str>> {
    let table = value.as_table()?;
    let mut sections = Vec::new();
    for key in ["worktree", "autoping", "agents"] {
        if table.contains_key(key) {
            sections.push(key);
        }
    }
    if let Some(sidebar) = table.get("sidebar").and_then(toml::Value::as_table) {
        for key in [
            "style",
            "theme",
            "glyphs",
            "providers",
            "animations",
            "attention",
            "pets",
        ] {
            if sidebar.contains_key(key) {
                sections.push(match key {
                    "style" => "sidebar.style",
                    "theme" => "sidebar.theme",
                    "glyphs" => "sidebar.glyphs",
                    "providers" => "sidebar.providers",
                    "animations" => "sidebar.animations",
                    "attention" => "sidebar.attention",
                    "pets" => "sidebar.pets",
                    _ => key,
                });
            }
        }
    }
    Some(sections)
}

#[cfg(test)]
#[path = "config/template_tests.rs"]
mod template_tests;

#[cfg(test)]
#[path = "config/tests.rs"]
mod tests;
