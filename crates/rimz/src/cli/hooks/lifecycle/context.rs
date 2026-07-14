//! Agent context sidecar merge and realtime-cost enrichment.

use super::*;

pub(super) fn manage_agent_context(ctx: AgentContextHook<'_>) {
    let AgentContextHook {
        workspace,
        store,
        agent,
        context,
    } = ctx;
    let LifecycleEventContext {
        event_name,
        payload,
        agent_id,
        model_hint,
        transcript_path,
        turn_ended,
        observed_turn_error,
    } = context;
    // Tombstone the session's statusline context sidecar so it cannot pin stale
    // enrichment to a session the rollup has dropped.
    if agent.ends_session(event_name)
        && let Err(err) = rimz::store::agent_context::remove(
            store.runtime_paths(),
            agent.descriptor().kind,
            agent_id,
        )
    {
        warn!(
            agent = agent.descriptor().kind,
            event = %event_name,
            error = %err,
            "lifecycle: failed to remove the session's context sidecar",
        );
    }
    // Refresh the activity heartbeat on progress-proving events so the
    // sidebar's `last_activity` advances per tool call, not just per turn.
    if agent.descriptor().records_activity(event_name)
        && let Err(err) =
            rimz::agent_activity::touch(store.runtime_paths(), agent.descriptor().kind, agent_id)
    {
        warn!(
            agent = agent.descriptor().kind,
            event = %event_name,
            error = %err,
            "lifecycle: failed to touch the agent activity heartbeat",
        );
    }
    if let Some(context_agent_id) = payload_context_agent_id(payload) {
        merge_agent_context_sidecars(ContextSidecarInput {
            workspace,
            store,
            agent,
            event_name,
            payload,
            context_agent_id,
            model_hint,
            transcript_path,
            turn_ended,
            observed_turn_error,
        });
    }
    // An adapter can request a detached `rimz` helper after a lifecycle event.
    // Spawned with fresh stdio and never awaited, so it adds no latency to the
    // agent's turn.
    let refresh_ctx = rimz::agents::LifecycleRefreshCtx {
        agent_id,
        workspace_id: workspace.workspace_id.as_str(),
        model_hint,
        server_url: payload.get("server_url").and_then(Value::as_str),
    };
    if let Some(spawn) =
        agent.context_refresh_spawn(rimz::agents::RefreshTrigger::Hook(event_name), &refresh_ctx)
    {
        spawn_refresh_detached(&spawn);
    }
}

