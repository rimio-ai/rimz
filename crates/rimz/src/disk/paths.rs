//! Disk, runtime, and shared-cache path resolution.
//!
//! Everything that outlives a boot lives under one home, [`rimz_home`]
//! (`$RIMZ_HOME`, else `~/.rimz`): config files at the top, the agent library
//! (`agents/ subagents/ teams/ traits/ skills/`, relocated by
//! `$RIMZ_AGENTS_HOME`), trust grants under `trust/`, workspace state under
//! `ws/<name>/`, the provider homes under `accounts/`, and the machine-wide
//! `logs/`, `loops/`, `web/`, `builds/`, and `cache/` dirs (provider caches
//! under `cache/providers/`). Runtime paths stay on tmpfs under
//! `$XDG_RUNTIME_DIR/rimz/ws/<name>/`, falling back to
//! `/tmp/rimz-<uid>/rimz/ws/<name>/` at mode `0700` per
//! `docs/internals/store.md`; shared election locks live under
//! `$XDG_RUNTIME_DIR/rimz/shared/`.
//!
//! A workspace dir name is a [`WorkspaceDirName`], `<basename>-<hex>`, never
//! the [`WorkspaceId`] itself. Resolving an id scans `ws/` for names whose hex
//! prefixes the id and lets each candidate's `workspace.json` decide; a site
//! holding a project root mints a new name when none exists, and a site holding
//! only an id falls back to `ws-<24hex>`. No constructor creates anything.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::ids::{PaneId, SidebarInstanceId, WORKSPACE_DIR_HEX_MIN, WorkspaceDirName, WorkspaceId};
use crate::sock::SockBudget;

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
    #[error("workspace {id} matches several dirs under {dir}: {candidates:?}")]
    AmbiguousWorkspaceDir {
        id: WorkspaceId,
        dir: PathBuf,
        candidates: Vec<String>,
    },
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

type Result<T> = std::result::Result<T, PathErr>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tier {
    State,
    Runtime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Class {
    Log,
    Records,
    Audit,
    Cache,
    Owned,
    Tmp,
    Sock,
    Live,
    Lanes,
    Locks,
}

impl Class {
    pub const fn dir_name(self) -> &'static str {
        match self {
            Self::Log => "log",
            Self::Records => "records",
            Self::Audit => "audit",
            Self::Cache => "cache",
            Self::Owned => "owned",
            Self::Tmp => "tmp",
            Self::Sock => "sock",
            Self::Live => "live",
            Self::Lanes => "lanes",
            Self::Locks => "locks",
        }
    }

    pub const fn tier(self) -> Tier {
        match self {
            Self::Log | Self::Records | Self::Audit | Self::Cache | Self::Owned | Self::Tmp => {
                Tier::State
            }
            Self::Sock | Self::Live | Self::Lanes | Self::Locks => Tier::Runtime,
        }
    }

    pub(crate) fn path_under(self, root: &Path) -> PathBuf {
        root.join(self.dir_name())
    }
}

#[derive(Clone, Debug)]
pub struct StatePaths {
    pub workspace_id: WorkspaceId,
    pub dir_name: WorkspaceDirName,
    pub root: PathBuf,
    pub tmp_dir: PathBuf,
    pub scratchpad_dir: PathBuf,
    pub agents_dir: PathBuf,
    pub shared_dir: PathBuf,
    pub subagents_dir: PathBuf,
    pub skills_dir: PathBuf,
    pub events_log: PathBuf,
    pub events_archive_dir: PathBuf,
    pub agents_carryover: PathBuf,
    pub snapshots_dir: PathBuf,
    pub latest_snapshot: PathBuf,
    pub rollup_cache: PathBuf,
    pub messages_dir: PathBuf,
    pub transcript_dir: PathBuf,
    pub runs_dir: PathBuf,
    pub waits_dir: PathBuf,
    pub cache_dir: PathBuf,
    /// Runtime lock reference for writers whose durable API takes only state paths.
    pub workspace_lock: PathBuf,
    pub(crate) publish_lock: PathBuf,
    pub workspace_record: PathBuf,
    pub room_bin: PathBuf,
    pub channels_record: PathBuf,
    pub(crate) boot_marker: PathBuf,
    pub live_roster: PathBuf,
    pub last_death_marker: PathBuf,
    pub doctor_watermark: PathBuf,
    pub auto_gc_stamp: PathBuf,
    pub crashes_dir: PathBuf,
}

impl StatePaths {
    /// Paths for the workspace born at `project_root`: its existing dir, else
    /// a freshly minted `<basename>-<hex>` name. Creates nothing.
    pub fn for_project_root(project_root: &Path) -> Result<Self> {
        Self::for_project_root_under(project_root, &rimz_home())
    }

    /// [`Self::for_project_root`] under an explicit home.
    pub fn for_project_root_under(project_root: &Path, home: &Path) -> Result<Self> {
        let workspace_id = WorkspaceId::from_project_root(project_root);
        let dir_name =
            workspace_dir_name_for_root(&workspaces_dir_under(home), &workspace_id, project_root)?;
        Ok(Self::under_named(workspace_id, dir_name, home))
    }

    /// Paths for a workspace known only by id: its existing dir, else the
    /// `ws-<24hex>` fallback name. Creates nothing.
    pub fn for_workspace(workspace_id: WorkspaceId) -> Result<Self> {
        let dir_name = workspace_dir_name(&workspaces_dir(), &workspace_id)?;
        Ok(Self::under_named(workspace_id, dir_name, &rimz_home()))
    }

    /// [`Self::for_workspace`] under an explicit home, for tests that should not
    /// mutate process env. Runtime lock references also use `home` as their
    /// isolated runtime root; opening a Store pairs them with its runtime paths.
    pub fn under(workspace_id: WorkspaceId, home: &Path) -> Result<Self> {
        let dir_name = workspace_dir_name(&workspaces_dir_under(home), &workspace_id)?;
        let runtime = RuntimePaths::under_named(workspace_id.clone(), dir_name.clone(), home);
        let mut paths = Self::under_named(workspace_id, dir_name, home);
        paths.bind_runtime_locks(&runtime);
        Ok(paths)
    }

