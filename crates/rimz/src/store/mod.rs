//! Durable workspace state — the [`Store`] handle, core errors, paths, and
//! lock-free reads. Snapshot schema lives under [`snapshot`]; mutation
//! vocabulary and write choreography live under [`writer`].
//!
//! Module split (the local contract lives in `AGENTS.md` beside this file):
//!
//! ```text
//! store/            file primitives live in sibling module `disk/`
//!   event_log.rs    framed append-log façade
//!   event_log/      frame codec, rotation, recovery, unit tests
//!   follow.rs       read-only lifecycle event-log follower
//!   message.rs     durable message vocabulary and selection
//!   message/       queue and history codec
//!   sidecar.rs      shared stat-gated enrichment sidecar store
//!   active_time.rs   grace-capped per-session working-time accumulator
//!   session_death.rs shared store-provable session death rules
//!   writer.rs       mutation vocabulary + choreography: lock → write → append → wait → publish
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
pub mod event;
pub mod event_log;
pub mod follow;
pub mod gc;
pub mod hibernate;
pub mod idle_stop;
pub mod ingress;
pub mod live_roster;
pub mod message;
pub(crate) mod pending_recovery;
pub mod run;
pub mod runtime;
pub(crate) mod session_death;
pub(crate) mod sidecar;
pub mod snapshot;
pub mod subagent_context;
pub mod workflow;
pub mod writer;

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use crate::disk::paths::{RuntimePaths, StatePaths};
use crate::disk::{lock, paths};
use crate::ids::{AgentKind, AgentSessionId};
use crate::store::event::EventEnvelope;
use crate::store::snapshot::SidebarSnapshot;

pub use crate::store::runtime::{RuntimeProjection, RuntimeScope};

/// Room defaults plus the cached live agents' account stamps.
pub(crate) fn room_logins_in_use(runtime: &RuntimePaths) -> crate::agents::RoomLoginSet {
    let agents = StatePaths::for_workspace(runtime.workspace_id.clone())
        .ok()
        .and_then(|paths| Store::open_existing(paths, runtime.clone()))
        .and_then(|store| store.snapshot_cached().ok())
        .map(|snapshot| snapshot.agents)
        .unwrap_or_default();
    crate::agents::RoomLoginSet::for_runtime(runtime).with_agents(&agents)
}

