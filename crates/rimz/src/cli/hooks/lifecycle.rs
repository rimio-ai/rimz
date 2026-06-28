use super::*;

use std::borrow::Cow;

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
        confirm_sent_message_for_lifecycle(ledger, agent, recorded, &workspace.session_name);
        if recorded.observation.signal == LifecycleSignal::Ended
            && let Some(agent_id) = agent_id
        {
            let kind = rimz::ids::AgentKind::new_unchecked(agent.descriptor().kind);
            if let Err(err) = ledger.archive_messages_for_card(
                &kind,
                &rimz::ids::AgentSessionId::from(agent_id),
                recorded.observation.agent_name.as_deref(),
                "receiver ended",
                &workspace.session_name,
            ) {
                warn!(
                    error = %err,
                    kind = agent.descriptor().kind,
                    agent_id,
                    "lifecycle: failed to archive receiver messages",
                );
            }
        }
        spawn_queue_delivery_if_checkpoint(workspace, ledger, agent, recorded);
        if recorded.appended_lifecycle {
            spawn_auto_rotation_if_due(workspace, ledger);
        }
    }
    Ok(())
}

struct RecordedLifecycle {
    model_hint: Option<String>,
    observation: AgentLifecycleObservation,
    appended_lifecycle: bool,
}

const AUTO_ROTATE_STAMP: &str = "auto-rotate.stamp";
const AUTO_ROTATE_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(60);

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
            observation.agent_name = agent_identity_env(
                &observation,
                rimz::run::ENV_AGENT_NAME,
                validate_agent_name_env,
            );
        }
        if observation.parent_agent_id.is_none()
            && (observation.role.is_none()
                || observation.channel.is_none()
                || observation.profile.is_none()
                || observation.model.is_none()
                || observation.effort.is_none())
        {
            let configured_identity = if observation.model.is_none() || observation.effort.is_none()
            {
                agent.configured_identity()
            } else {
                (None, None)
            };
            fill_root_launch_identity(&mut observation, configured_identity, |observation, var| {
                agent_identity_env(observation, var, validate_non_empty_identity_env)
            });
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
        let appended_lifecycle = if append_lifecycle {
            let event_observation = event_lifecycle_observation(&observation);
            let envelope = EventEnvelope::agent_lifecycle(
                workspace.workspace_id.clone(),
                &workspace.session_name,
                agent.descriptor().kind,
                event_name,
                &event_observation,
            );
            match ledger.append_event_and_expire(&envelope, append_expiry) {
                Ok(_) => true,
                Err(err) => {
                    warn!(
                        agent = agent.descriptor().kind,
                        event = %event_name,
                        error = %err,
                        "lifecycle: failed to record the agent.lifecycle event",
                    );
                    false
                }
            }
        } else {
            false
        };
        return Some(RecordedLifecycle {
            model_hint,
            observation,
            appended_lifecycle,
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

fn event_lifecycle_observation(
    observation: &AgentLifecycleObservation,
) -> Cow<'_, AgentLifecycleObservation> {
    if observation.signal.establishes_identity() {
        return Cow::Borrowed(observation);
    }
    let mut trimmed = observation.clone();
    // High-cadence progress events rely on the reducer's carry-forward
    // projection for per-session constants. A cold first-seen progress event
    // can miss this enrichment until the next identity-establishing event; the
    // sidebar already treats these fields as optional.
    trimmed.transcript_path = None;
    trimmed.worktree_path = None;
    trimmed.worktree_branch = None;
    // Lazy adapters can first recover their pane binding on TurnStarted, so the
    // reducer needs every event pane stamp that focus recovery supplies.
    Cow::Owned(trimmed)
}

fn spawn_auto_rotation_if_due(workspace: &ResolvedWorkspace, ledger: &Ledger) {
    let Ok(meta) = std::fs::metadata(&ledger.paths().events_log) else {
        return;
    };
    if !auto_rotation_size_due(meta.len()) {
        return;
    }
    if !auto_rotation_stamp_due(auto_rotate_stamp_age(ledger)) {
        return;
    }
    touch_auto_rotate_stamp(ledger);
    spawn_refresh_detached(&rimz::agents::RefreshSpawn {
        args: vec![
            "--root".to_owned(),
            workspace.project_root.display().to_string(),
            "workspace".to_owned(),
            "rotate-events".to_owned(),
        ],
    });
}

fn auto_rotation_size_due(log_len: u64) -> bool {
    log_len >= crate::cli::workspace::DEFAULT_EVENT_LOG_ROTATE_BYTES
}

fn auto_rotation_stamp_due(stamp_age: Option<std::time::Duration>) -> bool {
    stamp_age.is_none_or(|age| age >= AUTO_ROTATE_DEBOUNCE)
}

fn auto_rotate_stamp_age(ledger: &Ledger) -> Option<std::time::Duration> {
    let modified = std::fs::metadata(ledger.paths().locks_dir.join(AUTO_ROTATE_STAMP))
        .ok()?
        .modified()
        .ok()?;
    std::time::SystemTime::now().duration_since(modified).ok()
}

fn touch_auto_rotate_stamp(ledger: &Ledger) {
    let _ = std::fs::write(ledger.paths().locks_dir.join(AUTO_ROTATE_STAMP), b"");
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

fn confirm_sent_message_for_lifecycle(
    ledger: &Ledger,
    agent: &dyn AgentAdapter,
    recorded: &RecordedLifecycle,
    session_name: &str,
) {
    let body = match recorded.observation.signal {
        LifecycleSignal::TurnStarted => rimz::message::MessageBody::Prompt,
        LifecycleSignal::Compacting => rimz::message::MessageBody::Command,
        _ => return,
    };
    let Some(agent_id) = recorded.observation.agent_id.as_ref() else {
        return;
    };
    let kind = rimz::ids::AgentKind::new_unchecked(agent.descriptor().kind);
    if let Err(err) = ledger.confirm_delivered_for_card(
        &kind,
        agent_id,
        recorded.observation.agent_name.as_deref(),
        body,
        session_name,
    ) {
        warn!(
            agent = agent.descriptor().kind,
            agent_id = %agent_id,
            error = %err,
            "lifecycle: failed to confirm sent message delivery",
        );
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
                "message delivery skipped; queued messages unreadable",
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
        jiff::Timestamp::now(),
    ) else {
        return;
    };
    spawn_refresh_detached(&rimz::agents::RefreshSpawn {
        args: vec![
            "--root".to_owned(),
            workspace.project_root.display().to_string(),
            "message".to_owned(),
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

type IdentityValidator = fn(String, &str, &str) -> Option<String>;

fn agent_identity_env(
    observation: &AgentLifecycleObservation,
    var: &str,
    validate: IdentityValidator,
) -> Option<String> {
    std::env::var(var)
        .ok()
        .and_then(|raw| validate(raw, "env", var))
        .or_else(|| {
            let raw = rimz::proc::env_var(observation.agent_pid?, var)?;
            validate(raw, "process", var)
        })
}

pub(super) fn fill_root_launch_identity(
    observation: &mut AgentLifecycleObservation,
    configured_identity: (Option<String>, Option<String>),
    mut identity_env: impl FnMut(&AgentLifecycleObservation, &'static str) -> Option<String>,
) {
    if observation.parent_agent_id.is_some() {
        return;
    }
    if observation.role.is_none() {
        observation.role = identity_env(observation, rimz::run::ENV_AGENT_ROLE);
    }
    if observation.team.is_none() {
        observation.team = identity_env(observation, rimz::run::ENV_TEAM);
    }
    if observation.channel.is_none() {
        observation.channel = identity_env(observation, rimz::run::ENV_CHANNEL);
    }
    if observation.profile.is_none() {
        observation.profile = identity_env(observation, rimz::run::ENV_AGENT_PROFILE);
    }
    if observation.model.is_none() {
        observation.model =
            identity_env(observation, rimz::run::ENV_AGENT_MODEL).or(configured_identity.0);
    }
    if observation.effort.is_none() {
        observation.effort =
            identity_env(observation, rimz::run::ENV_AGENT_EFFORT).or(configured_identity.1);
    }
}

fn validate_agent_name_env(raw: String, source: &str, _var: &str) -> Option<String> {
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

fn validate_non_empty_identity_env(raw: String, source: &str, var: &str) -> Option<String> {
    let value = raw.trim();
    if !value.is_empty() {
        Some(value.to_owned())
    } else {
        warn!(
            env_var = var,
            source, "lifecycle: ignoring empty Rimz agent identity",
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
        merge_agent_context_sidecars(ContextSidecarInput {
            ledger,
            agent,
            event_name,
            payload,
            context_agent_id,
            model_hint,
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
    if let Some(spawn) = agent.post_lifecycle_refresh(event_name, &refresh_ctx) {
        spawn_refresh_detached(&spawn);
    }
}

struct ContextSidecarInput<'a> {
    ledger: &'a Ledger,
    agent: &'a dyn AgentAdapter,
    event_name: &'a str,
    payload: &'a Value,
    context_agent_id: &'a str,
    model_hint: Option<&'a str>,
    turn_ended: bool,
    observed_turn_error: Option<rimz::agents::AgentTurnError>,
}

fn merge_agent_context_sidecars(input: ContextSidecarInput<'_>) {
    let ContextSidecarInput {
        ledger,
        agent,
        event_name,
        payload,
        context_agent_id,
        model_hint,
        turn_ended,
        observed_turn_error,
    } = input;
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

    if payload_carries_observed_context(payload)
        && let Some(context) = agent.observe_context(agent.descriptor().kind, payload)
        && merge_observed_context(ledger, agent, context_agent_id, context)
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

const OBSERVED_CONTEXT_KEYS: &[&str] = &[
    "model",
    "effort",
    "rate_limits",
    "total_cost_usd",
    "context_window",
    "total_tokens",
    "context_pct",
];

fn payload_carries_observed_context(payload: &Value) -> bool {
    OBSERVED_CONTEXT_KEYS
        .iter()
        .any(|key| payload.get(*key).is_some())
}

fn merge_observed_context(
    ledger: &Ledger,
    agent: &dyn AgentAdapter,
    context_agent_id: &str,
    context: rimz::agents::AgentContext,
) -> bool {
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
    let mut changed = false;
    if let Some(rate_limits) = context.rate_limits
        && record.context.rate_limits.as_ref() != Some(&rate_limits)
    {
        record.context.rate_limits = Some(rate_limits);
        record.rate_limits_observed_at = Some(observed_at);
        changed = true;
    }
    if let Some(tokens) = context.tokens {
        changed |= merge_observed_tokens(&mut record.context.tokens, tokens);
    }
    if let Some(model_id) = context.model_id
        && record.context.model_id.as_ref() != Some(&model_id)
    {
        record.context.model_id = Some(model_id);
        changed = true;
    }
    if let Some(effort) = context.effort
        && record.context.effort.as_ref() != Some(&effort)
    {
        record.context.effort = Some(effort);
        changed = true;
    }
    if let Some(cost) = context.cost
        && let Some(total_cost_usd) = cost.total_cost_usd
    {
        let prior_total_cost = record
            .context
            .cost
            .as_ref()
            .and_then(|cost| cost.total_cost_usd);
        if prior_total_cost.is_none_or(|prior| total_cost_usd >= prior) {
            changed |= merge_observed_cost(&mut record.context.cost, cost, total_cost_usd);
        }
    }
    if !changed {
        return false;
    }
    record.context.source = kind.to_owned();
    record.context.observed_at = observed_at;
    match rimz::ledger::agent_context::write_record(ledger.runtime_paths(), &record) {
        Ok(()) => true,
        Err(err) => {
            warn!(
                agent = kind,
                session = %context_agent_id,
                tags.operation = "agent.context_observed_merge",
                error = &err as &dyn std::error::Error,
                "lifecycle: failed to merge observed context",
            );
            false
        }
    }
}

fn merge_observed_tokens(
    prior: &mut Option<rimz::agents::AgentTokenUsage>,
    incoming: rimz::agents::AgentTokenUsage,
) -> bool {
    let target = prior.get_or_insert_with(rimz::agents::AgentTokenUsage::default);
    let before = target.clone();
    if incoming.context_window_size.is_some() {
        target.context_window_size = incoming.context_window_size;
    }
    if incoming.used_percentage.is_some() {
        target.used_percentage = incoming.used_percentage;
    }
    if incoming.remaining_percentage.is_some() {
        target.remaining_percentage = incoming.remaining_percentage;
    }
    if let Some(current_usage) = incoming.current_usage {
        target.current_usage = Some(current_usage);
    }
    *target != before
}

fn merge_observed_cost(
    prior: &mut Option<rimz::agents::AgentCost>,
    incoming: rimz::agents::AgentCost,
    total_cost_usd: f64,
) -> bool {
    let target = prior.get_or_insert_with(rimz::agents::AgentCost::default);
    let before = target.clone();
    target.total_cost_usd = Some(total_cost_usd);
    if incoming.total_duration_ms.is_some() {
        target.total_duration_ms = incoming.total_duration_ms;
    }
    if incoming.total_api_duration_ms.is_some() {
        target.total_api_duration_ms = incoming.total_api_duration_ms;
    }
    if incoming.total_lines_added.is_some() {
        target.total_lines_added = incoming.total_lines_added;
    }
    if incoming.total_lines_removed.is_some() {
        target.total_lines_removed = incoming.total_lines_removed;
    }
    *target != before
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn test_ledger() -> (tempfile::TempDir, Ledger) {
        let dir = tempfile::TempDir::new().unwrap();
        let workspace_id =
            rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/hooks-test"));
        let paths = rimz::ledger::StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        let runtime = rimz::ledger::RuntimePaths::under(workspace_id, dir.path()).unwrap();
        let ledger = Ledger::open(paths, runtime).unwrap();
        (dir, ledger)
    }

    fn workspace_id() -> rimz::ids::WorkspaceId {
        rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/hooks-test"))
    }

    fn observed_at() -> jiff::Timestamp {
        jiff::Timestamp::from_second(1_700_000_000).unwrap()
    }

    fn observed_context() -> rimz::agents::AgentContext {
        rimz::agents::AgentContext {
            source: "pi".to_owned(),
            session_name: None,
            session_preview: None,
            model_id: Some("gpt-5.5".to_owned()),
            model_display_name: None,
            effort: Some("high".to_owned()),
            thinking_enabled: None,
            output_style: None,
            vim_mode: None,
            agent_version: None,
            exceeds_200k_tokens: None,
            cost: Some(rimz::agents::AgentCost {
                total_cost_usd: Some(0.5),
                ..rimz::agents::AgentCost::default()
            }),
            tokens: Some(rimz::agents::AgentTokenUsage {
                context_window_size: Some(272_000),
                used_percentage: Some(42),
                remaining_percentage: None,
                current_usage: Some(rimz::agents::AgentCurrentUsage {
                    input_tokens: Some(10),
                    output_tokens: Some(2),
                    cache_creation_input_tokens: Some(4),
                    cache_read_input_tokens: Some(30),
                }),
            }),
            rate_limits: Some(rimz::agents::AgentRateLimits {
                windows: vec![rimz::agents::RateLimitWindow {
                    used_percentage: Some(72),
                    resets_at: None,
                    duration_mins: Some(300),
                    observed_at: Some(observed_at()),
                    source: rimz::agents::context::WindowSource::BestEffort,
                }],
            }),
            pr: None,
            account: None,
            turn_error: None,
            turn_complete: None,
            observed_at: observed_at(),
        }
    }

    #[test]
    fn lifecycle_event_observation_trims_carry_forward_fields_after_identity() {
        let mut observation = AgentLifecycleObservation::new(
            Some(rimz::ids::AgentSessionId::from("sess-1")),
            LifecycleSignal::Registered,
        );
        observation.transcript_path = Some("/tmp/transcript.jsonl".to_owned());
        observation.worktree_path = Some("/tmp/project".to_owned());
        observation.worktree_branch = Some("feature".to_owned());
        observation.pane_id = Some(PaneId::from_parts(MuxName::Tmux, "%1"));

        let identity = event_lifecycle_observation(&observation);
        assert_eq!(
            identity.transcript_path.as_deref(),
            Some("/tmp/transcript.jsonl")
        );
        assert_eq!(identity.worktree_path.as_deref(), Some("/tmp/project"));
        assert_eq!(identity.worktree_branch.as_deref(), Some("feature"));
        assert_eq!(identity.pane_id.as_ref().map(PaneId::raw), Some("%1"));

        observation.signal = LifecycleSignal::TurnStarted;
        let trimmed = event_lifecycle_observation(&observation);
        assert!(trimmed.transcript_path.is_none());
        assert!(trimmed.worktree_path.is_none());
        assert!(trimmed.worktree_branch.is_none());
        assert_eq!(trimmed.pane_id.as_ref().map(PaneId::raw), Some("%1"));
        assert_eq!(
            observation.transcript_path.as_deref(),
            Some("/tmp/transcript.jsonl"),
            "downstream run-record/context paths keep the full observation"
        );
    }

    #[test]
    fn auto_rotation_decision_respects_threshold_and_debounce() {
        let threshold = crate::cli::workspace::DEFAULT_EVENT_LOG_ROTATE_BYTES;
        assert!(!auto_rotation_size_due(threshold - 1));
        assert!(auto_rotation_size_due(threshold));
        assert!(auto_rotation_stamp_due(None));
        assert!(!auto_rotation_stamp_due(Some(
            AUTO_ROTATE_DEBOUNCE - std::time::Duration::from_secs(1)
        )));
        assert!(auto_rotation_stamp_due(Some(AUTO_ROTATE_DEBOUNCE)));
    }

    #[test]
    fn observed_context_merge_preserves_fields_and_keeps_cost_monotonic() {
        let (_dir, ledger) = test_ledger();
        let agent = rimz::agents::PiAdapter;

        assert!(merge_observed_context(
            &ledger,
            &agent,
            "sess-1",
            observed_context()
        ));
        let first =
            rimz::ledger::agent_context::read_one(ledger.runtime_paths(), "pi", "sess-1").unwrap();
        assert_eq!(first.context.model_id.as_deref(), Some("gpt-5.5"));
        assert_eq!(first.context.effort.as_deref(), Some("high"));
        assert_eq!(
            first
                .context
                .cost
                .as_ref()
                .and_then(|cost| cost.total_cost_usd),
            Some(0.5)
        );
        assert_eq!(
            first
                .context
                .tokens
                .as_ref()
                .and_then(|tokens| tokens.current_usage.as_ref())
                .and_then(|usage| usage.cache_read_input_tokens),
            Some(30)
        );
        assert_eq!(
            first
                .context
                .rate_limits
                .as_ref()
                .map(|limits| limits.windows.len()),
            Some(1)
        );

        assert!(
            !merge_observed_context(&ledger, &agent, "sess-1", observed_context()),
            "an identical envelope is a no-op"
        );
        let after_repeat =
            rimz::ledger::agent_context::read_one(ledger.runtime_paths(), "pi", "sess-1").unwrap();
        assert_eq!(after_repeat, first);

        let mut lower_cost = observed_context();
        lower_cost.model_id = None;
        lower_cost.effort = None;
        lower_cost.tokens = None;
        lower_cost.rate_limits = None;
        lower_cost.cost = Some(rimz::agents::AgentCost {
            total_cost_usd: Some(0.25),
            ..rimz::agents::AgentCost::default()
        });
        assert!(
            !merge_observed_context(&ledger, &agent, "sess-1", lower_cost),
            "a resume-reset extension accumulator must not lower displayed cost"
        );
        let after_lower =
            rimz::ledger::agent_context::read_one(ledger.runtime_paths(), "pi", "sess-1").unwrap();
        assert_eq!(after_lower, first);

        let mut partial_tokens = observed_context();
        partial_tokens.cost = None;
        partial_tokens.rate_limits = None;
        partial_tokens.tokens = Some(rimz::agents::AgentTokenUsage {
            context_window_size: Some(300_000),
            used_percentage: None,
            remaining_percentage: None,
            current_usage: None,
        });
        assert!(merge_observed_context(
            &ledger,
            &agent,
            "sess-1",
            partial_tokens
        ));
        let merged =
            rimz::ledger::agent_context::read_one(ledger.runtime_paths(), "pi", "sess-1").unwrap();
        let tokens = merged.context.tokens.as_ref().unwrap();
        assert_eq!(tokens.context_window_size, Some(300_000));
        assert_eq!(tokens.used_percentage, Some(42));
        assert_eq!(
            tokens
                .current_usage
                .as_ref()
                .and_then(|usage| usage.input_tokens),
            Some(10),
            "missing token subfields preserve the last known values"
        );
    }

    #[test]
    fn lifecycle_context_merge_accepts_model_and_effort_only_enrichment() {
        let (_dir, ledger) = test_ledger();
        let workspace = rimz::ResolvedWorkspace {
            workspace_id: workspace_id(),
            project_root: std::path::PathBuf::from("/tmp/hooks-test"),
            root_class: rimz::workspace::RootClass::Directory,
            worktree_root: std::path::PathBuf::from("/tmp/hooks-test"),
            worktree_branch: None,
            session_name: "hooks-test".to_owned(),
            mux_hint: None,
        };
        let globals = GlobalFlags {
            mux: None,
            root: None,
            color: crate::cli::ColorWhen::Never,
        };

        handle_lifecycle_hook(
            &workspace,
            &ledger,
            &rimz::agents::PiAdapter,
            "model_select",
            &serde_json::json!({ "session_id": "sess-1", "model": "gpt-5.5" }),
            &globals,
        )
        .unwrap();
        handle_lifecycle_hook(
            &workspace,
            &ledger,
            &rimz::agents::PiAdapter,
            "thinking_level_select",
            &serde_json::json!({ "session_id": "sess-1", "effort": "high" }),
            &globals,
        )
        .unwrap();
        let merged =
            rimz::ledger::agent_context::read_one(ledger.runtime_paths(), "pi", "sess-1").unwrap();
        assert_eq!(merged.context.model_id.as_deref(), Some("gpt-5.5"));
        assert_eq!(merged.context.effort.as_deref(), Some("high"));
        assert!(ledger.snapshot().unwrap().agents.is_empty());
    }

    #[test]
    fn lifecycle_confirms_matching_message_body() {
        let (_dir, ledger) = test_ledger();
        let agent = test_agent();
        let command = rimz::message::MessageRecord::new(
            workspace_id(),
            &agent,
            "/compact".to_owned(),
            true,
            rimz::message::DeliveryGate::Done,
        )
        .with_body(rimz::message::MessageBody::Command);
        let prompt = rimz::message::MessageRecord::new(
            workspace_id(),
            &agent,
            "real prompt".to_owned(),
            true,
            rimz::message::DeliveryGate::Done,
        );
        ledger
            .record_sent_message(&command, "session")
            .unwrap()
            .expect("command sent");
        ledger
            .record_sent_message(&prompt, "session")
            .unwrap()
            .expect("prompt sent");

        let compact_observation = AgentLifecycleObservation::new(
            Some(agent.agent_id.clone()),
            LifecycleSignal::Compacting,
        );
        confirm_sent_message_for_lifecycle(
            &ledger,
            &rimz::agents::ClaudeAdapter,
            &RecordedLifecycle {
                model_hint: None,
                observation: compact_observation,
                appended_lifecycle: false,
            },
            "session",
        );
        let messages = ledger.list_messages().unwrap();
        assert_eq!(
            messages
                .iter()
                .find(|message| message.message_id == command.message_id)
                .unwrap()
                .status,
            rimz::message::MessageStatus::Delivered
        );
        assert_eq!(
            messages
                .iter()
                .find(|message| message.message_id == prompt.message_id)
                .unwrap()
                .status,
            rimz::message::MessageStatus::Sent,
            "compaction cannot confirm the prompt behind it"
        );

        let mut real_observation = AgentLifecycleObservation::new(
            Some(agent.agent_id.clone()),
            LifecycleSignal::TurnStarted,
        );
        real_observation.prompt = Some("real prompt".to_owned());
        confirm_sent_message_for_lifecycle(
            &ledger,
            &rimz::agents::ClaudeAdapter,
            &RecordedLifecycle {
                model_hint: None,
                observation: real_observation,
                appended_lifecycle: false,
            },
            "session",
        );
        let messages = ledger.list_messages().unwrap();
        assert_eq!(
            messages
                .iter()
                .find(|message| message.message_id == prompt.message_id)
                .unwrap()
                .status,
            rimz::message::MessageStatus::Delivered
        );
    }

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
        prior.context.cost = Some(rimz::agents::AgentCost {
            total_cost_usd: Some(0.25),
            ..rimz::agents::AgentCost::default()
        });
        prior.context.tokens = Some(rimz::agents::AgentTokenUsage {
            context_window_size: Some(128_000),
            used_percentage: Some(10),
            remaining_percentage: None,
            current_usage: Some(rimz::agents::AgentCurrentUsage {
                input_tokens: Some(12_800),
                output_tokens: None,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            }),
        });

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
            .as_ref()
            .and_then(|cost| cost.total_cost_usd)
            .expect("supplemented total cost");
        assert!((cost - 0.42).abs() < 1e-9);
        assert_eq!(
            refresh.transcript_path.as_deref(),
            Some(transcript.to_string_lossy().as_ref())
        );
        assert!(refresh.transcript_stat.is_some());
        assert_eq!(
            refresh
                .tokens
                .as_ref()
                .and_then(|tokens| tokens.used_percentage),
            Some(10),
            "the reconciling walk keeps live tokens when Pi's JSONL has none"
        );

        let workspace_id =
            rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/hooks-test"));
        let runtime = rimz::ledger::RuntimePaths::under(workspace_id, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        rimz::ledger::agent_context::merge_local_context(
            &runtime,
            "pi",
            "sess-1",
            Some(prior),
            refresh,
            observed_at,
        )
        .unwrap();
        let merged = rimz::ledger::agent_context::read_one(&runtime, "pi", "sess-1").unwrap();
        assert_eq!(
            merged
                .context
                .cost
                .as_ref()
                .and_then(|cost| cost.total_cost_usd),
            Some(0.42),
            "the turn-end transcript walk overwrites the live push with the authoritative sum"
        );
    }

    fn test_agent() -> AgentState {
        let now = jiff::Timestamp::now();
        AgentState {
            agent_id: rimz::ids::AgentSessionId::from("sess-1"),
            kind: rimz::ids::AgentKind::new_unchecked("claude"),
            name: None,
            kind_ordinal: None,
            profile: None,
            role: None,
            team: None,
            channel: None,
            status: rimz::agents::AgentStatus::Idle,
            phase: rimz::agents::TurnPhase::Idle,
            pane: None,
            agent_pid: None,
            agent_process_start: None,
            runtime_owner: None,
            parent_agent_id: None,
            worktree_path: None,
            worktree_branch: None,
            task: None,
            prompt: None,
            description: None,
            transcript_path: None,
            origin: None,
            recent_prompts: Vec::new(),
            model: None,
            effort: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            context: None,
            subagent_description: None,
            subagent_started_at: None,
            turn_started_at: None,
            compacting_since: None,
            compaction_count: 0,
            last_seen: now,
            last_activity: now,
            registered_at: Some(now),
        }
    }
}
