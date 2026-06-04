//! The agent-lifecycle reducer: folds `agent.lifecycle` events into
//! [`AgentState`] rollups, carrying turn, phase, subagent, and model
//! state forward.

use std::collections::BTreeMap;

use tracing::{debug, warn};

use super::panes::pane_ref_from_id;
use crate::agents::lifecycle::{self, Transition};
use crate::feed::{AgentState, RuntimeOwner, RuntimeOwnerKind};
use crate::ids::PaneId;
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
/// agent_id, keyed by `(agent_kind, agent_id)`. Anonymous lifecycle events
/// (no agent_id) collapse to a single rollup keyed by `agent_kind`. Events
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
    seed: BTreeMap<(String, String), AgentState>,
    events: &[EventEnvelope],
) -> BTreeMap<(String, String), AgentState> {
    let mut map = seed;
    for event in events {
        if event.method != "agent.lifecycle" {
            continue;
        }
        let kind = event.source.clone();
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
        let agent_id = event
            .params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("{kind}:anonymous"));
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
        let event_parent_agent_id = param_non_empty_string("parent_agent_id");
        let event_task = param_non_empty_string("task");
        if matches!(signal, lifecycle::LifecycleSignal::Ended) {
            map.remove(&(kind, agent_id));
            continue;
        }
        let prior = map.get(&(kind.clone(), agent_id.clone()));
        if matches!(signal, lifecycle::LifecycleSignal::SubagentStopped)
            && prior.is_none()
            && event_parent_agent_id.is_some()
            && event_task.is_none()
        {
            warn!(
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
                        agent_id.clone(),
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
            .map(pane_ref_from_id)
            .or_else(|| prior.and_then(|p| p.pane.clone()));
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
        };
        map.insert((kind, agent_id), state);
    }
    map
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    use super::super::view::{attach_sub_agents, row_from_agent, sub_agent_from_state};
    use crate::agent_activity::AgentActivity;
    use crate::agents::lifecycle::TurnPhase;
    use crate::feed::AgentStatus;
    use crate::ids::WorkspaceId;
    use crate::ledger::snapshot::SidebarSnapshot;
    use crate::ledger::snapshot::testkit::*;
    use jiff::Timestamp;

    #[test]
    fn thinking_phase_follows_the_turn_through_the_reducer() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let lifecycle = |params: serde_json::Value| {
            EventEnvelope::new(
                workspace.clone(),
                "session",
                "claude",
                "agent-hook",
                "agent.lifecycle",
                params,
            )
        };
        // A legacy `permission_posture` param rides along unread — replay of an
        // old log never errors on it.
        let start = lifecycle(serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "sess-1",
            "signal": { "signal": "registered" },
            "permission_posture": "plan",
        }));
        let prompt = lifecycle(serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-1",
            "signal": { "signal": "turn_started" },
        }));
        let running = reduce_agent_states(&[start.clone(), prompt.clone()]);
        assert_eq!(running[0].status, AgentStatus::Running);
        assert_eq!(
            running[0].phase,
            TurnPhase::Reasoning,
            "a fresh turn opens reasoning"
        );

        // A mutating-but-not-editing tool (a shell command) keeps the head.
        let shell = lifecycle(serde_json::json!({
            "event_name": "PostToolUse",
            "agent_id": "sess-1",
            "signal": { "signal": "tool_used", "mutates": true, "edits": false },
        }));
        let still = reduce_agent_states(&[start.clone(), prompt.clone(), shell.clone()]);
        assert_eq!(
            still[0].phase,
            TurnPhase::Reasoning,
            "a shell command is not a file edit"
        );

        // The turn's first file edit flips it to working.
        let edit = lifecycle(serde_json::json!({
            "event_name": "PostToolUse",
            "agent_id": "sess-1",
            "signal": { "signal": "tool_used", "mutates": true, "edits": true },
        }));
        let working = reduce_agent_states(&[start.clone(), prompt.clone(), shell, edit]);
        assert_eq!(working[0].status, AgentStatus::Running);
        assert_eq!(working[0].phase, TurnPhase::Acting);

        // The turn end clears the head regardless.
        let stop = lifecycle(serde_json::json!({
            "event_name": "Stop",
            "agent_id": "sess-1",
            "signal": { "signal": "turn_ended", "errored": false, "parked_on_background": false },
        }));
        let stopped = reduce_agent_states(&[start, prompt, stop]);
        assert_eq!(stopped[0].status, AgentStatus::Success);
        assert_eq!(stopped[0].phase, TurnPhase::Idle);
    }

    #[test]
    fn subagent_activity_does_not_change_parent_phase() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let last_seen = Timestamp::now() - std::time::Duration::from_secs(50);
        let mut parent = agent("claude", "sess-1", AgentStatus::Running, 50_000);
        parent.phase = TurnPhase::Reasoning;
        let subagent = agent("claude", "sess-1.sub", AgentStatus::Running, 50_000);

        let subagent_touch = AgentActivity {
            kind: "claude".to_owned(),
            agent_id: "sess-1.sub".to_owned(),
            at: last_seen + std::time::Duration::from_secs(15),
        };
        let snap =
            SidebarSnapshot::build_with_agents(workspace, Vec::new(), vec![parent, subagent])
                .with_agent_activity(&[subagent_touch]);
        let parent_phase = snap
            .agents
            .iter()
            .find(|a| a.agent_id == "sess-1")
            .unwrap()
            .phase;
        assert_eq!(
            parent_phase,
            TurnPhase::Reasoning,
            "a subagent heartbeat must not clobber the parent's turn phase"
        );
    }

    #[test]
    fn lifecycle_carries_capability_forward_when_event_omits_it() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let lifecycle = |params: serde_json::Value| {
            EventEnvelope::new(
                workspace.clone(),
                "session",
                "codex",
                "agent-hook",
                "agent.lifecycle",
                params,
            )
        };
        // SessionStart establishes the capability line.
        let start = lifecycle(serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "sess-1",
            "signal": { "signal": "registered" },
            "model": "GPT-5.5",
            "effort": "high",
            "context_window": 258_400,
            "worktree_branch": "main",
        }));
        // A prompt-submit moves the agent to running but reports no model.
        let prompt = lifecycle(serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-1",
            "signal": { "signal": "turn_started" },
            "task": "fix auth flow",
            "worktree_path": "/tmp/hook-subprocess-cwd",
            "worktree_branch": "wrong-branch",
        }));

        let agents = reduce_agent_states(&[start, prompt]);
        assert_eq!(agents.len(), 1);
        let agent = &agents[0];
        assert_eq!(agent.status, AgentStatus::Running);
        assert_eq!(agent.task.as_deref(), Some("fix auth flow"));
        // Capability survives the prompt.
        assert_eq!(agent.model.as_deref(), Some("GPT-5.5"));
        assert_eq!(agent.effort.as_deref(), Some("high"));
        assert_eq!(agent.context_window, Some(258_400));
        assert_eq!(agent.worktree_branch.as_deref(), Some("main"));
    }

    #[test]
    fn canonical_model_strips_capability_tag() {
        assert_eq!(canonical_model("claude-opus-4-8[1m]"), "claude-opus-4-8");
        // Idempotent on a bare id.
        assert_eq!(canonical_model("claude-opus-4-8"), "claude-opus-4-8");
        assert_eq!(canonical_model("gpt-5.5"), "gpt-5.5");
    }

    #[test]
    fn model_label_holds_canonical_across_suffix_drop() {
        // The live flip: SessionStart reports the suffixed id, the prompt omits
        // model entirely, and the first Stop falls back to the transcript's
        // bare id. Canonicalizing at reduce time keeps the label stable so the
        // `[1m]` tag never appears and then vanishes.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let lifecycle = |params: serde_json::Value| {
            EventEnvelope::new(
                workspace.clone(),
                "session",
                "claude",
                "agent-hook",
                "agent.lifecycle",
                params,
            )
        };
        let start = lifecycle(serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "sess-1",
            "signal": { "signal": "registered" },
            "model": "claude-opus-4-8[1m]",
        }));
        let prompt = lifecycle(serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-1",
            "signal": { "signal": "turn_started" },
        }));
        let stop = lifecycle(serde_json::json!({
            "event_name": "Stop",
            "agent_id": "sess-1",
            "signal": { "signal": "turn_ended", "errored": false, "parked_on_background": false },
            "model": "claude-opus-4-8",
        }));

        let agents = reduce_agent_states(&[start, prompt, stop]);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].model.as_deref(), Some("claude-opus-4-8"));
    }

    // ---- Subagent observability (M6): parent link, nesting, retention, reaping ----

    #[test]
    fn subagent_start_reduces_parent_link_that_survives_stop() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let lifecycle = |params: serde_json::Value| {
            EventEnvelope::new(
                workspace.clone(),
                "session",
                "claude",
                "agent-hook",
                "agent.lifecycle",
                params,
            )
        };
        let start = lifecycle(serde_json::json!({
            "event_name": "SubagentStart",
            "agent_id": "child-1",
            "signal": { "signal": "subagent_started" },
            "parent_agent_id": "sess-root",
            "task": "Explore",
        }));
        // SubagentStop omits the parent link — the reducer carries identity forward.
        let stop = lifecycle(serde_json::json!({
            "event_name": "SubagentStop",
            "agent_id": "child-1",
            "signal": { "signal": "subagent_stopped" },
            "task": "Explore",
        }));
        let agents = reduce_agent_states(&[start, stop]);
        let child = agents
            .iter()
            .find(|a| a.agent_id == "child-1")
            .expect("child row");
        assert_eq!(child.parent_agent_id.as_deref(), Some("sess-root"));
        assert_eq!(child.status, AgentStatus::Idle);
    }

    #[test]
    fn subagent_keeps_its_type_when_stop_omits_it() {
        // The regression: a subagent's type is identity, not activity, so a
        // task-less `SubagentStop` must not wipe the label the `SubagentStart`
        // established. Before the carry-forward, the finished child degraded to
        // a `subagent <id>` placeholder.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let lifecycle = |params: serde_json::Value| {
            EventEnvelope::new(
                workspace.clone(),
                "session",
                "claude",
                "agent-hook",
                "agent.lifecycle",
                params,
            )
        };
        let start = lifecycle(serde_json::json!({
            "event_name": "SubagentStart",
            "agent_id": "child-1",
            "signal": { "signal": "subagent_started" },
            "parent_agent_id": "sess-root",
            "task": "Explore",
        }));
        // SubagentStop carries a blank `task` — the exact shape that wiped the
        // label in live Claude events.
        let stop = lifecycle(serde_json::json!({
            "event_name": "SubagentStop",
            "agent_id": "child-1",
            "signal": { "signal": "subagent_stopped" },
            "task": "",
        }));
        let agents = reduce_agent_states(&[start, stop]);
        let child = agents
            .iter()
            .find(|a| a.agent_id == "child-1")
            .expect("child row");
        assert_eq!(child.status, AgentStatus::Idle);
        assert_eq!(
            child.task.as_deref(),
            Some("Explore"),
            "a task-less SubagentStop must not wipe the carried-forward type",
        );
        // The projected sidebar row reads the type, never the hash placeholder.
        let now = Timestamp::from_second(1_700_000_100).unwrap();
        assert_eq!(sub_agent_from_state(child, now).name, "Explore");
    }

    #[test]
    fn subagent_stop_without_start_keeps_parent_link_and_spares_the_parent() {
        // Claude can report a typed child only at `SubagentStop`. That Stop
        // still carries `parent_agent_id`; adopting it keeps the finished child
        // nested instead of letting it supersede the parent as a newer root on
        // the same pane.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let lifecycle = |params: serde_json::Value| {
            EventEnvelope::new(
                workspace.clone(),
                "session",
                "claude",
                "agent-hook",
                "agent.lifecycle",
                params,
            )
        };
        let pane = "tmux:%1";
        let root_start = lifecycle(serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "sess-root",
            "signal": { "signal": "registered" },
            "model": "claude-opus-4-8",
            "pane_id": pane,
            "worktree_path": "/repo/wt",
            "worktree_branch": "feature",
        }));
        let root_prompt = lifecycle(serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-root",
            "signal": { "signal": "turn_started" },
            "pane_id": pane,
            "worktree_path": "/repo/wt",
            "worktree_branch": "feature",
        }));
        let child_stop = lifecycle(serde_json::json!({
            "event_name": "SubagentStop",
            "agent_id": "child-1",
            "signal": { "signal": "subagent_stopped" },
            "parent_agent_id": "sess-root",
            "task": "Explore",
            "pane_id": pane,
            "worktree_path": "/repo/wt",
            "worktree_branch": "feature",
        }));

        let agents = reduce_agent_states(&[root_start, root_prompt, child_stop]);
        let child = agents
            .iter()
            .find(|a| a.agent_id == "child-1")
            .expect("child row");
        assert_eq!(child.parent_agent_id.as_deref(), Some("sess-root"));

        let mut snapshot =
            SidebarSnapshot::build_with_carryover(workspace, Vec::new(), Vec::new(), agents);
        snapshot.reap_stale_sessions(Timestamp::now());
        assert!(
            snapshot.agents.iter().any(|a| a.agent_id == "sess-root"),
            "a Stop-only child must not reap its live parent",
        );
    }

    #[test]
    fn typeless_subagent_stop_without_start_is_ignored() {
        // Claude can also emit extra SubagentStop hooks for task ids that never
        // had a SubagentStart and carry an empty task label. Those are not useful
        // child identity; reducing them used to mint `subagent <hash>` entries in
        // the parent's expanded card.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let lifecycle = |params: serde_json::Value| {
            EventEnvelope::new(
                workspace.clone(),
                "session",
                "claude",
                "agent-hook",
                "agent.lifecycle",
                params,
            )
        };
        let root_start = lifecycle(serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "sess-root",
            "signal": { "signal": "registered" },
        }));
        let root_prompt = lifecycle(serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-root",
            "signal": { "signal": "turn_started" },
        }));
        let child_start = lifecycle(serde_json::json!({
            "event_name": "SubagentStart",
            "agent_id": "child-real",
            "signal": { "signal": "subagent_started" },
            "parent_agent_id": "sess-root",
            "task": "Explore",
        }));
        let stray_stop = lifecycle(serde_json::json!({
            "event_name": "SubagentStop",
            "agent_id": "a833a787ad884cee2",
            "signal": { "signal": "subagent_stopped" },
            "parent_agent_id": "sess-root",
            "task": "",
            "total_tokens": 36_410,
        }));

        let agents = reduce_agent_states(&[root_start, root_prompt, child_start, stray_stop]);
        assert!(
            agents.iter().all(|a| a.agent_id != "a833a787ad884cee2"),
            "an unknown blank-label stop must not become a child row",
        );
        let mut rows = vec![row_from_agent(
            agents
                .iter()
                .find(|a| a.agent_id == "sess-root")
                .expect("root row"),
        )];
        attach_sub_agents(&mut rows, &agents, Timestamp::now());
        assert_eq!(rows[0].sub_agents.len(), 1);
        assert_eq!(rows[0].sub_agents[0].id, "child-real");
        assert_eq!(rows[0].sub_agents[0].name, "Explore");
    }

    #[test]
    fn turn_started_tracks_prompt_never_stop() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let lifecycle = |params: serde_json::Value| {
            EventEnvelope::new(
                workspace.clone(),
                "session",
                "claude",
                "agent-hook",
                "agent.lifecycle",
                params,
            )
        };
        let start = lifecycle(
            serde_json::json!({ "event_name": "SessionStart", "agent_id": "s1", "signal": { "signal": "registered" } }),
        );
        let prompt = lifecycle(
            serde_json::json!({ "event_name": "UserPromptSubmit", "agent_id": "s1", "signal": { "signal": "turn_started" } }),
        );
        let prompt_ts = prompt.timestamp;
        let stop = lifecycle(
            serde_json::json!({ "event_name": "Stop", "agent_id": "s1", "signal": { "signal": "turn_ended", "errored": false, "parked_on_background": false } }),
        );
        let agents = reduce_agent_states(&[start, prompt, stop]);
        // The boundary is the prompt; the later Stop must not advance it (that is
        // what keeps a finished child visible until the *next* prompt).
        assert_eq!(agents[0].turn_started_at, Some(prompt_ts));
    }

    #[test]
    fn prompt_persists_past_stop_while_task_clears() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let lifecycle = |params: serde_json::Value| {
            EventEnvelope::new(
                workspace.clone(),
                "session",
                "claude",
                "agent-hook",
                "agent.lifecycle",
                params,
            )
        };
        let prompt = lifecycle(serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "s1",
            "signal": { "signal": "turn_started" },
            "task": "fix auth flow",
            "prompt": "fix auth flow",
        }));
        // Stop carries neither task nor prompt: task is activity-bound and clears,
        // but the prompt persists to label the unnamed session past its turn.
        let stop = lifecycle(
            serde_json::json!({ "event_name": "Stop", "agent_id": "s1", "signal": { "signal": "turn_ended", "errored": false, "parked_on_background": false } }),
        );
        let agents = reduce_agent_states(&[prompt, stop]);
        let agent = agents.iter().find(|a| a.agent_id == "s1").expect("agent");
        assert_eq!(agent.task, None, "the task clears on idle");
        assert_eq!(
            agent.prompt.as_deref(),
            Some("fix auth flow"),
            "the latest prompt persists past the Stop"
        );
    }

    #[test]
    fn lifecycle_carries_enrichment_forward() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let lifecycle = |params: serde_json::Value| {
            EventEnvelope::new(
                workspace.clone(),
                "session",
                "claude",
                "agent-hook",
                "agent.lifecycle",
                params,
            )
        };
        let start = lifecycle(serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "sess-1",
            "signal": { "signal": "registered" },
            "context_pct": 38,
            "total_tokens": 12_400,
            "todo_done": 3,
            "todo_total": 5,
        }));
        let prompt = lifecycle(serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-1",
            "signal": { "signal": "turn_started" },
            "task": "fix auth flow",
        }));

        let agents = reduce_agent_states(&[start, prompt]);
        assert_eq!(agents.len(), 1);
        let agent = &agents[0];
        assert_eq!(agent.context_pct, Some(38));
        assert_eq!(agent.total_tokens, Some(12_400));
        assert_eq!(agent.todo_done, Some(3));
        assert_eq!(agent.todo_total, Some(5));
        assert_eq!(agent.task.as_deref(), Some("fix auth flow"));
    }

    #[test]
    fn lifecycle_reduces_pane_id_and_carries_it_forward() {
        // The hook stamps the mux pane id on every lifecycle event so the
        // reducer can bind each agent to its own pane. A later event that omits
        // pane_id must not unbind the agent.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let lifecycle = |params: serde_json::Value| {
            EventEnvelope::new(
                workspace.clone(),
                "session",
                "claude",
                "agent-hook",
                "agent.lifecycle",
                params,
            )
        };
        let start = lifecycle(serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "sess-1",
            "signal": { "signal": "registered" },
            "pane_id": "tmux:%7",
        }));
        let prompt = lifecycle(serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-1",
            "signal": { "signal": "turn_started" },
        }));

        let agents = reduce_agent_states(&[start, prompt]);
        assert_eq!(agents.len(), 1);
        let bound = agents[0].pane.as_ref().expect("pane carries forward");
        assert_eq!(bound.pane_id.raw(), "%7");
    }
}
