//! Lifecycle observation ingestion and transition checks.

use super::*;
use rimz::agents::{HookIngressOwner, SubagentCorrelationInput, SubagentSpawnInput};

const MAX_SUBAGENT_PARENT_CANDIDATES: usize = 64;

pub(super) fn record_lifecycle_observation(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &AgentDefinition,
    decoded: &mut HookOutput,
    ingress_owner: HookIngressOwner,
    globals: &GlobalFlags,
) -> Option<RecordedLifecycle> {
    let mut observation = decoded.take_lifecycle()?;
    if let LifecycleSignal::AwaitingInput { ask_id, detail, .. } = &mut observation.signal {
        ask_id.get_or_insert_with(rimz::ids::AskId::new);
        if detail.is_none() {
            *detail = decoded.ask_detail().map(ToOwned::to_owned);
        }
    }
    attach_agent_owner(ingress_owner, &mut observation);
    attach_agent_pane(&mut observation);
    correlate_subagent_observation(workspace, store, agent, &mut observation);
    Some(record_mapped_lifecycle_observation(
        workspace,
        store,
        agent,
        decoded.event_name(),
        observation,
        globals,
    ))
}

pub(super) fn record_derived_lifecycle_observation(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &AgentDefinition,
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
    agent: &AgentDefinition,
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
            rimz::harness::launch::ENV_AGENT_NAME,
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
        agent.spec().kind,
        agent.spec().capabilities.registers_lazily,
        globals.mux,
        workspace,
        store,
        &mut observation,
    );
    let model_hint = observation.launch.model.clone();
    let spawned_subagents = if observation.parent_agent_id.is_none()
        && matches!(
            observation.signal,
            LifecycleSignal::ToolUsed { .. } | LifecycleSignal::TurnEnded { .. }
        ) {
        observation
            .agent_id
            .as_ref()
            .map_or_else(Vec::new, |parent_id| {
                agent.spawned_subagents(SubagentSpawnInput {
                    parent_agent_id: parent_id,
                    parent_transcript_path: observation.transcript_path.as_deref().map(Path::new),
                    parent_workspace: observation.worktree_path.as_deref().map(Path::new),
                })
            })
    } else {
        Vec::new()
    };
    let receipt = match store.append_agent_lifecycle(AgentLifecycleIntent {
        session_name: &workspace.session_name,
        agent_kind: agent.spec().kind_id(),
        event_name,
        observation: &observation,
        spawned_subagents: &spawned_subagents,
    }) {
        Ok(receipt) => receipt,
        Err(err) => {
            warn!(
                agent = agent.spec().kind,
                event = %event_name,
                error = %err,
                "lifecycle: failed to record the agent.lifecycle event",
            );
            return RecordedLifecycle {
                model_hint,
                observation,
                primary_event_id: None,
                events: Vec::new(),
                rotation_due: false,
                waiting_cleared: false,
            };
        }
    };
    log_lifecycle_receipt(agent.spec().kind, &observation, &receipt);
    RecordedLifecycle {
        model_hint,
        observation,
        primary_event_id: receipt.primary_event_id,
        events: receipt.events,
        rotation_due: receipt.rotation_due,
        waiting_cleared: receipt.waiting_cleared,
    }
}

