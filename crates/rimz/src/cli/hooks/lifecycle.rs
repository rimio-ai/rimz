use super::*;

pub(super) fn handle_lifecycle_hook(
    workspace: &ResolvedWorkspace,
    ledger: &Ledger,
    agent: &dyn AgentAdapter,
    event_name: &str,
    payload: &Value,
    globals: &GlobalFlags,
) -> Result<()> {
    let agent_id = payload_agent_id(payload);
    let fallback_expiry_scope = expiry_scope_for_event_name(agent, event_name);
    let fallback_expiry = match (agent_id, fallback_expiry_scope) {
        (Some(agent_id), Some(scope)) => Some((agent_id, scope)),
        _ => None,
    };
    let recorded = record_lifecycle_observation(
        workspace,
        ledger,
        agent,
        event_name,
        payload,
        globals,
        fallback_expiry,
    );
    let model_hint = recorded
        .as_ref()
        .and_then(|recorded| recorded.model_hint.as_deref());
    let turn_ended = recorded.as_ref().is_some_and(|recorded| {
        matches!(
            recorded.observation.signal,
            LifecycleSignal::TurnEnded { .. }
        )
    });
    if let Some(agent_id) = agent_id {
        let observed_turn_error = recorded
            .as_ref()
            .and_then(|recorded| recorded.observation.turn_error.clone());
        manage_agent_context(AgentContextHook {
            workspace,
            ledger,
            agent,
            context: LifecycleEventContext {
                event_name,
                payload,
                agent_id,
                model_hint,
                turn_ended,
                observed_turn_error,
            },
        });
    }
    if let Some(recorded) = recorded.as_ref() {
        record_run_lifecycle(ledger, agent, event_name, payload, recorded);
        spawn_queue_delivery_if_checkpoint(workspace, ledger, agent, recorded);
    }
    Ok(())
}

struct RecordedLifecycle {
    model_hint: Option<String>,
    observation: AgentLifecycleObservation,
}

struct AgentContextHook<'a> {
    workspace: &'a ResolvedWorkspace,
    ledger: &'a Ledger,
    agent: &'a dyn AgentAdapter,
    context: LifecycleEventContext<'a>,
}

struct LifecycleEventContext<'a> {
    event_name: &'a str,
    payload: &'a Value,
    agent_id: &'a str,
    model_hint: Option<&'a str>,
    turn_ended: bool,
    observed_turn_error: Option<rimz::agents::AgentTurnError>,
}