    /// Paths for `workspace_id` in the dir `dir_name` under `home`.
    pub fn under_named(workspace_id: WorkspaceId, dir_name: WorkspaceDirName, home: &Path) -> Self {
        let root = workspaces_dir_under(home).join(dir_name.as_str());
        let cache_dir = Class::Cache.path_under(&root);
        let records_dir = Class::Records.path_under(&root);
        let audit_dir = Class::Audit.path_under(&root);
        let log_dir = Class::Log.path_under(&root);
        let owned_dir = Class::Owned.path_under(&root);
        let snapshots_dir = cache_dir.join("snapshots");
        let messages_dir = records_dir.join("messages");
        let transcript_dir = audit_dir.join("transcript");
        let runs_dir = owned_dir.join("runs");
        let tmp_dir = root.join(Class::Tmp.dir_name());
        let runtime =
            RuntimePaths::under_named(workspace_id.clone(), dir_name.clone(), &runtime_home());
        Self {
            workspace_id,
            dir_name,
            scratchpad_dir: tmp_dir.join("scratchpad"),
            agents_dir: owned_dir.join("agents"),
            shared_dir: tmp_dir.join("shared"),
            subagents_dir: tmp_dir.join("rimz-subagents"),
            waits_dir: tmp_dir.join("rimz-waits"),
            skills_dir: tmp_dir.join("skills"),
            tmp_dir,
            events_log: log_dir.join("events.log.jsonl"),
            events_archive_dir: log_dir.join("archive"),
            agents_carryover: records_dir.join("agents-carryover.json"),
            latest_snapshot: snapshots_dir.join("latest.json"),
            rollup_cache: snapshots_dir.join("rollup.json"),
            snapshots_dir,
            messages_dir,
            transcript_dir,
            runs_dir,
            workspace_lock: runtime.lock_path("workspace.lock"),
            publish_lock: runtime.lock_path("publish.lock"),
            workspace_record: root.join("workspace.json"),
            room_bin: root.join("rimz"),
            channels_record: records_dir.join("channels.json"),
            boot_marker: records_dir.join("boot.json"),
            live_roster: cache_dir.join("live-roster.json"),
            last_death_marker: records_dir.join("last-death.json"),
            doctor_watermark: cache_dir.join("doctor-cleared.json"),
            auto_gc_stamp: cache_dir.join("auto-gc.json"),
            crashes_dir: audit_dir.join("crashes"),
            cache_dir,
            root,
        }
    }

    pub(crate) fn bind_runtime_locks(&mut self, runtime: &RuntimePaths) {
        self.workspace_lock = runtime.lock_path("workspace.lock");
        self.publish_lock = runtime.lock_path("publish.lock");
    }

    pub(crate) fn lock_path(&self, name: &str) -> PathBuf {
        // Every constructor builds workspace_lock inside the runtime lock directory.
        self.workspace_lock
            .parent()
            .expect("workspace lock has a parent")
            .join(name)
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        mkdir_p(&self.snapshots_dir)?;
        mkdir_p(&self.runs_dir)?;
        mkdir_p(&self.cache_dir)?;
        Ok(())
    }

    /// Prepare the room tmp layout, bound at `/tmp` under sandbox isolation.
    pub fn ensure_tmp_dir(&self) -> Result<()> {
        ensure_private_runtime_dir(&self.tmp_dir)?;
        mkdir_p(&self.scratchpad_dir)?;
        ensure_private_runtime_dir(&self.agents_dir)?;
        mkdir_p(&self.shared_dir)?;
        mkdir_p(&self.waits_dir)?;
        mkdir_p(&self.subagents_dir)
    }

    /// The launch's private scratch dir, bound at `/tmp/scratchpad` under
    /// sandbox isolation: `owned/agents/<handle>/scratch` for a named agent, the shared
    /// `scratchpad` for a launch without a handle. Handles are path-safe
    /// (`petname::valid_agent_name`).
    pub fn scratch_dir(&self, handle: Option<&str>) -> PathBuf {
        handle.map_or_else(
            || self.scratchpad_dir.clone(),
            |handle| self.agents_dir.join(handle).join("scratch"),
        )
    }

    pub fn agent_skills_dir(&self, handle: Option<&str>) -> PathBuf {
        handle.map_or_else(
            || self.skills_dir.clone(),
            |handle| self.agents_dir.join(handle).join("skills"),
        )
    }

    /// Resolve classed files for readers that scan room roots without opening a store.
    pub fn class_path(root: &Path, class: Class, name: impl AsRef<Path>) -> PathBuf {
        class.path_under(root).join(name)
    }

    pub fn audit_path(&self, name: impl AsRef<Path>) -> PathBuf {
        Self::class_path(&self.root, Class::Audit, name)
    }

    /// Room-local state paths; runtime lock references are inventoried by RuntimePaths.
    pub fn all_paths(&self) -> Vec<PathBuf> {
        vec![
            self.tmp_dir.clone(),
            self.scratchpad_dir.clone(),
            self.agents_dir.clone(),
            self.shared_dir.clone(),
            self.subagents_dir.clone(),
            self.skills_dir.clone(),
            self.events_log.clone(),
            self.events_archive_dir.clone(),
            self.agents_carryover.clone(),
            self.snapshots_dir.clone(),
            self.latest_snapshot.clone(),
            self.rollup_cache.clone(),
            self.messages_dir.clone(),
            self.transcript_dir.clone(),
            self.runs_dir.clone(),
            self.waits_dir.clone(),
            self.cache_dir.clone(),
            self.workspace_record.clone(),
            self.room_bin.clone(),
            self.channels_record.clone(),
            self.boot_marker.clone(),
            self.live_roster.clone(),
            self.last_death_marker.clone(),
            self.doctor_watermark.clone(),
            self.auto_gc_stamp.clone(),
            self.crashes_dir.clone(),
            Class::Audit.path_under(&self.root),
            Class::Records.path_under(&self.root),
        ]
    }

    pub fn ensure_scratch_dir(&self, handle: Option<&str>) -> Result<PathBuf> {
        self.ensure_tmp_dir()?;
        let dir = self.scratch_dir(handle);
        mkdir_p(&dir)?;
        Ok(dir)
    }

