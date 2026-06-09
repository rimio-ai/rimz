//! The agent-lifecycle reducer: folds `agent.lifecycle` events into
//! [`AgentState`] rollups, carrying turn, phase, subagent, and model
//! state forward.

use std::collections::BTreeMap;

use jiff::Timestamp;
use tracing::debug;

use crate::agents::AgentLifecycleObservation;
use crate::agents::lifecycle::{self, Transition};
use crate::feed::{AgentState, AgentStatus, PaneRef, RuntimeOwner, RuntimeOwnerKind};
use crate::ids::{AgentKind, AgentSessionId};
use crate::schema::event::{EventEnvelope, EventKind};

/// How many user prompts a session's rollup keeps (`AgentState::recent_prompts`,
/// newest last). The events are durable, so the cap bounds only the projected
/// history, not the record; 16 covers a long working session without growing
/// the rollup cache unbounded.
const RECENT_PROMPTS_LIMIT: usize = 16;

/// Strip a trailing capability tag (`claude-opus-4-8[1m]` → `claude-opus-4-8`)
/// so the sidebar shows one stable model id per agent. The tag rides only on a
/// fresh-launch SessionStart payload — it is absent after `/clear`, the
/// transcript records the bare id, and no model env var exposes it — so it can
/// never be shown reliably. Idempotent on an already-bare id.
fn canonical_model(model: &str) -> String {
    match model.split_once('[') {
        Some((base, _)) => base.trim_end().to_owned(),
        None => model.to_owned(),
    }
}

/// Fold `agent.lifecycle` events into the latest [`AgentState`] per
/// agent_id, keyed by `(agent_kind, agent_id)`. A session-less event is
/// quarantined (logged, folded to nothing) — identity is required. Events
/// are walked in log order, so the newest observation wins.
///
/// Each event is a *partial* update: `status` always comes from the event,
/// but the stable capability/identity fields (`model`, `effort`,
/// `context_window`, worktree, pane) carry forward from the prior state when
/// the event omits them. A `UserPromptSubmit` therefore moves the agent to
/// running without erasing its model line.
pub(super) fn reduce_agent_states(events: &[EventEnvelope]) -> Vec<AgentState> {
    reduce_agent_states_seeded(BTreeMap::new(), events)
        .into_values()
        .collect()
}