fn correlate_subagent_observation(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &AgentDefinition,
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
                kind = agent.spec().kind,
                child_id = child_id.as_str(),
                error = %err,
                "lifecycle: skipped subagent correlation because the prior rollup was unreadable",
            );
            return;
        }
    };
    let prior_state = snapshot
        .agents
        .iter()
        .find(|state| state.kind.as_str() == agent.spec().kind && state.agent_id == *child_id);
    // Pane-backed launched children keep ordinary multi-turn lifecycle and run
    // completion semantics. Their durable parent stamp is carried by the
    // rollup; only provider-native children normalize root turn signals into
    // subagent signals here.
    if prior_state.is_some_and(|state| state.launch_depth.is_some()) {
        return;
    }
    if let Some(parent_id) = prior_state
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
            state.kind.as_str() == agent.spec().kind
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
            kind = agent.spec().kind,
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
        let root_parent_kind = candidate
            .parent_agent_kind
            .as_ref()
            .unwrap_or(&candidate.kind)
            .clone();
        (root_parent != *child_id).then_some((root_parent, root_parent_kind, correlation))
    });
    let Some((parent_id, parent_kind, correlation)) = matched.next() else {
        return;
    };
    if matched.next().is_some() {
        debug!(
            kind = agent.spec().kind,
            child_id = child_id.as_str(),
            "lifecycle: ambiguous pane-local subagent parents — correlation quarantined",
        );
        return;
    }
    observation.parent_agent_id = Some(parent_id);
    if parent_kind.as_str() != agent.spec().kind {
        observation.launch.parent_agent_kind = Some(parent_kind);
    }
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

fn log_lifecycle_receipt(
    kind: &str,
    observation: &AgentLifecycleObservation,
    receipt: &rimz::store::writer::AgentLifecycleReceipt,
) {
    let Some(agent_id) = observation.agent_id.as_deref() else {
        warn!(
            target: "rimz::agent::lifecycle",
            kind,
            signal = ?observation.signal,
            "session-less agent.lifecycle event — the reducer will quarantine it",
        );
        return;
    };
    if receipt.prior_status.is_none() && !observation.signal.establishes_identity() {
        debug!(
            target: "rimz::agent::binding",
            kind,
            agent_id,
            signal = ?observation.signal,
            "non-start lifecycle event observed for an unseen session",
        );
    }
    if let Some(transition) = receipt.transition {
        log_transition(
            kind,
            agent_id,
            observation.parent_agent_id.as_deref(),
            &observation.signal,
            transition,
        );
        if transition.compaction_closed
            && !matches!(observation.signal, LifecycleSignal::CompactionEnded { .. })
        {
            debug!(
                target: "rimz::agent::lifecycle",
                kind,
                agent_id,
                signal = ?observation.signal,
                "closed compaction bracket on a non-compaction signal",
            );
        }
    }

    for event in receipt
        .events
        .iter()
        .filter(|event| receipt.primary_event_id.as_ref() != Some(&event.event_id))
    {
        log_canonical_transition(kind, event);
    }
}

fn log_canonical_transition(kind: &str, event: &rimz::agents::LifecycleEvent) {
    match &event.transition {
        rimz::agents::LifecycleTransition::Reconciled { from, reason } => warn!(
            target: "rimz::agent::lifecycle",
            kind,
            agent_id = event.agent_id.as_str(),
            parent_agent_id = event.parent_agent_id.as_deref().unwrap_or(""),
            from = ?from,
            to = ?event.status,
            signal = ?event.signal,
            reason,
            "reconciled lifecycle transition",
        ),
        rimz::agents::LifecycleTransition::Ignored { reason } => debug!(
            target: "rimz::agent::lifecycle",
            kind,
            agent_id = event.agent_id.as_str(),
            signal = ?event.signal,
            reason,
            "ignored lifecycle signal",
        ),
        rimz::agents::LifecycleTransition::Normal => {}
    }
}

fn log_transition(
    kind: &str,
    agent_id: &str,
    parent_agent_id: Option<&str>,
    signal: &LifecycleSignal,
    transition: agent_lifecycle::Transition,
) {
    match transition.kind {
        TransitionKind::Reconciled { from, reason } => warn!(
            target: "rimz::agent::lifecycle",
            kind,
            agent_id,
            parent_agent_id = parent_agent_id.unwrap_or(""),
            from = ?from,
            to = ?transition.next.status,
            signal = ?signal,
            reason,
            "reconciled lifecycle transition",
        ),
        TransitionKind::Ignored { reason } => debug!(
            target: "rimz::agent::lifecycle",
            kind,
            agent_id,
            signal = ?signal,
            reason,
            "ignored lifecycle signal",
        ),
        TransitionKind::Normal => {}
    }
}