    pub fn remove_tmp_dir(&self) -> Result<()> {
        match fs::remove_dir_all(&self.tmp_dir) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(PathErr::Io {
                path: self.tmp_dir.clone(),
                source,
            }),
        }
    }

    pub(crate) fn remove_skills_dir(&self) -> Result<()> {
        match fs::remove_dir_all(&self.skills_dir) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(PathErr::Io {
                path: self.skills_dir.clone(),
                source,
            }),
        }
    }
}

/// The workspace dir name `workspace_id` resolves to in `ws_dir`, or the
/// `ws-<24hex>` fallback when it has none.
fn workspace_dir_name(ws_dir: &Path, workspace_id: &WorkspaceId) -> Result<WorkspaceDirName> {
    let names: Vec<WorkspaceDirName> = workspace_dir_names(ws_dir)?.collect();
    Ok(find_workspace_dir(ws_dir, &names, workspace_id, |_| true)?
        .unwrap_or_else(|| WorkspaceDirName::fallback(workspace_id)))
}

/// Longest basename slug a minted name carries, bounding the socket budget.
const WORKSPACE_DIR_SLUG_MAX: usize = 32;

/// The existing dir for `workspace_id` in `ws_dir`, else a minted
/// `<basename>-<hex>` whose hex no dir in `ws_dir` already carries.
fn workspace_dir_name_for_root(
    ws_dir: &Path,
    workspace_id: &WorkspaceId,
    project_root: &Path,
) -> Result<WorkspaceDirName> {
    let slug = WorkspaceDirName::basename_slug(project_root, WORKSPACE_DIR_SLUG_MAX);
    let fallback = WorkspaceDirName::fallback(workspace_id);
    // An unrecorded dir is this root's only when its name says so: another
    // project whose id shares the hex prefix must mint its own dir.
    let owns_unrecorded = |name: &WorkspaceDirName| {
        *name == fallback || name.as_str() == format!("{slug}-{}", name.hex())
    };
    // One listing serves lookup and mint, so a concurrent first mint of the
    // same root is either seen and adopted or unseen and minted identically.
    let taken: Vec<WorkspaceDirName> = workspace_dir_names(ws_dir)?.collect();
    if let Some(found) = find_workspace_dir(ws_dir, &taken, workspace_id, owns_unrecorded)? {
        return Ok(found);
    }
    let hex_len = (WORKSPACE_DIR_HEX_MIN..workspace_id.hex().len())
        .step_by(2)
        .find(|&len| {
            let hex = &workspace_id.hex()[..len];
            !taken.iter().any(|name| name.hex() == hex)
        })
        .unwrap_or(workspace_id.hex().len());
    Ok(WorkspaceDirName::mint(&slug, workspace_id, hex_len))
}

/// Every parseable workspace dir name in `ws_dir`; a missing dir has none.
fn workspace_dir_names(ws_dir: &Path) -> Result<impl Iterator<Item = WorkspaceDirName>> {
    let entries = match fs::read_dir(ws_dir) {
        Ok(entries) => Some(entries),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(source) => {
            return Err(PathErr::Io {
                path: ws_dir.to_path_buf(),
                source,
            });
        }
    };
    Ok(entries
        .into_iter()
        .flatten()
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| entry.file_name().to_str().and_then(WorkspaceDirName::parse)))
}

/// Locate `workspace_id`'s dir among `names` listed from `ws_dir` by hex prefix. A candidate whose
/// `workspace.json` names the id wins; one naming another id is skipped; a
/// candidate without a readable record (half-born, or a runtime tree) is
/// accepted only when `may_own_unrecorded` admits it and it is the sole such
/// candidate.
fn find_workspace_dir(
    ws_dir: &Path,
    names: &[WorkspaceDirName],
    workspace_id: &WorkspaceId,
    may_own_unrecorded: impl Fn(&WorkspaceDirName) -> bool,
) -> Result<Option<WorkspaceDirName>> {
    let mut unrecorded = Vec::new();
    for name in names
        .iter()
        .filter(|name| name.may_name(workspace_id))
        .cloned()
    {
        match recorded_workspace_id(&ws_dir.join(name.as_str())) {
            Some(recorded) if recorded == workspace_id.as_str() => return Ok(Some(name)),
            Some(_) => {}
            None if may_own_unrecorded(&name) => unrecorded.push(name),
            None => {}
        }
    }
    if unrecorded.len() > 1 {
        return Err(PathErr::AmbiguousWorkspaceDir {
            id: workspace_id.clone(),
            dir: ws_dir.to_path_buf(),
            candidates: unrecorded.iter().map(|name| name.to_string()).collect(),
        });
    }
    Ok(unrecorded.pop())
}

/// The id a workspace dir's `workspace.json` names. `disk` sits below
/// `workspace`, so this reads the one field it needs instead of the record type.
fn recorded_workspace_id(dir: &Path) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Recorded {
        workspace_id: String,
    }
    let bytes = fs::read(dir.join("workspace.json")).ok()?;
    serde_json::from_slice::<Recorded>(&bytes)
        .ok()
        .map(|recorded| recorded.workspace_id)
}

/// Workspace state dirs, `<home>/ws`.
pub fn workspaces_dir() -> PathBuf {
    workspaces_dir_under(&rimz_home())
}

pub fn workspaces_dir_under(home: &Path) -> PathBuf {
    home.join("ws")
}

/// Staged `rimz reload` builds, `<home>/builds`.
pub(crate) fn builds_dir_under(home: &Path) -> PathBuf {
    home.join("builds")
}

