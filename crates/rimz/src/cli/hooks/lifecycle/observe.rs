//! Lifecycle observation ingestion and transition checks.

use super::*;
use rimz::agents::{HookIngressOwner, SubagentCorrelationInput, SubagentSpawnInput};

const MAX_SUBAGENT_PARENT_CANDIDATES: usize = 64;

pub(super) fn record_lifecycle_observation(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &dyn AgentAdapter,
    event_name: &str,
    payload: &Value,
    ingress_owner: HookIngressOwner,
    globals: &GlobalFlags,
) -> Option<RecordedLifecycle> {
    let mut observation = agent.observe_lifecycle(event_name, payload)?;
    if let LifecycleSignal::AwaitingInput { ask_id, detail, .. } = &mut observation.signal {
        ask_id.get_or_insert_with(rimz::ids::AskId::new);
        if detail.is_none() {
            *detail = agent.ask_detail(event_name, payload);
        }
    }
    attach_agent_owner(ingress_owner, &mut observation);
    attach_agent_pane(&mut observation);
    correlate_subagent_observation(workspace, store, agent, &mut observation);
    Some(record_mapped_lifecycle_observation(
        workspace,
        store,
        agent,
        event_name,
        observation,
        globals,
    ))
}

pub(super) fn record_derived_lifecycle_observation(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &dyn AgentAdapter,
    event_name: &str,
    mut observation: AgentLifecycleObservation,
    ingress_owner: HookIngressOwner,
    globals: &GlobalFlags,
) -> RecordedLifecycle {
    attach_agent_owner(ingress_owner, &mut observation);
    attach_agent_pane(&mut observation);
    record_mapped_lifecycle_observation(workspace, store, agent, event_name, observation, globals)
}