/// Record the agent lifecycle observation and expire superseded asks in the
/// same ledger write when the adapter emitted a durable transition.
fn record_lifecycle_observation(
    workspace: &ResolvedWorkspace,
    ledger: &Ledger,
    agent: &dyn AgentAdapter,
    event_name: &str,
    payload: &Value,
    globals: &GlobalFlags,
    fallback_expiry: Option<(&str, AskExpiry)>,
) -> Option<RecordedLifecycle> {
    if let Some(mut observation) = agent.observe_lifecycle(event_name, payload) {
        attach_agent_owner(agent.descriptor().kind, &mut observation);
        attach_agent_pane(&mut observation);
        if observation.agent_name.is_none() {
            observation.agent_name = env_agent_name().or_else(|| proc_agent_name(&observation));
        }
        if observation.agent_alias.is_none() {
            observation.agent_alias = env_agent_alias().or_else(|| proc_agent_alias(&observation));
        }
        if observation.worktree_path.is_none() {
            observation.worktree_path = Some(workspace.worktree_root.display().to_string());
        }
        if observation.worktree_branch.is_none() {
            observation.worktree_branch = workspace.worktree_branch.clone();
        }
        recover_focused_pane_binding(
            agent.descriptor().kind,
            agent.descriptor().capabilities.registers_lazily,
            globals.mux,
            workspace,
            ledger,
            &mut observation,
        );
        let model_hint = observation.model.clone();
        // Validate the transition this event drives against the prior rollup
        // and log any anomaly once, here at ingestion. Replay re-derives the
        // same state silently.
        let transition = log_lifecycle_transition(ledger, agent.descriptor().kind, &observation);
        if transition.is_some_and(|transition| {
            transition.compaction_closed
                && !matches!(observation.signal, LifecycleSignal::CompactionEnded { .. })
        }) {
            debug!(
                target: "rimz::agent::lifecycle",
                kind = agent.descriptor().kind,
                agent_id = observation.agent_id.as_deref().unwrap_or(""),
                signal = ?observation.signal,
                "closed compaction bracket on a non-compaction signal",
            );
        }
        let envelope = EventEnvelope::agent_lifecycle(
            workspace.workspace_id.clone(),
            &workspace.session_name,
            agent.descriptor().kind,
            event_name,
            &observation,
        );
        // `ToolUsed { false, false }` is reserved for non-blocking PreToolUse
        // proof-of-work. PostToolUse observations are emitted only from the
        // `tool_mutates` arm, so they always carry `mutates: true`; this gate
        // keeps PreToolUse out of the durable log unless a fresh snapshot shows
        // it reconciling a resting row or closing a compaction bracket. If the
        // transition read fails, the proof-of-work signal drops and the bracket
        // closes on the next durable lifecycle signal.
        let append_lifecycle = append_lifecycle_event(&observation.signal, transition);
        let append_expiry = observation
            .agent_id
            .as_deref()
            .zip(expiry_scope_for_signal(&observation.signal))
            .map(|(agent_id, scope)| (agent.descriptor().kind, agent_id, scope));
        if append_lifecycle
            && let Err(err) = ledger.append_event_and_expire(&envelope, append_expiry)
        {
            warn!(
                agent = agent.descriptor().kind,
                event = %event_name,
                error = %err,
                "lifecycle: failed to record the agent.lifecycle event",
            );
        }
        return Some(RecordedLifecycle {
            model_hint,
            observation,
        });
    }

    if let Some((agent_id, scope)) = fallback_expiry {
        // A boundary event the adapter doesn't observe still expires the
        // session's superseded asks through the standalone path.
        let result = match scope {
            AskExpiry::SessionEnded => ledger.expire_agent_session(
                agent.descriptor().kind,
                agent_id,
                &workspace.session_name,
            ),
            AskExpiry::MovedOn => ledger.expire_agent_native_ui_asks(
                agent.descriptor().kind,
                agent_id,
                &workspace.session_name,
            ),
        };
        if let Err(err) = result {
            warn!(
                agent = agent.descriptor().kind,
                event = %event_name,
                error = %err,
                "lifecycle: failed to expire the session's pending asks",
            );
        }
    }
    None
}

fn expiry_scope_for_event_name(agent: &dyn AgentAdapter, event_name: &str) -> Option<AskExpiry> {
    // Fallback for boundary events the adapter intentionally does not observe:
    // the adapter's native predicates still carry the answer for these paths.
    if agent.ends_session(event_name) {
        Some(AskExpiry::SessionEnded)
    } else if agent.moves_on(event_name) {
        Some(AskExpiry::MovedOn)
    } else {
        None
    }
}

fn expiry_scope_for_signal(signal: &LifecycleSignal) -> Option<AskExpiry> {
    // A lifecycle boundary can strand the session's pending native_ui asks:
    // the agent answers those in its own UI and never reports back, so they
    // pile up as duplicate attention. Session end expires every surface; a
    // live session moving on expires only native_ui asks so an in-flight bridge
    // ask keeps resolving.
    match signal {
        LifecycleSignal::Ended => Some(AskExpiry::SessionEnded),
        LifecycleSignal::TurnStarted | LifecycleSignal::TurnEnded { .. } => {
            Some(AskExpiry::MovedOn)
        }
        _ => None,
    }
}

