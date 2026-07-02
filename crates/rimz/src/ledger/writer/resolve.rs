use jiff::Timestamp;
use serde_json::json;
use tracing::warn;

use crate::feed::{
    AbandonReason, FeedItem, FeedStatus, Resolution, ResolutionMethod, ResolverStepState,
};
use crate::ids::{AgentKind, AgentSessionId, RequestId, ResolverId};
use crate::ledger::event::EventEnvelope;

use super::super::{
    AbstainOutcome, ElapseOutcome, Ledger, ResolveOutcome, Result, TimeoutOutcome, feed_store,
};
use super::PublishPolicy;

impl Ledger {
    /// Apply a resolver decision. CAS on `status = Pending`. Late answers
    /// (status = `TimedOut`) are accepted but recorded `effective: false` per
    /// the docs.
    #[must_use = "durability barrier; check the result"]
    pub fn resolve_feed_item(
        &self,
        request_id: &RequestId,
        mut resolution: Resolution,
        override_chain: bool,
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

            if !override_chain && let Some(active) = item.chain_active_resolver.as_ref() {
                let provided = resolution.resolver_id.as_ref();
                if provided != Some(active) {
                    return Err(feed_store::FeedStoreErr::ResolverNotActive {
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
                    return Err(feed_store::FeedStoreErr::NotPending {
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
                feed_store::write(&txn.paths.feed_dir, &item)?;
                txn.wake_item(&item);
                Some(item.clone())
            } else {
                warn!(
                    request_id = %request_id,
                    "late resolver answer recorded as audit-only (item already timed out)"
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
                    "resolver_id": resolution.resolver_id.clone(),
                    "reason": resolution.reason.clone(),
                }),
            ))?;

            Ok(item_to_wake)
        })?;

        if let Some(item) = &item_to_wake {
            self.record_transcript_answer_best_effort(item);
        }

        Ok(ResolveOutcome {
            request_id: request_id.clone(),
            effective: item_to_wake.is_some(),
            late: item_to_wake.is_none(),
        })
    }

    fn record_transcript_answer_best_effort(&self, item: &FeedItem) {
        if item.source_kind != "agent-hook" || !item.kind.is_ask() {
            return;
        }
        let Some(resolution) = item.resolution.as_ref() else {
            return;
        };
        let Some(agent_id) = item.agent_session_id().map(AgentSessionId::from) else {
            return;
        };
        let text = crate::ledger::transcript_log::answer_text(&resolution.decision);
        let entry = crate::ledger::transcript_log::TranscriptEntry {
            at: resolution.resolved_at,
            kind: AgentKind::new_unchecked(item.source.clone()),
            agent_id,
            channel: crate::harness::target::compose_channel(
                None,
                item.worktree_branch.as_deref(),
                item.worktree_path.as_deref().and_then(path_basename),
                None,
            ),
            name: None,
            profile: None,
            role: None,
            entry: crate::ledger::transcript_log::TranscriptKind::Answer,
            request_id: Some(item.request_id.clone()),
            from: Some(resolution_from(resolution)),
            text: text.clone(),
            questions: Vec::new(),
            answers: vec![crate::agents::AskAnswer {
                question: None,
                chosen: vec![text],
                note: None,
            }],
        };
        if let Err(err) = crate::ledger::transcript_log::append(self.paths(), &entry) {
            warn!(
                request_id = %item.request_id,
                error = %err,
                "resolve: failed to record transcript answer",
            );
        }
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
            item.mark_active_resolver_budget_elapsed(reason);
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

    /// Explicit chain handoff. The active resolver calls this to pass on a
    /// request without answering. Records a `feed.abstain` audit event and
    /// marks the matching `ResolverStep` as `Abstained`; the next chain link's
    /// id is returned for the caller to log.
    #[must_use = "durability barrier; check the result"]
    pub fn abstain_feed_item(
        &self,
        request_id: &RequestId,
        resolver_id: &ResolverId,
        reason: Option<String>,
        session_name: &str,
    ) -> Result<AbstainOutcome> {
        self.commit(PublishPolicy::Skip, |txn| {
            let mut item = feed_store::load(&txn.paths.feed_dir, request_id)?;
            if !item.surface.supports_resolve() {
                return Err(feed_store::FeedStoreErr::SurfaceMismatch {
                    request_id: request_id.clone(),
                    surface: item.surface,
                    verb: "abstain",
                }
                .into());
            }
            if !item.status.allows_resolution() {
                return Err(feed_store::FeedStoreErr::NotPending {
                    request_id: request_id.clone(),
                    status: item.status,
                }
                .into());
            }
            if item.chain_active_resolver.as_ref() != Some(resolver_id) {
                return Err(feed_store::FeedStoreErr::ResolverNotActive {
                    request_id: request_id.clone(),
                    resolver: resolver_id.clone(),
                }
                .into());
            }

            for step in &mut item.chain {
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
            feed_store::write(&txn.paths.feed_dir, &item)?;
            txn.append(&EventEnvelope::new(
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
            ))?;
            let outcome = AbstainOutcome {
                request_id: request_id.clone(),
                next_resolver: next,
            };
            Ok(outcome)
        })
    }

    /// Involuntary chain handoff. Called by the hook bridge when the active
    /// resolver's per-step budget elapses or its heartbeat goes stale before
    /// it answered.
    #[must_use = "durability barrier; check the result"]
    pub fn elapse_chain_step(
        &self,
        request_id: &RequestId,
        current: &ResolverId,
        reason: AbandonReason,
        session_name: &str,
    ) -> Result<ElapseOutcome> {
        self.commit(PublishPolicy::Skip, |txn| {
            let mut item = feed_store::load(&txn.paths.feed_dir, request_id)?;
            if !item.surface.supports_resolve() {
                return Err(feed_store::FeedStoreErr::SurfaceMismatch {
                    request_id: request_id.clone(),
                    surface: item.surface,
                    verb: "chain_elapse",
                }
                .into());
            }
            if !item.status.allows_resolution() {
                return Err(feed_store::FeedStoreErr::NotPending {
                    request_id: request_id.clone(),
                    status: item.status,
                }
                .into());
            }
            if item.chain_active_resolver.as_ref() != Some(current) {
                return Err(feed_store::FeedStoreErr::ResolverNotActive {
                    request_id: request_id.clone(),
                    resolver: current.clone(),
                }
                .into());
            }

            item.mark_active_resolver_budget_elapsed(reason);
            let next = item.advance_resolver_chain_after(current);
            item.updated_at = Timestamp::now();
            feed_store::write(&txn.paths.feed_dir, &item)?;
            txn.append(&EventEnvelope::new(
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
            ))?;
            let outcome = ElapseOutcome {
                request_id: request_id.clone(),
                next_resolver: next,
            };
            Ok(outcome)
        })
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

fn path_basename(path: &str) -> Option<&str> {
    path.rsplit('/').next().filter(|value| !value.is_empty())
}

fn resolution_from(resolution: &Resolution) -> String {
    match resolution.method {
        ResolutionMethod::Cli
        | ResolutionMethod::Sidebar
        | ResolutionMethod::PaneSend
        | ResolutionMethod::Dismiss => "you".to_owned(),
        ResolutionMethod::HookBridge => resolution
            .resolver_id
            .as_ref()
            .map(|resolver| format!("@{}", resolver.as_str()))
            .unwrap_or_else(|| "resolver".to_owned()),
        ResolutionMethod::AgentMovedOn
        | ResolutionMethod::OwnerExited
        | ResolutionMethod::WorkspaceReset => "resolver".to_owned(),
    }
}