fn record_mapped_lifecycle_observation(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &dyn AgentAdapter,
    event_name: &str,
    mut observation: AgentLifecycleObservation,
    globals: &GlobalFlags,
) -> RecordedLifecycle {
    // Launch identity belongs to the pane's root session. A child stop can
    // omit its optional label fields; filling those from the parent
    // process environment would overwrite the child's carried identity.
    if observation.parent_agent_id.is_none() && observation.agent_name.is_none() {
        observation.agent_name = agent_identity_env(
            &observation,
            rimz::harness::run::ENV_AGENT_NAME,
            validate_agent_name_env,
        );
    }
    if observation.parent_agent_id.is_none()
        && (observation.launch.role.is_none()
            || observation.launch.channel.is_none()
            || observation.launch.profile.is_none()
            || observation.launch.model.is_none()
            || observation.launch.effort.is_none())
    {
        let configured_identity =
            if observation.launch.model.is_none() || observation.launch.effort.is_none() {
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
    enrich_pane_stamp_from_cache(workspace, store, &mut observation);
    recover_focused_pane_binding(
        agent.descriptor().kind,
        agent.descriptor().capabilities.registers_lazily,
        globals.mux,
        workspace,
        store,
        &mut observation,
    );
    let model_hint = observation.launch.model.clone();
    // Validate the transition this event drives against the prior rollup
    // and log any anomaly once, here at ingestion. Replay re-derives the
    // same state silently.
    let transition = log_lifecycle_transition(store, agent.descriptor().kind, &observation);
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
    let waiting_cleared = transition.is_some_and(|transition| transition.waiting_cleared);
    // Capture child rows before the triggering event changes their shape. A
    // parent Stop can make a resting provisional root look superseded, while
    // keyed child evidence can update a self-registered root before the
    // guarded adoption append below reparents it.
    let pre_adoption_snapshot = ((observation.parent_agent_id.is_none()
        && matches!(
            observation.signal,
            LifecycleSignal::ToolUsed { .. } | LifecycleSignal::TurnEnded { .. }
        ))
        || (observation.parent_agent_id.is_some()
            && matches!(
                observation.signal,
                LifecycleSignal::SubagentStarted | LifecycleSignal::SubagentStopped { .. }
            )))
    .then(|| store.snapshot_cached().ok())
    .flatten();
    let (rotation_due, observation_recorded) =
        match store.append_agent_lifecycle(AgentLifecycleIntent {
            session_name: &workspace.session_name,
            agent_kind: rimz::ids::AgentKind::new_unchecked(agent.descriptor().kind),
            event_name,
            observation: &observation,
            transition,
        }) {
            Ok(AgentLifecycleOutcome::RotationDue) => (true, true),
            Ok(AgentLifecycleOutcome::Suppressed | AgentLifecycleOutcome::Appended) => {
                (false, true)
            }
            Err(err) => {
                warn!(
                    agent = agent.descriptor().kind,
                    event = %event_name,
                    error = %err,
                    "lifecycle: failed to record the agent.lifecycle event",
                );
                (false, false)
            }
        };
    if observation_recorded {
        adopt_observed_subagent(
            workspace,
            store,
            agent,
            &observation,
            pre_adoption_snapshot.as_ref(),
        );
        reconcile_spawned_subagents(
            workspace,
            store,
            agent,
            &observation,
            pre_adoption_snapshot.as_ref(),
        );
    }
    RecordedLifecycle {
        model_hint,
        observation,
        rotation_due,
        waiting_cleared,
    }
}

fn adopt_observed_subagent(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &dyn AgentAdapter,
    observation: &AgentLifecycleObservation,
    pre_adoption_snapshot: Option<&rimz::store::snapshot::SidebarSnapshot>,
) {
    if !matches!(
        observation.signal,
        LifecycleSignal::SubagentStarted | LifecycleSignal::SubagentStopped { .. }
    ) {
        return;
    }
    let (Some(child_id), Some(parent_id), Some(snapshot)) = (
        observation.agent_id.as_ref(),
        observation.parent_agent_id.as_ref(),
        pre_adoption_snapshot,
    ) else {
        return;
    };
    let child_is_unparented_root = snapshot.agents.iter().any(|state| {
        state.kind.as_str() == agent.descriptor().kind
            && state.agent_id == *child_id
            && state.parent_agent_id.is_none()
    });
    if !child_is_unparented_root {
        return;
    }
    append_subagent_adoption(
        workspace,
        store,
        agent,
        snapshot,
        parent_id,
        observation.clone(),
        observation.signal.clone(),
    );
}

fn reconcile_spawned_subagents(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &dyn AgentAdapter,
    parent_observation: &AgentLifecycleObservation,
    pre_adoption_snapshot: Option<&rimz::store::snapshot::SidebarSnapshot>,
) {
    if parent_observation.parent_agent_id.is_some()
        || !matches!(
            parent_observation.signal,
            LifecycleSignal::ToolUsed { .. } | LifecycleSignal::TurnEnded { .. }
        )
    {
        return;
    }
    let Some(parent_id) = parent_observation.agent_id.as_ref() else {
        return;
    };
    let spawned = agent.spawned_subagents(SubagentSpawnInput {
        parent_agent_id: parent_id,
        parent_transcript_path: parent_observation.transcript_path.as_deref().map(Path::new),
        parent_workspace: parent_observation.worktree_path.as_deref().map(Path::new),
    });
    if spawned.is_empty() {
        return;
    }
    let Some(snapshot) = pre_adoption_snapshot else {
        debug!(
            kind = agent.descriptor().kind,
            parent_id = parent_id.as_str(),
            "lifecycle: skipped subagent adoption because the prior rollup was unreadable",
        );
        return;
    };
    for child in spawned {
        let child_state = snapshot.agents.iter().find(|state| {
            state.kind.as_str() == agent.descriptor().kind && state.agent_id == child.child_agent_id
        });
        let errored =
            child_state.is_some_and(|state| state.status == rimz::agents::AgentStatus::Failed);
        let mut observation = AgentLifecycleObservation::new(
            Some(child.child_agent_id),
            LifecycleSignal::SubagentStopped { errored },
        );
        observation.agent_name = child.agent_name;
        observation.launch.role = child.role.clone();
        observation.launch.model = child.model.clone();
        observation.task = child.role.or_else(|| child.prompt.clone());
        observation.prompt = child.prompt;
        observation.total_tokens = child.total_tokens;
        observation.pane_id = parent_observation.pane_id.clone();
        if child_state.is_some_and(|state| state.parent_agent_id.is_some()) {
            append_subagent_reconciliation(
                workspace,
                store,
                agent,
                snapshot,
                parent_id,
                observation,
            );
        } else {
            append_subagent_adoption(
                workspace,
                store,
                agent,
                snapshot,
                parent_id,
                observation,
                LifecycleSignal::SubagentStopped { errored },
            );
        }
    }
}

fn append_subagent_reconciliation(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &dyn AgentAdapter,
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    parent_id: &rimz::ids::AgentSessionId,
    mut observation: AgentLifecycleObservation,
) {
    let Some(child_id) = observation.agent_id.as_ref() else {
        return;
    };
    let root_parent_id = snapshot
        .agents
        .iter()
        .find(|state| {
            state.kind.as_str() == agent.descriptor().kind && state.agent_id == *parent_id
        })
        .and_then(|state| state.parent_agent_id.clone())
        .unwrap_or_else(|| parent_id.clone());
    let Some(child_state) = snapshot.agents.iter().find(|state| {
        state.kind.as_str() == agent.descriptor().kind && state.agent_id == *child_id
    }) else {
        return;
    };
    if child_state.parent_agent_id.as_ref() != Some(&root_parent_id)
        || child_state
            .pane
            .as_ref()
            .is_some_and(|pane| observation.pane_id.as_ref() != Some(&pane.pane_id))
    {
        return;
    }
    let model_changed = observation
        .launch
        .model
        .as_ref()
        .is_some_and(|model| child_state.model.as_ref() != Some(model));
    let tokens_changed = observation
        .total_tokens
        .is_some_and(|tokens| child_state.total_tokens != Some(tokens));
    if !model_changed && !tokens_changed {
        return;
    }
    observation.agent_name = None;
    observation.launch.role = None;
    observation.task = None;
    observation.prompt = None;
    observation.parent_agent_id = Some(root_parent_id);
    let errored = child_state.status == rimz::agents::AgentStatus::Failed;
    observation.signal = LifecycleSignal::SubagentStopped { errored };
    let transition = log_lifecycle_transition(store, agent.descriptor().kind, &observation);
    if let Err(err) = store.append_agent_lifecycle(AgentLifecycleIntent {
        session_name: &workspace.session_name,
        agent_kind: rimz::ids::AgentKind::new_unchecked(agent.descriptor().kind),
        event_name: "SubagentReconciled",
        observation: &observation,
        transition,
    }) {
        warn!(
            agent = agent.descriptor().kind,
            event = "SubagentReconciled",
            child_id = observation.agent_id.as_deref().unwrap_or(""),
            error = %err,
            "lifecycle: failed to record subagent metadata reconciliation",
        );
    }
}

fn append_subagent_adoption(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &dyn AgentAdapter,
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    parent_id: &rimz::ids::AgentSessionId,
    mut observation: AgentLifecycleObservation,
    signal: LifecycleSignal,
) {
    let Some(child_id) = observation.agent_id.as_ref() else {
        return;
    };
    if child_id == parent_id {
        return;
    }
    let child_state = snapshot.agents.iter().find(|state| {
        state.kind.as_str() == agent.descriptor().kind && state.agent_id == *child_id
    });
    if child_state.is_some_and(|state| state.parent_agent_id.is_some()) {
        return;
    }
    if child_state
        .and_then(|state| state.pane.as_ref())
        .is_some_and(|pane| observation.pane_id.as_ref() != Some(&pane.pane_id))
    {
        return;
    }
    let root_parent_id = snapshot
        .agents
        .iter()
        .find(|state| {
            state.kind.as_str() == agent.descriptor().kind && state.agent_id == *parent_id
        })
        .and_then(|state| state.parent_agent_id.clone())
        .unwrap_or_else(|| parent_id.clone());
    observation.signal = signal;
    observation.parent_agent_id = Some(root_parent_id);
    let transition = log_lifecycle_transition(store, agent.descriptor().kind, &observation);
    if let Err(err) = store.append_agent_lifecycle(AgentLifecycleIntent {
        session_name: &workspace.session_name,
        agent_kind: rimz::ids::AgentKind::new_unchecked(agent.descriptor().kind),
        event_name: "SubagentAdopted",
        observation: &observation,
        transition,
    }) {
        warn!(
            agent = agent.descriptor().kind,
            event = "SubagentAdopted",
            child_id = observation.agent_id.as_deref().unwrap_or(""),
            error = %err,
            "lifecycle: failed to record retroactive subagent adoption",
        );
    }
}

fn correlate_subagent_observation(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &dyn AgentAdapter,
    observation: &mut AgentLifecycleObservation,
) {
    if observation.parent_agent_id.is_some() {
        return;
    }
    if !matches!(
        observation.signal,
        LifecycleSignal::TurnStarted
            | LifecycleSignal::TurnEnded { .. }
            | LifecycleSignal::ToolUsed { .. }
    ) {
        return;
    }
    let Some(child_id) = observation.agent_id.as_ref() else {
        return;
    };
    let snapshot = match store.snapshot_cached() {
        Ok(snapshot) => snapshot,
        Err(err) => {
            debug!(
                kind = agent.descriptor().kind,
                child_id = child_id.as_str(),
                error = %err,
                "lifecycle: skipped subagent correlation because the prior rollup was unreadable",
            );
            return;
        }
    };
    if let Some(parent_id) = snapshot
        .agents
        .iter()
        .find(|state| state.kind.as_str() == agent.descriptor().kind && state.agent_id == *child_id)
        .and_then(|state| state.parent_agent_id.clone())
        .filter(|parent_id| parent_id != child_id)
    {
        observation.parent_agent_id = Some(parent_id);
        normalize_correlated_subagent_signal(observation);
        return;
    }
    if !matches!(
        observation.signal,
        LifecycleSignal::TurnStarted
            | LifecycleSignal::TurnEnded {
                parked_on_background: false,
                ..
            }
    ) {
        return;
    }
    let (Some(pane_id), Some(child_workspace)) = (
        observation.pane_id.as_ref(),
        observation.worktree_path.as_deref().map(Path::new),
    ) else {
        return;
    };
    let mut candidates = snapshot
        .agents
        .iter()
        .filter(|state| {
            state.kind.as_str() == agent.descriptor().kind
                && state.agent_id != *child_id
                && state
                    .pane
                    .as_ref()
                    .is_some_and(|pane| pane.pane_id == *pane_id)
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|state| std::cmp::Reverse(state.last_activity));
    if candidates.len() > MAX_SUBAGENT_PARENT_CANDIDATES {
        debug!(
            kind = agent.descriptor().kind,
            child_id = child_id.as_str(),
            candidates = candidates.len(),
            "lifecycle: skipped subagent correlation because the pane-local parent set exceeded its bound",
        );
        return;
    }

    let mut matched = candidates.into_iter().filter_map(|candidate| {
        let correlation = agent.correlate_subagent(SubagentCorrelationInput {
            child_agent_id: child_id,
            child_workspace: Some(child_workspace),
            parent_agent_id: &candidate.agent_id,
            parent_workspace: Some(
                candidate
                    .worktree_path
                    .as_deref()
                    .map(Path::new)
                    .unwrap_or(&workspace.worktree_root),
            ),
            parent_transcript_path: candidate.transcript_path.as_deref().map(Path::new),
        })?;
        let root_parent = candidate
            .parent_agent_id
            .as_ref()
            .unwrap_or(&candidate.agent_id)
            .clone();
        (root_parent != *child_id).then_some((root_parent, correlation))
    });
    let Some((parent_id, correlation)) = matched.next() else {
        return;
    };
    if matched.next().is_some() {
        debug!(
            kind = agent.descriptor().kind,
            child_id = child_id.as_str(),
            "lifecycle: ambiguous pane-local subagent parents — correlation quarantined",
        );
        return;
    }
    observation.parent_agent_id = Some(parent_id);
    observation.agent_name = correlation.agent_name;
    observation.launch.role = correlation.role;
    observation.task = correlation.task;
    observation.prompt = correlation.prompt;
    observation.launch.model = correlation.model;
    normalize_correlated_subagent_signal(observation);
}

fn normalize_correlated_subagent_signal(observation: &mut AgentLifecycleObservation) {
    observation.signal = match observation.signal {
        LifecycleSignal::TurnStarted => LifecycleSignal::SubagentStarted,
        LifecycleSignal::TurnEnded {
            errored,
            parked_on_background: false,
        } => LifecycleSignal::SubagentStopped { errored },
        ref signal => signal.clone(),
    };
}

/// Fold this observation's signal onto the prior rollup state through the shared
/// `lifecycle::step` table and log any anomaly once, under the
/// `rimz::agent::lifecycle` target (stderr — never stdout, the hook decision
/// channel). Best-effort: a missing cached snapshot just skips the check. The
/// reducer re-derives the same state on replay, silently — this call exists only
/// to surface a reconciled or ignored transition while we still have the event
/// in hand to attribute it.
pub(super) fn log_lifecycle_transition(
    store: &Store,
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
    let snapshot = match store.snapshot_cached() {
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
    let prior = snapshot
        .agents
        .into_iter()
        .find(|agent| agent.kind == kind && agent.agent_id == agent_id);
    let prev = prior.as_ref().map(|agent| agent.lifecycle());
    if prev.is_none() && !observation.signal.establishes_identity() {
        // Create-on-miss: a non-start event for an agent with no prior rollup
        // entry usually materializes the session. Compaction signals are the
        // exception: the reducer quarantines unknown compaction ids because
        // some providers rotate ids before the replacement session is real.
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
            "non-start lifecycle event observed for an unseen session",
        );
    }
    let transition = agent_lifecycle::step(
        prev.as_ref(),
        prior
            .as_ref()
            .and_then(|agent| agent.open_ask.as_ref())
            .and_then(|ask| ask.native_key.as_deref()),
        &observation.signal,
    );
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