#[derive(Clone, Debug)]
pub struct RuntimePaths {
    pub workspace_id: WorkspaceId,
    pub dir_name: WorkspaceDirName,
    /// `$XDG_RUNTIME_DIR` or its fallback; hardened, never RimZ-owned.
    runtime_root: PathBuf,
    /// `<runtime_root>/rimz`.
    rimz_root: PathBuf,
    pub root: PathBuf,
    /// User-scoped election locks. Data caches use [`Self::persistent_shared_root`].
    pub shared_root: PathBuf,
    /// User-scoped provider caches. Production constructors root this under
    /// [`providers_cache_dir`], while [`Self::under`] roots it under the
    /// supplied runtime root for test isolation and byte-identical
    /// cross-workspace cache paths.
    pub persistent_shared_root: PathBuf,
    pub sock_dir: PathBuf,
    pub live_dir: PathBuf,
    pub lanes_dir: PathBuf,
    pub locks_dir: PathBuf,
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
    pub(crate) subagent_context_dir: PathBuf,
    /// Provider telemetry exported inside one room. This is disposable live
    /// cache data: provider processes append here while the room is alive and
    /// runtime GC reclaims stale files with the rest of the workspace root.
    pub(crate) agent_telemetry_dir: PathBuf,
    /// Per-agent activity heartbeats (see [`crate::agent_activity`]). Latency
    /// hints the snapshot folds into each agent's `last_activity`.
    pub agent_activity_dir: PathBuf,
    /// Per-root-session estimated active-time accumulators. Hook producers
    /// update them under per-record locks; snapshot enrichment reads them.
    pub(crate) active_time_dir: PathBuf,
}

pub(crate) fn is_workspace_spending_file(name: &str) -> bool {
    name.strip_prefix("workspace-spending.")
        .and_then(|rest| rest.strip_suffix(".json"))
        .is_some()
}

impl RuntimePaths {
    /// Runtime paths for a workspace known only by id, named after its state
    /// dir (or the `ws-<24hex>` fallback).
    pub fn for_workspace(workspace_id: WorkspaceId) -> Result<Self> {
        let dir_name = workspace_dir_name(&workspaces_dir(), &workspace_id)?;
        Self::validated(workspace_id, dir_name, &runtime_home())
    }

    /// Runtime paths for the workspace born at `project_root`, under the same
    /// name [`StatePaths::for_project_root`] resolves.
    pub fn for_project_root(project_root: &Path) -> Result<Self> {
        let workspace_id = WorkspaceId::from_project_root(project_root);
        let dir_name = workspace_dir_name_for_root(&workspaces_dir(), &workspace_id, project_root)?;
        Self::validated(workspace_id, dir_name, &runtime_home())
    }

    /// Runtime paths paired with state paths already in hand.
    pub fn for_state(state: &StatePaths) -> Result<Self> {
        Self::validated(
            state.workspace_id.clone(),
            state.dir_name.clone(),
            &runtime_home(),
        )
    }

    /// Account-global runtime paths with no bound room, for readers that run
    /// outside a workspace — chiefly the `rimz stats` panel. Only the `shared_*`
    /// accessors carry meaning here; the per-workspace fields resolve under a
    /// reserved all-zero sentinel id and are never created.
    pub fn shared() -> Self {
        let sentinel = WorkspaceId::parse("ws_000000000000000000000000")
            .expect("reserved all-zero workspace id is well-formed");
        let dir_name = WorkspaceDirName::fallback(&sentinel);
        let mut paths = Self::under_named(sentinel, dir_name, &runtime_home());
        paths.persistent_shared_root = providers_cache_dir();
        paths
    }

    /// Build runtime paths rooted at `runtime_root`, naming the dir after a
    /// matching one already in that runtime tree (else `ws-<24hex>`). Tests
    /// prefer this so they don't need to set `XDG_RUNTIME_DIR`. This raw
    /// constructor deliberately skips the socket budget; ambient production
    /// callers use [`Self::for_workspace`] so a long runtime root fails before
    /// any session side effect. Shared data and lock paths both root under
    /// `runtime_root` here so tests stay isolated; [`Self::for_workspace`] and
    /// [`Self::shared`] move shared data to its persistent home.
    pub fn under(workspace_id: WorkspaceId, runtime_root: &Path) -> Result<Self> {
        let ws_dir = runtime_root.join("rimz").join("ws");
        let dir_name = workspace_dir_name(&ws_dir, &workspace_id)?;
        Ok(Self::under_named(workspace_id, dir_name, runtime_root))
    }

    /// Runtime paths for `workspace_id` in the dir `dir_name` under
    /// `runtime_root`, without the socket budget.
    pub fn under_named(
        workspace_id: WorkspaceId,
        dir_name: WorkspaceDirName,
        runtime_root: &Path,
    ) -> Self {
        let rimz_root = runtime_rimz_root_under(runtime_root);
        let root = rimz_root.join("ws").join(dir_name.as_str());
        let shared_root = rimz_root.join("shared");
        let persistent_shared_root = shared_root.clone();
        let sock_dir = root.join(Class::Sock.dir_name());
        let live_dir = root.join(Class::Live.dir_name());
        let lanes_dir = root.join(Class::Lanes.dir_name());
        let locks_dir = root.join(Class::Locks.dir_name());
        let heartbeat_dir = live_dir.join("heartbeat");
        let read_marks_dir = live_dir.join("read-marks");
        let agent_context_dir = live_dir.join("agent_context");
        let subagent_context_dir = live_dir.join("subagent_context");
        let agent_telemetry_dir = live_dir.join("agent-telemetry");
        let agent_activity_dir = live_dir.join("agent-activity");
        let active_time_dir = live_dir.join("active-time");
        Self {
            workspace_id,
            dir_name,
            runtime_root: runtime_root.to_path_buf(),
            rimz_root,
            root,
            shared_root,
            persistent_shared_root,
            sock_dir,
            live_dir,
            lanes_dir,
            locks_dir,
            heartbeat_dir,
            read_marks_dir,
            agent_context_dir,
            subagent_context_dir,
            agent_telemetry_dir,
            agent_activity_dir,
            active_time_dir,
        }
    }

    /// Production runtime paths: socket budget checked, shared data persistent.
    fn validated(
        workspace_id: WorkspaceId,
        dir_name: WorkspaceDirName,
        runtime_root: &Path,
    ) -> Result<Self> {
        let mut paths = Self::budgeted(workspace_id, dir_name, runtime_root)?;
        paths.persistent_shared_root = providers_cache_dir();
        Ok(paths)
    }

    fn budgeted(
        workspace_id: WorkspaceId,
        dir_name: WorkspaceDirName,
        runtime_root: &Path,
    ) -> Result<Self> {
        let paths = Self::under_named(workspace_id, dir_name, runtime_root);
        SockBudget::for_sock_dir(&paths.sock_dir).validate()?;
        Ok(paths)
    }

