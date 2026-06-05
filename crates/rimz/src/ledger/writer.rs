//! The ledger write path: every mutation's lock → feed-write → event-append
//! critical section, and the off-lock wakeup + publish tail that follows a
//! commit. The read side (snapshots, projections) stays in `mod.rs`; nothing
//! here is imported outside the ledger module.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use jiff::Timestamp;
use serde_json::json;
use tracing::warn;

use crate::feed::{
    AbandonReason, FeedItem, FeedStatus, Resolution, ResolutionMethod, ResolverStepState,
};
use crate::ids::{RequestId, ResolverId, WorkspaceId};
use crate::schema::event::EventEnvelope;
use crate::workspace::ResolvedWorkspace;

use super::feed_store::FeedStoreErr;
use super::{
    AbstainOutcome, AskExpiry, ElapseOutcome, EventLogRotationOutcome, Ledger, LedgerErr,
    ResolveOutcome, Result, StatePaths, TimeoutOutcome, WorkspaceRewriteOutcome, atomic, event_log,
    feed_store, lock, runtime, snapshot, wakeup, workspace_record,
};

/// How often the write path is willing to pay the dead-owner sweep. Read-side
/// expel hides a dead-owner item from runtime views the instant it dies, so
/// the sweep only owes the durable `abandoned` record within this window.
const ABANDON_SWEEP_INTERVAL: Duration = Duration::from_secs(2);

/// Stamp recording the last dead-owner sweep. Lives beside the workspace lock
/// so feed-dir scans (item lists, gc's history classification) never see it.
fn abandon_sweep_stamp(paths: &StatePaths) -> PathBuf {
    paths.locks_dir.join("abandon-sweep.stamp")
}

/// Age of a debounce stamp's mtime. `None` when the stamp is missing or
/// unreadable, or its mtime sits in the future (clock skew) — every gate
/// reads `None` as due, erring toward one redundant run, never a stale skip.
fn stamp_age(path: &std::path::Path) -> Option<Duration> {
    let modified = std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()?;
    SystemTime::now().duration_since(modified).ok()
}

/// Best-effort: a failed touch only means the next write runs the gated
/// task again.
fn touch_stamp(path: &std::path::Path) {
    let _ = std::fs::write(path, b"");
}

fn abandon_sweep_due(paths: &StatePaths) -> bool {
    stamp_age(&abandon_sweep_stamp(paths)).is_none_or(|age| age >= ABANDON_SWEEP_INTERVAL)
}

/// How long appended event-log bytes may ride the page cache before a write
/// tail forces them down. Bounds power-cut loss to about a second of
/// trailing events under sustained load — decisions included, not just
/// observational ones; a lost resolution is benign because the cut killed
/// its waiter too, so the resurrected ask is expelled and abandoned.
/// Per-record fsyncs were the write path's dominant latency, and the
/// product reconstructs attention state from live agents on restart, so a
/// short tail is recoverable noise.
const LOG_SYNC_INTERVAL: Duration = Duration::from_secs(1);

/// Stamp recording the last event-log group sync. Lives beside the workspace
/// lock with the other write-path debounce stamps.
fn log_sync_stamp(paths: &StatePaths) -> PathBuf {
    paths.locks_dir.join("log-sync.stamp")
}

fn log_sync_due(paths: &StatePaths) -> bool {
    stamp_age(&log_sync_stamp(paths)).is_none_or(|age| age >= LOG_SYNC_INTERVAL)
}

/// Group-commit the relaxed event-log appends at most once per
/// [`LOG_SYNC_INTERVAL`]. One fdatasync flushes the inode's dirty pages
/// regardless of which process wrote them, so a single writer per interval
/// makes the whole fleet's appends durable — headless loss stays bounded
/// without an elected syncer. A stamp race costs one redundant sync, so the
/// stamp goes unlocked. Runs off every lock: the sync is durability
/// housekeeping, never a commit or publish precondition.
fn sync_log_debounced(paths: &StatePaths) {
    if !log_sync_due(paths) {
        return;
    }
    match atomic::sync_file_data(&paths.events_log) {
        Ok(()) => touch_stamp(&log_sync_stamp(paths)),
        // A rotation can rename the log away between the append and this
        // tail; its pre-rename sync already made those bytes durable.
        Err(atomic::AtomicErr::Io { ref source, .. })
            if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => warn!(error = %err, "event-log group sync failed; the next write retries"),
    }
}