fn record_run_lifecycle(
    ledger: &Ledger,
    agent: &dyn AgentAdapter,
    event_name: &str,
    payload: &Value,
    recorded: &RecordedLifecycle,
) {
    let Some(run_id) = env_run_id() else {
        return;
    };
    let last_message = rimz::run::terminal_status_for_signal(&recorded.observation.signal)
        .is_some()
        .then(|| agent.last_assistant_message(event_name, payload, &recorded.observation))
        .flatten();
    match rimz::run::record_lifecycle(
        ledger.paths(),
        &run_id,
        agent.descriptor().kind,
        &recorded.observation,
        last_message,
    ) {
        Ok(Some(record)) => {
            if let Err(err) = rimz::ledger::wakeup::wake_run(ledger.runtime_paths(), &record) {
                warn!(
                    agent = agent.descriptor().kind,
                    event = %event_name,
                    run_id = %run_id,
                    error = %err,
                    "lifecycle: failed to wake the completed run",
                );
            }
        }
        Ok(None) => {}
        Err(err) => {
            warn!(
                agent = agent.descriptor().kind,
                event = %event_name,
                run_id = %run_id,
                error = %err,
                "lifecycle: failed to update the supervised run",
            );
        }
    }
}

fn spawn_queue_delivery_if_checkpoint(
    workspace: &ResolvedWorkspace,
    ledger: &Ledger,
    agent: &dyn AgentAdapter,
    recorded: &RecordedLifecycle,
) {
    if !rimz::message::delivery_checkpoint(&recorded.observation.signal) {
        return;
    }
    let Some(agent_id) = recorded.observation.agent_id.as_ref() else {
        return;
    };
    let pending = match ledger.list_pending_messages() {
        Ok(messages) => messages,
        Err(err) => {
            debug!(
                agent = agent.descriptor().kind,
                agent_id = %agent_id,
                error = %err,
                "queue delivery skipped; pending messages unreadable",
            );
            return;
        }
    };
    let kind = rimz::ids::AgentKind::new_unchecked(agent.descriptor().kind);
    // FIFO spans this card's provisional and registered ids, so the stable
    // agent name folds a message queued before registration into the same queue.
    let Some(head) = rimz::message::queue_head(
        pending.iter(),
        &kind,
        agent_id,
        recorded.observation.agent_name.as_deref(),
    ) else {
        return;
    };
    spawn_refresh_detached(&rimz::agents::RefreshSpawn {
        args: vec![
            "--root".to_owned(),
            workspace.project_root.display().to_string(),
            "queue".to_owned(),
            "deliver".to_owned(),
            "--message-id".to_owned(),
            head.message_id.to_string(),
        ],
    });
}

fn env_run_id() -> Option<rimz::RunId> {
    let raw = std::env::var(rimz::run::ENV_RUN_ID).ok()?;
    match raw.parse() {
        Ok(run_id) => Some(run_id),
        Err(err) => {
            warn!(
                run_id = %raw,
                error = %err,
                "lifecycle: ignoring invalid supervised run id",
            );
            None
        }
    }
}

fn env_agent_name() -> Option<String> {
    let raw = std::env::var(rimz::run::ENV_AGENT_NAME).ok()?;
    validate_agent_name_env(raw, "env")
}

fn proc_agent_name(observation: &AgentLifecycleObservation) -> Option<String> {
    let raw = rimz::proc::env_var(observation.agent_pid?, rimz::run::ENV_AGENT_NAME)?;
    validate_agent_name_env(raw, "process")
}

fn env_agent_alias() -> Option<String> {
    let raw = std::env::var(rimz::run::ENV_AGENT_ALIAS).ok()?;
    validate_agent_alias_env(raw, "env")
}

fn proc_agent_alias(observation: &AgentLifecycleObservation) -> Option<String> {
    let raw = rimz::proc::env_var(observation.agent_pid?, rimz::run::ENV_AGENT_ALIAS)?;
    validate_agent_alias_env(raw, "process")
}

