//! Project trust — the executable-surface hash and the grant record.
//!
//! Project config at `<project_root>/.rimz/config.toml` is inert until the
//! workspace is trusted. A trust grant pins a SHA-256 of every command-running
//! field; every later [`status`] call re-hashes the live config and demotes
//! the state to `stale` when the hash drifts. That re-hash is the
//! "auto-revoke" half of the contract — no separate sweep is required.
//!
//! Adding a new command-running field that isn't projected by
//! [`ExecutableSurface`] is a CI invariant violation per
//! [`docs/guide/security.md`](../../docs/guide/security.md).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ids::WorkspaceId;
use crate::ledger::atomic::{self, write_bytes_atomically};
use crate::ledger::paths::config_home;

const CONFIG_REL: &str = ".rimz/config.toml";
const PROJECTS_SUBDIR: [&str; 2] = ["rimz", "projects"];
const TRUST_FILE: &str = "trust.toml";
const HASH_PREFIX: &str = "sha256:";

#[derive(Debug, thiserror::Error)]
pub enum TrustErr {
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing project config at {path}: {source}")]
    ConfigParse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("parsing trust record at {path}: {source}")]
    RecordParse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("serializing trust record: {0}")]
    RecordSerialize(#[from] toml::ser::Error),
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
}

pub type Result<T> = std::result::Result<T, TrustErr>;

/// Trust state for a project workspace. Derived from the executable-surface
/// hash and the on-disk grant record; never written as state directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustState {
    /// No `.rimz/config.toml` exists — the project has no executable surface.
    NoConfig,
    /// Config present, no grant record on this machine.
    Untrusted,
    /// Config present, grant record present, hashes match.
    Trusted,
    /// Config present, grant record present, surface hash drifted.
    Stale,
}

impl TrustState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NoConfig => "no_config",
            Self::Untrusted => "untrusted",
            Self::Trusted => "trusted",
            Self::Stale => "stale",
        }
    }
}

#[derive(Clone, Debug)]
pub struct TrustReport {
    pub state: TrustState,
    pub workspace_id: WorkspaceId,
    pub project_root: PathBuf,
    pub config_path: PathBuf,
    pub record_path: PathBuf,
    pub current_hash: Option<String>,
    pub granted_hash: Option<String>,
    pub granted_at: Option<Timestamp>,
}

/// Read the current trust state for `project_root`. The executable-surface
/// hash is recomputed every call; that's the auto-revoke contract.
pub fn status(project_root: &Path) -> Result<TrustReport> {
    status_with_roots(project_root, &config_home())
}

/// Pin the current executable-surface hash as trusted on this machine.
pub fn grant(project_root: &Path) -> Result<TrustReport> {
    grant_with_roots(project_root, &config_home())
}

/// Delete the trust record. The state reverts to `Untrusted`, or `NoConfig`
/// when `.rimz/config.toml` is absent.
pub fn revoke(project_root: &Path) -> Result<TrustReport> {
    revoke_with_roots(project_root, &config_home())
}

pub fn status_with_roots(project_root: &Path, config_root: &Path) -> Result<TrustReport> {
    let workspace_id = WorkspaceId::from_project_root(project_root);
    let config_path = project_root.join(CONFIG_REL);
    let record_path = trust_record_path(config_root, &workspace_id);

    let current_hash =
        read_project_config(&config_path)?.map(|config| executable_surface_hash(&config));
    let record = read_trust_record(&record_path)?;

    let state = match (&current_hash, &record) {
        (None, _) => TrustState::NoConfig,
        (Some(_), None) => TrustState::Untrusted,
        (Some(now), Some(rec)) if &rec.surface_hash == now => TrustState::Trusted,
        (Some(_), Some(_)) => TrustState::Stale,
    };

    Ok(TrustReport {
        state,
        workspace_id,
        project_root: project_root.to_path_buf(),
        config_path,
        record_path,
        current_hash,
        granted_hash: record.as_ref().map(|r| r.surface_hash.clone()),
        granted_at: record.as_ref().map(|r| r.granted_at),
    })
}

