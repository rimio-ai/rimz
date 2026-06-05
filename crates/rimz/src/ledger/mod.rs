//! Durable workspace state — ledger paths, atomic helpers, event log, feed
//! item store, snapshots.
//!
//! Module split (the local contract lives in `AGENTS.md` beside this file):
//!
//! ```text
//! ledger/
//!   paths.rs        StatePaths, RuntimePaths, XDG resolution
//!   atomic.rs       temp+rename, length-framed append
//!   lock.rs         workspace advisory lock
//!   event_log.rs    framed append log
//!   feed_store.rs   atomic feed item I/O + status CAS
//!   writer.rs       write choreography: lock → write → append → wake → publish
//!   snapshot/       reduced snapshot rebuild
//!     fold.rs       resumable event-log rollup + carryover
//!     project.rs    agent-lifecycle reducer
//!     panes.rs      pane binding + own/daemon view
//!     process.rs    non-agent process rows + command classification
//!     view.rs       sidebar view-model assembly
//!     assemble.rs   read entry points + fresh-latest fast path
//! ```
//!
//! [`Ledger`] is a `Clone`able handle around `Arc<LedgerInner>`. This file is
//! the handle: types, constructor, accessors, and the lock-free read methods.
//! Every mutator lives in `writer.rs` and takes the workspace lock for its
//! critical section — there is no in-process actor. Cross-process
//! serialization is the workspace lock's job; every writer is a short-lived
//! CLI process serialized through `workspace.lock` (flock).

pub mod agent_context;
pub mod atomic;
pub mod event_log;
pub mod feed_store;
pub mod gc;
pub mod lock;
pub(crate) mod parse_cache;
pub mod paths;
pub mod runtime;
pub mod single_flight;
pub mod snapshot;
pub mod subagent_context;
pub mod wakeup;
pub mod workspace_record;

mod writer;

use std::path::PathBuf;
use std::sync::Arc;

use crate::feed::{AbandonReason, FeedItem, FeedStatus, Surface};
use crate::ids::{RequestId, ResolverId, WorkspaceId};
use crate::schema::event::EventEnvelope;

pub use crate::ledger::feed_store::FeedStoreErr;
pub use crate::ledger::paths::{RuntimePaths, StatePaths};
pub use crate::ledger::runtime::{RuntimeProjection, RuntimeScope};
pub use crate::ledger::snapshot::{
    SidebarOwnView, SidebarProviderPanel, SidebarResolverState, SidebarRow, SidebarRowKind,
    SidebarSnapshot, SidebarStatusCount, SidebarSubAgent, SidebarWorktreeGroup,
    SidebarWorktreeKind,
};
pub use crate::ledger::workspace_record::WorkspaceRecord;

/// Why a session's pending agent-hook asks are being expired. The variant
/// both scopes which surfaces are eligible and supplies the audit reason, so
/// the two can never drift apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AskExpiry {
    /// The session ended outright; expire every surface it left pending.
    SessionEnded,
    /// A live session moved on; expire only its native_ui asks. Bridge asks
    /// resolve via their own socket and must stay live.
    MovedOn,
}

impl AskExpiry {
    fn reason(self) -> AbandonReason {
        match self {
            Self::SessionEnded => AbandonReason::AgentSessionEnded,
            Self::MovedOn => AbandonReason::AgentMovedOn,
        }
    }

    fn includes(self, surface: Surface) -> bool {
        match self {
            Self::SessionEnded => true,
            Self::MovedOn => surface == Surface::NativeUi,
        }
    }
}

/// High-level handle to a workspace's durable state. Cheap to clone — the
/// inner state lives behind an `Arc`. Reads here are lock-free; every
/// mutator (in `writer.rs`) takes the workspace lock for its critical
/// section and writes through the `event_log`, `feed_store`, and `snapshot`
/// modules directly.
#[derive(Clone, Debug)]
pub struct Ledger {
    inner: Arc<LedgerInner>,
}

#[derive(Debug)]
struct LedgerInner {
    paths: StatePaths,
    runtime: RuntimePaths,
}

#[derive(Debug, thiserror::Error)]
pub enum LedgerErr {
    #[error(transparent)]
    Path(#[from] paths::PathErr),
    #[error(transparent)]
    EventLog(#[from] event_log::EventLogErr),
    #[error(transparent)]
    FeedStore(#[from] feed_store::FeedStoreErr),
    #[error(transparent)]
    Lock(#[from] lock::LockErr),
    #[error(transparent)]
    Snapshot(#[from] snapshot::SnapshotErr),
    #[error(transparent)]
    Wakeup(#[from] wakeup::WakeupErr),
    #[error(transparent)]
    WorkspaceRecord(#[from] workspace_record::WorkspaceRecordErr),
}

pub type Result<T> = std::result::Result<T, LedgerErr>;

#[derive(Clone, Debug)]
pub struct ResolveOutcome {
    pub request_id: RequestId,
    pub effective: bool,
    pub late: bool,
}

#[derive(Clone, Debug)]
pub struct TimeoutOutcome {
    pub request_id: RequestId,
    pub status: FeedStatus,
    pub transitioned: bool,
}

#[derive(Clone, Debug)]
pub struct AbstainOutcome {
    pub request_id: RequestId,
    pub next_resolver: Option<ResolverId>,
}

#[derive(Clone, Debug)]
pub struct ElapseOutcome {
    pub request_id: RequestId,
    pub next_resolver: Option<ResolverId>,
}

#[derive(Clone, Debug)]
pub struct WorkspaceRewriteOutcome {
    pub workspace_id: WorkspaceId,
    pub feed_items_rewritten: usize,
    pub events_rewritten: usize,
}

#[derive(Clone, Debug)]
pub struct EventLogRotationOutcome {
    pub rotation: event_log::RotationOutcome,
    pub pruned: event_log::PruneOutcome,
    pub carryover_agents: usize,
}

impl Ledger {
    pub fn open(paths: StatePaths, runtime: RuntimePaths) -> Result<Self> {
        paths.ensure_dirs()?;
        runtime.ensure_dirs()?;
        Ok(Self {
            inner: Arc::new(LedgerInner { paths, runtime }),
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

    pub fn load_feed_item(&self, request_id: &RequestId) -> Result<FeedItem> {
        Ok(feed_store::load(&self.inner.paths.feed_dir, request_id)?)
    }

    pub fn list_feed_items(&self) -> Result<Vec<FeedItem>> {
        Ok(feed_store::list(&self.inner.paths.feed_dir)?)
    }

    /// Project the live runtime state (feed items + event-log agent rollup)
    /// for the CLI read entry points (`cli::doctor`, `cli::feed list`,
    /// resume planning).
    ///
    /// Lock-free. The agent rollup resumes from the persisted fold base, so
    /// an in-flight tail frame a reader races is simply not folded yet — it
    /// can never drop a previously-folded event, and the write that
    /// completes the frame posts the wakeup that folds it.
    pub fn runtime_projection(
        &self,
        scope: runtime::RuntimeScope,
    ) -> Result<runtime::RuntimeProjection> {
        let items = feed_store::list(&self.inner.paths.feed_dir)?;
        let events = event_log::read_all(&self.inner.paths.events_log)?;
        let (_, agents) = snapshot::catch_up_rollup(&self.inner.paths)?;
        Ok(runtime::RuntimeProjection::from_parts(
            items, events, agents, scope,
        ))
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
}