/// Accept a stamped role alias only if it reads as a layout cell word — the
/// same shape `[agents.aliases]` validates at config load, so a garbled env
/// value never becomes an addressable handle.
fn validate_agent_alias_env(raw: String, source: &str) -> Option<String> {
    let valid = !raw.is_empty()
        && !raw
            .chars()
            .any(|ch| ch.is_whitespace() || ch == ',' || ch == '+' || ch == ':' || ch == '#');
    if valid {
        Some(raw)
    } else {
        warn!(
            agent_alias = %raw,
            source,
            "lifecycle: ignoring invalid Rimz agent alias",
        );
        None
    }
}

fn validate_agent_name_env(raw: String, source: &str) -> Option<String> {
    if rimz::petname::valid_name(&raw)
        && !rimz::petname::collides_with_reserved_prefix(&raw, rimz::agents::known_kinds())
    {
        Some(raw)
    } else {
        warn!(
            agent_name = %raw,
            source,
            "lifecycle: ignoring invalid Rimz agent name",
        );
        None
    }
}

fn manage_agent_context(ctx: AgentContextHook<'_>) {
    let AgentContextHook {
        workspace,
        ledger,
        agent,
        context,
    } = ctx;
    let LifecycleEventContext {
        event_name,
        payload,
        agent_id,
        model_hint,
        turn_ended,
        observed_turn_error,
    } = context;
    // Tombstone the session's statusline context sidecar so it cannot pin stale
    // enrichment to a session the rollup has dropped.
    if agent.ends_session(event_name)
        && let Err(err) = rimz::ledger::agent_context::remove(
            ledger.runtime_paths(),
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
            rimz::agent_activity::touch(ledger.runtime_paths(), agent.descriptor().kind, agent_id)
    {
        warn!(
            agent = agent.descriptor().kind,
            event = %event_name,
            error = %err,
            "lifecycle: failed to touch the agent activity heartbeat",
        );
    }
    if let Some(context_agent_id) = payload_context_agent_id(payload) {
        merge_agent_context_sidecars(
            ledger,
            agent,
            event_name,
            payload,
            context_agent_id,
            model_hint,
            turn_ended,
            observed_turn_error,
        );
    }
    // An adapter can request a detached `rimz` helper after a lifecycle event.
    // Spawned with fresh stdio and never awaited, so it adds no latency to the
    // agent's turn.
    let refresh_ctx = rimz::agents::LifecycleRefreshCtx {
        agent_id,
        workspace_id: workspace.workspace_id.as_str(),
        model_hint,
    };
    if let Some(spawn) = agent.post_lifecycle_refresh(event_name, &refresh_ctx) {
        spawn_refresh_detached(&spawn);
    }
}

fn merge_agent_context_sidecars(
    ledger: &Ledger,
    agent: &dyn AgentAdapter,
    event_name: &str,
    payload: &Value,
    context_agent_id: &str,
    model_hint: Option<&str>,
    turn_ended: bool,
    observed_turn_error: Option<rimz::agents::AgentTurnError>,
) {
    let mut turn_error_updated = false;
    if let Some(marker) = observed_turn_error {
        turn_error_updated |=
            merge_turn_error_marker(ledger, agent, event_name, context_agent_id, marker);
    } else if let Some(marker) = agent.observe_turn_error_from_hook(event_name, payload) {
        turn_error_updated |=
            merge_turn_error_marker(ledger, agent, event_name, context_agent_id, marker);
    } else if turn_error_refresh_event(event_name)
        && let Some(marker) = agent.observe_turn_error(payload)
    {
        turn_error_updated |=
            merge_turn_error_marker(ledger, agent, event_name, context_agent_id, marker);
    }
    if turn_error_updated {
        let _ = rimz::ledger::wakeup::wake_sidebars(ledger.runtime_paths());
    }

    if payload.get("rate_limits").is_some()
        && let Some(context) = agent.observe_context(agent.descriptor().kind, payload)
        && merge_rate_limit_context(ledger, agent, context_agent_id, context)
    {
        let _ = rimz::ledger::wakeup::wake_sidebars(ledger.runtime_paths());
    }

    let prior = rimz::ledger::agent_context::read_one(
        ledger.runtime_paths(),
        agent.descriptor().kind,
        context_agent_id,
    );
    let local_model_hint = model_hint.or_else(|| {
        prior
            .as_ref()
            .and_then(|record| record.context.model_id.as_deref())
    });
    let refresh_ctx = rimz::agents::LocalContextRefreshCtx {
        agent_id: context_agent_id,
        model_hint: local_model_hint,
        prior_effort: prior
            .as_ref()
            .and_then(|record| record.context.effort.as_deref()),
        prior_transcript_path: prior
            .as_ref()
            .and_then(|record| record.transcript_path.as_deref()),
        prior_transcript_stat: prior
            .as_ref()
            .and_then(|record| record.transcript_stat.as_ref()),
    };
    let mut refresh = agent.local_context_refresh(event_name, &refresh_ctx);
    supplement_realtime_cost(
        agent,
        context_agent_id,
        turn_ended,
        prior.as_ref(),
        &mut refresh,
    );
    let Some(refresh) = refresh else {
        return;
    };
    if let Err(err) = rimz::ledger::agent_context::merge_local_context(
        ledger.runtime_paths(),
        agent.descriptor().kind,
        context_agent_id,
        prior,
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
        let _ = rimz::ledger::wakeup::wake_sidebars(ledger.runtime_paths());
    }
}

fn supplement_realtime_cost(
    agent: &dyn AgentAdapter,
    context_agent_id: &str,
    turn_ended: bool,
    prior: Option<&rimz::ledger::agent_context::AgentContextRecord>,
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
    if !partial && prior_total_cost(prior).is_some() {
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
    if prior_total_cost(prior).is_some()
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

    let Some(cost) = rimz::agents::spending::session_cost_usd(
        agent,
        context_agent_id,
        &path,
        &rimz::agents::PriceBook::embedded(),
    ) else {
        return;
    };

    let refresh = refresh.get_or_insert_with(|| rimz::agents::LocalContextRefresh {
        model_id: None,
        effort: prior.and_then(|record| record.context.effort.clone()),
        tokens: prior.and_then(|record| record.context.tokens.clone()),
        cost: None,
        turn_complete: prior.and_then(|record| record.context.turn_complete),
        transcript_path: None,
        transcript_stat: None,
    });
    refresh.cost = Some(cost);
    refresh.transcript_path = Some(path.to_string_lossy().into_owned());
    refresh.transcript_stat = Some(stat);
}

fn realtime_cost_coverage(agent: &dyn AgentAdapter) -> Option<rimz::agents::ConcernCoverage> {
    agent
        .descriptor()
        .coverage
        .iter()
        .find(|(concern, _)| *concern == rimz::agents::IntegrationConcern::RealtimeCost)
        .map(|(_, coverage)| *coverage)
}

fn refresh_total_cost(refresh: Option<&rimz::agents::LocalContextRefresh>) -> Option<f64> {
    refresh
        .and_then(|refresh| refresh.cost.as_ref())
        .and_then(|cost| cost.total_cost_usd)
}

fn prior_total_cost(
    prior: Option<&rimz::ledger::agent_context::AgentContextRecord>,
) -> Option<f64> {
    prior
        .and_then(|record| record.context.cost.as_ref())
        .and_then(|cost| cost.total_cost_usd)
}

fn local_transcript_stat(path: &Path) -> Option<rimz::agents::TranscriptStat> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let since_epoch = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(rimz::agents::TranscriptStat {
        mtime_secs: since_epoch.as_secs().try_into().unwrap_or(i64::MAX),
        mtime_nanos: since_epoch.subsec_nanos(),
        len: meta.len(),
    })
}

fn merge_rate_limit_context(
    ledger: &Ledger,
    agent: &dyn AgentAdapter,
    context_agent_id: &str,
    context: rimz::agents::AgentContext,
) -> bool {
    let Some(rate_limits) = context.rate_limits else {
        return false;
    };
    let kind = agent.descriptor().kind;
    let observed_at = context.observed_at;
    let prior =
        rimz::ledger::agent_context::read_one(ledger.runtime_paths(), kind, context_agent_id);
    let mut record = prior.unwrap_or_else(|| {
        rimz::ledger::agent_context::new_record(
            kind,
            context_agent_id,
            rimz::ledger::agent_context::empty_context(kind, observed_at),
        )
    });
    if record.context.rate_limits.as_ref() == Some(&rate_limits) {
        return false;
    }
    record.context.source = kind.to_owned();
    record.context.rate_limits = Some(rate_limits);
    record.context.observed_at = observed_at;
    record.rate_limits_observed_at = Some(observed_at);
    match rimz::ledger::agent_context::write_record(ledger.runtime_paths(), &record) {
        Ok(()) => true,
        Err(err) => {
            warn!(
                agent = kind,
                session = %context_agent_id,
                tags.operation = "agent.rate_limits_merge",
                error = &err as &dyn std::error::Error,
                "lifecycle: failed to merge rate-limit context",
            );
            false
        }
    }
}

fn turn_error_refresh_event(event_name: &str) -> bool {
    matches!(event_name, "Stop")
}

fn merge_turn_error_marker(
    ledger: &Ledger,
    agent: &dyn AgentAdapter,
    event_name: &str,
    context_agent_id: &str,
    marker: rimz::agents::AgentTurnError,
) -> bool {
    let kind = agent.descriptor().kind;
    let class = marker.class;
    let label = marker.label.clone();
    match rimz::ledger::agent_context::merge_turn_error(
        ledger.runtime_paths(),
        kind,
        context_agent_id,
        marker,
    ) {
        Ok(updated) => {
            if updated {
                // The agent's turn ended on a provider condition (rate limit,
                // overload, or other API failure) — observed, not a Rimz fault.
                // Warn once per transition; the Sentry bridge lifts it to a
                // warning event keyed by `class`.
                warn!(
                    target: "rimz::agent::turn_error",
                    agent = kind,
                    session = %context_agent_id,
                    tags.operation = "agent.turn_error",
                    class = ?class,
                    label = label.as_deref().unwrap_or_default(),
                    "agent turn ended on a provider error",
                );
            }
            updated
        }
        Err(err) => {
            warn!(
                agent = kind,
                session = %context_agent_id,
                event = %event_name,
                tags.operation = "agent.turn_error_merge",
                error = &err as &dyn std::error::Error,
                "lifecycle: failed to merge turn-error marker",
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn turn_end_supplements_partial_realtime_cost_from_prior_transcript() {
        let dir = tempfile::TempDir::new().unwrap();
        let transcript = dir.path().join("2026-06-02T10-00-00-000Z_sess-1.jsonl");
        let mut file = std::fs::File::create(&transcript).unwrap();
        writeln!(
            file,
            r#"{{"type":"message","timestamp":"2026-06-02T10:00:00.000Z","message":{{"role":"assistant","model":"gpt-5","usage":{{"input":100,"output":50,"cost":{{"total":0.42}}}}}}}}"#
        )
        .unwrap();

        let observed_at = jiff::Timestamp::from_second(1_780_394_400).unwrap();
        let mut prior = rimz::ledger::agent_context::new_record(
            "pi",
            "sess-1",
            rimz::ledger::agent_context::empty_context("pi", observed_at),
        );
        prior.transcript_path = Some(transcript.to_string_lossy().into_owned());

        let mut skipped = None;
        supplement_realtime_cost(
            &rimz::agents::PiAdapter,
            "sess-1",
            false,
            Some(&prior),
            &mut skipped,
        );
        assert!(skipped.is_none());

        let mut refresh = None;
        supplement_realtime_cost(
            &rimz::agents::PiAdapter,
            "sess-1",
            true,
            Some(&prior),
            &mut refresh,
        );

        let refresh = refresh.expect("turn end supplements cost");
        let cost = refresh
            .cost
            .and_then(|cost| cost.total_cost_usd)
            .expect("supplemented total cost");
        assert!((cost - 0.42).abs() < 1e-9);
        assert_eq!(
            refresh.transcript_path.as_deref(),
            Some(transcript.to_string_lossy().as_ref())
        );
        assert!(refresh.transcript_stat.is_some());
    }
}

/// Fold this observation's signal onto the prior rollup state through the shared
/// `lifecycle::step` table and log any anomaly once, under the
/// `rimz::agent::lifecycle` target (stderr — never stdout, the hook decision
/// channel). Best-effort: a missing cached snapshot just skips the check. The
/// reducer re-derives the same state on replay, silently — this call exists only
/// to surface a reconciled or ignored transition while we still have the event
/// in hand to attribute it.
pub(super) fn log_lifecycle_transition(
    ledger: &Ledger,
    kind: &str,
    observation: &AgentLifecycleObservation,
) -> Option<agent_lifecycle::Transition> {
    let Some(agent_id) = observation.agent_id.as_deref() else {
        // The reducer quarantines a session-less event (no rollup entry) and
        // stays quiet on replay — this is the once-per-fresh-event warning.
        warn!(
            target: "rimz::agent::lifecycle",
            kind,
            signal = ?observation.signal,
            "session-less agent.lifecycle event — the reducer will quarantine it",
        );
        return None;
    };
    // The prior state for this one agent, from the lock-free cached snapshot —
    // the projection of every event before this one, exactly the `prev` the
    // reducer folds this event onto.
    let snapshot = match ledger.snapshot_cached() {
        Ok(snapshot) => snapshot,
        Err(err) => {
            debug!(
                target: "rimz::agent::lifecycle",
                kind,
                agent_id,
                error = %err,
                "skipped lifecycle transition check because the prior rollup was unreadable",
            );
            return None;
        }
    };
    let prev = snapshot
        .agents
        .into_iter()
        .find(|agent| agent.kind == kind && agent.agent_id == agent_id)
        .map(|agent| agent.lifecycle());
    if prev.is_none() && !observation.signal.establishes_identity() {
        // Create-on-miss: a non-start event for an agent with no prior rollup
        // entry materializes the session, which the reducer does by design.
        // The authoritative reducer logs this same condition at debug! (see
        // `snapshot/project.rs`), and the cached snapshot read here can lag a
        // just-appended start, so a warn! is a per-event false positive that
        // floods the off-box channel. Keep it at debug! for local binding
        // diagnosis, matching the reducer; it never reaches Sentry.
        debug!(
            target: "rimz::agent::binding",
            kind,
            agent_id,
            signal = ?observation.signal,
            "non-start lifecycle event created an unseen session",
        );
    }
    let transition = agent_lifecycle::step(prev.as_ref(), &observation.signal);
    match transition.kind {
        TransitionKind::Reconciled { from, reason } => warn!(
            target: "rimz::agent::lifecycle",
            kind,
            agent_id,
            parent_agent_id = observation.parent_agent_id.as_deref().unwrap_or(""),
            from = ?from,
            to = ?transition.next.status,
            signal = ?observation.signal,
            reason,
            "reconciled lifecycle transition",
        ),
        TransitionKind::Ignored { reason } => debug!(
            target: "rimz::agent::lifecycle",
            kind,
            agent_id,
            signal = ?observation.signal,
            reason,
            "ignored lifecycle signal",
        ),
        TransitionKind::Normal => {}
    }
    Some(transition)
}

pub(super) fn proof_of_work_pre_tool(signal: &LifecycleSignal) -> bool {
    matches!(
        signal,
        LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false
        }
    )
}

pub(super) fn append_lifecycle_event(
    signal: &LifecycleSignal,
    transition: Option<agent_lifecycle::Transition>,
) -> bool {
    !proof_of_work_pre_tool(signal)
        || transition.is_some_and(|transition| {
            transition.compaction_closed
                || matches!(transition.kind, TransitionKind::Reconciled { .. })
        })
}
