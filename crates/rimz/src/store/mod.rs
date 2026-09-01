//! Durable workspace state — the [`Store`] handle, core errors, paths, and
//! lock-free reads. Snapshot schema lives under [`snapshot`]; mutation
//! vocabulary and write choreography live under [`writer`].
//!
//! Module split (the local contract lives in `AGENTS.md` beside this file):
//!
//! ```text
//! store/
//!   paths.rs        StatePaths, RuntimePaths, XDG resolution
//!   atomic.rs       temp+rename, length-framed append
//!   lock.rs         workspace advisory lock
//!   event_log.rs    framed append-log façade
//!   event_log/      frame codec, rotation, recovery, unit tests
//!   message_store.rs live message queue JSONL store
//!   sidecar.rs      shared stat-gated enrichment sidecar store
//!   active_time.rs   grace-capped per-session working-time accumulator
//!   session_death.rs shared store-provable session death rules
//!   writer.rs       mutation vocabulary + choreography: lock → write → append → wake → publish
//!   writer/         debounce, lifecycle policy, publish, queue, reap, reset
//!   gc.rs           maintenance façade
//!   gc/             runtime collection and dead-workspace pruning
//!   snapshot/       reduced snapshot rebuild
//!     fold.rs       resumable event-log rollup + carryover
//!     project.rs    agent-lifecycle reducer
//!     panes.rs      pane binding + own/daemon view; lazy pairing in panes/
//!     process.rs    non-agent process rows; command classifier in process/
//!     view.rs       sidebar view-model façade; live/group/provider projection in view/
//!     assemble.rs   read entry points + fresh-latest fast path
//! ```
//!
//! [`Store`] is a `Clone`able handle around `Arc<StoreInner>`. This file is
//! the handle: types, constructor, accessors, and the lock-free read methods.
//! Every mutator lives in `writer.rs` and takes the workspace lock for its
//! critical section — there is no in-process actor. Cross-process
//! serialization is the workspace lock's job; every writer is a short-lived
//! CLI process serialized through `workspace.lock` (flock).

pub mod active_time;
pub mod agent_context;
pub mod atomic;
pub mod event;
pub mod event_log;
pub mod gc;
pub mod live_roster;
pub mod lock;
mod message_store;
pub(crate) mod parse_cache;
pub mod paths;
pub(crate) mod run_store;
pub mod runtime;
pub(crate) mod session_death;
pub(crate) mod sidecar;
pub mod single_flight;
pub mod snapshot;
pub mod subagent_context;
pub mod workspace_record;

pub mod writer;

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use crate::store::event::EventEnvelope;
use crate::store::snapshot::SidebarSnapshot;

pub use crate::store::paths::{RuntimePaths, StatePaths};
pub use crate::store::runtime::{RuntimeProjection, RuntimeScope};
pub use message_store::MessageStoreErr;

/// High-level handle to a workspace's durable state. Cheap to clone — the
/// inner state lives behind an `Arc`. Reads here are lock-free; every
/// mutator (in `writer.rs`) takes the workspace lock for its critical
/// section and writes through the `event_log` and `snapshot`
/// modules directly.
#[derive(Clone, Debug)]
pub struct Store {
    inner: Arc<StoreInner>,
}