/// [`reduce_agent_states`] resuming from a prior fold map. Each lifecycle
/// event reads only its own key's prior state, and the rebirth boundary is a
/// pointwise transform of the whole map at its log position — either way,
/// folding a delta onto the map the earlier prefix produced equals folding
/// the whole log from scratch, the property the incremental
/// [`catch_up_rollup`] and the rotation carryover both stand on.
pub(super) fn reduce_agent_states_seeded(
    seed: BTreeMap<(AgentKind, AgentSessionId), AgentState>,
    events: &[EventEnvelope],
) -> BTreeMap<(AgentKind, AgentSessionId), AgentState> {
    let mut map = seed;
    for event in events {
        // A mux rebirth renumbers panes from zero, so every stamp recorded
        // before the boundary names a pane that no longer exists — and the
        // reborn session reuses those ids for new panes. Clear them all here,
        // in log order, so a prior incarnation's session can never bind (or
        // block recovery of) a reused pane id; a stamp recorded by a later
        // event is the new incarnation's and stays. Sessions themselves are
        // kept — the boundary unstamps, it never tombstones.
        let payload = match event.kind() {
            EventKind::SessionRebirth => {
                for state in map.values_mut() {
                    state.pane = None;
                }
                continue;
            }
            EventKind::AgentLifecycle(payload) => *payload,
            EventKind::Other {
                method: "agent.lifecycle",
                ..
            } => {
                debug!(
                    target: "rimz::agent::lifecycle",
                    event_id = %event.event_id,
                    "non-conforming agent.lifecycle event ignored",
                );
                continue;
            }
            EventKind::Other { .. } => continue,
        };
        let kind = AgentKind::new_unchecked(event.source.clone());
        // The agent-agnostic lifecycle intent this event carries. The status
        // and the phase/compacting heads are all derived from it through the
        // one shared `lifecycle::step` table — never taken verbatim — so an
        // illegal jump can't slip through unvalidated. Replay is silent here;
        // the ingestion path logs anomalies once per fresh event.
        let observation = payload.observation;
        let signal = observation.signal;
        // Identity is required: a session-less event is quarantined (folded
        // to nothing), mirroring the malformed-subagent-identity rule —
        // never silently merged into a shared per-kind bucket where two
        // distinct instances would collapse into one row. The ingestion path
        // warns once with the event in hand; here a `debug!` keeps every
        // cold rebuild's re-fold quiet.
        let Some(agent_id) = observation.agent_id.clone() else {
            debug!(
                target: "rimz::agent::lifecycle",
                event_id = %event.event_id,
                workspace = %event.workspace_id,
                kind = %kind,
                "session-less agent.lifecycle event quarantined",
            );
            continue;
        };
        let event_name = payload.event_name.as_deref();
        let event_parent_agent_id =
            non_empty_string(observation.parent_agent_id.as_deref()).map(AgentSessionId::from);
        let event_task = non_empty_string(observation.task.as_deref());
        if matches!(signal, lifecycle::LifecycleSignal::Ended) {
            map.remove(&(kind, agent_id));
            continue;
        }
        let prior = map.get(&(kind.clone(), agent_id.clone()));
        if matches!(signal, lifecycle::LifecycleSignal::SubagentStopped { .. })
            && prior.is_none()
            && event_parent_agent_id.is_some()
            && event_task.is_none()
        {
            debug!(
                target: "rimz::agent::lifecycle",
                event_id = %event.event_id,
                workspace = %event.workspace_id,
                session = %event.session_name,
                kind = %kind,
                source_kind = %event.source_kind,
                timestamp = %event.timestamp,
                event_name = event_name.unwrap_or(""),
                parent = event_parent_agent_id.as_deref().unwrap_or(""),
                child = %agent_id,
                "typeless SubagentStop for unknown child — ignored",
            );
            continue;
        }
        let establishes_identity = matches!(
            signal,
            lifecycle::LifecycleSignal::Registered | lifecycle::LifecycleSignal::SubagentStarted
        );
        if prior.is_none() && !establishes_identity {
            debug!(
                target: "rimz::agent::binding",
                kind = %kind,
                agent_id = %agent_id,
                signal = ?signal,
                event_name = event_name.unwrap_or(""),
                "non-start lifecycle event created an unseen session in the reducer",
            );
        }
        let state = assemble_agent_state(AgentStateInput {
            kind: &kind,
            agent_id: &agent_id,
            event,
            event_name,
            observation: &observation,
            signal,
            prior,
            event_parent_agent_id,
            event_task,
            establishes_identity,
        });
        map.insert((kind, agent_id), state);
    }
    map
}

struct AgentStateInput<'a> {
    kind: &'a AgentKind,
    agent_id: &'a AgentSessionId,
    event: &'a EventEnvelope,
    event_name: Option<&'a str>,
    observation: &'a AgentLifecycleObservation,
    signal: lifecycle::LifecycleSignal,
    prior: Option<&'a AgentState>,
    event_parent_agent_id: Option<AgentSessionId>,
    event_task: Option<String>,
    establishes_identity: bool,
}

