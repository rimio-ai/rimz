//! The agent-lifecycle reducer: folds `agent.lifecycle` events into
//! [`AgentState`] rollups, carrying turn, phase, subagent, and model
//! state forward.

use std::collections::BTreeMap;

use tracing::debug;

use crate::agents::lifecycle::{self, Transition};
use crate::feed::{AgentState, PaneRef, RuntimeOwner, RuntimeOwnerKind};
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::schema::event::EventEnvelope;

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

/// [`reduce_agent_states`] resuming from a prior fold map. Each event reads
/// only its own key's prior state, so folding a delta onto the map the
/// earlier prefix produced equals folding the whole log from scratch — the
/// property the incremental [`catch_up_rollup`] and the rotation carryover
/// both stand on.
pub(super) fn reduce_agent_states_seeded(
    seed: BTreeMap<(AgentKind, AgentSessionId), AgentState>,
    events: &[EventEnvelope],
) -> BTreeMap<(AgentKind, AgentSessionId), AgentState> {
    let mut map = seed;
    for event in events {
        if event.method != "agent.lifecycle" {
            continue;
        }
        let kind = AgentKind::new_unchecked(event.source.clone());
        // The agent-agnostic lifecycle intent this event carries. The status
        // and the phase/compacting heads are all derived from it through the
        // one shared `lifecycle::step` table — never taken verbatim — so an
        // illegal jump can't slip through unvalidated. Replay is silent here;
        // the ingestion path logs anomalies once per fresh event. A payload
        // without the (required) explicit signal folds to nothing.
        let Some(signal) = lifecycle::signal_from_event_params(&event.params) else {
            debug!(
                target: "rimz::agent::lifecycle",
                event_id = %event.event_id,
                "signal-less agent.lifecycle event ignored",
            );
            continue;
        };
        // Identity is required: a session-less event is quarantined (folded
        // to nothing), mirroring the malformed-subagent-identity rule —
        // never silently merged into a shared per-kind bucket where two
        // distinct instances would collapse into one row. The ingestion path
        // warns once with the event in hand; here a `debug!` keeps every
        // cold rebuild's re-fold quiet.
        let Some(agent_id) = event
            .params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(AgentSessionId::from)
        else {
            debug!(
                target: "rimz::agent::lifecycle",
                event_id = %event.event_id,
                workspace = %event.workspace_id,
                kind = %kind,
                "session-less agent.lifecycle event quarantined",
            );
            continue;
        };
        let event_name = event.params.get("event_name").and_then(|v| v.as_str());
        let param_non_empty_string = |key: &str| {
            event
                .params
                .get(key)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(ToOwned::to_owned)
        };
        let event_parent_agent_id =
            param_non_empty_string("parent_agent_id").map(AgentSessionId::from);
        let event_task = param_non_empty_string("task");
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
        let prev_state = prior.map(AgentState::lifecycle);
        let Transition { next, .. } = lifecycle::step(prev_state.as_ref(), &signal);
        let status = next.status;
        let phase = next.phase;
        // Compaction stamps the moment it began and preserves it across the
        // multi-event head; any other signal clears the marker. A crashed
        // mid-compact can't stick — the projection also expires it past
        // `COMPACTING_WINDOW_SECS`.
        let compacting_since = if next.compacting {
            prior
                .and_then(|p| p.compacting_since)
                .or(Some(event.timestamp))
        } else {
            None
        };
        let param_string = |key: &str| {
            event
                .params
                .get(key)
                .and_then(|v| v.as_str())
                .map(ToOwned::to_owned)
        };
        let param_number = |key: &str| event.params.get(key).and_then(|v| v.as_u64());
        // Enrichment fields carry forward when an event omits them.
        let context_pct = param_number("context_pct")
            .map(|v| v.min(100) as u8)
            .or_else(|| prior.and_then(|p| p.context_pct));
        let context_window =
            param_number("context_window").or_else(|| prior.and_then(|p| p.context_window));
        let total_tokens =
            param_number("total_tokens").or_else(|| prior.and_then(|p| p.total_tokens));
        let cache_read_input_tokens = param_number("cache_read_input_tokens")
            .or_else(|| prior.and_then(|p| p.cache_read_input_tokens));
        let fresh_input_tokens =
            param_number("fresh_input_tokens").or_else(|| prior.and_then(|p| p.fresh_input_tokens));
        let output_tokens =
            param_number("output_tokens").or_else(|| prior.and_then(|p| p.output_tokens));
        let todo_done = param_number("todo_done")
            .map(|v| v.min(u32::MAX as u64) as u32)
            .or_else(|| prior.and_then(|p| p.todo_done));
        let todo_total = param_number("todo_total")
            .map(|v| v.min(u32::MAX as u64) as u32)
            .or_else(|| prior.and_then(|p| p.todo_total));
        let establishes_identity = matches!(
            signal,
            lifecycle::LifecycleSignal::Registered | lifecycle::LifecycleSignal::SubagentStarted
        );
        // The parent link is pure identity: only ever set, never cleared. Adopt
        // it from any event that carries it, then carry it forward. A typed
        // `SubagentStop` can be the first useful child event Claude reports;
        // without its parent link, that Stop-only child would masquerade as a
        // root session on its parent's pane. A typeless stop-only event is
        // ignored above, since it is not enough identity to create a child row.
        // Root agents never carry one.
        let parent_agent_id =
            event_parent_agent_id.or_else(|| prior.and_then(|p| p.parent_agent_id.clone()));
        // The current turn's start instant — advanced only by a turn start,
        // never by a turn end. It is the "next prompt" boundary the
        // subagent-list retention reads; carried forward across all other
        // events.
        let turn_started_at = if matches!(signal, lifecycle::LifecycleSignal::TurnStarted) {
            Some(event.timestamp)
        } else {
            prior.and_then(|p| p.turn_started_at)
        };
        let event_worktree_path = param_string("worktree_path");
        let event_worktree_branch = param_string("worktree_branch");
        let prior_worktree_path = prior.and_then(|p| p.worktree_path.clone());
        let prior_worktree_branch = prior.and_then(|p| p.worktree_branch.clone());
        let worktree_path = if establishes_identity || event_name.is_none() {
            event_worktree_path.or(prior_worktree_path)
        } else {
            prior_worktree_path.or(event_worktree_path)
        };
        let worktree_branch = if establishes_identity || event_name.is_none() {
            event_worktree_branch.or(prior_worktree_branch)
        } else {
            prior_worktree_branch.or(event_worktree_branch)
        };
        let agent_pid = event
            .params
            .get("agent_pid")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .or_else(|| prior.and_then(|p| p.agent_pid));
        let agent_process_start = param_string("agent_process_start")
            .or_else(|| prior.and_then(|p| p.agent_process_start.clone()));
        let runtime_owner = event
            .params
            .get("runtime_owner")
            .and_then(|v| serde_json::from_value::<RuntimeOwner>(v.clone()).ok())
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
        // A root's `task` is activity: a fresh event replaces it and idle clears
        // it back to "—" (the persisted `prompt` then labels the unnamed
        // session). A subagent's `task` is its *type* ("Explore", "review") —
        // identity, not activity — so carry it forward like the parent link
        // above: a task-less or blank-task `SubagentStop` (or any later child
        // event) then leaves a finished child labeled instead of degrading it to
        // `subagent <hash>`.
        let task = if parent_agent_id.is_some() {
            event_task.or_else(|| prior.and_then(|p| p.task.clone()))
        } else {
            param_non_empty_string("task")
        };
        // The latest prompt, unlike `task`, persists: only the prompt-bearing
        // event sets it, so carry the prior one forward to label an unnamed
        // session past idle until it earns a real name.
        let prompt = param_string("prompt").or_else(|| prior.and_then(|p| p.prompt.clone()));
        // Always store the canonical model id. The agent reports a suffixed id
        // (`claude-opus-4-8[1m]`) only on a fresh-launch SessionStart; every
        // other event (and the transcript fallback) carries the bare id, so the
        // `.or(prior)` carry-forward would otherwise flip the label the first
        // time a suffix-less event arrived. Canonicalizing at reduce time pins
        // the label and keeps the event log faithful to the raw payload.
        let model = param_string("model")
            .map(|raw| canonical_model(&raw))
            .or_else(|| prior.and_then(|p| p.model.clone()));
        let effort = param_string("effort").or_else(|| prior.and_then(|p| p.effort.clone()));
        // The hook stamps the mux pane id it ran inside on every lifecycle
        // event; carry it forward when an event omits it so a `Stop` doesn't
        // unbind the agent from its pane. Only the pane id is reduced — the
        // rest of `PaneRef` is filled by the live `pane list` overlay.
        let pane = param_string("pane_id")
            .and_then(|raw| PaneId::parse(&raw).ok())
            .map(PaneRef::from_id)
            .or_else(|| prior.and_then(|p| p.pane.clone()));
        // Identity, never activity: the first event's instant, carried forward
        // unchanged — the durable spawn key the sidebar's calm tiebreak falls
        // back to when a pane reports no process start.
        let registered_at = prior
            .and_then(|p| p.registered_at)
            .or(Some(event.timestamp));
        let state = AgentState {
            agent_id: agent_id.clone(),
            kind: kind.clone(),
            status,
            phase,
            pane,
            agent_pid,
            agent_process_start,
            runtime_owner,
            parent_agent_id,
            worktree_path,
            worktree_branch,
            task,
            prompt,
            model,
            effort,
            context_pct,
            context_window,
            total_tokens,
            cache_read_input_tokens,
            fresh_input_tokens,
            output_tokens,
            todo_done,
            todo_total,
            // Never reduced from events — the snapshot CLI folds the latest
            // statusline context in via `with_agent_context`, and the per-child
            // `subagentStatusLine` enrichment in via `with_subagent_context`.
            context: None,
            subagent_description: None,
            subagent_started_at: None,
            turn_started_at,
            compacting_since,
            last_seen: event.timestamp,
            last_activity: event.timestamp,
            registered_at,
        };
        map.insert((kind, agent_id), state);
    }
    map
}

#[cfg(test)]
mod tests;
