use jiff::Timestamp;
use serde_json::json;
use tracing::warn;

use crate::feed::{AbandonReason, FeedStatus, Resolution, ResolutionMethod};
use crate::ids::RequestId;
use crate::ledger::event::EventEnvelope;

use super::super::{Ledger, ResolveOutcome, Result, TimeoutOutcome, feed_store};
use super::PublishPolicy;

impl Ledger {
    /// Apply a feed decision. CAS on `status = Pending`. Late answers
    /// (status = `TimedOut`) are accepted but recorded `effective: false` per
    /// the docs.
    #[must_use = "durability barrier; check the result"]
    pub fn resolve_feed_item(
        &self,
        request_id: &RequestId,
        mut resolution: Resolution,
        session_name: &str,
    ) -> Result<ResolveOutcome> {
        let item_to_wake = self.commit(PublishPolicy::Skip, |txn| {
            let mut item = feed_store::load(&txn.paths.feed_dir, request_id)?;

            if !item.surface.supports_resolve() {
                return Err(feed_store::FeedStoreErr::SurfaceMismatch {
                    request_id: request_id.clone(),
                    surface: item.surface,
                    verb: "resolve",
                }
                .into());
            }

            let (effective, late) = match item.status {
                FeedStatus::Pending => (true, false),
                FeedStatus::TimedOut => (false, true),
                other => {
                    return Err(feed_store::FeedStoreErr::NotPending {
                        request_id: request_id.clone(),
                        status: other,
                    }
                    .into());
                }
            };

            resolution.effective = effective;
            resolution.late = late;
            if late && resolution.reason.is_none() {
                resolution.reason = Some(AbandonReason::ScriptWaitTimeout.as_str().to_owned());
            }

            let item_to_wake = if effective {
                item.status = FeedStatus::Resolved;
                item.resolution = Some(resolution.clone());
                item.updated_at = Timestamp::now();
                feed_store::write(&txn.paths.feed_dir, &item)?;
                txn.wake_item(&item);
                Some(item.clone())
            } else {
                warn!(
                    request_id = %request_id,
                    "late feed resolution recorded as audit-only (item already timed out)"
                );
                None
            };

            txn.append(&EventEnvelope::new(
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
                    "by": resolution.by.clone(),
                    "reason": resolution.reason.clone(),
                }),
            ))?;

            Ok(item_to_wake)
        })?;

        Ok(ResolveOutcome {
            request_id: request_id.clone(),
            effective: item_to_wake.is_some(),
            late: item_to_wake.is_none(),
            resolved_item: item_to_wake,
        })
    }

    #[must_use = "durability barrier; check the result"]
    pub fn mark_feed_item_timed_out(
        &self,
        request_id: &RequestId,
        session_name: &str,
        reason: AbandonReason,
    ) -> Result<TimeoutOutcome> {
        let outcome = self.commit(PublishPolicy::Skip, |txn| {
            let mut item = feed_store::load(&txn.paths.feed_dir, request_id)?;

            if !item.surface.supports_resolve() {
                return Err(feed_store::FeedStoreErr::SurfaceMismatch {
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
            item.updated_at = Timestamp::now();
            feed_store::write(&txn.paths.feed_dir, &item)?;
            txn.append(&EventEnvelope::new(
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
            ))?;
            Ok(TimeoutOutcome {
                request_id: request_id.clone(),
                status: FeedStatus::TimedOut,
                transitioned: true,
            })
        })?;

        Ok(outcome)
    }

    /// Mark a `native_ui` feed item as acknowledged locally. Never reaches the
    /// agent — that's the docs' contract.
    #[must_use = "durability barrier; check the result"]
    pub fn dismiss_feed_item(
        &self,
        request_id: &RequestId,
        reason: Option<String>,
        session_name: &str,
    ) -> Result<()> {
        self.commit(PublishPolicy::Skip, |txn| {
            let mut item = feed_store::load(&txn.paths.feed_dir, request_id)?;
            if !item.surface.supports_dismiss() {
                return Err(feed_store::FeedStoreErr::SurfaceMismatch {
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
            feed_store::write(&txn.paths.feed_dir, &item)?;
            txn.append(&EventEnvelope::new(
                item.workspace_id.clone(),
                session_name,
                "rimz",
                "cli",
                "feed.dismiss",
                json!({
                    "request_id": request_id,
                    "reason": reason,
                }),
            ))?;
            Ok(())
        })
    }
}