    /// Resolve another workspace while preserving this instance's runtime and
    /// persistent shared roots. The spending service uses this after validating
    /// a typed workspace id instead of accepting caller-supplied output paths.
    pub(crate) fn for_sibling_workspace(&self, workspace_id: WorkspaceId) -> Result<Self> {
        let dir_name = workspace_dir_name(&workspaces_dir(), &workspace_id)?;
        let mut paths = Self::budgeted(workspace_id, dir_name, &self.runtime_root)?;
        paths.shared_root = self.shared_root.clone();
        paths.persistent_shared_root = self.persistent_shared_root.clone();
        Ok(paths)
    }

    /// Content-addressed launch and system prompt artifacts.
    pub fn prompt_dir(&self) -> PathBuf {
        self.live_path("prompt")
    }

    pub fn live_path(&self, name: impl AsRef<Path>) -> PathBuf {
        self.live_dir.join(name)
    }

    pub fn lane_path(&self, name: impl AsRef<Path>) -> PathBuf {
        self.lanes_dir.join(name)
    }

    pub fn lock_path(&self, name: impl AsRef<Path>) -> PathBuf {
        self.locks_dir.join(name)
    }

    /// Room-local paths only; account-shared paths are outside the class model.
    /// Keyed file families are represented by their containing directory.
    pub fn all_paths(&self) -> Vec<PathBuf> {
        vec![
            self.sock_dir.clone(),
            self.live_dir.clone(),
            self.lanes_dir.clone(),
            self.locks_dir.clone(),
            self.heartbeat_dir.clone(),
            self.read_marks_dir.clone(),
            self.agent_context_dir.clone(),
            self.subagent_context_dir.clone(),
            self.agent_telemetry_dir.clone(),
            self.agent_activity_dir.clone(),
            self.active_time_dir.clone(),
            self.prompt_dir(),
            self.copilot_otel_path(),
            self.sidebar_width_path(),
            self.sidebar_filter_path(),
            self.unread_path(),
            self.pane_frame_path(),
            self.agent_projection_path(),
            self.topology_writer_lock(),
            self.authoritative_pane_probe_path(),
            self.authoritative_pane_probe_lock(),
            self.diff_stats_path(),
            self.cohort_spend_path(),
            self.pipeline_path(),
            self.pr_state_path(),
            self.focus_anchor_path(),
            self.focus_anchor_lock(),
            self.codex_app_server_socket_path(),
        ]
    }

    /// Detach disposable classes before removal so late writers cannot refill
    /// the tree being removed. Lock inodes remain in place across a reset.
    pub(crate) fn remove_disposable_dirs(&self) -> Result<bool> {
        let mut removed = false;
        for path in [&self.sock_dir, &self.live_dir, &self.lanes_dir] {
            removed |= remove_runtime_dir_with(path, |detached| {
                fs::remove_dir_all(detached).map_err(|source| PathErr::Io {
                    path: detached.to_path_buf(),
                    source,
                })?;
                Ok(true)
            })?;
        }
        Ok(removed)
    }

    /// Room-scoped Copilot OpenTelemetry JSONL exporter cache.
    pub fn copilot_otel_path(&self) -> PathBuf {
        self.agent_telemetry_dir.join("copilot-otel.jsonl")
    }

    /// Path of a sidebar instance's heartbeat file. The freshness scan in
    /// [`crate::sidebar::fresh_sidebar_present`] keys on the `sidebar.*.json`
    /// shape this produces; the sidebar process removes this file on exit so a
    /// later launch sees an honest "no sidebar here".
    pub fn sidebar_heartbeat_path(&self, instance_id: &SidebarInstanceId) -> PathBuf {
        self.heartbeat_dir
            .join(format!("sidebar.{}.json", instance_id.as_str()))
    }

    /// Wakeup socket owned by one sidebar render worker.
    pub(crate) fn sidebar_socket_path(&self, instance_id: &SidebarInstanceId) -> PathBuf {
        // The short id keeps the bound path inside the platform AF_UNIX budget.
        self.sock_dir
            .join(format!("sidebar.{}.sock", instance_id.short()))
    }

    /// Path of a sidebar instance's read-mark receipt file. Receipts outlive the
    /// writer so peer renderers can consume them on their next fold; the orphan
    /// sweep reaps stale files once the owning heartbeat has expired.
    pub fn sidebar_read_marks_path(&self, instance_id: &SidebarInstanceId) -> PathBuf {
        self.read_marks_dir
            .join(format!("sidebar.{}.json", instance_id.as_str()))
    }

    /// Room-runtime sidebar width selected by the renderer.
    pub(crate) fn sidebar_width_path(&self) -> PathBuf {
        self.lane_path("sidebar-width.json")
    }

    /// Shared cockpit body filter adopted by every renderer in the room.
    pub(crate) fn sidebar_filter_path(&self) -> PathBuf {
        self.lane_path("sidebar-filter.json")
    }

    /// The workspace-wide set of open unread episodes. The producer owns writes
    /// for status-derived opens and row-gone pruning; renderers and CLI commands
    /// write read receipts that derive this set back to read on the next fold.
    pub fn unread_path(&self) -> PathBuf {
        self.lane_path("unread.json")
    }

    pub fn pane_frame_path(&self) -> PathBuf {
        self.lane_path("snapshot.json")
    }

    /// Producer-published adapter wiring and provider-local sessions.
    pub fn agent_projection_path(&self) -> PathBuf {
        self.lane_path("agent-projection.json")
    }

    /// Serializes Zellij topology writer fencing and cache publication.
    pub(crate) fn topology_writer_lock(&self) -> PathBuf {
        self.lock_path("topology-writer.lock")
    }

    pub(crate) fn authoritative_pane_probe_path(&self) -> PathBuf {
        self.lane_path("authoritative-pane-probe.json")
    }

    pub(crate) fn authoritative_pane_probe_lock(&self) -> PathBuf {
        self.lock_path("authoritative-pane-probe.lock")
    }

    pub fn diff_stats_path(&self) -> PathBuf {
        self.lane_path("diff-stats.json")
    }

    pub(crate) fn cohort_spend_path(&self) -> PathBuf {
        self.lane_path("cohort-spend.json")
    }

    pub(crate) fn pipeline_path(&self) -> PathBuf {
        self.lane_path("pipeline.json")
    }