pub(super) fn merge_agent_context_sidecars(input: ContextSidecarInput<'_>) {
    let ContextSidecarInput {
        workspace,
        store,
        agent,
        event_name,
        payload,
        context_agent_id,
        model_hint,
        transcript_path,
        turn_ended,
        observed_turn_error,
    } = input;
    let mut turn_error_updated = false;
    if let Some(marker) = observed_turn_error {
        turn_error_updated |= merge_turn_error_marker_and_transcript(
            workspace,
            store,
            agent,
            event_name,
            context_agent_id,
            marker,
        );
    } else if let Some(marker) = agent.observe_turn_error_from_hook(event_name, payload) {
        turn_error_updated |= merge_turn_error_marker_and_transcript(
            workspace,
            store,
            agent,
            event_name,
            context_agent_id,
            marker,
        );
    } else if turn_error_refresh_event(event_name)
        && let Some(marker) = agent.observe_turn_error(payload)
    {
        turn_error_updated |= merge_turn_error_marker_and_transcript(
            workspace,
            store,
            agent,
            event_name,
            context_agent_id,
            marker,
        );
    }
    if turn_error_updated {
        let _ = rimz::store::wakeup::wake_sidebars(store.runtime_paths());
    }

    if payload_carries_observed_context(payload)
        && let Some(context) = agent.observe_context(agent.descriptor().kind, payload)
    {
        let kind = agent.descriptor().kind;
        match rimz::store::agent_context::merge_observed(
            store.runtime_paths(),
            kind,
            context_agent_id,
            context,
        ) {
            Ok(true) => {
                let _ = rimz::store::wakeup::wake_sidebars(store.runtime_paths());
            }
            Ok(false) => {}
            Err(err) => {
                warn!(
                    agent = kind,
                    session = %context_agent_id,
                    tags.operation = "agent.context_observed_merge",
                    error = &err as &dyn std::error::Error,
                    "lifecycle: failed to merge observed context",
                );
            }
        }
    }

    let shared_pricing_cache_path = store.runtime_paths().shared_pricing_cache_path();
    let estimated_cost_updated = {
        let prices = rimz::agents::pricing::cached_book(&shared_pricing_cache_path);
        agent
            .estimate_turn_cost(event_name, payload, &prices)
            .is_some_and(|estimate| {
                match rimz::store::agent_context::merge_estimated_cost(
                    store.runtime_paths(),
                    agent.descriptor().kind,
                    context_agent_id,
                    &estimate,
                ) {
                    Ok(changed) => changed,
                    Err(err) => {
                        warn!(
                            agent = agent.descriptor().kind,
                            event = %event_name,
                            error = %err,
                            "lifecycle: failed to merge estimated turn cost",
                        );
                        false
                    }
                }
            })
    };

    let prior = rimz::store::agent_context::read_one(
        store.runtime_paths(),
        agent.descriptor().kind,
        context_agent_id,
    );
    let local_model_hint = model_hint.or_else(|| {
        prior
            .as_ref()
            .and_then(|record| record.context.model_id.as_deref())
    });
    let prior_transcript_path = prior
        .as_ref()
        .and_then(|record| record.transcript_path.as_deref());
    let selected_transcript_path = transcript_path.or(prior_transcript_path);
    let prior_transcript_stat = (selected_transcript_path == prior_transcript_path)
        .then(|| {
            prior
                .as_ref()
                .and_then(|record| record.transcript_stat.as_ref())
        })
        .flatten();
    let refresh_ctx = rimz::agents::LocalContextRefreshCtx {
        agent_id: context_agent_id,
        model_hint: local_model_hint,
        current_transcript_path: transcript_path,
        prior_transcript_path: selected_transcript_path,
        prior_transcript_stat,
        shared_pricing_cache_path: &shared_pricing_cache_path,
    };
    let mut refresh =
        agent.local_context_refresh(rimz::agents::RefreshTrigger::Hook(event_name), &refresh_ctx);
    supplement_realtime_cost(
        agent,
        context_agent_id,
        &shared_pricing_cache_path,
        turn_ended,
        prior.as_ref(),
        &mut refresh,
    );
    let Some(refresh) = refresh else {
        if estimated_cost_updated {
            let _ = rimz::store::wakeup::wake_sidebars(store.runtime_paths());
        }
        return;
    };
    if let Err(err) = rimz::store::agent_context::merge_local_context(
        store.runtime_paths(),
        agent.descriptor().kind,
        context_agent_id,
        refresh,
        jiff::Timestamp::now(),
    ) {
        warn!(
            agent = agent.descriptor().kind,
            event = %event_name,
            error = %err,
            "lifecycle: failed to merge local context sidecar",
        );
    } else {
        let _ = rimz::store::wakeup::wake_sidebars(store.runtime_paths());
    }
}