fn assemble_agent_state(input: AgentStateInput<'_>) -> AgentState {
    let lifecycle = lifecycle_projection(input.prior, input.event.timestamp, input.signal);
    let enrichment = enrichment_projection(input.observation, input.prior);
    let parent_agent_id = input
        .event_parent_agent_id
        .or_else(|| input.prior.and_then(|p| p.parent_agent_id.clone()));
    let worktree = worktree_projection(
        input.observation,
        input.prior,
        input.establishes_identity,
        input.event_name,
    );
    let runtime = runtime_projection(input.observation, input.prior, input.agent_id);
    let prompt = prompt_projection(
        input.observation,
        input.prior,
        parent_agent_id.is_some(),
        input.event_task,
    );
    AgentState {
        agent_id: input.agent_id.clone(),
        kind: input.kind.clone(),
        status: lifecycle.status,
        phase: lifecycle.phase,
        pane: pane_projection(input.observation, input.prior),
        agent_pid: runtime.agent_pid,
        agent_process_start: runtime.agent_process_start,
        runtime_owner: runtime.runtime_owner,
        parent_agent_id,
        worktree_path: worktree.path,
        worktree_branch: worktree.branch,
        task: prompt.task,
        prompt: prompt.prompt,
        transcript_path: transcript_path_projection(input.observation, input.prior),
        recent_prompts: prompt.recent_prompts,
        model: model_projection(input.observation, input.prior),
        effort: effort_projection(input.observation, input.prior),
        context_pct: enrichment.context_pct,
        context_window: enrichment.context_window,
        total_tokens: enrichment.total_tokens,
        cache_read_input_tokens: enrichment.cache_read_input_tokens,
        fresh_input_tokens: enrichment.fresh_input_tokens,
        output_tokens: enrichment.output_tokens,
        todo_done: enrichment.todo_done,
        todo_total: enrichment.todo_total,
        context: None,
        subagent_description: None,
        subagent_started_at: None,
        turn_started_at: lifecycle.turn_started_at,
        compacting_since: lifecycle.compacting_since,
        compaction_count: lifecycle.compaction_count,
        last_seen: input.event.timestamp,
        last_activity: input.event.timestamp,
        registered_at: lifecycle.registered_at,
    }
}

struct LifecycleProjection {
    status: AgentStatus,
    phase: lifecycle::TurnPhase,
    compacting_since: Option<Timestamp>,
    compaction_count: u32,
    turn_started_at: Option<Timestamp>,
    registered_at: Option<Timestamp>,
}

fn lifecycle_projection(
    prior: Option<&AgentState>,
    timestamp: Timestamp,
    signal: lifecycle::LifecycleSignal,
) -> LifecycleProjection {
    let prev_state = prior.map(AgentState::lifecycle);
    let Transition { next, .. } = lifecycle::step(prev_state.as_ref(), &signal);
    let compacting_since = if next.compacting {
        prior.and_then(|p| p.compacting_since).or(Some(timestamp))
    } else {
        None
    };
    let compaction_count = prior.map_or(0, |p| p.compaction_count)
        + u32::from(matches!(
            signal,
            lifecycle::LifecycleSignal::CompactionEnded { .. }
        ));
    let turn_started_at = if matches!(signal, lifecycle::LifecycleSignal::TurnStarted) {
        Some(timestamp)
    } else {
        prior.and_then(|p| p.turn_started_at)
    };
    LifecycleProjection {
        status: next.status,
        phase: next.phase,
        compacting_since,
        compaction_count,
        turn_started_at,
        registered_at: prior.and_then(|p| p.registered_at).or(Some(timestamp)),
    }
}

struct EnrichmentProjection {
    context_pct: Option<u8>,
    context_window: Option<u64>,
    total_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    fresh_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    todo_done: Option<u32>,
    todo_total: Option<u32>,
}

fn enrichment_projection(
    observation: &AgentLifecycleObservation,
    prior: Option<&AgentState>,
) -> EnrichmentProjection {
    EnrichmentProjection {
        context_pct: observation
            .context_pct
            .or_else(|| prior.and_then(|p| p.context_pct)),
        context_window: observation
            .context_window
            .or_else(|| prior.and_then(|p| p.context_window)),
        total_tokens: observation
            .total_tokens
            .or_else(|| prior.and_then(|p| p.total_tokens)),
        cache_read_input_tokens: observation
            .cache_read_input_tokens
            .or_else(|| prior.and_then(|p| p.cache_read_input_tokens)),
        fresh_input_tokens: observation
            .fresh_input_tokens
            .or_else(|| prior.and_then(|p| p.fresh_input_tokens)),
        output_tokens: observation
            .output_tokens
            .or_else(|| prior.and_then(|p| p.output_tokens)),
        todo_done: observation
            .todo_done
            .or_else(|| prior.and_then(|p| p.todo_done)),
        todo_total: observation
            .todo_total
            .or_else(|| prior.and_then(|p| p.todo_total)),
    }
}

struct WorktreeProjection {
    path: Option<String>,
    branch: Option<String>,
}