/// High-level handle to a workspace's durable state. Cheap to clone — the
/// inner state lives behind an `Arc`. Reads here are lock-free; every
/// mutator (in `writer.rs`) takes the workspace lock for its critical
/// section and writes through the `event_log` and `snapshot`
/// modules directly.
#[derive(Clone, Debug)]
pub struct Store {
    inner: Arc<StoreInner>,
    ingress: Option<(crate::ids::EventId, jiff::Timestamp)>,
    hook_transaction: Option<Arc<writer::HookTransaction>>,
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
    MessageStore(#[from] message::MessageStoreErr),
    #[error(transparent)]
    RunStore(#[from] run::RunStoreErr),
    #[error(transparent)]
    Lock(#[from] lock::LockErr),
    #[error(transparent)]
    Snapshot(#[from] snapshot::SnapshotErr),
    #[error(transparent)]
    WorkspaceRecord(#[from] crate::workspace::record::WorkspaceRecordErr),
    #[error(transparent)]
    RoomLogin(#[from] Box<crate::agents::RoomLoginErr>),
    #[error("{0}")]
    AgentLaunchIdentity(String),
    #[error(
        "molt refused: @{successor} already stands as successor; only one successor is allowed at a time. If it is stuck, `rimz agents stop @{successor}` frees the slot; do not retry this command"
    )]
    MoltSuccessor { successor: String },
    #[error(
        "this room uses {kind} account `{current}`, not `{requested}`; switch it with `rimz accounts use {kind} {requested}`"
    )]
    RoomLoginsFrozen {
        kind: crate::ids::AgentKind,
        current: crate::ids::LoginName,
        requested: crate::ids::LoginName,
    },
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

pub(crate) type Result<T> = std::result::Result<T, StoreErr>;

impl Store {
    pub fn open(mut paths: StatePaths, mut runtime: RuntimePaths) -> Result<Self> {
        crate::disk::paths::check_workspace_layout(&paths.root)?;
        runtime.bind_state_locks(&paths);
        paths.bind_runtime_checkpoint(&runtime);
        paths.ensure_dirs()?;
        runtime.ensure_dirs()?;
        Ok(Self {
            inner: Arc::new(StoreInner { paths, runtime }),
            ingress: None,
            hook_transaction: None,
        })
    }

    /// Open a room with a readable workspace record without creating directories.
    #[must_use]
    pub fn open_existing(mut paths: StatePaths, mut runtime: RuntimePaths) -> Option<Self> {
        if crate::workspace::record::read(&paths.workspace_record).is_err() {
            return None;
        }
        runtime.bind_state_locks(&paths);
        paths.bind_runtime_checkpoint(&runtime);
        Some(Self {
            inner: Arc::new(StoreInner { paths, runtime }),
            ingress: None,
            hook_transaction: None,
        })
    }

    pub fn paths(&self) -> &StatePaths {
        &self.inner.paths
    }

    pub fn hook_ingress_id(&self) -> Option<&crate::ids::EventId> {
        self.ingress.as_ref().map(|(id, _)| id)
    }

    pub fn runtime_paths(&self) -> &RuntimePaths {
        &self.inner.runtime
    }

    /// Project the live runtime state (event-log agent rollup) for CLI read
    /// entry points and resume planning.
    ///
    /// Lock-free. The agent rollup resumes from the thread's held fold or the
    /// persisted fold base, so an in-flight tail frame a reader races is
    /// simply not folded yet — it can never drop a previously-folded event,
    /// and the write that completes the frame posts the wakeup that folds it.
    pub fn runtime_projection(
        &self,
        scope: runtime::RuntimeScope,
    ) -> Result<runtime::RuntimeProjection> {
        let rollup =
            snapshot::catch_up_with_pending(&self.inner.paths, &self.pending_hook_events())?;
        Ok(runtime::RuntimeProjection::from_parts(
            &rollup.agents,
            scope,
            rollup.resume_outcomes,
        ))
    }

    /// Build a fresh snapshot in memory (no disk write). Lock-free and
    /// O(delta): the rollup resumes from the thread's held fold or the
    /// persisted fold base.
    pub fn snapshot(&self) -> Result<SidebarSnapshot> {
        if self.hook_transaction.is_some() {
            return Ok(snapshot::build_from_pending(
                &self.inner.paths,
                &self.pending_hook_events(),
            )?);
        }
        Ok(snapshot::build_from(&self.inner.paths)?)
    }

    /// Like [`Self::snapshot`] but O(1) in the common case: serve the pre-built
    /// `latest.json` rollup when its extent stamp matches the live log,
    /// falling back to a re-projection on a miss. The fast path is lock-free
    /// — it reads no event log and takes no lock — so a hot fleet's snapshot
    /// fetches never contend with the agent hooks appending events.
    /// `latest.json` publication is cadence-gated, and the cached view carries
    /// the runtime liveness expel as of its last publish: an owner that died
    /// since stays until the next append. A reader that decides on current
    /// liveness rechecks each row with [`runtime::agent_liveness`], as
    /// `harness::schedule::takeover::read` does. The extent stamp catches a
    /// fetch racing a just-appended event and re-projects instead. Cached
    /// snapshots also attach cache-class agent context so every address
    /// resolver sees the same rest certificates as pane ownership.
    /// Reads that need only selected context use [`Self::snapshot_with_context`].
    pub fn snapshot_cached(&self) -> Result<SidebarSnapshot> {
        self.snapshot_with_context(|_, _| true)
    }

    /// The same cached rows and membership as [`Self::snapshot_cached`], with
    /// context attached only to selected `(kind, agent_id)` keys. Unselected
    /// rows stay raw. An empty selection performs no sidecar read and leaves
    /// this thread's sidecar parse cache intact.
    pub fn snapshot_with_context(
        &self,
        mut select: impl FnMut(&AgentKind, &AgentSessionId) -> bool,
    ) -> Result<SidebarSnapshot> {
        let snapshot = match self
            .hook_transaction
            .is_none()
            .then(|| snapshot::read_fresh_latest(&self.inner.paths))
            .flatten()
        {
            Some(snapshot) => snapshot,
            None => self.snapshot()?,
        };
        let mut keys = snapshot
            .agents
            .iter()
            .filter(|agent| select(&agent.kind, &agent.agent_id))
            .map(|agent| (agent.kind.as_str(), agent.agent_id.as_str()))
            .peekable();
        if keys.peek().is_none() {
            return Ok(snapshot);
        }
        let context = agent_context::read_for_keys(&self.inner.runtime, keys);
        Ok(snapshot.with_agent_context(context))
    }

    /// Whether a rebirth parked this agent for the user's recovery decision
    /// and no settlement has resumed or ended it yet. Lock-free.
    pub fn is_pending_recovery(&self, kind: &AgentKind, agent_id: &AgentSessionId) -> bool {
        pending_recovery::read(&self.inner.paths.pending_recovery)
            .contains(&(kind.clone(), agent_id.clone()))
    }

    /// Walk the event log, returning every parseable record and logging
    /// torn records at `warn`.
    pub fn read_events(&self) -> Result<Vec<EventEnvelope>> {
        Ok(event_log::read_all(&self.inner.paths.events_log)?)
    }

    fn pending_hook_events(&self) -> Vec<EventEnvelope> {
        self.hook_transaction
            .as_ref()
            .map(|transaction| {
                transaction
                    .tail
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .0
                    .clone()
            })
            .unwrap_or_default()
    }

    /// Return a frame-aligned active-log offset for incremental wait polls.
    pub(crate) fn wait_fold_base(&self) -> Result<u64> {
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

        let store = Store::open(paths.clone(), runtime.clone()).unwrap();

        assert_eq!(
            store.paths().snapshots_dir(),
            Some(crate::disk::paths::Class::Snapshots.path_under(&runtime.root))
        );

        for path in [&paths.runs_dir, &paths.cache_dir] {
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
    fn pending_recovery_membership_reads_the_parked_record() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(workspace_id.clone(), &dir.path().join("state")).unwrap();
        let runtime = RuntimePaths::under(workspace_id, &dir.path().join("runtime")).unwrap();
        let store = Store::open(paths.clone(), runtime).unwrap();
        let kind = AgentKind::new_unchecked("claude");
        let parked = AgentSessionId::from("parked");

        pending_recovery::park(&paths, &[(kind.clone(), parked.clone())].into()).unwrap();

        assert!(store.is_pending_recovery(&kind, &parked));
        assert!(!store.is_pending_recovery(&kind, &AgentSessionId::from("other")));
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
            LifecycleSignal::TurnStarted { turn_id: None },
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
        agent_context::write(&runtime, "claude", "agent-0", &context, None).unwrap();
        agent_context::write(
            &runtime,
            "claude",
            "unrelated-agent",
            &AgentContext::new("claude", error_at),
            None,
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

    #[test]
    fn selected_context_preserves_membership_masking_and_cache_freshness() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
        let store = Store::open(paths.clone(), runtime.clone()).unwrap();
        let paths = store.paths().clone();
        let at = Timestamp::now();
        for index in 0..2 {
            let context = AgentContext {
                tokens: Some(crate::agents::AgentTokenUsage {
                    context_window_size: Some(200_000),
                    current_context_tokens: Some(42),
                    used_percentage: Some(1),
                    ..Default::default()
                }),
                ..AgentContext::new("claude", at)
            };
            agent_context::write(
                &runtime,
                "claude",
                &format!("agent-{index}"),
                &context,
                None,
            )
            .unwrap();
        }
        let mut raw = store.snapshot().unwrap();
        raw.agents = (0..2)
            .map(|index| {
                crate::agents::AgentState::stub(
                    "claude",
                    &format!("agent-{index}"),
                    AgentStatus::Idle,
                )
            })
            .collect::<Vec<_>>()
            .into();
        Arc::make_mut(&mut raw.agents)[0].retired_context_tokens = Some(42);
        crate::disk::atomic::write_temp_then_rename_cache(&paths.latest_snapshot().unwrap(), &raw)
            .unwrap();
        let full = store.snapshot_cached().unwrap();
        let own = |kind: &AgentKind, id: &AgentSessionId| kind == "claude" && id == "agent-0";
        let selected = store.snapshot_with_context(own).unwrap();
        assert_eq!(selected.agents.len(), full.agents.len());
        assert_eq!(selected.agents[0], full.agents[0]);
        assert_eq!(selected.agents[1].agent_id, full.agents[1].agent_id);
        assert!(
            selected.agents[1].context.is_none(),
            "unselected row stays raw"
        );
        let tokens = selected.agents[0]
            .context
            .as_ref()
            .unwrap()
            .tokens
            .as_ref()
            .unwrap();
        assert_eq!(tokens.current_context_tokens, None);
        assert_eq!(tokens.used_percentage, None);
        assert_eq!(tokens.context_window_size, Some(200_000));

        let before = sidecar::testkit::read_counts();
        let empty = store.snapshot_with_context(|_, _| false).unwrap();
        assert!(empty.agents.iter().all(|agent| agent.context.is_none()));
        store.snapshot_with_context(own).unwrap();
        let after = sidecar::testkit::read_counts();
        assert_eq!(
            (after.0 - before.0, after.1 - before.1, after.2 - before.2),
            (1, 0, 0)
        );

        let replacement = AgentContext {
            tokens: Some(crate::agents::AgentTokenUsage {
                current_context_tokens: Some(9_999),
                ..Default::default()
            }),
            ..AgentContext::new("claude", at + std::time::Duration::from_secs(1))
        };
        agent_context::write(&runtime, "claude", "agent-0", &replacement, None).unwrap();
        let selected = store.snapshot_with_context(own).unwrap();
        assert_eq!(
            selected.agents[0]
                .context
                .as_ref()
                .unwrap()
                .tokens
                .as_ref()
                .unwrap()
                .current_context_tokens,
            Some(9_999)
        );
        agent_context::remove(&runtime, "claude", "agent-0").unwrap();
        assert!(
            store.snapshot_with_context(own).unwrap().agents[0]
                .context
                .is_none()
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
        let store = Store::open(paths, runtime).unwrap();
        let paths = store.paths();
        for index in 0..16 {
            event_log::append(&paths.events_log, &lifecycle(&workspace_id, index)).unwrap();
        }
        let log_len = std::fs::metadata(&paths.events_log).unwrap().len();
        snapshot::rebuild(paths).unwrap();

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