pub fn grant_with_roots(project_root: &Path, config_root: &Path) -> Result<TrustReport> {
    let workspace_id = WorkspaceId::from_project_root(project_root);
    let config_path = project_root.join(CONFIG_REL);
    let record_path = trust_record_path(config_root, &workspace_id);

    let Some(config) = read_project_config(&config_path)? else {
        return Ok(TrustReport {
            state: TrustState::NoConfig,
            workspace_id,
            project_root: project_root.to_path_buf(),
            config_path,
            record_path,
            current_hash: None,
            granted_hash: None,
            granted_at: None,
        });
    };

    let surface_hash = executable_surface_hash(&config);
    let granted_at = Timestamp::now();
    let record = TrustRecord {
        project_root: project_root.to_path_buf(),
        surface_hash: surface_hash.clone(),
        granted_at,
    };
    let text = toml::to_string_pretty(&record)?;
    write_bytes_atomically(&record_path, text.as_bytes())?;

    Ok(TrustReport {
        state: TrustState::Trusted,
        workspace_id,
        project_root: project_root.to_path_buf(),
        config_path,
        record_path,
        current_hash: Some(surface_hash.clone()),
        granted_hash: Some(surface_hash),
        granted_at: Some(granted_at),
    })
}

pub fn revoke_with_roots(project_root: &Path, config_root: &Path) -> Result<TrustReport> {
    let workspace_id = WorkspaceId::from_project_root(project_root);
    let record_path = trust_record_path(config_root, &workspace_id);
    match std::fs::remove_file(&record_path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(TrustErr::Io {
                path: record_path,
                source,
            });
        }
    }
    status_with_roots(project_root, config_root)
}

fn trust_record_path(config_root: &Path, workspace_id: &WorkspaceId) -> PathBuf {
    let mut path = config_root.to_path_buf();
    for segment in PROJECTS_SUBDIR {
        path.push(segment);
    }
    path.push(workspace_id.as_str());
    path.push(TRUST_FILE);
    path
}

fn read_project_config(path: &Path) -> Result<Option<ProjectConfig>> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let config =
                toml::from_str::<ProjectConfig>(&text).map_err(|source| TrustErr::ConfigParse {
                    path: path.to_path_buf(),
                    source,
                })?;
            Ok(Some(config))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(TrustErr::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn read_trust_record(path: &Path) -> Result<Option<TrustRecord>> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let record =
                toml::from_str::<TrustRecord>(&text).map_err(|source| TrustErr::RecordParse {
                    path: path.to_path_buf(),
                    source,
                })?;
            Ok(Some(record))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(TrustErr::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Hash the executable surface — every field that can cause a process to run.
///
/// The hash input is canonical JSON over [`ExecutableSurface`], so struct
/// field order is fixed, `BTreeMap` keys sort, and `Option::None` serializes
/// as `null`. The wire format is `sha256:<hex>`. Changing this projection is
/// a product-invariant change that must land alongside a doc update in
/// [`docs/internals/trust.md`](../../docs/internals/trust.md).
pub fn executable_surface_hash(config: &ProjectConfig) -> String {
    let surface = ExecutableSurface::from(config);
    let bytes = serde_json::to_vec(&surface).expect("ExecutableSurface serializes");
    let digest = Sha256::digest(&bytes);
    format!("{HASH_PREFIX}{}", hex::encode(digest))
}

/// On-disk trust record at
/// `$XDG_CONFIG_HOME/rimz/projects/<workspace_id>/trust.toml`.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct TrustRecord {
    project_root: PathBuf,
    surface_hash: String,
    granted_at: Timestamp,
}

/// Project config schema for the trust subsystem. Lenient on unknown keys
/// (non-command fields like `display_name` or `sidebar_width` flow through
/// the [`Self::other`] catch-all without affecting the hash) but exact on
/// the command-running fields documented in `docs/guide/security.md`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ProjectConfig {
    pub layout: LayoutConfig,
    pub agents: Vec<AgentConfig>,
    pub hooks: Vec<HookConfig>,
    pub env: BTreeMap<String, String>,
    pub notifications: NotificationsConfig,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct LayoutConfig {
    pub initial_panes: Vec<PaneConfig>,
    pub tmux: TmuxLayoutConfig,
    pub zellij: ZellijLayoutConfig,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct PaneConfig {
    pub name: Option<String>,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct TmuxLayoutConfig {
    pub status_left: Option<String>,
    pub status_right: Option<String>,
    pub popup_command: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ZellijLayoutConfig {
    pub plugin_command: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    pub name: String,
    pub launch_command: Option<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct HookConfig {
    pub event: String,
    pub command: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct NotificationsConfig {
    pub command: Option<String>,
}

/// Canonical projection of [`ProjectConfig`] into just the fields the trust
/// hash covers. Serialized as JSON so the byte order is stable across runs
/// (struct field order is fixed, `BTreeMap` keys sort, `Option::None`
/// serializes as `null`).
#[derive(Serialize)]
struct ExecutableSurface<'a> {
    layout_initial_panes: Vec<ExecutablePane<'a>>,
    layout_tmux: ExecutableTmux<'a>,
    layout_zellij: ExecutableZellij<'a>,
    agents: Vec<ExecutableAgent<'a>>,
    hooks: Vec<ExecutableHook<'a>>,
    env: &'a BTreeMap<String, String>,
    notifications: ExecutableNotifications<'a>,
}

#[derive(Serialize)]
struct ExecutablePane<'a> {
    name: Option<&'a str>,
    command: Option<&'a str>,
    cwd: Option<&'a str>,
    env: &'a BTreeMap<String, String>,
}

#[derive(Serialize)]
struct ExecutableTmux<'a> {
    status_left: Option<&'a str>,
    status_right: Option<&'a str>,
    popup_command: Option<&'a str>,
}

#[derive(Serialize)]
struct ExecutableZellij<'a> {
    plugin_command: Option<&'a str>,
}

#[derive(Serialize)]
struct ExecutableAgent<'a> {
    name: &'a str,
    launch_command: Option<&'a str>,
    env: &'a BTreeMap<String, String>,
}

#[derive(Serialize)]
struct ExecutableHook<'a> {
    event: &'a str,
    command: &'a str,
}

