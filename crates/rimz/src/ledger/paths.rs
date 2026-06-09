//! Disk and runtime path resolution.
//!
//! State paths live under `$XDG_STATE_HOME/rimz/workspaces/<id>/`.
//! Runtime paths live under `$XDG_RUNTIME_DIR/rimz/<id>/`, falling back to
//! `/tmp/rimz-<uid>/<id>/` at mode `0700` per `docs/internals/ledger.md`.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::ids::{SidebarInstanceId, WorkspaceId};

#[derive(Debug, thiserror::Error)]
pub enum PathErr {
    #[error("io error preparing {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

pub type Result<T> = std::result::Result<T, PathErr>;

#[derive(Clone, Debug)]
pub struct StatePaths {
    pub workspace_id: WorkspaceId,
    pub root: PathBuf,
    pub events_log: PathBuf,
    pub events_archive_dir: PathBuf,
    pub agents_carryover: PathBuf,
    pub snapshots_dir: PathBuf,
    pub latest_snapshot: PathBuf,
    pub rollup_cache: PathBuf,
    pub feed_dir: PathBuf,
    pub runs_dir: PathBuf,
    pub locks_dir: PathBuf,
    pub workspace_lock: PathBuf,
    pub publish_lock: PathBuf,
    pub workspace_record: PathBuf,
}

impl StatePaths {
    pub fn for_workspace(workspace_id: WorkspaceId) -> Result<Self> {
        Self::under(workspace_id, &state_home())
    }

    /// Build paths rooted at `state_root` instead of `state_home()`. Used by
    /// tests so they don't need to mutate process env; production callers
    /// take the XDG-based [`Self::for_workspace`].
    pub fn under(workspace_id: WorkspaceId, state_root: &Path) -> Result<Self> {
        let root = state_root
            .join("rimz")
            .join("workspaces")
            .join(workspace_id.as_str());
        let snapshots_dir = root.join("snapshots");
        let feed_dir = root.join("feed");
        let runs_dir = root.join("runs");
        let locks_dir = root.join("locks");
        Ok(Self {
            workspace_id,
            events_log: root.join("events.log.jsonl"),
            events_archive_dir: root.join("events.log.archive"),
            agents_carryover: root.join("agents.carryover.json"),
            latest_snapshot: snapshots_dir.join("latest.json"),
            rollup_cache: snapshots_dir.join("rollup.json"),
            snapshots_dir,
            feed_dir,
            runs_dir,
            workspace_lock: locks_dir.join("workspace.lock"),
            publish_lock: locks_dir.join("publish.lock"),
            workspace_record: root.join("workspace.json"),
            locks_dir,
            root,
        })
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        mkdir_p(&self.snapshots_dir)?;
        mkdir_p(&self.feed_dir)?;
        mkdir_p(&self.runs_dir)?;
        mkdir_p(&self.locks_dir)?;
        Ok(())
    }
}

pub fn workspaces_dir() -> PathBuf {
    workspaces_dir_under(&state_home())
}

pub fn workspaces_dir_under(state_root: &Path) -> PathBuf {
    state_root.join("rimz").join("workspaces")
}

#[derive(Clone, Debug)]
pub struct RuntimePaths {
    pub workspace_id: WorkspaceId,
    pub root: PathBuf,
    pub shared_root: PathBuf,
    pub sock_dir: PathBuf,
    pub heartbeat_dir: PathBuf,
    /// Holds one latest-wins agent-context sidecar per session. Written by CLI
    /// producer paths (statusline feed, hook/local transcript refresh, detached
    /// helpers, snapshot producer backstops) and folded by snapshot reads.
    pub agent_context_dir: PathBuf,
    /// Holds one latest-wins subagent-context sidecar per child (Claude's
    /// `subagentStatusLine` enrichment: description, token count, start time).
    /// Written by the feed process, read by the snapshot CLI — never the sidebar.
    /// Kept apart from `agent_context/` so each reader deserializes only its own
    /// record shape.
    pub subagent_context_dir: PathBuf,
    /// Per-agent activity heartbeats (see [`crate::agent_activity`]). Latency
    /// hints the snapshot folds into each agent's `last_activity`.
    pub agent_activity_dir: PathBuf,
}

impl RuntimePaths {
    pub fn for_workspace(workspace_id: WorkspaceId) -> Result<Self> {
        Self::under(workspace_id, &runtime_home())
    }

