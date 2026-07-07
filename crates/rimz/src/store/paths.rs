//! Disk, runtime, and shared-cache path resolution.
//!
//! State paths live under `$XDG_STATE_HOME/rimz/workspaces/<id>/`.
//! Runtime paths live under `$XDG_RUNTIME_DIR/rimz/<id>/`, falling back to
//! `/tmp/rimz-<uid>/rimz/<id>/` at mode `0700` per `docs/internals/store/store.md`.
//! Shared data caches live under `$XDG_STATE_HOME/rimz/shared/`; shared
//! election locks live under `$XDG_RUNTIME_DIR/rimz/shared/`.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::ids::{SidebarInstanceId, WorkspaceId};
use crate::sock::SockBudget;
use crate::store::sidecar;

#[derive(Debug, thiserror::Error)]
pub enum PathErr {
    #[error("io error preparing {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    SocketBudgetExceeded(#[from] crate::sock::SocketPathTooLong),
    #[error("invalid Rimz runtime path layout under {path}")]
    InvalidRuntimeLayout { path: PathBuf },
    #[error("runtime path {path} is not a directory")]
    RuntimePathNotDirectory { path: PathBuf },
    #[cfg(unix)]
    #[error(
        "runtime directory {path} is a symbolic link; set XDG_RUNTIME_DIR to a real private directory"
    )]
    RuntimeDirSymlink { path: PathBuf },
    #[cfg(unix)]
    #[error(
        "runtime directory {path} is owned by uid {owner}, not uid {current}; set XDG_RUNTIME_DIR to a private directory you own"
    )]
    RuntimeDirWrongOwner {
        path: PathBuf,
        owner: u32,
        current: u32,
    },
    #[cfg(unix)]
    #[error("runtime directory {path} is mode {mode:o}; expected no group or other permissions")]
    RuntimeDirInsecure { path: PathBuf, mode: u32 },
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
    pub messages_dir: PathBuf,
    pub transcript_dir: PathBuf,
    pub runs_dir: PathBuf,
    pub locks_dir: PathBuf,
    pub workspace_lock: PathBuf,
    pub publish_lock: PathBuf,
    pub workspace_record: PathBuf,
    pub channels_record: PathBuf,
    pub boot_marker: PathBuf,
    pub last_death_marker: PathBuf,
    pub crashes_dir: PathBuf,
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
        let messages_dir = root.join("messages");
        let transcript_dir = root.join("transcript");
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
            messages_dir,
            transcript_dir,
            runs_dir,
            workspace_lock: locks_dir.join("workspace.lock"),
            publish_lock: locks_dir.join("publish.lock"),
            workspace_record: root.join("workspace.json"),
            channels_record: root.join("channels.json"),
            boot_marker: root.join("boot.json"),
            last_death_marker: root.join("last-death.json"),
            crashes_dir: root.join("crashes"),
            locks_dir,
            root,
        })
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        mkdir_p(&self.snapshots_dir)?;
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
    /// User-scoped election locks. Data caches use [`Self::persistent_shared_root`].
    pub shared_root: PathBuf,
    /// User-scoped shared data caches. Production constructors root this under
    /// [`state_home`], while [`Self::under`] roots it under the supplied runtime
    /// root for test isolation and byte-identical cross-workspace cache paths.
    pub persistent_shared_root: PathBuf,
    pub sock_dir: PathBuf,
    pub heartbeat_dir: PathBuf,
    /// Per-renderer read receipts for unread sidebar rows. Disposable runtime
    /// sidecars merged by every renderer so focusing a pane in one tab clears it
    /// everywhere in the workspace.
    pub read_marks_dir: PathBuf,
    /// Holds one latest-wins agent-context sidecar per session. Written by CLI
    /// producer paths (statusline, hook/local transcript refresh, detached
    /// helpers, snapshot producer backstops) and folded by snapshot reads.
    pub agent_context_dir: PathBuf,
    /// Holds one latest-wins subagent-context sidecar per child (Claude's
    /// `subagentStatusLine` enrichment: description, token count, start time).
    /// Written by CLI producers, read by the snapshot CLI — never the sidebar.
    /// Kept apart from `agent_context/` so each reader deserializes only its own
    /// record shape.
    pub subagent_context_dir: PathBuf,
    /// Per-agent activity heartbeats (see [`crate::agent_activity`]). Latency
    /// hints the snapshot folds into each agent's `last_activity`.
    pub agent_activity_dir: PathBuf,
}