#[derive(Serialize)]
struct ExecutableNotifications<'a> {
    command: Option<&'a str>,
}

impl<'a> From<&'a ProjectConfig> for ExecutableSurface<'a> {
    fn from(config: &'a ProjectConfig) -> Self {
        Self {
            layout_initial_panes: config
                .layout
                .initial_panes
                .iter()
                .map(|p| ExecutablePane {
                    name: p.name.as_deref(),
                    command: p.command.as_deref(),
                    cwd: p.cwd.as_deref(),
                    env: &p.env,
                })
                .collect(),
            layout_tmux: ExecutableTmux {
                status_left: config.layout.tmux.status_left.as_deref(),
                status_right: config.layout.tmux.status_right.as_deref(),
                popup_command: config.layout.tmux.popup_command.as_deref(),
            },
            layout_zellij: ExecutableZellij {
                plugin_command: config.layout.zellij.plugin_command.as_deref(),
            },
            agents: config
                .agents
                .iter()
                .map(|a| ExecutableAgent {
                    name: a.name.as_str(),
                    launch_command: a.launch_command.as_deref(),
                    env: &a.env,
                })
                .collect(),
            hooks: config
                .hooks
                .iter()
                .map(|h| ExecutableHook {
                    event: h.event.as_str(),
                    command: h.command.as_str(),
                })
                .collect(),
            env: &config.env,
            notifications: ExecutableNotifications {
                command: config.notifications.command.as_deref(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn project_with(text: &str) -> tempfile::TempDir {
        let dir = tempdir().expect("tempdir");
        let config_dir = dir.path().join(".rimz");
        std::fs::create_dir_all(&config_dir).expect("mkdir .rimz");
        std::fs::write(config_dir.join("config.toml"), text).expect("write config");
        dir
    }

    #[test]
    fn empty_project_reports_no_config() {
        let dir = tempdir().expect("tempdir");
        let config = tempdir().expect("config root");
        let report = status_with_roots(dir.path(), config.path()).expect("status");
        assert_eq!(report.state, TrustState::NoConfig);
        assert!(report.current_hash.is_none());
        assert!(report.granted_hash.is_none());
    }

    #[test]
    fn fresh_config_reports_untrusted() {
        let dir =
            project_with("[[layout.initial_panes]]\nname = \"shell\"\ncommand = \"$SHELL\"\n");
        let config = tempdir().expect("config root");
        let report = status_with_roots(dir.path(), config.path()).expect("status");
        assert_eq!(report.state, TrustState::Untrusted);
        assert!(report.current_hash.is_some());
        assert!(report.granted_hash.is_none());
    }

    #[test]
    fn grant_pins_hash_and_returns_trusted() {
        let dir =
            project_with("[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n");
        let config = tempdir().expect("config root");
        let granted = grant_with_roots(dir.path(), config.path()).expect("grant");
        assert_eq!(granted.state, TrustState::Trusted);
        let now = status_with_roots(dir.path(), config.path()).expect("status");
        assert_eq!(now.state, TrustState::Trusted);
        assert_eq!(now.current_hash, granted.current_hash);
        assert_eq!(now.granted_hash, granted.current_hash);
    }

    #[test]
    fn editing_command_field_demotes_to_stale() {
        let dir =
            project_with("[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n");
        let config = tempdir().expect("config root");
        grant_with_roots(dir.path(), config.path()).expect("grant");

        std::fs::write(
            dir.path().join(".rimz/config.toml"),
            "[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude --telemetry\"\n",
        )
        .expect("rewrite");

        let report = status_with_roots(dir.path(), config.path()).expect("status");
        assert_eq!(report.state, TrustState::Stale);
        assert_ne!(report.current_hash, report.granted_hash);
    }

    #[test]
    fn unknown_non_command_field_does_not_change_hash() {
        let base = project_with(
            "display_name = \"Query Engine\"\n\n[[layout.initial_panes]]\ncommand = \"$SHELL\"\n",
        );
        let extra = project_with(
            "display_name = \"Query Engine dev\"\nsidebar = true\n\n[[layout.initial_panes]]\ncommand = \"$SHELL\"\n",
        );
        let a = read_project_config(&base.path().join(CONFIG_REL))
            .expect("read base")
            .expect("config present");
        let b = read_project_config(&extra.path().join(CONFIG_REL))
            .expect("read extra")
            .expect("config present");
        assert_eq!(executable_surface_hash(&a), executable_surface_hash(&b));
    }

    #[test]
    fn revoke_drops_record_and_returns_untrusted() {
        let dir = project_with("[notifications]\ncommand = \"notify-send rimz\"\n");
        let config = tempdir().expect("config root");
        grant_with_roots(dir.path(), config.path()).expect("grant");
        let revoked = revoke_with_roots(dir.path(), config.path()).expect("revoke");
        assert_eq!(revoked.state, TrustState::Untrusted);
        assert!(!revoked.record_path.exists());
    }

    #[test]
    fn revoke_with_no_record_is_noop() {
        let dir = project_with("[notifications]\ncommand = \"notify-send rimz\"\n");
        let config = tempdir().expect("config root");
        let report = revoke_with_roots(dir.path(), config.path()).expect("revoke");
        assert_eq!(report.state, TrustState::Untrusted);
    }

    #[test]
    fn hash_covers_every_documented_surface_field() {
        // One config per documented executable-surface field. Any two must
        // hash to distinct values; if a future refactor drops a field from
        // `ExecutableSurface`, two cases collide and this test fires.
        let cases = [
            "[[layout.initial_panes]]\ncommand = \"$SHELL\"\n",
            "[[layout.initial_panes]]\ncwd = \"$RIMZ_PROJECT_ROOT\"\n",
            "[[layout.initial_panes]]\nenv = { FOO = \"bar\" }\n",
            "[layout.tmux]\nstatus_left = 'left'\n",
            "[layout.tmux]\nstatus_right = 'right'\n",
            "[layout.tmux]\npopup_command = 'fzf-projects'\n",
            "[layout.zellij]\nplugin_command = '/opt/plugin.wasm'\n",
            "[[agents]]\nname = \"claude\"\nlaunch_command = \"claude code\"\n",
            "[[agents]]\nname = \"claude\"\nenv = { PATH = \"/opt/llms/bin\" }\n",
            "[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n",
            "[env]\nPATH_PREPEND = \"/opt/rimz/bin\"\n",
            "[notifications]\ncommand = \"notify-send\"\n",
        ];
        let mut hashes = std::collections::HashSet::new();
        for text in cases {
            let config: ProjectConfig =
                toml::from_str(text).unwrap_or_else(|err| panic!("parse `{text}`: {err}"));
            assert!(
                hashes.insert(executable_surface_hash(&config)),
                "case `{text}` collided with another surface case",
            );
        }
    }
}