    /// Build runtime paths rooted at `runtime_root`. Tests prefer this so they
    /// don't need to set `XDG_RUNTIME_DIR`.
    pub fn under(workspace_id: WorkspaceId, runtime_root: &Path) -> Result<Self> {
        let root = runtime_root.join("rimz").join(workspace_id.as_str());
        let shared_root = runtime_root.join("rimz").join("shared");
        let sock_dir = root.join("sock");
        let heartbeat_dir = root.join("heartbeat");
        let agent_context_dir = root.join("agent_context");
        let subagent_context_dir = root.join("subagent_context");
        let agent_activity_dir = root.join("agent-activity");
        Ok(Self {
            workspace_id,
            root,
            shared_root,
            sock_dir,
            heartbeat_dir,
            agent_context_dir,
            subagent_context_dir,
            agent_activity_dir,
        })
    }

    /// Sidecar file for one agent session's rich context, keyed by
    /// `(kind, agent_id)`. The filename is a digest so an arbitrary session id
    /// (a free string, possibly path-hostile) maps to a safe, fixed-width name.
    pub fn agent_context_path(&self, kind: &str, agent_id: &str) -> PathBuf {
        let mut hasher = Sha256::new();
        hasher.update(kind.as_bytes());
        hasher.update([0]);
        hasher.update(agent_id.as_bytes());
        let digest = hex::encode(hasher.finalize());
        self.agent_context_dir
            .join(format!("ctx.{}.json", &digest[..32]))
    }

    /// Sidecar file for one subagent's `subagentStatusLine` enrichment, keyed by
    /// `(kind, agent_id)` and digested to a safe, fixed-width name like
    /// [`Self::agent_context_path`].
    pub fn subagent_context_path(&self, kind: &str, agent_id: &str) -> PathBuf {
        let mut hasher = Sha256::new();
        hasher.update(kind.as_bytes());
        hasher.update([0]);
        hasher.update(agent_id.as_bytes());
        let digest = hex::encode(hasher.finalize());
        self.subagent_context_dir
            .join(format!("sub.{}.json", &digest[..32]))
    }

    /// Path of a sidebar instance's heartbeat file. The freshness scan in
    /// [`crate::sidebar::fresh_sidebar_present`] keys on the `sidebar.*.json`
    /// shape this produces; the sidebar process removes this file on exit so a
    /// later launch sees an honest "no sidebar here".
    pub fn sidebar_heartbeat_path(&self, instance_id: &SidebarInstanceId) -> PathBuf {
        self.heartbeat_dir
            .join(format!("sidebar.{}.json", instance_id.as_str()))
    }

    /// The per-session Codex app-server broker socket. The broker
    /// ([`crate::agents::codex::broker`]) binds it; the enrichment client
    /// ([`crate::agents::codex::app_server`]) connects to it. Both derive it from
    /// the same `workspace_id`, so it needs no env var to agree.
    pub fn codex_app_server_socket_path(&self) -> PathBuf {
        self.sock_dir.join("codex-app-server.sock")
    }

    pub fn shared_accounts_path(&self) -> PathBuf {
        self.shared_root.join("accounts.json")
    }

    pub fn shared_accounts_lock(&self) -> PathBuf {
        self.shared_root.join("accounts.lock")
    }

    pub fn shared_rate_limits_path(&self) -> PathBuf {
        self.shared_root.join("rate_limits.json")
    }

    pub fn shared_rate_limits_lock(&self) -> PathBuf {
        self.shared_root.join("rate_limits.lock")
    }

    pub fn shared_provider_spending_path(&self) -> PathBuf {
        self.shared_root.join("provider-spending.json")
    }

    pub fn shared_spending_lock(&self) -> PathBuf {
        self.shared_root.join("spending.lock")
    }

    pub fn shared_spending_cursor_path(&self) -> PathBuf {
        self.shared_root.join("spending.json")
    }

    pub fn shared_pricing_cache_path(&self) -> PathBuf {
        self.shared_root.join("pricing-cache.json")
    }

    pub fn live_spend_baselines_path(&self) -> PathBuf {
        self.root.join("live-spend-baselines.json")
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        mkdir_p(&self.sock_dir)?;
        mkdir_p(&self.heartbeat_dir)?;
        mkdir_p(&self.agent_context_dir)?;
        mkdir_p(&self.subagent_context_dir)?;
        mkdir_p(&self.agent_activity_dir)?;
        mkdir_p(&self.shared_root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [&self.root, &self.shared_root] {
                let mut perms = fs::metadata(path)
                    .map_err(|e| PathErr::Io {
                        path: path.to_path_buf(),
                        source: e,
                    })?
                    .permissions();
                perms.set_mode(0o700);
                fs::set_permissions(path, perms).map_err(|e| PathErr::Io {
                    path: path.to_path_buf(),
                    source: e,
                })?;
            }
        }
        Ok(())
    }
}

fn mkdir_p(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|e| PathErr::Io {
        path: path.to_path_buf(),
        source: e,
    })
}

