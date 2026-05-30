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
}

/// How much of each agent card the sidebar renders by default (unselected).
/// Selecting a row always reveals the full card, so density only sets the
/// resting height — it never hides data a selection can't bring back.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SidebarDensity {
    /// Identity, description, and the context bar. The 5h/7d budget bars and the
    /// token/work stats stay reveal-on-select. The calm default — most agents
    /// fit on screen.
    #[default]
    Compact,
    /// Adds the 5h/7d budget bars so all three meters show on every row;
    /// selection still reveals the token/work stats.
    Bars,
    /// The whole card on every row — three bars plus token and work stats.
    /// Richest, and tallest, so the fewest agents fit.
    Full,
}

impl SidebarDensity {
    /// Whether the resting (unselected) card includes the 5h/7d budget bars.
    pub fn shows_budget_bars(self) -> bool {
        matches!(self, Self::Bars | Self::Full)
    }

    /// Whether the resting card includes the token and work stat lines.
    pub fn shows_stats(self) -> bool {
        matches!(self, Self::Full)
    }
}

/// Sidebar display preferences. A personal, machine-wide tuning of how the
/// renderer paints; it never affects ledger correctness.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct SidebarConfig {
    pub density: SidebarDensity,
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
    /// Auto-launch `codex remote-control start` — the Codex app-server daemon
    /// with remote control enabled — in the same view. `remote-control start`
    /// boots its daemon from the managed standalone install, so when this is on
    /// that install must be present (a `codex` on PATH alone won't do); otherwise
    /// `rimz start` refuses fail-fast with the fix. The daemon it brings up is
    /// the one Codex enrichment re-uses.
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
    fn malformed_toml_surfaces_an_error() {
        let dir = tempdir().expect("tempdir");
        let err = MachineConfig::load_from(&write(&dir, "[remote_control]\nclaude = \"yes\"\n"))
            .expect_err("type mismatch should fail");
        assert!(matches!(err, ConfigErr::Parse { .. }));
    }

    #[test]
    fn sidebar_density_defaults_to_compact() {
        let dir = tempdir().expect("tempdir");
        let config = MachineConfig::load_from(&write(&dir, "")).expect("load");
        assert_eq!(config.sidebar.density, SidebarDensity::Compact);
    }

    #[test]
    fn sidebar_density_parses_each_level() {
        let dir = tempdir().expect("tempdir");
        let bars = MachineConfig::load_from(&write(&dir, "[sidebar]\ndensity = \"bars\"\n"))
            .expect("load");
        assert_eq!(bars.sidebar.density, SidebarDensity::Bars);
        let full = MachineConfig::load_from(&write(&dir, "[sidebar]\ndensity = \"full\"\n"))
            .expect("load");
        assert_eq!(full.sidebar.density, SidebarDensity::Full);
    }

    #[test]
    fn sidebar_unknown_density_surfaces_an_error() {
        let dir = tempdir().expect("tempdir");
        let err = MachineConfig::load_from(&write(&dir, "[sidebar]\ndensity = \"cozy\"\n"))
            .expect_err("unknown density should fail");
        assert!(matches!(err, ConfigErr::Parse { .. }));
    }
}