fn worktree_projection(
    observation: &AgentLifecycleObservation,
    prior: Option<&AgentState>,
    establishes_identity: bool,
    event_name: Option<&str>,
) -> WorktreeProjection {
    let event_path = observation.worktree_path.clone();
    let event_branch = observation.worktree_branch.clone();
    let prior_path = prior.and_then(|p| p.worktree_path.clone());
    let prior_branch = prior.and_then(|p| p.worktree_branch.clone());
    let event_first = establishes_identity || event_name.is_none();
    WorktreeProjection {
        path: if event_first {
            event_path.or(prior_path)
        } else {
            prior_path.or(event_path)
        },
        branch: if event_first {
            event_branch.or(prior_branch)
        } else {
            prior_branch.or(event_branch)
        },
    }
}

struct RuntimeProjection {
    agent_pid: Option<u32>,
    agent_process_start: Option<String>,
    runtime_owner: Option<RuntimeOwner>,
}

fn runtime_projection(
    observation: &AgentLifecycleObservation,
    prior: Option<&AgentState>,
    agent_id: &AgentSessionId,
) -> RuntimeProjection {
    let agent_pid = observation
        .agent_pid
        .or_else(|| prior.and_then(|p| p.agent_pid));
    let agent_process_start = observation
        .agent_process_start
        .clone()
        .or_else(|| prior.and_then(|p| p.agent_process_start.clone()));
    let runtime_owner = observation
        .runtime_owner
        .clone()
        .or_else(|| {
            agent_pid.map(|pid| {
                RuntimeOwner::new(
                    RuntimeOwnerKind::Agent,
                    agent_id.to_string(),
                    pid,
                    agent_process_start.clone(),
                )
            })
        })
        .or_else(|| prior.and_then(|p| p.runtime_owner.clone()));
    RuntimeProjection {
        agent_pid,
        agent_process_start,
        runtime_owner,
    }
}

struct PromptProjection {
    task: Option<String>,
    prompt: Option<String>,
    recent_prompts: Vec<String>,
}

fn prompt_projection(
    observation: &AgentLifecycleObservation,
    prior: Option<&AgentState>,
    is_subagent: bool,
    event_task: Option<String>,
) -> PromptProjection {
    let task = if is_subagent {
        event_task.or_else(|| prior.and_then(|p| p.task.clone()))
    } else {
        non_empty_string(observation.task.as_deref())
    };
    let event_prompt = observation.prompt.clone();
    let mut recent_prompts = prior.map(|p| p.recent_prompts.clone()).unwrap_or_default();
    if let Some(prompt) = event_prompt.as_deref().filter(|prompt| !prompt.is_empty()) {
        recent_prompts.push(prompt.to_owned());
        let excess = recent_prompts.len().saturating_sub(RECENT_PROMPTS_LIMIT);
        if excess > 0 {
            recent_prompts.drain(0..excess);
        }
    }
    PromptProjection {
        task,
        prompt: event_prompt.or_else(|| prior.and_then(|p| p.prompt.clone())),
        recent_prompts,
    }
}

fn transcript_path_projection(
    observation: &AgentLifecycleObservation,
    prior: Option<&AgentState>,
) -> Option<String> {
    observation
        .transcript_path
        .clone()
        .or_else(|| prior.and_then(|p| p.transcript_path.clone()))
}

fn model_projection(
    observation: &AgentLifecycleObservation,
    prior: Option<&AgentState>,
) -> Option<String> {
    observation
        .model
        .clone()
        .map(|raw| canonical_model(&raw))
        .or_else(|| prior.and_then(|p| p.model.clone()))
}

fn effort_projection(
    observation: &AgentLifecycleObservation,
    prior: Option<&AgentState>,
) -> Option<String> {
    observation
        .effort
        .clone()
        .or_else(|| prior.and_then(|p| p.effort.clone()))
}

fn pane_projection(
    observation: &AgentLifecycleObservation,
    prior: Option<&AgentState>,
) -> Option<PaneRef> {
    observation
        .pane_id
        .clone()
        .map(PaneRef::from_id)
        .or_else(|| prior.and_then(|p| p.pane.clone()))
}

fn non_empty_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests;