pub fn state_home() -> PathBuf {
    if let Some(value) = env_path("XDG_STATE_HOME") {
        return value;
    }
    if let Some(home) = env_path("HOME") {
        return home.join(".local/state");
    }
    env::temp_dir().join("rimz-state")
}

pub fn runtime_home() -> PathBuf {
    if let Some(value) = env_path("XDG_RUNTIME_DIR") {
        return value;
    }
    // Containers and minimal hosts often lack XDG_RUNTIME_DIR. Use a
    // /tmp/rimz-<uid> namespace per the docs; the 0700 mode is applied to
    // `RuntimePaths::root` after creation.
    let uid = current_uid();
    env::temp_dir().join(format!("rimz-{uid}"))
}

/// Per-user, per-machine config root. Hosts the resolver allowlist and any
/// other configuration that survives reboots but is not per-workspace.
pub fn config_home() -> PathBuf {
    if let Some(value) = env_path("XDG_CONFIG_HOME") {
        return value;
    }
    if let Some(home) = env_path("HOME") {
        return home.join(".config");
    }
    env::temp_dir().join("rimz-config")
}

/// Per-user data root. Rimz stores stable, user-level artifacts here, including
/// the materialized embedded Zellij presence plugin.
pub fn data_home() -> PathBuf {
    if let Some(value) = env_path("XDG_DATA_HOME") {
        return value;
    }
    if let Some(home) = env_path("HOME") {
        return home.join(".local/share");
    }
    env::temp_dir().join("rimz-data")
}

/// Per-user cache root, where Zellij keeps its serialized-session cache
/// (`<cache>/zellij/<contract_version>/session_info/<name>`). `rimz reset` wipes
/// the matching entry so a stuck room cannot be resurrected.
pub fn cache_home() -> PathBuf {
    if let Some(value) = env_path("XDG_CACHE_HOME") {
        return value;
    }
    if let Some(home) = env_path("HOME") {
        return home.join(".cache");
    }
    env::temp_dir().join("rimz-cache")
}

/// Read an environment variable as a path, treating an empty value as unset.
pub fn env_path(key: &str) -> Option<PathBuf> {
    env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

#[cfg(unix)]
fn current_uid() -> String {
    nix::unistd::Uid::current().as_raw().to_string()
}

#[cfg(not(unix))]
fn current_uid() -> String {
    "unknown".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::WorkspaceId;

    #[test]
    fn state_paths_resolve_under_state_home() {
        let id = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let paths = StatePaths::for_workspace(id.clone()).unwrap();
        assert!(paths.root.ends_with(Path::new(id.as_str())));
        assert_eq!(paths.events_log.file_name().unwrap(), "events.log.jsonl");
        assert_eq!(paths.latest_snapshot.file_name().unwrap(), "latest.json");
        assert_eq!(paths.rollup_cache.file_name().unwrap(), "rollup.json");
        assert!(paths.rollup_cache.starts_with(&paths.snapshots_dir));
        assert_eq!(paths.runs_dir.file_name().unwrap(), "runs");
        assert_eq!(
            paths.workspace_record.file_name().unwrap(),
            "workspace.json"
        );
        assert_eq!(paths.workspace_lock.file_name().unwrap(), "workspace.lock");
    }

    #[test]
    fn runtime_paths_share_user_scoped_cache_files() {
        let root = Path::new("/tmp/rimz-runtime-test");
        let first = WorkspaceId::from_project_root(Path::new("/tmp/project-a"));
        let second = WorkspaceId::from_project_root(Path::new("/tmp/project-b"));
        let first_paths = RuntimePaths::under(first, root).unwrap();
        let second_paths = RuntimePaths::under(second, root).unwrap();

        assert_ne!(first_paths.root, second_paths.root);
        assert_ne!(
            first_paths.live_spend_baselines_path(),
            second_paths.live_spend_baselines_path()
        );
        assert_eq!(first_paths.shared_root, second_paths.shared_root);
        assert_eq!(
            first_paths.shared_accounts_path(),
            second_paths.shared_accounts_path()
        );
        assert_eq!(
            first_paths.shared_rate_limits_path(),
            second_paths.shared_rate_limits_path()
        );
        assert_eq!(
            first_paths.shared_provider_spending_path(),
            second_paths.shared_provider_spending_path()
        );
        assert_eq!(
            first_paths.shared_spending_cursor_path(),
            second_paths.shared_spending_cursor_path()
        );
        assert_eq!(
            first_paths.shared_pricing_cache_path(),
            second_paths.shared_pricing_cache_path()
        );
    }
}