#[derive(Debug)]
struct StoreInner {
    paths: StatePaths,
    runtime: RuntimePaths,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreErr {
    #[error(transparent)]
    Path(#[from] paths::PathErr),
    #[error(transparent)]
    EventLog(#[from] event_log::EventLogErr),
    #[error(transparent)]
    MessageStore(#[from] MessageStoreErr),
    #[error(transparent)]
    RunStore(#[from] crate::harness::run::RunStoreErr),
    #[error(transparent)]
    Lock(#[from] lock::LockErr),
    #[error(transparent)]
    Snapshot(#[from] snapshot::SnapshotErr),
    #[error(transparent)]
    WorkspaceRecord(#[from] workspace_record::WorkspaceRecordErr),
    #[error("{0}")]
    AgentLaunchIdentity(String),
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

pub type Result<T> = std::result::Result<T, StoreErr>;

impl Store {
    pub fn open(paths: StatePaths, runtime: RuntimePaths) -> Result<Self> {
        paths.ensure_dirs()?;
        runtime.ensure_dirs()?;
        Ok(Self {
            inner: Arc::new(StoreInner { paths, runtime }),
        })
    }

    /// Open an existing store for read paths without creating directories.
    #[must_use]
    pub fn open_existing(paths: StatePaths, runtime: RuntimePaths) -> Option<Self> {
        if !paths.root.is_dir() {
            return None;
        }
        Some(Self {
            inner: Arc::new(StoreInner { paths, runtime }),
        })
    }

    pub fn paths(&self) -> &StatePaths {
        &self.inner.paths
    }

    pub fn runtime_paths(&self) -> &RuntimePaths {
        &self.inner.runtime
    }

    pub fn workspace_lock_path(&self) -> &PathBuf {
        &self.inner.paths.workspace_lock
    }

    /// Project the live runtime state (event-log agent rollup) for CLI read
    /// entry points and resume planning.
    ///
    /// Lock-free. The agent rollup resumes from the persisted fold base, so
    /// an in-flight tail frame a reader races is simply not folded yet — it
    /// can never drop a previously-folded event, and the write that
    /// completes the frame posts the wakeup that folds it.
    pub fn runtime_projection(
        &self,
        scope: runtime::RuntimeScope,
    ) -> Result<runtime::RuntimeProjection> {
        let (_, agents, _) = snapshot::catch_up_rollup(&self.inner.paths)?;
        Ok(runtime::RuntimeProjection::from_parts(agents, scope))
    }

    /// Build a fresh snapshot in memory (no disk write). Lock-free and
    /// O(delta): the rollup resumes from the persisted fold base.
    pub fn snapshot(&self) -> Result<SidebarSnapshot> {
        Ok(snapshot::build_from(&self.inner.paths)?)
    }

    /// Like [`Self::snapshot`] but O(1) in the common case: serve the pre-built
    /// `latest.json` rollup when its extent stamp matches the live log,
    /// falling back to a re-projection on a miss. The fast path is lock-free
    /// — it reads no event log and takes no lock — so a hot fleet's snapshot
    /// fetches never contend with the agent hooks appending events.
    /// `latest.json` is published after every mutation's lock releases and
    /// carries the same runtime liveness expel as [`Self::snapshot`], so the
    /// served rollup matches the live read; the extent stamp catches a fetch
    /// racing a just-appended event and re-projects instead. Cached snapshots
    /// also attach cache-class agent context so every address resolver sees the
    /// same rest certificates as pane ownership.
    pub fn snapshot_cached(&self) -> Result<SidebarSnapshot> {
        let snapshot = match snapshot::read_fresh_latest(&self.inner.paths) {
            Some(snapshot) => snapshot,
            None => self.snapshot()?,
        };
        let context = agent_context::read_for_keys(
            &self.inner.runtime,
            snapshot
                .agents
                .iter()
                .map(|agent| (agent.kind.as_str(), agent.agent_id.as_str())),
        );
        Ok(snapshot.with_agent_context(context))
    }

    /// Walk the event log, returning every parseable record and logging
    /// torn records at `warn`.
    pub fn read_events(&self) -> Result<Vec<EventEnvelope>> {
        Ok(event_log::read_all(&self.inner.paths.events_log)?)
    }

    /// Return a frame-aligned active-log offset for incremental wait polls.
    pub fn wait_fold_base(&self) -> Result<u64> {
        let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
        match std::fs::metadata(&self.inner.paths.events_log) {
            Ok(meta) => Ok(meta.len()),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(0),
            Err(source) => Err(StoreErr::Io {
                path: self.inner.paths.events_log.clone(),
                source,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::lifecycle::LifecycleSignal;
    use crate::agents::{
        AgentContext, AgentLifecycleObservation, AgentStatus, AgentTurnError, TurnErrorClass,
    };
    use crate::ids::{AgentSessionId, WorkspaceId};
    use jiff::Timestamp;

    #[test]
    fn open_existing_missing_root_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let state_root = dir.path().join("absent-state");
        let runtime_root = dir.path().join("absent-runtime");
        let paths = StatePaths::under(workspace_id.clone(), &state_root).unwrap();
        let runtime = RuntimePaths::under(workspace_id, &runtime_root).unwrap();

        assert!(Store::open_existing(paths.clone(), runtime.clone()).is_none());
        assert!(!paths.root.exists());
        assert!(!runtime.root.exists());
        assert!(!runtime.shared_root.exists());
    }

    #[test]
    fn open_creates_state_and_runtime_tree() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(workspace_id.clone(), &dir.path().join("state")).unwrap();
        let runtime = RuntimePaths::under(workspace_id, &dir.path().join("runtime")).unwrap();

        Store::open(paths.clone(), runtime.clone()).unwrap();

        for path in [&paths.snapshots_dir, &paths.runs_dir, &paths.locks_dir] {
            assert!(path.is_dir(), "{} was not created", path.display());
        }
        for path in [
            &runtime.root,
            &runtime.shared_root,
            &runtime.sock_dir,
            &runtime.heartbeat_dir,
            &runtime.read_marks_dir,
            &runtime.agent_context_dir,
            &runtime.subagent_context_dir,
            &runtime.agent_telemetry_dir,
            &runtime.agent_activity_dir,
            &runtime.active_time_dir,
        ] {
            assert!(path.is_dir(), "{} was not created", path.display());
        }
    }

    #[test]
    fn wait_fold_base_is_zero_then_frame_aligned() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(workspace_id.clone(), &dir.path().join("state")).unwrap();
        let runtime =
            RuntimePaths::under(workspace_id.clone(), &dir.path().join("runtime")).unwrap();
        let store = Store::open(paths.clone(), runtime).unwrap();

        assert_eq!(store.wait_fold_base().unwrap(), 0);
        event_log::append(&paths.events_log, &lifecycle(&workspace_id, 0)).unwrap();
        let base = store.wait_fold_base().unwrap();
        assert_eq!(base, std::fs::metadata(&paths.events_log).unwrap().len());
        assert_eq!(
            event_log::read_from_offset(&paths.events_log, base).unwrap(),
            (Vec::new(), base)
        );
    }

    #[test]
    fn cached_snapshot_attaches_rest_certificates_for_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(workspace_id.clone(), &dir.path().join("state")).unwrap();
        let runtime =
            RuntimePaths::under(workspace_id.clone(), &dir.path().join("runtime")).unwrap();
        let store = Store::open(paths.clone(), runtime.clone()).unwrap();
        let error_at = Timestamp::now();
        let started_at = error_at - std::time::Duration::from_secs(1);
        let registered_at = error_at - std::time::Duration::from_secs(2);
        let mut registered = lifecycle(&workspace_id, 0);
        registered.timestamp = registered_at;
        let started = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("agent-0")),
            LifecycleSignal::TurnStarted,
        );
        let mut started = EventEnvelope::agent_lifecycle(
            workspace_id,
            "session",
            "claude",
            "UserPromptSubmit",
            &started,
        );
        started.timestamp = started_at;
        event_log::append(&paths.events_log, &registered).unwrap();
        event_log::append(&paths.events_log, &started).unwrap();
        let context = AgentContext {
            turn_error: Some(AgentTurnError {
                class: TurnErrorClass::PausedOverloaded,
                at: error_at,
                label: Some("server_overloaded".to_owned()),
            }),
            ..AgentContext::new("claude", error_at)
        };
        agent_context::write(&runtime, "claude", "agent-0", &context).unwrap();
        agent_context::write(
            &runtime,
            "claude",
            "unrelated-agent",
            &AgentContext::new("claude", error_at),
        )
        .unwrap();

        sidecar::testkit::reset_parse_reads();
        let snapshot = store.snapshot_cached().unwrap();
        let agent = &snapshot.agents[0];

        assert_eq!(agent.status, AgentStatus::Running);
        assert!(!agent.holds_open_turn());
        assert_eq!(
            sidecar::testkit::parse_reads(),
            1,
            "cached snapshots read context only for projected agent keys"
        );
    }

    fn lifecycle(workspace_id: &WorkspaceId, index: usize) -> EventEnvelope {
        let mut observation = AgentLifecycleObservation::new(
            Some(AgentSessionId::from(format!("agent-{index}"))),
            LifecycleSignal::Registered,
        );
        observation.agent_name = Some(format!("agent-{index}"));
        EventEnvelope::agent_lifecycle(
            workspace_id.clone(),
            "session",
            "claude",
            "SessionStart",
            &observation,
        )
    }

    #[test]
    fn runtime_projection_uses_the_rollup_cache_not_the_full_log() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        runtime.ensure_dirs().unwrap();
        for index in 0..16 {
            event_log::append(&paths.events_log, &lifecycle(&workspace_id, index)).unwrap();
        }
        let log_len = std::fs::metadata(&paths.events_log).unwrap().len();
        snapshot::rebuild(&paths).unwrap();
        let store = Store::open(paths.clone(), runtime).unwrap();

        let warm_before = event_log::testkit::bytes_read();
        let projection = store
            .runtime_projection(RuntimeScope::Audit)
            .expect("projection");
        let warm_bytes = event_log::testkit::bytes_read() - warm_before;
        assert_eq!(projection.agents.len(), 16);
        assert_eq!(
            warm_bytes, 0,
            "fresh rollup cache avoids rereading the {log_len}-byte history"
        );

        event_log::append(&paths.events_log, &lifecycle(&workspace_id, 16)).unwrap();
        let appended = std::fs::metadata(&paths.events_log).unwrap().len() - log_len;
        let delta_before = event_log::testkit::bytes_read();
        let projection = store
            .runtime_projection(RuntimeScope::Audit)
            .expect("projection after append");
        let delta_bytes = event_log::testkit::bytes_read() - delta_before;
        assert_eq!(projection.agents.len(), 17);
        assert_eq!(
            delta_bytes, appended,
            "runtime projection folds only the appended frame"
        );
    }
}
