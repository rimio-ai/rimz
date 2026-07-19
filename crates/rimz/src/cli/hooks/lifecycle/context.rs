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
        decoded,
        payload,
        agent_id,
        parent_agent_id,
        model_hint,
        transcript_path,
        turn_ended,
    } = context;
    // Remove the session's statusline context sidecar before the normal
    // activity, merge, and refresh fall-through. Refresh-capable adapters can
    // then repopulate the ended row with their final local context reading.
    if decoded.ends_session()
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
    if (decoded.records_progress() || parent_agent_id.is_some())
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
    // Child details travel on the durable child observation. Provider payloads
    // can repeat root model/transcript fields, so root-scoped sidecars and
    // detached refreshes stay out of every exact-ID child path.
    if parent_agent_id.is_some() {
        return;
    }
    if let Some(context_agent_id) = decoded.context_agent_id().cloned() {
        merge_agent_context_sidecars(ContextSidecarInput {
            workspace,
            store,
            agent,
            event_name,
            decoded,
            payload,
            context_agent_id: context_agent_id.as_str(),
            model_hint,
            transcript_path,
            turn_ended,
        });
    }
    // An adapter can request a detached `rimz` helper after a lifecycle event.
    // Spawned with fresh stdio and never awaited, so it adds no latency to the
    // agent's turn.
    let refresh_ctx = rimz::agents::LifecycleRefreshCtx {
        agent_id,
        workspace_id: workspace.workspace_id.as_str(),
        model_hint,
        server_url: decoded.routing().server_url(),
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
        decoded,
        payload,
        context_agent_id,
        model_hint,
        transcript_path,
        turn_ended,
    } = input;
    let mut turn_error_updated = false;
    if let Some(marker) = decoded.turn_error() {
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

    if let Some(context) = decoded.take_observed_context() {
        let kind = agent.descriptor().kind;
        match rimz::store::agent_context::merge_observed(
            store.runtime_paths(),
            kind,
            context.agent_id.as_str(),
            context.context,
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
    let locally_priced_cost_updated = {
        let prices = rimz::agents::pricing::cached_book(&shared_pricing_cache_path);
        agent
            .price_turn_locally(event_name, payload, &prices)
            .is_some_and(|priced| {
                match rimz::store::agent_context::merge_locally_priced_cost(
                    store.runtime_paths(),
                    agent.descriptor().kind,
                    context_agent_id,
                    &priced,
                ) {
                    Ok(changed) => changed,
                    Err(err) => {
                        warn!(
                            agent = agent.descriptor().kind,
                            event = %event_name,
                            error = %err,
                            "lifecycle: failed to merge locally priced turn cost",
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
    let prior_spend_fold = (selected_transcript_path == prior_transcript_path)
        .then(|| prior.as_ref().and_then(|record| record.spend_fold.as_ref()))
        .flatten();
    let refresh_ctx = rimz::agents::LocalContextRefreshCtx {
        agent_id: context_agent_id,
        model_hint: local_model_hint,
        current_transcript_path: transcript_path,
        prior_transcript_path: selected_transcript_path,
        prior_transcript_stat,
        prior_spend_fold,
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
        if locally_priced_cost_updated {
            let _ = rimz::store::wakeup::wake_sidebars(store.runtime_paths());
        }
        return;
    };
    if let Err(err) = rimz::store::agent_context::merge_local_context(
        store.runtime_paths(),
        agent.descriptor(),
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
    let coverage = realtime_cost_coverage(agent);
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
    let Some(stat) = agent.transcript_stat(&path) else {
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

    let refresh = refresh.get_or_insert_with(rimz::agents::LocalContextRefresh::sparse);
    refresh.context.cost = rimz::agents::FieldPatch::Set(cost);
    refresh.transcript_path = Some(path.to_string_lossy().into_owned());
    refresh.transcript_stat = Some(stat);
}

pub(super) fn realtime_cost_coverage(agent: &dyn AgentAdapter) -> rimz::agents::ConcernCoverage {
    agent
        .descriptor()
        .concern_coverage(rimz::agents::IntegrationConcern::RealtimeCost)
}

pub(super) fn refresh_total_cost(
    refresh: Option<&rimz::agents::LocalContextRefresh>,
) -> Option<f64> {
    refresh
        .and_then(|refresh| match &refresh.context.cost {
            rimz::agents::FieldPatch::Set(cost) => Some(cost),
            rimz::agents::FieldPatch::Keep | rimz::agents::FieldPatch::Clear => None,
        })
        .and_then(|cost| cost.total_cost_usd)
}

pub(super) fn prior_total_cost(
    prior: Option<&rimz::store::agent_context::AgentContextRecord>,
) -> Option<f64> {
    prior
        .and_then(|record| record.context.cost.as_ref())
        .and_then(|cost| cost.total_cost_usd)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct SessionEndRefreshAdapter;

    impl AgentAdapter for SessionEndRefreshAdapter {
        fn descriptor(&self) -> &'static rimz::agents::AgentDescriptor {
            rimz::agents::GrokAdapter.descriptor()
        }

        fn decode_hook(
            &self,
            event_name: &str,
            payload: &Value,
        ) -> rimz::agents::Result<DecodedHook> {
            rimz::agents::GrokAdapter.decode_hook(event_name, payload)
        }

        fn local_context_refresh(
            &self,
            _trigger: rimz::agents::RefreshTrigger<'_>,
            _ctx: &rimz::agents::LocalContextRefreshCtx<'_>,
        ) -> Option<rimz::agents::LocalContextRefresh> {
            let mut refresh = rimz::agents::LocalContextRefresh::sparse();
            refresh.context.session_preview =
                rimz::agents::FieldPatch::Set("final local context".to_owned());
            Some(refresh)
        }
    }

    #[test]
    fn native_session_end_repopulates_context_from_local_refresh() {
        let dir = tempfile::TempDir::new().unwrap();
        let workspace_id =
            rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/hooks-test"));
        let paths = rimz::store::StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        let runtime = rimz::store::RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
        let store = Store::open(paths, runtime).unwrap();
        let workspace = ResolvedWorkspace {
            workspace_id,
            project_root: "/tmp/hooks-test".into(),
            root_class: rimz::workspace::RootClass::Directory,
            worktree_root: "/tmp/hooks-test".into(),
            worktree_branch: None,
            session_name: "hooks-test".to_owned(),
            mux_hint: None,
        };
        let adapter = SessionEndRefreshAdapter;
        let payload = serde_json::json!({
            "sessionId": "root-session",
            "cwd": "/tmp/hooks-test"
        });
        let mut decoded = adapter
            .decode_hook("SessionEnd", &payload)
            .expect("session end decodes");
        assert!(decoded.ends_session());
        let event_name = decoded.event_name().to_owned();
        let agent_id = decoded.context_agent_id().unwrap().to_string();

        let mut context = rimz::agents::AgentContext::new("grok", jiff::Timestamp::now());
        context.model_id = Some("stale-model".to_owned());
        rimz::store::agent_context::merge_observed(
            store.runtime_paths(),
            "grok",
            "root-session",
            context,
        )
        .expect("context sidecar writes");
        manage_agent_context(AgentContextHook {
            workspace: &workspace,
            store: &store,
            agent: &adapter,
            context: LifecycleEventContext {
                event_name: &event_name,
                decoded: &mut decoded,
                payload: &payload,
                agent_id: &agent_id,
                parent_agent_id: None,
                model_hint: None,
                transcript_path: None,
                turn_ended: false,
            },
        });

        let context =
            rimz::store::agent_context::read_one(store.runtime_paths(), "grok", "root-session")
                .expect("local refresh repopulates ended context")
                .context;
        assert_eq!(context.model_id, None);
        assert_eq!(
            context.session_preview.as_deref(),
            Some("final local context")
        );
    }
}