/// A publish failure caused by a corrupt event-log frame — the one failure
/// [`Ledger::repair_event_log`] heals, distinct from environment errors it
/// cannot.
fn publish_hit_corruption(err: &LedgerErr) -> bool {
    matches!(
        err,
        LedgerErr::Snapshot(snapshot::SnapshotErr::EventLog(log_err)) if log_err.is_corruption()
    )
}

/// Ceiling on how stale the published checkpoint may grow under sustained
/// writes. Consumers are woken per event and fold the log tail from their
/// own cursor, so the checkpoint is a cold-start accelerator, not the
/// freshness path; once per second bounds the writers' JSON work while
/// keeping a cold reader's catch-up fold short.
const PUBLISH_INTERVAL: Duration = Duration::from_secs(1);

/// Byte ceiling on the unpublished log tail: the gate forces an early
/// checkpoint once a cold reader would have to fold this much past the
/// stamp, whatever the stamp's age.
const PUBLISH_BYTE_BUDGET: u64 = 64 * 1024;

/// Stamp recording the last published checkpoint: the `LogExtent` the
/// publish reflected as content, the publish instant as mtime. Written with
/// a plain `fs::write` under the publish lock — a torn or unparseable stamp
/// reads as due, erring toward a redundant publish, never a stale skip.
fn publish_stamp(paths: &StatePaths) -> PathBuf {
    paths.locks_dir.join("publish.stamp")
}

fn write_publish_stamp(paths: &StatePaths, extent: Option<event_log::LogExtent>) {
    let Some(extent) = extent else {
        return;
    };
    if let Ok(bytes) = serde_json::to_vec(&extent) {
        let _ = std::fs::write(publish_stamp(paths), bytes);
    }
}

/// The cheap pre-lock cadence gate: one open of the ~40-byte stamp decides
/// whether this tail pays the checkpoint. The stamp gates cadence alone —
/// freshness truth stays in `latest.json`'s own extent stamp, which readers
/// verify against the live log — so the worst a wrong verdict costs is one
/// redundant publish or one bounded catch-up fold.
fn publish_due(paths: &StatePaths) -> bool {
    use std::io::Read;
    // Missing or unreadable stamp: never published (or retracted) — due.
    let Ok(mut stamp) = std::fs::File::open(publish_stamp(paths)) else {
        return true;
    };
    let Some(age) = stamp
        .metadata()
        .and_then(|meta| meta.modified())
        .ok()
        // Stamp mtime in the future (clock skew): treat the publish as due.
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
    else {
        return true;
    };
    if age >= PUBLISH_INTERVAL {
        // The interval alone decides the common due verdict — skip the
        // stamp-body read and the log stat.
        return true;
    }
    let mut bytes = Vec::with_capacity(64);
    let Some(extent) = stamp
        .read_to_end(&mut bytes)
        .ok()
        .and_then(|_| serde_json::from_slice::<event_log::LogExtent>(&bytes).ok())
    else {
        return true;
    };
    let log_len = std::fs::metadata(&paths.events_log)
        .map(|meta| meta.len())
        .unwrap_or(0);
    should_publish(age, extent.offset, log_len)
}

/// Pure gate core over the stamp's age and offset beside the live log
/// length. Due when the interval elapsed, the log shrank (a rotation or
/// rewrite swapped the file), or the unpublished tail crossed the byte
/// budget.
fn should_publish(age: Duration, stamp_offset: u64, log_len: u64) -> bool {
    age >= PUBLISH_INTERVAL
        || log_len < stamp_offset
        || log_len - stamp_offset >= PUBLISH_BYTE_BUDGET
}

/// Drop the publish stamp when the log it describes was swapped or cut —
/// rotation, identity rewrite, repair — so the next mutation's gate reads
/// "never published" instead of comparing offsets across two different
/// files. Best-effort: a surviving stamp risks at most one deferred
/// checkpoint (≤ the interval), never a stale read, because freshness truth
/// stays in `latest.json`'s own extent stamp.
fn retract_publish_stamp(paths: &StatePaths) {
    let _ = std::fs::remove_file(publish_stamp(paths));
}