/// Data-cache filenames that lived in the runtime `shared/` dir before
/// c44f3ec6 moved them to `persistent_shared_root`. Swept on ensure so stale
/// pre-migration copies stop pinning tmpfs (RAM).
const LEGACY_RUNTIME_SHARED_CACHES: [&str; 6] = [
    "accounts.json",
    "rate_limits.json",
    "credits.json",
    "provider-spending.json",
    "spending.json",
    "pricing-cache.json",
];

impl RuntimePaths {
    pub fn for_workspace(workspace_id: WorkspaceId) -> Result<Self> {
        Self::for_workspace_with_shared_root(
            workspace_id,
            &runtime_home(),
            persistent_shared_home(),
        )
    }

    /// Account-global runtime paths with no bound room, for readers that run
    /// outside a workspace — the provider-dashboard pace view (`rimz stats`) and
    /// the lobby. Only the `shared_*` accessors carry meaning here; the
    /// per-workspace fields resolve under a reserved all-zero sentinel id and are
    /// never created.
    pub fn shared() -> Self {
        let sentinel = WorkspaceId::parse("ws_000000000000000000000000")
            .expect("reserved all-zero workspace id is well-formed");
        let mut paths = Self::under(sentinel, &runtime_home())
            .expect("under() builds paths without IO and cannot fail");
        paths.persistent_shared_root = persistent_shared_home();
        paths
    }

    /// Build runtime paths rooted at `runtime_root`. Tests prefer this so they
    /// don't need to set `XDG_RUNTIME_DIR`. This raw constructor deliberately
    /// skips the socket budget; ambient production callers use
    /// [`Self::for_workspace`] so a long runtime root fails before any session
    /// side effect. Shared data and lock paths both root under `runtime_root`
    /// here so tests stay isolated; [`Self::for_workspace`] and [`Self::shared`]
    /// move shared data to [`persistent_shared_home`].
    pub fn under(workspace_id: WorkspaceId, runtime_root: &Path) -> Result<Self> {
        let root = runtime_root.join("rimz").join(workspace_id.as_str());
        let shared_root = runtime_root.join("rimz").join("shared");
        let persistent_shared_root = shared_root.clone();
        let sock_dir = root.join("sock");
        let heartbeat_dir = root.join("heartbeat");
        let read_marks_dir = root.join("read-marks");
        let agent_context_dir = root.join("agent_context");
        let subagent_context_dir = root.join("subagent_context");
        let agent_activity_dir = root.join("agent-activity");
        Ok(Self {
            workspace_id,
            root,
            shared_root,
            persistent_shared_root,
            sock_dir,
            heartbeat_dir,
            read_marks_dir,
            agent_context_dir,
            subagent_context_dir,
            agent_activity_dir,
        })
    }

    pub fn validated_under(workspace_id: WorkspaceId, runtime_root: &Path) -> Result<Self> {
        let paths = Self::under(workspace_id, runtime_root)?;
        let budget = SockBudget::for_sock_dir(&paths.sock_dir);
        budget.validate()?;
        Ok(paths)
    }

    fn for_workspace_with_shared_root(
        workspace_id: WorkspaceId,
        runtime_root: &Path,
        persistent_shared_root: PathBuf,
    ) -> Result<Self> {
        let mut paths = Self::validated_under(workspace_id, runtime_root)?;
        paths.persistent_shared_root = persistent_shared_root;
        Ok(paths)
    }

    /// Sidecar file for one agent session's rich context, keyed by
    /// `(kind, agent_id)`. The filename is a digest so an arbitrary session id
    /// (a free string, possibly path-hostile) maps to a safe, fixed-width name.
    pub fn agent_context_path(&self, kind: &str, agent_id: &str) -> PathBuf {
        sidecar::path(&self.agent_context_dir, "ctx", kind, agent_id)
    }

    /// Sidecar file for one subagent's `subagentStatusLine` enrichment, keyed by
    /// `(kind, agent_id)` and digested to a safe, fixed-width name like
    /// [`Self::agent_context_path`].
    pub fn subagent_context_path(&self, kind: &str, agent_id: &str) -> PathBuf {
        sidecar::path(&self.subagent_context_dir, "sub", kind, agent_id)
    }

    /// Path of a sidebar instance's heartbeat file. The freshness scan in
    /// [`crate::sidebar::fresh_sidebar_present`] keys on the `sidebar.*.json`
    /// shape this produces; the sidebar process removes this file on exit so a
    /// later launch sees an honest "no sidebar here".
    pub fn sidebar_heartbeat_path(&self, instance_id: &SidebarInstanceId) -> PathBuf {
        self.heartbeat_dir
            .join(format!("sidebar.{}.json", instance_id.as_str()))
    }

