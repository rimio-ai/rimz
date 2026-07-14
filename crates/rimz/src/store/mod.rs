//! Durable workspace state — store paths, atomic helpers, event log, and
//! snapshots.
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
//!   session_death.rs shared store-provable session death rules
//!   writer.rs       write choreography façade: lock → write → append → wake → publish
//!   writer/         debounce, publish, queue, reap, reset
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

pub mod agent_context;
pub mod atomic;
pub mod event;
pub mod event_log;
pub mod gc;
pub mod live_roster;
pub mod lock;
pub mod message_store;
pub(crate) mod parse_cache;
pub mod paths;
pub mod run_store;
pub mod runtime;
pub(crate) mod session_death;
pub(crate) mod sidecar;
pub mod single_flight;
pub mod snapshot;
pub mod subagent_context;
pub mod wakeup;
pub mod workspace_record;

mod writer;

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use crate::agents::LaunchParams;
use crate::ids::{AgentKind, AgentSessionId, PaneId, RunId, WorkspaceId};
use crate::store::event::{AgentLaunchState, EventEnvelope};

pub use crate::store::paths::{RuntimePaths, StatePaths};
pub use crate::store::runtime::{RuntimeProjection, RuntimeScope};
pub use crate::store::snapshot::{
    AgentCard, DailyBudgetView, PaneAgent, PresenceSample, ProcessCard, ProcessState,
    RemoteControlBadge, RowCallSplit, RowCard, SidebarLinkFreshness, SidebarLinkHealth,
    SidebarOwnView, SidebarPresence, SidebarProviderPanel, SidebarRow, SidebarSnapshot,
    SidebarStatusCount, SidebarSubAgent, SidebarWorktreeGroup, SidebarWorktreeKind, TruthNotice,
    WorktreePrState, WorktreeTrunkSync, actionable_unread_count, lead_unread_row, triage_key,
};
pub use crate::store::workspace_record::WorkspaceRecord;
pub use crate::store::writer::{EditOutcome, MessageEdit};

/// Terminal audit-only message outcome for a target that never resolved to a
/// durable receiver card.
pub struct UnresolvedMessage<'a> {
    pub workspace_id: WorkspaceId,
    pub session_name: &'a str,
    pub address: &'a str,
    pub channel: Option<&'a str>,
    pub sender: &'a crate::message::MessageSender,
    pub text_len: usize,
    pub reason: &'a str,
}

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
    MessageStore(#[from] message_store::MessageStoreErr),
    #[error(transparent)]
    RunStore(#[from] run_store::RunStoreErr),
    #[error(transparent)]
    Lock(#[from] lock::LockErr),
    #[error(transparent)]
    Snapshot(#[from] snapshot::SnapshotErr),
    #[error(transparent)]
    Wakeup(#[from] wakeup::WakeupErr),
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

#[derive(Clone, Debug)]
pub struct WorkspaceRewriteOutcome {
    pub workspace_id: WorkspaceId,
    pub messages_rewritten: usize,
    pub events_rewritten: usize,
}

#[derive(Clone, Debug)]
pub struct EventLogRotationOutcome {
    pub rotation: event_log::RotationOutcome,
    pub pruned: atomic::PruneOutcome,
    pub carryover_agents: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentLaunchName {
    Mint,
    Soft(String),
    Explicit(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentLaunchRequest {
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    pub name: AgentLaunchName,
    pub launch: LaunchParams,
    pub run_id: Option<RunId>,
    pub prompt: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentLaunchIdentity {
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    pub name: String,
    pub name_explicit: bool,
    pub launch: LaunchParams,
    pub run_id: Option<RunId>,
    pub prompt: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentLaunchAppend {
    pub workspace_id: WorkspaceId,
    pub session_name: String,
    pub cwd: PathBuf,
    pub worktree_name: Option<String>,
    pub channel: Option<String>,
    pub description: Option<String>,
    pub state: AgentLaunchState,
    pub pane_id: Option<PaneId>,
}

#[derive(Clone, Debug)]
pub struct ResetRecordsOutcome {
    pub runs_canceled: usize,
    pub state_entries_removed: usize,
    pub runtime_removed: bool,
    pub rotation: event_log::RotationOutcome,
    pub carryover_agents: usize,
    pub hard: bool,
}

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
        let (cache, agents, _) = snapshot::catch_up_rollup(&self.inner.paths)?;
        let ended = cache.tombstones.into_iter().collect();
        Ok(runtime::RuntimeProjection::from_parts(ended, agents, scope))
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
    /// racing a just-appended event and re-projects instead.
    pub fn snapshot_cached(&self) -> Result<SidebarSnapshot> {
        if let Some(snapshot) = snapshot::read_fresh_latest(&self.inner.paths) {
            return Ok(snapshot);
        }
        self.snapshot()
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
    use crate::agents::AgentLifecycleObservation;
    use crate::agents::lifecycle::LifecycleSignal;

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
