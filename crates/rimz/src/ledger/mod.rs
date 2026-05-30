//! Durable workspace state — ledger paths, atomic helpers, event log, feed
//! item store, snapshots.
//!
//! Module split mirrors `ARCHITECTURE.md`:
//!
//! ```text
//! ledger/
//!   paths.rs        StatePaths, RuntimePaths, XDG resolution
//!   atomic.rs       temp+rename, length-framed append
//!   lock.rs         workspace advisory lock
//!   event_log.rs    framed append log
//!   feed_store.rs   atomic feed item I/O + status CAS
//!   snapshot.rs     reduced snapshot rebuild
//! ```
//!
//! [`Ledger`] is a `Clone`able handle around `Arc<LedgerInner>`. Public
//! methods take the workspace lock for their critical section and do the
//! file work directly — there is no in-process actor. Cross-process
//! serialization is the workspace lock's job; every writer is a short-lived
//! CLI process serialized through `workspace.lock` (flock).

pub mod agent_context;
pub mod atomic;
pub mod event_log;
pub mod feed_store;
pub mod gc;
pub mod lock;
pub mod paths;
pub mod runtime;
pub mod single_flight;
pub mod snapshot;
pub mod wakeup;
pub mod workspace_record;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;
use serde_json::json;
use tracing::warn;

use crate::feed::{
    AbandonReason, FeedItem, FeedStatus, Resolution, ResolutionMethod, ResolverStepState, Surface,
};
use crate::ids::{RequestId, ResolverId, WorkspaceId};
use crate::schema::event::EventEnvelope;
use crate::workspace::ResolvedWorkspace;

pub use crate::ledger::feed_store::FeedStoreErr;
pub use crate::ledger::paths::{RuntimePaths, StatePaths};
pub use crate::ledger::runtime::{RuntimeProjection, RuntimeScope};
pub use crate::ledger::snapshot::{
    SidebarActivity, SidebarOwnView, SidebarResolverState, SidebarRow, SidebarRowKind,
    SidebarSnapshot, SidebarStatusCount, SidebarSubAgent, SidebarWorktreeGroup, SidebarWorktreeKind,
};
pub use crate::ledger::workspace_record::WorkspaceRecord;

/// Why a session's pending agent-hook asks are being expired. The variant
/// both scopes which surfaces are eligible and supplies the audit reason, so
/// the two can never drift apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AskExpiry {
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
/// inner state lives behind an `Arc`. Every public method takes the
/// workspace lock for its critical section and writes through the
/// `event_log`, `feed_store`, and `snapshot` modules directly.
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