    /// Path of a sidebar instance's read-mark receipt file. Receipts outlive the
    /// writer so peer renderers can consume them on their next fold; the orphan
    /// sweep reaps stale files once the owning heartbeat has expired.
    pub fn sidebar_read_marks_path(&self, instance_id: &SidebarInstanceId) -> PathBuf {
        self.read_marks_dir
            .join(format!("sidebar.{}.json", instance_id.as_str()))
    }

    /// The workspace-wide set of open unread episodes. The producer owns writes
    /// for status-derived opens and row-gone pruning; renderers and CLI commands
    /// write read receipts that derive this set back to read on the next fold.
    pub fn unread_path(&self) -> PathBuf {
        self.root.join("unread.json")
    }

    pub fn pane_frame_path(&self) -> PathBuf {
        self.root.join("snapshot.json")
    }

    pub fn diff_stats_path(&self) -> PathBuf {
        self.root.join("diff-stats.json")
    }

    pub fn pr_state_path(&self) -> PathBuf {
        self.root.join("pr-state.json")
    }

    /// The workspace's last jump scroll anchor: the pane a jump focused plus the
    /// viewport offset that keeps its card where the user clicked. Renderers read
    /// it on the fold that adopts the focus, so a cross-tab jump lands the card at
    /// the same on-screen row. Display-only runtime state, TTL-gated.
    pub fn focus_anchor_path(&self) -> PathBuf {
        self.root.join("focus-anchor.json")
    }

    /// The per-session Codex app-server broker socket. The broker
    /// ([`crate::agents::codex::broker`]) binds it; the enrichment client
    /// ([`crate::agents::codex::app_server`]) connects to it. Both derive it from
    /// the same `workspace_id`, so it needs no env var to agree.
    pub fn codex_app_server_socket_path(&self) -> PathBuf {
        self.sock_dir.join("codex-app-server.sock")
    }

    pub fn shared_accounts_path(&self) -> PathBuf {
        self.persistent_shared_root.join("accounts.json")
    }

    pub fn shared_accounts_lock(&self) -> PathBuf {
        self.shared_root.join("accounts.lock")
    }

    pub fn shared_rate_limits_path(&self) -> PathBuf {
        self.persistent_shared_root.join("rate_limits.json")
    }

    pub fn shared_rate_limits_lock(&self) -> PathBuf {
        self.shared_root.join("rate_limits.lock")
    }

    pub fn shared_credits_path(&self) -> PathBuf {
        self.persistent_shared_root.join("credits.json")
    }

    pub fn shared_credits_lock(&self) -> PathBuf {
        self.shared_root.join("credits.lock")
    }

    pub fn shared_provider_spending_path(&self) -> PathBuf {
        self.persistent_shared_root.join("provider-spending.json")
    }

    pub fn shared_spending_lock(&self) -> PathBuf {
        self.shared_root.join("spending.lock")
    }

    pub fn shared_spending_cursor_path(&self) -> PathBuf {
        self.persistent_shared_root.join("spending.json")
    }

    pub fn shared_pricing_cache_path(&self) -> PathBuf {
        self.persistent_shared_root.join("pricing-cache.json")
    }

    pub fn workspace_spending_path(&self, scope_hash: &str) -> PathBuf {
        let prefix = scope_hash.get(..32).unwrap_or(scope_hash);
        self.root.join(format!("workspace-spending.{prefix}.json"))
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        let rimz_root = self
            .root
            .parent()
            .ok_or_else(|| PathErr::InvalidRuntimeLayout {
                path: self.root.clone(),
            })?;
        let runtime_root = rimz_root
            .parent()
            .ok_or_else(|| PathErr::InvalidRuntimeLayout {
                path: self.root.clone(),
            })?;
        ensure_private_runtime_dir(runtime_root)?;
        ensure_private_runtime_dir(rimz_root)?;
        ensure_private_runtime_dir(&self.root)?;
        ensure_private_runtime_dir(&self.shared_root)?;
        if self.shared_root != self.persistent_shared_root {
            for name in LEGACY_RUNTIME_SHARED_CACHES {
                let path = self.shared_root.join(name);
                match fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                    Err(err) => tracing::debug!(
                        path = %path.display(),
                        error = %err,
                        "legacy runtime shared cache sweep failed"
                    ),
                }
            }
        }
        mkdir_p(&self.persistent_shared_root)?;
        mkdir_p(&self.sock_dir)?;
        mkdir_p(&self.heartbeat_dir)?;
        mkdir_p(&self.read_marks_dir)?;
        mkdir_p(&self.agent_context_dir)?;
        mkdir_p(&self.subagent_context_dir)?;
        mkdir_p(&self.agent_activity_dir)?;
        Ok(())
    }
}