pub(super) fn supplement_realtime_cost(
    agent: &dyn AgentAdapter,
    context_agent_id: &str,
    pricing_cache_path: &Path,
    turn_ended: bool,
    prior: Option<&rimz::store::agent_context::AgentContextRecord>,
    refresh: &mut Option<rimz::agents::LocalContextRefresh>,
) {
    if !turn_ended || refresh_total_cost(refresh.as_ref()).is_some() {
        return;
    }
    let Some(coverage) = realtime_cost_coverage(agent) else {
        return;
    };
    let partial = matches!(coverage, rimz::agents::ConcernCoverage::Partial { .. });
    if matches!(coverage, rimz::agents::ConcernCoverage::Unsupported { .. }) {
        return;
    }

    let prior_path = refresh
        .as_ref()
        .and_then(|refresh| refresh.transcript_path.as_deref())
        .or_else(|| prior.and_then(|record| record.transcript_path.as_deref()))
        .map(Path::new);
    let Some(path) = agent.session_transcript(context_agent_id, prior_path) else {
        return;
    };
    let Some(stat) = local_transcript_stat(&path) else {
        return;
    };
    if !partial
        && prior_total_cost(prior).is_some()
        && prior
            .and_then(|record| record.transcript_stat.as_ref())
            .is_some_and(|prior_stat| *prior_stat == stat)
        && refresh
            .as_ref()
            .and_then(|refresh| refresh.transcript_stat.as_ref())
            .is_none_or(|refresh_stat| *refresh_stat == stat)
    {
        return;
    }

    let prices = rimz::agents::pricing::cached_book(pricing_cache_path);
    let Some(cost) =
        rimz::agents::spending::session_cost_usd(agent, context_agent_id, &path, &prices)
    else {
        return;
    };

    // A Wired realtime signal, such as Claude's statusline, is live-session-only
    // and under-reports a resumed session. Reconcile upward from the transcript
    // without dragging down a fresher live figure the price book cannot rebuild.
    if !partial
        && prior_total_cost(prior)
            .is_some_and(|prior_usd| cost.total_cost_usd.unwrap_or_default() <= prior_usd)
    {
        return;
    }

    let refresh = refresh.get_or_insert_with(|| rimz::agents::LocalContextRefresh {
        model_id: None,
        effort: prior.and_then(|record| record.context.effort.clone()),
        tokens: prior.and_then(|record| record.context.tokens.clone()),
        cost: None,
        turn_error: prior.and_then(|record| record.context.turn_error.clone()),
        turn_complete: prior.and_then(|record| record.context.turn_complete),
        plan_proposed: prior.and_then(|record| record.context.plan_proposed),
        turn_interrupted: prior.and_then(|record| record.context.turn_interrupted),
        transcript_path: None,
        transcript_stat: None,
    });
    refresh.cost = Some(cost);
    refresh.transcript_path = Some(path.to_string_lossy().into_owned());
    refresh.transcript_stat = Some(stat);
}

pub(super) fn realtime_cost_coverage(
    agent: &dyn AgentAdapter,
) -> Option<rimz::agents::ConcernCoverage> {
    agent
        .descriptor()
        .coverage
        .iter()
        .find(|(concern, _)| *concern == rimz::agents::IntegrationConcern::RealtimeCost)
        .map(|(_, coverage)| *coverage)
}

pub(super) fn refresh_total_cost(
    refresh: Option<&rimz::agents::LocalContextRefresh>,
) -> Option<f64> {
    refresh
        .and_then(|refresh| refresh.cost.as_ref())
        .and_then(|cost| cost.total_cost_usd)
}

pub(super) fn prior_total_cost(
    prior: Option<&rimz::store::agent_context::AgentContextRecord>,
) -> Option<f64> {
    prior
        .and_then(|record| record.context.cost.as_ref())
        .and_then(|cost| cost.total_cost_usd)
}

pub(super) fn local_transcript_stat(path: &Path) -> Option<rimz::agents::TranscriptStat> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let since_epoch = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(rimz::agents::TranscriptStat {
        mtime_secs: since_epoch.as_secs().try_into().unwrap_or(i64::MAX),
        mtime_nanos: since_epoch.subsec_nanos(),
        len: meta.len(),
    })
}

pub(super) const OBSERVED_CONTEXT_KEYS: &[&str] = &[
    "model",
    "effort",
    "rate_limits",
    "total_cost_usd",
    "context_window",
    "total_tokens",
    "context_pct",
];

pub(super) fn payload_carries_observed_context(payload: &Value) -> bool {
    payload
        .get("hook_event_name")
        .and_then(Value::as_str)
        .is_some_and(|event| event == "context")
        || OBSERVED_CONTEXT_KEYS
            .iter()
            .any(|key| payload.get(*key).is_some())
}
