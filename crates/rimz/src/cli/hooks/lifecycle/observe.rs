//! Lifecycle observation ingestion and transition checks.

use super::*;

pub(super) fn record_lifecycle_observation(
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
                rimz::harness::run::ENV_AGENT_NAME,
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

pub(super) fn event_lifecycle_observation(
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
    trimmed.role = None;
    trimmed.team = None;
    trimmed.channel = None;
    trimmed.profile = None;
    // Lazy adapters can first recover their pane binding on TurnStarted, so the
    // reducer needs every event pane stamp that focus recovery supplies.
    Cow::Owned(trimmed)
}

pub(super) fn expiry_scope_for_event_name(
    agent: &dyn AgentAdapter,
    event_name: &str,
) -> Option<AskExpiry> {
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

pub(super) fn expiry_scope_for_signal(signal: &LifecycleSignal) -> Option<AskExpiry> {
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

pub(in crate::cli::hooks) fn append_lifecycle_event(
    signal: &LifecycleSignal,
    transition: Option<agent_lifecycle::Transition>,
) -> bool {
    !proof_of_work_pre_tool(signal)
        || transition.is_some_and(|transition| {
            transition.compaction_closed
                || matches!(transition.kind, TransitionKind::Reconciled { .. })
        })
}