    pub(crate) fn pr_state_path(&self) -> PathBuf {
        self.lane_path("pr-state.json")
    }

    /// The workspace's last jump scroll anchor: the pane a jump focused plus the
    /// viewport offset that keeps its card where the user clicked. Renderers read
    /// it on the fold that adopts the focus, so a cross-tab jump lands the card at
    /// the same on-screen row. Display-only runtime state, TTL-gated.
    pub(crate) fn focus_anchor_path(&self) -> PathBuf {
        self.lane_path("focus-anchor.json")
    }

    /// Serializes nonce-gated focus action intent transitions.
    pub(crate) fn focus_anchor_lock(&self) -> PathBuf {
        self.lock_path("focus-anchor.lock")
    }

    /// Serializes complete pane writes, including their submit key.
    pub fn pane_write_lock(&self, pane: &PaneId) -> PathBuf {
        self.shared_root
            .join("pane-write")
            .join(format!("{}.lock", hex::encode(pane.as_str())))
    }

    /// Serializes stage transitions for a canonical worktree across rooms.
    pub(crate) fn board_lock(&self, worktree: &Path) -> PathBuf {
        self.shared_root.join("board-write").join(format!(
            "{}.lock",
            hex::encode(Sha256::digest(worktree.as_os_str().as_encoded_bytes()))
        ))
    }

    /// The per-session Codex app-server broker socket. The broker
    /// (`crate::agents::adapters::codex::broker`) binds it; the enrichment client
    /// (`crate::agents::adapters::codex::app_server`) connects to it. Both derive it from
    /// the same `workspace_id`, so it needs no env var to agree.
    pub fn codex_app_server_socket_path(&self) -> PathBuf {
        self.sock_dir.join("codex-app-server.sock")
    }

    pub fn shared_accounts_path(&self) -> PathBuf {
        self.persistent_shared_root.join("accounts.json")
    }

    pub(crate) fn shared_accounts_lock(&self) -> PathBuf {
        self.shared_root.join("accounts.lock")
    }

    pub fn shared_rate_limits_path(&self) -> PathBuf {
        self.persistent_shared_root.join("rate_limits.json")
    }

    pub(crate) fn shared_rate_limits_lock(&self) -> PathBuf {
        self.shared_root.join("rate_limits.lock")
    }

    pub fn shared_credits_path(&self) -> PathBuf {
        self.persistent_shared_root.join("credits.json")
    }

    pub(crate) fn shared_credits_lock(&self) -> PathBuf {
        self.shared_root.join("credits.lock")
    }

    pub(crate) fn shared_auto_redeem_path(&self, key: &crate::ids::LoginKey) -> PathBuf {
        self.persistent_shared_root
            .join(format!("auto_redeem.{key}.json"))
    }

    pub(crate) fn shared_auto_redeem_rate_path(&self, key: &crate::ids::LoginKey) -> PathBuf {
        self.persistent_shared_root
            .join(format!("auto_redeem_rate.{key}.json"))
    }

    pub(crate) fn shared_auto_redeem_lock(&self, key: &crate::ids::LoginKey) -> PathBuf {
        self.shared_root.join(format!("auto_redeem.{key}.lock"))
    }

    pub fn shared_provider_spending_path(&self) -> PathBuf {
        self.persistent_shared_root.join("provider-spending.json")
    }

    pub(crate) fn shared_spending_lock(&self) -> PathBuf {
        self.shared_root.join("spending.lock")
    }

    /// Socket and lifetime-owner lock for the warm account-global spending
    /// walker. Both names discriminate every wire-visible cache version and the
    /// persistent/discovery namespace so incompatible clients elect independent
    /// owners.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn shared_spending_service_socket_path(
        &self,
        protocol_version: u32,
        cache_version: u32,
        provider_version: u32,
        workspace_version: u32,
        namespace: &str,
    ) -> PathBuf {
        self.shared_root.join(format!(
            "spending.v{protocol_version}.c{cache_version}.p{provider_version}.w{workspace_version}.n{namespace}.sock"
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn shared_spending_service_owner_lock(
        &self,
        protocol_version: u32,
        cache_version: u32,
        provider_version: u32,
        workspace_version: u32,
        namespace: &str,
    ) -> PathBuf {
        self.shared_root.join(format!(
            "spending.v{protocol_version}.c{cache_version}.p{provider_version}.w{workspace_version}.n{namespace}.lock"
        ))
    }

    pub fn shared_spending_cursor_path(&self) -> PathBuf {
        self.persistent_shared_root.join("spending.json")
    }

    pub fn shared_pricing_cache_path(&self) -> PathBuf {
        self.persistent_shared_root.join("pricing-cache.json")
    }

    pub fn workspace_spending_path(&self, scope_hash: &str) -> PathBuf {
        let prefix = scope_hash.get(..32).unwrap_or(scope_hash);
        self.lane_path(format!("workspace-spending.{prefix}.json"))
    }

    pub(crate) fn workspace_spending_files(&self) -> Vec<PathBuf> {
        fs::read_dir(&self.lanes_dir)
            .into_iter()
            .flatten()
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(is_workspace_spending_file)
            })
            .collect()
    }

    /// Prepare only user-shared runtime ownership and persistent cache roots.
    /// Account-global readers use this without materializing the reserved
    /// all-zero workspace tree returned by [`Self::shared`].
    pub fn ensure_shared_dirs(&self) -> Result<()> {
        ensure_private_runtime_dir(&self.runtime_root)?;
        ensure_private_runtime_dir(&self.rimz_root)?;
        ensure_private_runtime_dir(&self.shared_root)?;
        mkdir_p(&self.persistent_shared_root)
    }

    /// Prepare the workspace root needed by workspace-scoped disposable
    /// publications without creating every renderer subdirectory.
    pub(crate) fn ensure_workspace_root(&self) -> Result<()> {
        for dir in [
            self.runtime_root.as_path(),
            self.rimz_root.as_path(),
            self.rimz_root.join("ws").as_path(),
            self.root.as_path(),
            self.lanes_dir.as_path(),
            self.locks_dir.as_path(),
        ] {
            ensure_private_runtime_dir(dir)?;
        }
        Ok(())
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        self.ensure_workspace_root()?;
        ensure_private_runtime_dir(&self.shared_root)?;
        mkdir_p(&self.persistent_shared_root)?;
        mkdir_p(&self.sock_dir)?;
        mkdir_p(&self.heartbeat_dir)?;
        mkdir_p(&self.read_marks_dir)?;
        mkdir_p(&self.agent_context_dir)?;
        mkdir_p(&self.subagent_context_dir)?;
        mkdir_p(&self.agent_telemetry_dir)?;
        mkdir_p(&self.agent_activity_dir)?;
        mkdir_p(&self.active_time_dir)?;
        Ok(())
    }
}