fn mkdir_p(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|e| PathErr::Io {
        path: path.to_path_buf(),
        source: e,
    })
}

#[cfg(unix)]
pub fn ensure_private_runtime_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|e| PathErr::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
    let mut metadata = runtime_dir_metadata(path)?;
    let current = nix::unistd::Uid::current().as_raw();
    if metadata.uid() != current {
        return Err(PathErr::RuntimeDirWrongOwner {
            path: path.to_path_buf(),
            owner: metadata.uid(),
            current,
        });
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|e| PathErr::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        metadata = runtime_dir_metadata(path)?;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(PathErr::RuntimeDirInsecure {
                path: path.to_path_buf(),
                mode: metadata.permissions().mode() & 0o777,
            });
        }
    }
    Ok(())
}

#[cfg(unix)]
fn runtime_dir_metadata(path: &Path) -> Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path).map_err(|e| PathErr::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    if metadata.file_type().is_symlink() {
        return Err(PathErr::RuntimeDirSymlink {
            path: path.to_path_buf(),
        });
    }
    if !metadata.is_dir() {
        return Err(PathErr::RuntimePathNotDirectory {
            path: path.to_path_buf(),
        });
    }
    Ok(metadata)
}

#[cfg(not(unix))]
pub fn ensure_private_runtime_dir(path: &Path) -> Result<()> {
    mkdir_p(path)?;
    let metadata = fs::metadata(path).map_err(|e| PathErr::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    if !metadata.is_dir() {
        return Err(PathErr::RuntimePathNotDirectory {
            path: path.to_path_buf(),
        });
    }
    Ok(())
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
    // Containers and minimal hosts often lack XDG_RUNTIME_DIR. Use the short
    // /tmp/rimz-<uid> namespace per the docs; RuntimePaths::ensure_dirs verifies
    // and hardens the fallback root, rimz root, workspace root, and shared root.
    runtime_fallback_home()
}

pub fn persistent_shared_home() -> PathBuf {
    state_home().join("rimz").join("shared")
}

#[cfg(unix)]
fn runtime_fallback_home() -> PathBuf {
    PathBuf::from("/tmp").join(format!("rimz-{}", current_uid()))
}

#[cfg(not(unix))]
fn runtime_fallback_home() -> PathBuf {
    env::temp_dir().join(format!("rimz-{}", current_uid()))
}

/// Per-user, per-machine config root. Hosts configuration that survives
/// reboots but is not per-workspace.
pub fn config_home() -> PathBuf {
    if let Some(value) = env_path("XDG_CONFIG_HOME") {
        return value;
    }
    if let Some(home) = env_path("HOME") {
        return home.join(".config");
    }
    env::temp_dir().join("rimz-config")
}

/// Per-user agent library root. Rimz discovers drop-in profile and team
/// fragments here, and `RIMZ_AGENTS_HOME` relocates it.
pub fn agents_home() -> PathBuf {
    if let Some(value) = env_path("RIMZ_AGENTS_HOME") {
        return value;
    }
    if let Some(home) = env_path("HOME") {
        return home.join(".agents");
    }
    env::temp_dir().join("rimz-agents")
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

    fn short_tempdir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("r")
            .tempdir_in("/tmp")
            .expect("short tempdir")
    }

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
        assert_eq!(paths.transcript_dir.file_name().unwrap(), "transcript");
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

    #[test]
    fn production_runtime_paths_persist_shared_data_and_keep_locks_runtime() {
        let temp = short_tempdir();
        let state_root = temp.path().join("state");
        let runtime_root = temp.path().join("runtime");
        let persistent_shared_root = state_root.join("rimz").join("shared");
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/x"));

        let paths = RuntimePaths::for_workspace_with_shared_root(
            workspace_id,
            &runtime_root,
            persistent_shared_root.clone(),
        )
        .unwrap();

        assert_eq!(paths.shared_root, runtime_root.join("rimz").join("shared"));
        assert_eq!(paths.persistent_shared_root, persistent_shared_root);
        assert_eq!(
            paths.shared_provider_spending_path(),
            state_root
                .join("rimz")
                .join("shared")
                .join("provider-spending.json")
        );
        assert_eq!(
            paths.shared_accounts_path(),
            state_root.join("rimz").join("shared").join("accounts.json")
        );
        assert_eq!(
            paths.shared_spending_cursor_path(),
            state_root.join("rimz").join("shared").join("spending.json")
        );
        assert_eq!(
            paths.shared_spending_lock(),
            runtime_root
                .join("rimz")
                .join("shared")
                .join("spending.lock")
        );

        paths.ensure_dirs().unwrap();

        assert!(paths.persistent_shared_root.is_dir());
        assert!(paths.shared_root.is_dir());
    }

    #[test]
    fn ensure_dirs_sweeps_legacy_runtime_shared_caches() {
        let temp = short_tempdir();
        let state_root = temp.path().join("state");
        let runtime_root = temp.path().join("runtime");
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let paths = RuntimePaths::for_workspace_with_shared_root(
            workspace_id,
            &runtime_root,
            state_root.join("rimz").join("shared"),
        )
        .unwrap();
        fs::create_dir_all(&paths.shared_root).unwrap();
        for name in LEGACY_RUNTIME_SHARED_CACHES {
            fs::write(paths.shared_root.join(name), b"legacy").unwrap();
        }
        let lock = paths.shared_root.join("spending.lock");
        let probe = paths.shared_root.join("session-context-probe.x");
        fs::write(&lock, b"lock").unwrap();
        fs::write(&probe, b"probe").unwrap();

        paths.ensure_dirs().unwrap();

        for name in LEGACY_RUNTIME_SHARED_CACHES {
            assert!(!paths.shared_root.join(name).exists(), "{name} swept");
        }
        assert!(lock.exists());
        assert!(probe.exists());
    }

    #[test]
    fn ensure_dirs_keeps_shared_cache_when_roots_match() {
        let temp = short_tempdir();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let paths = RuntimePaths::under(workspace_id, temp.path()).unwrap();
        paths.ensure_dirs().unwrap();
        let cache = paths.shared_root.join("spending.json");
        fs::write(&cache, b"live").unwrap();

        paths.ensure_dirs().unwrap();

        assert!(cache.exists());
    }

    #[test]
    fn runtime_fallback_uses_short_tmp_root() {
        let fallback = runtime_fallback_home();

        #[cfg(unix)]
        {
            let expected = format!("rimz-{}", current_uid());
            assert_eq!(fallback.parent(), Some(Path::new("/tmp")));
            assert_eq!(
                fallback.file_name().and_then(|name| name.to_str()),
                Some(expected.as_str())
            );
        }

        #[cfg(not(unix))]
        assert!(fallback.starts_with(env::temp_dir()));
    }

    #[test]
    fn validated_under_fails_fast_with_the_xdg_remedy() {
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let deep_root = Path::new("/tmp").join("d".repeat(crate::sock::AF_UNIX_PATH_LIMIT));

        let err =
            RuntimePaths::validated_under(workspace_id, &deep_root).expect_err("overlong root");
        let rendered = err.to_string();

        match err {
            PathErr::SocketBudgetExceeded(source) => {
                assert!(source.path.starts_with(&deep_root));
                assert!(source.used > source.limit);
                assert!(rendered.contains(crate::sock::XDG_REMEDY));
            }
            other => panic!("expected SocketBudgetExceeded, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn ensure_dirs_hardens_runtime_root_before_workspace_children() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let runtime_root = dir.path().join("runtime");
        let rimz_root = runtime_root.join("rimz");
        fs::create_dir_all(&rimz_root).unwrap();
        for path in [&runtime_root, &rimz_root] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o777)).unwrap();
        }
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let runtime = RuntimePaths::under(workspace_id, &runtime_root).unwrap();

        runtime.ensure_dirs().unwrap();

        for path in [
            runtime_root.as_path(),
            rimz_root.as_path(),
            runtime.root.as_path(),
            runtime.shared_root.as_path(),
        ] {
            let mode = fs::metadata(path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "{} is private", path.display());
        }
    }

    #[cfg(unix)]
    #[test]
    fn ensure_dirs_rejects_symlinked_runtime_root() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let real_root = dir.path().join("real");
        fs::create_dir(&real_root).unwrap();
        let runtime_root = dir.path().join("runtime");
        symlink(&real_root, &runtime_root).unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let runtime = RuntimePaths::under(workspace_id, &runtime_root).unwrap();

        let err = runtime.ensure_dirs().expect_err("symlinked root");

        match err {
            PathErr::RuntimeDirSymlink { path } => assert_eq!(path, runtime_root),
            other => panic!("expected RuntimeDirSymlink, got {other:?}"),
        }
    }
}
