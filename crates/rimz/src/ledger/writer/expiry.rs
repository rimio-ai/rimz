use jiff::Timestamp;
use serde_json::json;

use crate::feed::{AbandonReason, FeedStatus, Resolution, ResolutionMethod};
use crate::ids::RequestId;

use super::super::{AskExpiry, Ledger, Result, StatePaths, event_log, feed_store, lock, runtime};
use super::debounce::{abandon_sweep_due, abandon_sweep_stamp, touch_stamp};
use crate::schema::event::EventEnvelope;

/// Run the dead-owner sweep at most once per the abandon sweep interval.
/// Caller holds the workspace lock. The common case is one stamp stat — the
/// write path itself stays O(1) regardless of feed history.
pub(super) fn sweep_dead_owned_items_debounced(
    paths: &StatePaths,
    session_name: &str,
) -> Result<Vec<RequestId>> {
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
pub(super) fn expire_agent_asks_locked(
    paths: &StatePaths,
    source: &str,
    agent_id: &str,
    session_name: &str,
    expiry: AskExpiry,
) -> Result<Vec<RequestId>> {
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
        expired.push(item.request_id.clone());
    }
    Ok(expired)
}

fn abandon_dead_owned_items_locked(
    paths: &StatePaths,
    session_name: &str,
) -> Result<Vec<RequestId>> {
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
        abandoned.push(item.request_id.clone());
    }
    Ok(abandoned)
}

impl Ledger {
    #[must_use = "durability barrier; check the result"]
    pub fn abandon_dead_owned_items(&self, session_name: &str) -> Result<usize> {
        let abandoned = {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            let abandoned = abandon_dead_owned_items_locked(&self.inner.paths, session_name)?;
            touch_stamp(&abandon_sweep_stamp(&self.inner.paths));
            abandoned
        };
        for request_id in &abandoned {
            self.wake_sidebars_best_effort(request_id);
        }
        if !abandoned.is_empty() {
            // Forced: gc reports from the checkpoint right after the sweep.
            self.publish_snapshot_forced();
        }
        Ok(abandoned.len())
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
    /// [`Self::append_event_and_expire`] and [`Self::push_feed_item_superseding`].
    /// Closing the loop here is deterministic; the snapshot's read-side guard
    /// self-heals anything that races this write. Returns the number of items
    /// expired.
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
        for request_id in &expired {
            self.wake_sidebars_best_effort(request_id);
        }
        if !expired.is_empty() {
            // Forced: the standalone expiry's caller reads the checkpoint
            // right after its verdict.
            self.publish_snapshot_forced();
        }
        Ok(expired.len())
    }
}