fn remove_runtime_dir_with(
    path: &Path,
    remove: impl FnOnce(&Path) -> Result<bool>,
) -> Result<bool> {
    let detached = path.with_extension(format!("reset-{}", uuid::Uuid::now_v7().simple()));
    match fs::rename(path, &detached) {
        Ok(()) => {
            remove(&detached)?;
            Ok(true)
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(PathErr::Io {
            path: path.to_path_buf(),
            source,
        }),
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

/// The one RimZ home: `$RIMZ_HOME`, else `$HOME/.rimz`.
pub fn rimz_home() -> PathBuf {
    let explicit = env_path("RIMZ_HOME");
    #[cfg(test)]
    {
        explicit.unwrap_or_else(unit_test_rimz_home)
    }
    #[cfg(not(test))]
    {
        rimz_home_from(
            explicit.as_deref(),
            env_path("HOME").as_deref(),
            &env::temp_dir(),
        )
    }
}

/// Resolve the RimZ home from an explicit environment view.
pub(crate) fn rimz_home_from(
    rimz_home: Option<&Path>,
    home: Option<&Path>,
    tmpdir: &Path,
) -> PathBuf {
    rimz_home
        .map(Path::to_path_buf)
        .or_else(|| home.map(|home| home.join(".rimz")))
        .unwrap_or_else(|| tmpdir.join("rimz-home"))
}

/// Whether `dir`'s `.rimz` is the RimZ home `home` itself. The default home
/// `~/.rimz` shares its name with a project's `.rimz/`, so a directory holding
/// the home is no project marker, and its `config.toml` is the machine config,
/// never a project layer.
pub fn holds_rimz_home(dir: &Path, home: &Path) -> bool {
    let dot_rimz = dir.join(".rimz");
    dot_rimz == home
        || matches!(
            (dot_rimz.canonicalize(), home.canonicalize()),
            (Ok(a), Ok(b)) if a == b
        )
}

/// Under `cfg(test)`, the lib crate resolves the implicit home to a
/// process-unique, uncreated temp path. Read-only callers leave no residue;
/// mutating tests own a [`tempfile::TempDir`] and use [`StatePaths::under`].
/// Tests that need a specific root set `RIMZ_HOME`.
#[cfg(test)]
fn unit_test_rimz_home() -> PathBuf {
    use std::sync::LazyLock;

    static ROOT: LazyLock<PathBuf> = LazyLock::new(|| {
        env::temp_dir().join(format!(
            "rimz-unit-test-home-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7().simple()
        ))
    });

    ROOT.clone()
}

/// Provider homes RimZ placed for named accounts, `<home>/accounts/<kind>/<name>`.
/// They carry provider credentials and history that nothing can regenerate,
/// so no RimZ command removes this tree.
pub fn accounts_dir() -> PathBuf {
    rimz_home().join("accounts")
}

/// Account-global provider caches, `<home>/cache/providers`: probe memos,
/// rate limits, credits, the spend cursor and aggregate, and the pricing
/// book. Every file rebuilds from the providers' own files; deleting the
/// tree costs a cold dashboard and one full spending walk.
pub fn providers_cache_dir() -> PathBuf {
    cache_dir().join("providers")
}

/// Account-global JSONL logs (assists, focus repairs, loop runs, user inputs).
pub fn logs_dir() -> PathBuf {
    rimz_home().join("logs")
}

/// Loop arming and strike overlays with their locks.
pub fn loops_dir() -> PathBuf {
    rimz_home().join("loops")
}

/// `rimz web` ttyd and share records, their locks, and the Zellij web config.
pub fn web_dir() -> PathBuf {
    rimz_home().join("web")
}

/// Staged `rimz reload` builds.
pub fn builds_dir() -> PathBuf {
    builds_dir_under(&rimz_home())
}

/// RimZ's own regenerable artifacts: pet assets, ttyd binaries and fonts, the
/// materialized Zellij presence plugin, and the provider caches under
/// [`providers_cache_dir`]. Safe to delete at any time.
pub fn cache_dir() -> PathBuf {
    rimz_home().join("cache")
}

/// Handoff notes; the name is reserved here and owned by the skills that write it.
pub fn handoffs_dir() -> PathBuf {
    rimz_home().join("handoffs")
}

pub fn runtime_home() -> PathBuf {
    // Containers and minimal hosts often lack XDG_RUNTIME_DIR. Use the short
    // /tmp/rimz-<uid> namespace per the docs; RuntimePaths::ensure_dirs verifies
    // and hardens the fallback root, rimz root, workspace root, and shared root.
    runtime_home_from(env_path("XDG_RUNTIME_DIR").as_deref(), current_uid())
}

/// Resolve the runtime root from an explicit environment view.
pub(crate) fn runtime_home_from(xdg_runtime: Option<&Path>, uid: u32) -> PathBuf {
    xdg_runtime
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/tmp").join(format!("rimz-{uid}")))
}

/// RimZ's runtime tree, `<runtime>/rimz`.
pub fn runtime_rimz_root() -> PathBuf {
    runtime_rimz_root_under(&runtime_home())
}

pub fn lsp_runtime_dir() -> PathBuf {
    runtime_rimz_root().join("lsp")
}

pub fn lsp_history_path() -> PathBuf {
    rimz_home().join("lsp-history.jsonl")
}

/// [`runtime_rimz_root`] for an explicit runtime root.
pub(crate) fn runtime_rimz_root_under(runtime_root: &Path) -> PathBuf {
    runtime_root.join("rimz")
}

/// Workspace runtime dirs, `<runtime>/rimz/ws`.
pub(crate) fn runtime_workspaces_dir() -> PathBuf {
    runtime_rimz_root().join("ws")
}

/// The resolved runtime domain as concrete environment values.
///
/// A managed session stamps these at birth and on every ensure, so a pane
/// resolves the same store and the same mux endpoint as the client that
/// created it — rather than inheriting whatever ambient environment that
/// client happened to carry, or nothing at all under a re-parented daemon.
///
/// Every value is the resolved one, so a variable the user left unset is
/// pinned to the default RimZ already computes instead of drifting per pane.
/// Deriving these beside [`runtime_home`] is what keeps socket identity and
/// stamped environment two projections of one domain. The `XDG_*` values
/// locate provider homes, not RimZ's own.
pub(crate) fn runtime_domain_env() -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    let mut put = |key: &str, path: PathBuf| {
        env.insert(key.to_owned(), path.display().to_string());
    };
    if let Some(home) = env_path("HOME") {
        put("HOME", home);
    }
    put("RIMZ_HOME", rimz_home());
    put("XDG_CONFIG_HOME", config_home());
    put("XDG_DATA_HOME", data_home());
    put("XDG_CACHE_HOME", cache_home());
    put("XDG_STATE_HOME", state_home());
    put("XDG_RUNTIME_DIR", runtime_home());
    env
}

#[cfg(test)]
fn runtime_fallback_home() -> PathBuf {
    runtime_home_from(None, current_uid())
}

/// Pre-`~/.rimz` RimZ roots still present on this host.
pub fn legacy_roots() -> Vec<PathBuf> {
    existing_legacy_roots([config_home(), state_home(), data_home(), cache_home()])
}

/// Pre-home roots that may hold configuration; a stale cache cannot block room entry.
pub fn legacy_config_roots() -> Vec<PathBuf> {
    existing_legacy_roots([config_home(), state_home(), data_home()])
}

fn existing_legacy_roots(roots: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    roots
        .into_iter()
        .map(|root| root.join("rimz"))
        .filter(|root| root.exists())
        .collect()
}

/// How to migrate `legacy` roots and delete obsolete caches; RimZ reads only the home.
pub fn legacy_roots_fix(legacy: &[PathBuf], home: &Path) -> String {
    let roots = legacy
        .iter()
        .map(|root| root.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let cache = cache_home().join("rimz");
    if legacy.contains(&cache) {
        let to_move = legacy
            .iter()
            .filter(|root| **root != cache)
            .map(|root| root.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        if to_move.is_empty() {
            return format!(
                "RimZ now keeps everything under {home} and no longer reads {roots}; delete the obsolete cache at {cache} (see docs/guide/configuration.md#moving-from-the-xdg-roots)",
                home = home.display(),
                cache = cache.display()
            );
        }
        return format!(
            "RimZ now keeps everything under {home} and no longer reads {roots}; move the contents of {to_move} into {home}, and delete the obsolete cache at {cache} (see docs/guide/configuration.md#moving-from-the-xdg-roots), or set RIMZ_HOME",
            home = home.display(),
            cache = cache.display()
        );
    }
    format!(
        "RimZ now keeps everything under {home} and no longer reads {roots}; move their contents into {home} (see docs/guide/configuration.md#moving-from-the-xdg-roots), or set RIMZ_HOME",
        home = home.display()
    )
}

/// The XDG config root. RimZ's own config lives under [`rimz_home`]; this
/// locates user-level units RimZ installs for other programs (systemd).
pub fn config_home() -> PathBuf {
    xdg_home("XDG_CONFIG_HOME", ".config")
}

/// The XDG state root, stamped for providers and probed for legacy roots.
fn state_home() -> PathBuf {
    xdg_home("XDG_STATE_HOME", ".local/state")
}

/// The XDG data root, stamped for providers and probed for legacy roots.
fn data_home() -> PathBuf {
    xdg_home("XDG_DATA_HOME", ".local/share")
}

/// Per-user cache root, where Zellij keeps its serialized-session cache
/// (`<cache>/zellij/<contract_version>/session_info/<name>`). `rimz reset` wipes
/// the matching entry so a stuck room cannot be resurrected.
pub(crate) fn cache_home() -> PathBuf {
    xdg_home("XDG_CACHE_HOME", ".cache")
}

/// `$<key>`, else `$HOME/<under_home>`. Without either a unit test resolves
/// to an uncreated temp path and production to `<tmp>/rimz-xdg/<under_home>`.
fn xdg_home(key: &str, under_home: &str) -> PathBuf {
    if let Some(value) = env_path(key) {
        return value;
    }
    #[cfg(test)]
    {
        unit_test_rimz_home().join("xdg").join(under_home)
    }
    #[cfg(not(test))]
    {
        env_path("HOME")
            .map(|home| home.join(under_home))
            .unwrap_or_else(|| env::temp_dir().join("rimz-xdg").join(under_home))
    }
}

/// Per-user agent library root: `$RIMZ_AGENTS_HOME`, else [`rimz_home`].
/// `RIMZ_AGENTS_HOME` relocates profiles, teams, and skills together.
pub fn agents_home() -> PathBuf {
    env_path("RIMZ_AGENTS_HOME").unwrap_or_else(rimz_home)
}

/// Resolve the agent library root from a launch environment, ignoring empty values.
fn agents_home_in(env: &BTreeMap<String, String>) -> Option<PathBuf> {
    let path = |key: &str| {
        env.get(key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    path("RIMZ_AGENTS_HOME")
        .or_else(|| path("RIMZ_HOME"))
        .or_else(|| path("HOME").map(|home| home.join(".rimz")))
}

const SKILLS_SUBDIR: &str = "skills";

/// Shared RimZ skill library under the agent library root.
pub fn skills_library() -> PathBuf {
    agents_home().join(SKILLS_SUBDIR)
}

/// Resolve the shared skill library from a launch environment.
pub(crate) fn skills_library_in(env: &BTreeMap<String, String>) -> Option<PathBuf> {
    agents_home_in(env).map(|root| root.join(SKILLS_SUBDIR))
}

/// Read an environment variable as a path, treating an empty value as unset.
pub fn env_path(key: &str) -> Option<PathBuf> {
    env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

#[cfg(unix)]
fn current_uid() -> u32 {
    nix::unistd::Uid::current().as_raw()
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

#[cfg(test)]
mod tests;