fn abandon_dead_owned_items_locked(
    paths: &StatePaths,
    session_name: &str,
) -> Result<Vec<(WorkspaceId, RequestId)>> {
    let mut abandoned = Vec::new();
    for mut item in feed_store::list(&paths.feed_dir)? {
        if item.status != FeedStatus::Pending {
            continue;
        }
        let Some(owner) = item.runtime_owner.clone() else {
            continue;
        };
        if runtime::owner_is_live(&owner) {
            continue;
        }

        item.mark_active_resolver_budget_elapsed(AbandonReason::OwnerProcessExited);
        let mut resolution =
            Resolution::new(json!({ "abandoned": true }), ResolutionMethod::OwnerExited);
        resolution.reason = Some(AbandonReason::OwnerProcessExited.as_str().to_owned());
        item.status = FeedStatus::Abandoned;
        item.resolution = Some(resolution);
        item.updated_at = Timestamp::now();
        feed_store::write(&paths.feed_dir, &item)?;
        event_log::append(
            &paths.events_log,
            &EventEnvelope::new(
                item.workspace_id.clone(),
                session_name,
                "rimz",
                "cli",
                "feed.abandon",
                json!({
                    "request_id": item.request_id,
                    "surface": item.surface,
                    "reason": AbandonReason::OwnerProcessExited.as_str(),
                    "owner": owner,
                }),
            ),
        )?;
        abandoned.push((item.workspace_id.clone(), item.request_id.clone()));
    }
    Ok(abandoned)
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

    /// Persist the project-root index used by maintenance commands. This does
    /// not change feed state and does not wake sidebars.
    #[must_use = "durability barrier; check the result"]
    pub fn record_workspace(&self, workspace: &ResolvedWorkspace) -> Result<()> {
        let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
        let record = workspace_record::WorkspaceRecord::from_resolved(workspace);
        workspace_record::write(&self.inner.paths, &record)?;
        Ok(())
    }

    /// Rewrite durable workspace identity after a project root move.
    ///
    /// The caller has already moved the state directory to the new
    /// `<workspace_id>` path. This method updates feed files, event envelopes,
    /// the workspace metadata record, and the rebuilt snapshot under one
    /// workspace lock.
    #[must_use = "durability barrier; check the result"]
    pub fn rewrite_workspace_identity(
        &self,
        workspace: &ResolvedWorkspace,
    ) -> Result<WorkspaceRewriteOutcome> {
        let (feed_items_rewritten, events_rewritten) = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;

            let mut items = feed_store::list(&self.inner.paths.feed_dir)?;
            let feed_items_rewritten = items.len();
            for item in &mut items {
                item.workspace_id = workspace.workspace_id.clone();
                feed_store::write(&self.inner.paths.feed_dir, item)?;
            }

            let mut events = event_log::read_all(&self.inner.paths.events_log)?;
            let events_rewritten = events.len();
            for event in &mut events {
                event.workspace_id = workspace.workspace_id.clone();
            }
            event_log::replace_all(&self.inner.paths.events_log, &events)?;

            let record = workspace_record::WorkspaceRecord::from_resolved(workspace);
            workspace_record::write(&self.inner.paths, &record)?;
            snapshot::rebuild(&self.inner.paths)?;

            (feed_items_rewritten, events_rewritten)
        };

        Ok(WorkspaceRewriteOutcome {
            workspace_id: workspace.workspace_id.clone(),
            feed_items_rewritten,
            events_rewritten,
        })
    }

    /// Append a freestanding event (no feed item write).
    #[must_use = "durability barrier; check the result"]
    pub fn append_event(&self, event: &EventEnvelope) -> Result<()> {
        let abandoned = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            let abandoned =
                abandon_dead_owned_items_locked(&self.inner.paths, &event.session_name)?;
            event_log::append(&self.inner.paths.events_log, event)?;
            snapshot::rebuild(&self.inner.paths)?;
            abandoned
        };
        for (workspace_id, request_id) in &abandoned {
            self.wake_sidebars_best_effort(workspace_id, request_id);
        }
        self.wake_sidebars_for_event_best_effort(event);
        Ok(())
    }

    /// Write a new feed item to disk, append a `feed.push` event, and rebuild
    /// the latest snapshot. The whole sequence is taken under the workspace
    /// lock so partial writes can't surface to the sidebar.
    #[must_use = "durability barrier; check the result"]
    pub fn push_feed_item(&self, item: &FeedItem, session_name: &str) -> Result<()> {
        let abandoned = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            let abandoned = abandon_dead_owned_items_locked(&self.inner.paths, session_name)?;
            feed_store::write(&self.inner.paths.feed_dir, item)?;
            event_log::append(
                &self.inner.paths.events_log,
                &EventEnvelope::feed_pushed(item, session_name),
            )?;
            snapshot::rebuild(&self.inner.paths)?;
            abandoned
        };
        for (workspace_id, request_id) in &abandoned {
            self.wake_sidebars_best_effort(workspace_id, request_id);
        }
        self.wake_sidebars_best_effort(&item.workspace_id, &item.request_id);
        Ok(())
    }

    pub fn load_feed_item(&self, request_id: &RequestId) -> Result<FeedItem> {
        Ok(feed_store::load(&self.inner.paths.feed_dir, request_id)?)
    }

    pub fn list_feed_items(&self) -> Result<Vec<FeedItem>> {
        Ok(feed_store::list(&self.inner.paths.feed_dir)?)
    }

    #[must_use = "durability barrier; check the result"]
    pub fn abandon_dead_owned_items(&self, session_name: &str) -> Result<usize> {
        let abandoned = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            let abandoned = abandon_dead_owned_items_locked(&self.inner.paths, session_name)?;
            if !abandoned.is_empty() {
                snapshot::rebuild(&self.inner.paths)?;
            }
            abandoned
        };
        for (workspace_id, request_id) in &abandoned {
            self.wake_sidebars_best_effort(workspace_id, request_id);
        }
        Ok(abandoned.len())
    }

    /// Project the live runtime state (feed items + event-log agent rollup).
    ///
    /// Reads under the workspace lock. Every writer appends a framed event-log
    /// record while holding this lock, and a record is one `write_all` of
    /// `len ' ' json '\n'`. A lock-free reader could observe the length prefix
    /// before the body and newline land, so `event_log::read_all` would treat
    /// the in-flight record as a torn trailing record and silently skip it —
    /// momentarily dropping an agent's only/latest lifecycle event from the
    /// rollup, which un-links its live pane and flashes it as a bare `process`
    /// row until the next read. Holding the lock serializes against writers, so
    /// the reader only ever sees committed records and the torn-trailing skip
    /// fires solely for genuine crash corpses (a SIGKILLed writer's flock
    /// auto-releases). Callers are top-level read entry points (`snapshot`,
    /// `cli::doctor`, `cli::feed list`) that hold no lock, so this never
    /// re-enters the non-reentrant flock.
    pub fn runtime_projection(
        &self,
        scope: runtime::RuntimeScope,
    ) -> Result<runtime::RuntimeProjection> {
        let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
        let items = feed_store::list(&self.inner.paths.feed_dir)?;
        let events = event_log::read_all(&self.inner.paths.events_log)?;
        let carryover = snapshot::read_carryover(&self.inner.paths.agents_carryover)?;
        let agents = snapshot::agent_rollup_with_carryover(&events, carryover.agents);
        Ok(runtime::RuntimeProjection::from_parts(
            items, events, agents, scope,
        ))
    }

    /// Apply a resolver decision. CAS on `status = Pending`. Late answers
    /// (status = `TimedOut`) are accepted but recorded `effective: false`
    /// per the docs.
    #[must_use = "durability barrier; check the result"]
    pub fn resolve_feed_item(
        &self,
        request_id: &RequestId,
        mut resolution: Resolution,
        override_chain: bool,
        session_name: &str,
    ) -> Result<ResolveOutcome> {
        let item_to_wake = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            let mut item = feed_store::load(&self.inner.paths.feed_dir, request_id)?;

            if !item.surface.supports_resolve() {
                return Err(FeedStoreErr::SurfaceMismatch {
                    request_id: request_id.clone(),
                    surface: item.surface,
                    verb: "resolve",
                }
                .into());
            }

            if !override_chain && let Some(active) = item.chain_active_resolver.as_ref() {
                let provided = resolution.resolver_id.as_ref();
                if provided != Some(active) {
                    return Err(FeedStoreErr::ResolverNotActive {
                        request_id: request_id.clone(),
                        resolver: provided
                            .cloned()
                            .unwrap_or_else(|| ResolverId::new_unchecked("missing")),
                    }
                    .into());
                }
            }

            let (effective, late) = match item.status {
                FeedStatus::Pending => (true, false),
                FeedStatus::TimedOut => (false, true),
                other => {
                    return Err(FeedStoreErr::NotPending {
                        request_id: request_id.clone(),
                        status: other,
                    }
                    .into());
                }
            };

            resolution.effective = effective;
            resolution.late = late;
            resolution.override_chain = override_chain;
            if late && resolution.reason.is_none() {
                resolution.reason = Some(
                    AbandonReason::HookAlreadyReturnedNeutral
                        .as_str()
                        .to_owned(),
                );
            }

            let item_to_wake = if effective {
                let responder = resolution.resolver_id.clone();
                item.status = FeedStatus::Resolved;
                item.mark_resolver_answered(responder.as_ref());
                item.resolution = Some(resolution.clone());
                item.updated_at = Timestamp::now();
                feed_store::write(&self.inner.paths.feed_dir, &item)?;
                snapshot::rebuild(&self.inner.paths)?;
                Some(item.clone())
            } else {
                warn!(
                    request_id = %request_id,
                    "late resolver answer recorded as audit-only (item already timed out)"
                );
                None
            };

            event_log::append(
                &self.inner.paths.events_log,
                &EventEnvelope::new(
                    item.workspace_id.clone(),
                    session_name,
                    "rimz",
                    "cli",
                    "feed.resolve",
                    json!({
                        "request_id": request_id,
                        "effective": effective,
                        "late": late,
                        "method": resolution.method,
                        "resolver_id": resolution.resolver_id.clone(),
                        "reason": resolution.reason.clone(),
                    }),
                ),
            )?;

            item_to_wake
        };

        if let Some(item) = &item_to_wake {
            self.wake_per_request_best_effort(item);
            self.wake_sidebars_best_effort(&item.workspace_id, &item.request_id);
        }

        Ok(ResolveOutcome {
            request_id: request_id.clone(),
            effective: item_to_wake.is_some(),
            late: item_to_wake.is_none(),
        })
    }

    #[must_use = "durability barrier; check the result"]
    pub fn mark_feed_item_timed_out(
        &self,
        request_id: &RequestId,
        session_name: &str,
        reason: AbandonReason,
    ) -> Result<TimeoutOutcome> {
        let wake_target = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            let mut item = feed_store::load(&self.inner.paths.feed_dir, request_id)?;

            if !item.surface.supports_resolve() {
                return Err(FeedStoreErr::SurfaceMismatch {
                    request_id: request_id.clone(),
                    surface: item.surface,
                    verb: "timeout",
                }
                .into());
            }

            if !item.status.allows_resolution() {
                return Ok(TimeoutOutcome {
                    request_id: request_id.clone(),
                    status: item.status,
                    transitioned: false,
                });
            }

            item.status = FeedStatus::TimedOut;
            item.mark_active_resolver_budget_elapsed(reason);
            item.updated_at = Timestamp::now();
            feed_store::write(&self.inner.paths.feed_dir, &item)?;
            snapshot::rebuild(&self.inner.paths)?;
            event_log::append(
                &self.inner.paths.events_log,
                &EventEnvelope::new(
                    item.workspace_id.clone(),
                    session_name,
                    "rimz",
                    "cli",
                    "feed.timeout",
                    json!({
                        "request_id": request_id,
                        "surface": item.surface,
                        "reason": reason.as_str(),
                    }),
                ),
            )?;
            Some((item.workspace_id.clone(), item.request_id.clone()))
        };

        if let Some((workspace_id, request_id)) = &wake_target {
            self.wake_sidebars_best_effort(workspace_id, request_id);
        }

        Ok(TimeoutOutcome {
            request_id: request_id.clone(),
            status: FeedStatus::TimedOut,
            transitioned: wake_target.is_some(),
        })
    }

    /// Explicit chain handoff. The active resolver calls this to pass on a
    /// request without answering. Records a `feed.abstain` audit event and
    /// marks the matching `ResolverStep` as `Abstained`; the next chain
    /// link's id is returned for the caller to log. The feed item remains
    /// `Pending` so the bridge can advance.
    ///
    /// CAS: rejects with `ResolverNotActive` unless `chain_active_resolver`
    /// equals `resolver_id` — abstaining when no resolver is active is a
    /// no-op the caller should treat as an error.
    #[must_use = "durability barrier; check the result"]
    pub fn abstain_feed_item(
        &self,
        request_id: &RequestId,
        resolver_id: &ResolverId,
        reason: Option<String>,
        session_name: &str,
    ) -> Result<AbstainOutcome> {
        let (outcome, wake_target) = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            let mut item = feed_store::load(&self.inner.paths.feed_dir, request_id)?;
            if !item.surface.supports_resolve() {
                return Err(FeedStoreErr::SurfaceMismatch {
                    request_id: request_id.clone(),
                    surface: item.surface,
                    verb: "abstain",
                }
                .into());
            }
            if !item.status.allows_resolution() {
                return Err(FeedStoreErr::NotPending {
                    request_id: request_id.clone(),
                    status: item.status,
                }
                .into());
            }
            if item.chain_active_resolver.as_ref() != Some(resolver_id) {
                return Err(FeedStoreErr::ResolverNotActive {
                    request_id: request_id.clone(),
                    resolver: resolver_id.clone(),
                }
                .into());
            }

            for step in item.chain.iter_mut() {
                if &step.resolver_id == resolver_id {
                    step.state = ResolverStepState::Abstained;
                    if let Some(reason) = reason.clone() {
                        step.reason = Some(reason);
                    }
                    break;
                }
            }

            let next = item.advance_resolver_chain_after(resolver_id);
            item.updated_at = Timestamp::now();
            feed_store::write(&self.inner.paths.feed_dir, &item)?;
            snapshot::rebuild(&self.inner.paths)?;
            event_log::append(
                &self.inner.paths.events_log,
                &EventEnvelope::new(
                    item.workspace_id.clone(),
                    session_name,
                    "rimz",
                    "cli",
                    "feed.abstain",
                    json!({
                        "request_id": request_id,
                        "resolver_id": resolver_id,
                        "reason": reason,
                        "next_resolver": next.clone(),
                    }),
                ),
            )?;
            let outcome = AbstainOutcome {
                request_id: request_id.clone(),
                next_resolver: next,
            };
            (
                outcome,
                (item.workspace_id.clone(), item.request_id.clone()),
            )
        };

        self.wake_sidebars_best_effort(&wake_target.0, &wake_target.1);
        Ok(outcome)
    }

    /// Involuntary chain handoff. Called by the hook bridge when the active
    /// resolver's per-step budget elapses or its heartbeat goes stale before
    /// it answered. Records `feed.chain_elapse` with `reason ∈
    /// {`BudgetElapsed`, `HeartbeatStale`}` and advances to the next queued
    /// step (or leaves the chain empty so the caller can fall back to
    /// neutral).
    ///
    /// CAS: rejects with `ResolverNotActive` unless `chain_active_resolver`
    /// equals `current` — another writer (sibling pane abstain, late answer
    /// race) already moved the chain.
    #[must_use = "durability barrier; check the result"]
    pub fn elapse_chain_step(
        &self,
        request_id: &RequestId,
        current: &ResolverId,
        reason: AbandonReason,
        session_name: &str,
    ) -> Result<ElapseOutcome> {
        let (outcome, wake_target) = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            let mut item = feed_store::load(&self.inner.paths.feed_dir, request_id)?;
            if !item.surface.supports_resolve() {
                return Err(FeedStoreErr::SurfaceMismatch {
                    request_id: request_id.clone(),
                    surface: item.surface,
                    verb: "chain_elapse",
                }
                .into());
            }
            if !item.status.allows_resolution() {
                return Err(FeedStoreErr::NotPending {
                    request_id: request_id.clone(),
                    status: item.status,
                }
                .into());
            }
            if item.chain_active_resolver.as_ref() != Some(current) {
                return Err(FeedStoreErr::ResolverNotActive {
                    request_id: request_id.clone(),
                    resolver: current.clone(),
                }
                .into());
            }

            item.mark_active_resolver_budget_elapsed(reason);
            let next = item.advance_resolver_chain_after(current);
            item.updated_at = Timestamp::now();
            feed_store::write(&self.inner.paths.feed_dir, &item)?;
            snapshot::rebuild(&self.inner.paths)?;
            event_log::append(
                &self.inner.paths.events_log,
                &EventEnvelope::new(
                    item.workspace_id.clone(),
                    session_name,
                    "rimz",
                    "cli",
                    "feed.chain_elapse",
                    json!({
                        "request_id": request_id,
                        "resolver_id": current,
                        "reason": reason.as_str(),
                        "next_resolver": next.clone(),
                    }),
                ),
            )?;
            let outcome = ElapseOutcome {
                request_id: request_id.clone(),
                next_resolver: next,
            };
            (
                outcome,
                (item.workspace_id.clone(), item.request_id.clone()),
            )
        };

        self.wake_sidebars_best_effort(&wake_target.0, &wake_target.1);
        Ok(outcome)
    }

    /// Mark a `native_ui` feed item as acknowledged locally. Never reaches
    /// the agent — that's the docs' contract.
    #[must_use = "durability barrier; check the result"]
    pub fn dismiss_feed_item(
        &self,
        request_id: &RequestId,
        reason: Option<String>,
        session_name: &str,
    ) -> Result<()> {
        let wake_target = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            let mut item = feed_store::load(&self.inner.paths.feed_dir, request_id)?;
            if !item.surface.supports_dismiss() {
                return Err(FeedStoreErr::SurfaceMismatch {
                    request_id: request_id.clone(),
                    surface: item.surface,
                    verb: "dismiss",
                }
                .into());
            }
            if !item.status.allows_resolution() {
                return Ok(());
            }
            let mut resolution =
                Resolution::new(json!({ "dismissed": true }), ResolutionMethod::Dismiss);
            resolution.reason = reason.clone();
            item.status = FeedStatus::Resolved;
            item.resolution = Some(resolution);
            item.updated_at = Timestamp::now();
            feed_store::write(&self.inner.paths.feed_dir, &item)?;
            snapshot::rebuild(&self.inner.paths)?;
            event_log::append(
                &self.inner.paths.events_log,
                &EventEnvelope::new(
                    item.workspace_id.clone(),
                    session_name,
                    "rimz",
                    "cli",
                    "feed.dismiss",
                    json!({
                        "request_id": request_id,
                        "reason": reason,
                    }),
                ),
            )?;
            Some((item.workspace_id.clone(), item.request_id.clone()))
        };

        if let Some((workspace_id, request_id)) = &wake_target {
            self.wake_sidebars_best_effort(workspace_id, request_id);
        }
        Ok(())
    }

    /// Expire every pending agent-hook ask raised by a session that has *ended*.
    /// A dead session can't answer its own prompt on any surface, so all of its
    /// pending asks — native_ui and bridge alike — are expired. See
    /// [`Self::expire_agent_asks`] for the shared mechanics.
    #[must_use = "durability barrier; check the result"]
    pub fn expire_agent_session(
        &self,
        source: &str,
        agent_id: &str,
        session_name: &str,
    ) -> Result<usize> {
        self.expire_agent_asks(source, agent_id, session_name, AskExpiry::SessionEnded)
    }

    /// Expire a *live* session's pending native_ui asks because it moved on
    /// (a new prompt, the end of its turn, or a fresh ask superseding the old).
    /// Scoped to native_ui: the agent answers those in its own UI and never
    /// reports back, so they would otherwise pile up as duplicate attention.
    /// Bridge asks resolve through their own socket and stay untouched.
    #[must_use = "durability barrier; check the result"]
    pub fn expire_agent_native_ui_asks(
        &self,
        source: &str,
        agent_id: &str,
        session_name: &str,
    ) -> Result<usize> {
        self.expire_agent_asks(source, agent_id, session_name, AskExpiry::MovedOn)
    }

    /// Move a session's matching pending agent-hook asks to `Abandoned` with an
    /// `AgentMovedOn` resolution and a `feed.expire` audit event, then rebuild
    /// the snapshot and wake sidebars. `expiry` scopes which surfaces are
    /// eligible and supplies the audit reason. Closing the loop here is
    /// deterministic; the snapshot's read-side guard self-heals anything that
    /// races this write. Returns the number of items expired.
    #[must_use = "durability barrier; check the result"]
    fn expire_agent_asks(
        &self,
        source: &str,
        agent_id: &str,
        session_name: &str,
        expiry: AskExpiry,
    ) -> Result<usize> {
        let reason = expiry.reason();
        let mut expired = Vec::new();
        {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            for mut item in feed_store::list(&self.inner.paths.feed_dir)? {
                if item.source_kind != "agent-hook"
                    || item.source != source
                    || item.status != FeedStatus::Pending
                    || item.agent_session_id() != Some(agent_id)
                    || !expiry.includes(item.surface)
                {
                    continue;
                }
                item.mark_active_resolver_budget_elapsed(reason);
                let mut resolution =
                    Resolution::new(json!({ "expired": true }), ResolutionMethod::AgentMovedOn);
                resolution.reason = Some(reason.as_str().to_owned());
                item.status = FeedStatus::Abandoned;
                item.resolution = Some(resolution);
                item.updated_at = Timestamp::now();
                feed_store::write(&self.inner.paths.feed_dir, &item)?;
                event_log::append(
                    &self.inner.paths.events_log,
                    &EventEnvelope::new(
                        item.workspace_id.clone(),
                        session_name,
                        "rimz",
                        "cli",
                        "feed.expire",
                        json!({
                            "request_id": item.request_id,
                            "source": source,
                            "agent_id": agent_id,
                            "reason": reason.as_str(),
                        }),
                    ),
                )?;
                expired.push((item.workspace_id.clone(), item.request_id.clone()));
            }
            if !expired.is_empty() {
                snapshot::rebuild(&self.inner.paths)?;
            }
        }
        for (workspace_id, request_id) in &expired {
            self.wake_sidebars_best_effort(workspace_id, request_id);
        }
        Ok(expired.len())
    }

    /// Build a fresh snapshot in memory (no disk write).
    pub fn snapshot(&self) -> Result<SidebarSnapshot> {
        let projection = self.runtime_projection(runtime::RuntimeScope::Runtime)?;
        let mut snapshot = snapshot::SidebarSnapshot::build_with_agents(
            self.inner.paths.workspace_id.clone(),
            projection.items,
            projection.events,
            projection.agents,
        );
        snapshot.reap_stale_sessions(Timestamp::now());
        snapshot.display_name = snapshot::display_name_for(&self.inner.paths);
        let snapshot = snapshot.with_project_root(snapshot::project_root_for(&self.inner.paths));
        Ok(snapshot)
    }

    /// Like [`Self::snapshot`] but O(1) in the common case: serve the pre-built
    /// `latest.json` rollup when it already reflects every appended event,
    /// falling back to a full re-projection on a miss. The fast path is
    /// lock-free — it reads no event log and takes no workspace lock — so a hot
    /// fleet's snapshot fetches never contend with the agent hooks appending
    /// events. `latest.json` is rebuilt under the lock on every mutation and
    /// carries the same runtime liveness expel as [`Self::snapshot`], so the
    /// served rollup matches the live read; the staleness guard (its mtime vs
    /// the event log's) catches a fetch racing a just-appended event and
    /// re-projects instead.
    pub fn snapshot_cached(&self) -> Result<SidebarSnapshot> {
        if let Some(snapshot) = snapshot::read_fresh_latest(&self.inner.paths) {
            return Ok(snapshot);
        }
        self.snapshot()
    }

    /// Rotate the active event log when it exceeds `min_bytes`, preserving
    /// the agent rollup across the archive boundary.
    ///
    /// Steps under the workspace lock:
    /// 1. Project the current event log's agent rollup, merge it with the
    ///    existing carryover, and persist before the rename so a rotation
    ///    crash leaves both files coherent.
    /// 2. Rename the active log into `events.log.archive/`. UUIDv7 filenames
    ///    keep archives sorted chronologically without an external index.
    /// 3. Rebuild the snapshot so the sidebar's `recent_activity` no longer
    ///    references the rotated log.
    /// 4. Prune archives older than `archive_older_than` when set.
    #[must_use = "durability barrier; check the result"]
    pub fn rotate_event_log(
        &self,
        min_bytes: u64,
        archive_older_than: Option<Duration>,
    ) -> Result<EventLogRotationOutcome> {
        let outcome = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;

            let events = event_log::read_all(&self.inner.paths.events_log)?;
            let existing = snapshot::read_carryover(&self.inner.paths.agents_carryover)?;
            let merged = snapshot::agent_rollup_with_carryover(&events, existing.agents.clone());

            let rotation = event_log::rotate(
                &self.inner.paths.events_log,
                &self.inner.paths.events_archive_dir,
                min_bytes,
            )?;

            if rotation.is_rotated() {
                let carryover = snapshot::EventCarryover { agents: merged };
                snapshot::write_carryover(&self.inner.paths.agents_carryover, &carryover)?;
                snapshot::rebuild(&self.inner.paths)?;
            }

            let pruned = if let Some(older_than) = archive_older_than {
                event_log::prune_archive(&self.inner.paths.events_archive_dir, older_than)?
            } else {
                event_log::PruneOutcome::default()
            };

            let carryover_agents = match &rotation {
                event_log::RotationOutcome::Rotated { .. } => {
                    snapshot::read_carryover(&self.inner.paths.agents_carryover)?
                        .agents
                        .len()
                }
                event_log::RotationOutcome::Skipped { .. } => existing.agents.len(),
            };

            EventLogRotationOutcome {
                rotation,
                pruned,
                carryover_agents,
            }
        };
        Ok(outcome)
    }

    /// Walk the event log, returning every parseable record and logging
    /// torn records at `warn`.
    pub fn read_events(&self) -> Result<Vec<EventEnvelope>> {
        Ok(event_log::read_all(&self.inner.paths.events_log)?)
    }

    fn wake_per_request_best_effort(&self, item: &FeedItem) {
        if let Err(err) = wakeup::wake_per_request(&self.inner.runtime, item) {
            warn!(
                request_id = %item.request_id,
                error = %err,
                "per-request wakeup failed after ledger commit"
            );
        }
    }

    fn wake_sidebars_best_effort(&self, workspace_id: &WorkspaceId, request_id: &RequestId) {
        if let Err(err) = wakeup::wake_sidebars(&self.inner.runtime, workspace_id, request_id) {
            warn!(
                request_id = %request_id,
                error = %err,
                "sidebar wakeup failed after ledger commit"
            );
        }
    }

    fn wake_sidebars_for_event_best_effort(&self, event: &EventEnvelope) {
        if let Err(err) = wakeup::wake_sidebars_for_event(
            &self.inner.runtime,
            &event.workspace_id,
            &event.event_id,
        ) {
            warn!(
                event_id = %event.event_id,
                error = %err,
                "sidebar wakeup failed after ledger event commit"
            );
        }
    }
}
