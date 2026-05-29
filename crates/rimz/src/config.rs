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

use serde::Deserialize;

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
}

/// Claude Code Remote Control auto-launch policy. Off unless explicitly enabled
/// — Rimz never links your account or starts a remote-control host without
/// opt-in, so the absence of this section reads as "do nothing".
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RemoteControlConfig {
    /// Auto-launch `claude remote-control` in a managed background view when
    /// Claude is detected and a Rimz workspace starts.
    pub auto: bool,
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
        assert!(!config.remote_control.auto);
    }

    #[test]
    fn empty_file_keeps_remote_control_off() {
        let dir = tempdir().expect("tempdir");
        let config = MachineConfig::load_from(&write(&dir, "")).expect("load");
        assert!(!config.remote_control.auto);
    }

    #[test]
    fn auto_true_parses() {
        let dir = tempdir().expect("tempdir");
        let config = MachineConfig::load_from(&write(&dir, "[remote_control]\nauto = true\n"))
            .expect("load");
        assert!(config.remote_control.auto);
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let dir = tempdir().expect("tempdir");
        let text = "sound_profile = \"chime\"\n\n[remote_control]\nauto = true\ncapacity = 16\n";
        let config = MachineConfig::load_from(&write(&dir, text)).expect("load");
        assert!(config.remote_control.auto);
    }

    #[test]
    fn malformed_toml_surfaces_an_error() {
        let dir = tempdir().expect("tempdir");
        let err = MachineConfig::load_from(&write(&dir, "[remote_control]\nauto = \"yes\"\n"))
            .expect_err("type mismatch should fail");
        assert!(matches!(err, ConfigErr::Parse { .. }));
    }
}