/// Run the dead-owner sweep at most once per [`ABANDON_SWEEP_INTERVAL`].
/// Caller holds the workspace lock. The common case is one stamp stat —
/// the write path itself stays O(1) regardless of feed history.
fn sweep_dead_owned_items_debounced(
    paths: &StatePaths,
    session_name: &str,
) -> Result<Vec<(WorkspaceId, RequestId)>> {
    if !abandon_sweep_due(paths) {
        return Ok(Vec::new());
    }
    let abandoned = abandon_dead_owned_items_locked(paths, session_name)?;
    touch_stamp(&abandon_sweep_stamp(paths));
    Ok(abandoned)
}

/// Move a session's matching pending agent-hook asks to `Abandoned` with an
/// `AgentMovedOn`/`AgentSessionEnded` resolution and a `feed.expire` audit
/// event. Caller holds the workspace lock and owns the snapshot rebuild and
/// the wakeups for the returned targets.
fn expire_agent_asks_locked(
    paths: &StatePaths,
    source: &str,
    agent_id: &str,
    session_name: &str,
    expiry: AskExpiry,
) -> Result<Vec<(WorkspaceId, RequestId)>> {
    let reason = expiry.reason();
    let mut expired = Vec::new();
    for mut item in feed_store::list_pending(&paths.feed_dir)? {
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
        feed_store::write(&paths.feed_dir, &item)?;
        event_log::append(
            &paths.events_log,
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
    Ok(expired)
}

fn abandon_dead_owned_items_locked(
    paths: &StatePaths,
    session_name: &str,
) -> Result<Vec<(WorkspaceId, RequestId)>> {
    let mut abandoned = Vec::new();
    for mut item in feed_store::list_pending(&paths.feed_dir)? {
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
            // Also fence the snapshot publishers: this rewrite replaces the
            // caches in place, and a publisher mid-fold must not clobber
            // them. Ordering is workspace → publish; publishers take only
            // the publish lock, so the pair can never deadlock.
            let _publish_guard = lock::WorkspaceLock::acquire(&self.inner.paths.publish_lock)?;

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
            // The log was wholesale-replaced: every byte offset in the fold
            // base is void. Reseed it as a new generation before rebuilding.
            retract_publish_stamp(&self.inner.paths);
            snapshot::reseed_rollup_cache_for_rotation(&self.inner.paths)?;
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
        self.append_event_and_expire(event, None).map(|_| ())
    }

    /// Append a lifecycle event and expire the session's superseded pending
    /// asks under one lock cycle with one snapshot rebuild — the
    /// highest-cadence hook path (a turn boundary from every live agent)
    /// pays one flock acquire instead of two. `expiry` carries
    /// `(source, agent_id, scope)`; `None` is a plain append. Returns the
    /// number of asks expired.
    #[must_use = "durability barrier; check the result"]
    pub fn append_event_and_expire(
        &self,
        event: &EventEnvelope,
        expiry: Option<(&str, &str, AskExpiry)>,
    ) -> Result<usize> {
        let (abandoned, expired) = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            let abandoned =
                sweep_dead_owned_items_debounced(&self.inner.paths, &event.session_name)?;
            event_log::append(&self.inner.paths.events_log, event)?;
            let expired = match expiry {
                Some((source, agent_id, scope)) => expire_agent_asks_locked(
                    &self.inner.paths,
                    source,
                    agent_id,
                    &event.session_name,
                    scope,
                )?,
                None => Vec::new(),
            };
            (abandoned, expired)
        };
        for (workspace_id, request_id) in abandoned.iter().chain(expired.iter()) {
            self.wake_sidebars_best_effort(workspace_id, request_id);
        }
        self.wake_sidebars_for_event_best_effort(event);
        self.publish_snapshot_best_effort();
        Ok(expired.len())
    }

    /// Write a new feed item to disk, append a `feed.push` event, and rebuild
    /// the latest snapshot. The whole sequence is taken under the workspace
    /// lock so partial writes can't surface to the sidebar.
    #[must_use = "durability barrier; check the result"]
    pub fn push_feed_item(&self, item: &FeedItem, session_name: &str) -> Result<()> {
        self.push_feed_item_superseding(item, None, session_name)
    }

    /// [`Self::push_feed_item`] that also expires the session's prior
    /// native_ui asks in the same critical section — a fresh ask supersedes
    /// them before being pushed, under one lock cycle with one snapshot
    /// rebuild. `supersede` carries `(source, agent_id)`; `None` is a plain
    /// push.
    #[must_use = "durability barrier; check the result"]
    pub fn push_feed_item_superseding(
        &self,
        item: &FeedItem,
        supersede: Option<(&str, &str)>,
        session_name: &str,
    ) -> Result<()> {
        let (abandoned, expired) = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            let abandoned = sweep_dead_owned_items_debounced(&self.inner.paths, session_name)?;
            let expired = match supersede {
                Some((source, agent_id)) => expire_agent_asks_locked(
                    &self.inner.paths,
                    source,
                    agent_id,
                    session_name,
                    AskExpiry::MovedOn,
                )?,
                None => Vec::new(),
            };
            feed_store::write(&self.inner.paths.feed_dir, item)?;
            event_log::append(
                &self.inner.paths.events_log,
                &EventEnvelope::feed_pushed(item, session_name),
            )?;
            (abandoned, expired)
        };
        for (workspace_id, request_id) in abandoned.iter().chain(expired.iter()) {
            self.wake_sidebars_best_effort(workspace_id, request_id);
        }
        self.wake_sidebars_best_effort(&item.workspace_id, &item.request_id);
        self.publish_snapshot_best_effort();
        Ok(())
    }

    #[must_use = "durability barrier; check the result"]
    pub fn abandon_dead_owned_items(&self, session_name: &str) -> Result<usize> {
        let abandoned = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            let abandoned = abandon_dead_owned_items_locked(&self.inner.paths, session_name)?;
            touch_stamp(&abandon_sweep_stamp(&self.inner.paths));
            abandoned
        };
        for (workspace_id, request_id) in &abandoned {
            self.wake_sidebars_best_effort(workspace_id, request_id);
        }
        if !abandoned.is_empty() {
            // Forced: gc reports from the checkpoint right after the sweep.
            self.publish_snapshot_forced();
        }
        Ok(abandoned.len())
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
        self.publish_snapshot_best_effort();

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
        self.publish_snapshot_best_effort();

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
        self.publish_snapshot_best_effort();
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
        self.publish_snapshot_best_effort();
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
        self.publish_snapshot_best_effort();
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

    /// Standalone wrapper over [`expire_agent_asks_locked`]: one lock cycle,
    /// a snapshot rebuild when anything expired, then wakeups. The hook flows
    /// fold the same scan into their own critical sections via
    /// [`Self::append_event_and_expire`] and
    /// [`Self::push_feed_item_superseding`]. Closing the loop here is
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
        let expired = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            expire_agent_asks_locked(&self.inner.paths, source, agent_id, session_name, expiry)?
        };
        for (workspace_id, request_id) in &expired {
            self.wake_sidebars_best_effort(workspace_id, request_id);
        }
        if !expired.is_empty() {
            // Forced: the standalone expiry's caller reads the checkpoint
            // right after its verdict.
            self.publish_snapshot_forced();
        }
        Ok(expired.len())
    }

    /// Rotate the active event log when it exceeds `min_bytes`, preserving
    /// the agent rollup across the archive boundary.
    ///
    /// Steps under the workspace and publish locks:
    /// 1. Project the current event log's agent rollup, merge it with the
    ///    existing carryover, and persist before the rename so a rotation
    ///    crash leaves both files coherent.
    /// 2. Rename the active log into `events.log.archive/`. UUIDv7 filenames
    ///    keep archives sorted chronologically without an external index.
    /// 3. Retract the published `latest.json` — its extent stamp describes
    ///    the renamed-away log, so a crash before the rebuild below leaves
    ///    readers folding for themselves rather than trusting a stamp that
    ///    could alias into the fresh log.
    /// 4. Reseed the rollup fold base as a new generation and rebuild the
    ///    persisted snapshot (`latest.json`) from the merged rollup so
    ///    neither depends on the rotated log.
    /// 5. Prune archives older than `archive_older_than` when set.
    #[must_use = "durability barrier; check the result"]
    pub fn rotate_event_log(
        &self,
        min_bytes: u64,
        archive_older_than: Option<Duration>,
    ) -> Result<EventLogRotationOutcome> {
        let outcome = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            // Fence the snapshot publishers across the rename + reseed, same
            // workspace → publish ordering as the identity rewrite.
            let _publish_guard = lock::WorkspaceLock::acquire(&self.inner.paths.publish_lock)?;

            let events = event_log::read_all(&self.inner.paths.events_log)?;
            let existing = snapshot::read_carryover(&self.inner.paths.agents_carryover)?;
            let merged = snapshot::agent_rollup_with_carryover(&events, existing.agents.clone());

            let rotation = event_log::rotate(
                &self.inner.paths.events_log,
                &self.inner.paths.events_archive_dir,
                min_bytes,
            )?;

            if rotation.is_rotated() {
                // Retract the published snapshot before anything else: its
                // extent stamp describes the renamed-away log, and the
                // freshness check compares offsets only. A crash anywhere
                // between here and the rebuild below must leave readers on
                // the fold-it-yourself path — never a stale stamp that could
                // alias once the fresh log regrows to the stamped length.
                match std::fs::remove_file(&self.inner.paths.latest_snapshot) {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(source) => {
                        return Err(event_log::EventLogErr::Io {
                            path: self.inner.paths.latest_snapshot.clone(),
                            source,
                        }
                        .into());
                    }
                }
                let carryover = snapshot::EventCarryover { agents: merged };
                snapshot::write_carryover(&self.inner.paths.agents_carryover, &carryover)?;
                // The fresh log is a new generation: reseed the fold base at
                // offset zero with the generation bumped, so a reader's
                // pre-rotation extent can never alias into the new log.
                retract_publish_stamp(&self.inner.paths);
                snapshot::reseed_rollup_cache_for_rotation(&self.inner.paths)?;
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

    /// Truncate the event log at its first invalid frame and republish the
    /// snapshot caches from what survives — the answer to a post-power-cut
    /// corpse (`rimz gc`, and the publish tail's self-heal). Locks in the
    /// canonical workspace → publish order, the same nesting rotation uses;
    /// an intact log is a read-only no-op.
    ///
    /// After a cut, both persisted fold bases are retracted before the
    /// rebuild: their extents describe bytes the truncation removed, and once
    /// the log regrows an offset-only stamp could alias into fresh frames —
    /// the same hazard rotation answers by retract-and-reseed. The rebuild
    /// re-folds the repaired log from zero and republishes both. An
    /// in-memory cursor heals itself: its offset either regresses (a
    /// reload), or its warm fold lands mid-frame in regrown bytes, fails the
    /// frame CRC, and retries cold.
    #[must_use = "durability barrier; check the result"]
    pub fn repair_event_log(&self) -> Result<event_log::RepairOutcome> {
        let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
        let _publish_guard = lock::WorkspaceLock::acquire(&self.inner.paths.publish_lock)?;
        let outcome = event_log::repair(&self.inner.paths.events_log)?;
        if outcome.truncated() {
            for stale in [
                &self.inner.paths.latest_snapshot,
                &self.inner.paths.rollup_cache,
            ] {
                match std::fs::remove_file(stale) {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(source) => {
                        return Err(event_log::EventLogErr::Io {
                            path: (*stale).clone(),
                            source,
                        }
                        .into());
                    }
                }
            }
            retract_publish_stamp(&self.inner.paths);
            snapshot::rebuild(&self.inner.paths)?;
        }
        Ok(outcome)
    }

    /// The off-lock tail every high-cadence mutator runs: the debounced
    /// group fdatasync, then the checkpoint publish when [`publish_due`]
    /// says the stamp aged out, the unpublished tail crossed the byte
    /// budget, or the log was swapped. The wakeup already went out, and
    /// consumers fold the log tail from their own cursor, so the checkpoint
    /// is a catch-up accelerator, never the freshness path — a skip costs a
    /// cold reader a bounded fold, not staleness.
    fn publish_snapshot_best_effort(&self) {
        self.publish_tail(false);
    }

    /// [`Self::publish_snapshot_best_effort`] without the cadence gate, for
    /// mutators that run rarely and whose callers read the checkpoint right
    /// after (gc's abandon sweep, standalone ask expiry). Rotation, identity
    /// rewrite, and repair rebuild inline under both locks instead.
    fn publish_snapshot_forced(&self) {
        self.publish_tail(true);
    }

    /// The one off-lock tail body behind both publish entry points: the
    /// debounced group fdatasync always runs; `force` decides whether the
    /// checkpoint skips the cadence gate.
    fn publish_tail(&self, force: bool) {
        sync_log_debounced(&self.inner.paths);
        if force || publish_due(&self.inner.paths) {
            self.publish_snapshot_now();
        }
    }

    /// Catch the snapshot caches up to the live log, after the workspace
    /// lock released and the wakeups went out. Serialized on its own
    /// advisory lock so concurrent publishers group-commit: each holder
    /// folds to the log's *current* end, so the last queued publisher always
    /// lands the newest state regardless of arrival order, and a queued
    /// no-op pays only an O(0-delta) fold. Best-effort: the mutation is
    /// already committed to the event log and feed files, and a reader
    /// self-serves any missing delta through the same incremental fold — a
    /// failed or skipped publish costs the next reader latency, never truth.
    ///
    /// A fold that hits a corrupt frame — a post-power-cut corpse —
    /// self-heals through [`Self::repair_event_log`] instead of failing
    /// every future publish until an operator runs `rimz gc`.
    fn publish_snapshot_now(&self) {
        let publish = || -> Result<()> {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.publish_lock)?;
            let snapshot = snapshot::rebuild(&self.inner.paths)?;
            write_publish_stamp(&self.inner.paths, snapshot.reflects_log);
            Ok(())
        };
        let Err(err) = publish() else {
            return;
        };
        if !publish_hit_corruption(&err) {
            warn!(error = %err, "snapshot publish failed after ledger commit");
            return;
        }
        // The publish closure released its lock above, so the repair's
        // workspace → publish acquisition nests in the canonical order.
        warn!(error = %err, "publish fold hit a corrupt event-log frame; repairing");
        if let Err(err) = self.repair_event_log() {
            warn!(error = %err, "event-log repair failed; run `rimz gc`");
            return;
        }
        // The repair republished when it cut; re-running covers the
        // found-intact race for the cost of an O(0-delta) fold.
        if let Err(err) = publish() {
            warn!(error = %err, "snapshot publish failed after event-log repair");
        }
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
        if let Err(err) = wakeup::wake_sidebars_for_event(&self.inner.runtime, event) {
            warn!(
                event_id = %event.event_id,
                error = %err,
                "sidebar wakeup failed after ledger event commit"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_gate_is_boundary_exact_on_the_interval() {
        let just_inside = PUBLISH_INTERVAL - Duration::from_millis(1);
        assert!(
            !should_publish(just_inside, 100, 100),
            "mid-interval with nothing unpublished: skip"
        );
        assert!(
            should_publish(PUBLISH_INTERVAL, 100, 100),
            "due exactly at the interval"
        );
    }

    #[test]
    fn publish_gate_is_boundary_exact_on_the_byte_budget() {
        let fresh = Duration::ZERO;
        assert!(
            !should_publish(fresh, 100, 100 + PUBLISH_BYTE_BUDGET - 1),
            "an unpublished tail under the budget rides the next interval"
        );
        assert!(
            should_publish(fresh, 100, 100 + PUBLISH_BYTE_BUDGET),
            "due exactly at the byte budget, whatever the stamp's age"
        );
    }

    #[test]
    fn publish_gate_forces_on_a_shrunken_log() {
        // A log shorter than the stamped offset means rotation or an
        // identity rewrite swapped the file underneath the stamp.
        assert!(should_publish(Duration::ZERO, 100, 99));
        assert!(!should_publish(Duration::ZERO, 100, 101));
    }
}
