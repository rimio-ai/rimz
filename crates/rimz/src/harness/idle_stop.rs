//! Soft stop: end an agent once it has rested for a requested duration with
//! nothing owed (`rimz agents stop --when-idle`).
//!
//! The room host reads the durable requests and spawns the detached
//! `rimz agents idle-stop` helper for each one whose cheap terms are due. The
//! helper re-runs the whole decision against fresh reads and owns the stop;
//! this module writes only a disposable spawn-pacing record.

use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::owed::OwedWake;
use crate::agents::state::{IdleStop, PendingIdleStop, settled_outcome};
use crate::agents::{AgentState, AgentStatus, TurnPhase};
use crate::disk::atomic::write_temp_then_rename_cache;
use crate::ids::{AgentKind, AgentSessionId, PaneId, WorkspaceId};
use crate::store::StoreErr;
use crate::store::message::MessageRecord;
use crate::store::run::RunRecord;
use crate::store::snapshot::{SidebarSnapshot, find_agent};
use crate::{RuntimePaths, StatePaths, Store};

/// A declined helper leaves the request armed; this bounds how often the
/// producer asks again while a term only the helper reads still holds.
const IDLE_STOP_RESPAWN_THROTTLE: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdleStopHelperRequest {
    pub workspace_id: WorkspaceId,
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    pub pane_id: PaneId,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct FireRecord {
    fired_at: Timestamp,
}

/// Why a requested stop waits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hold {
    Ended,
    ProviderSubagent,
    AwaitingInput,
    BudgetPark,
    Compacting,
    /// Mid-turn, failed or paused, sleeping on a wait, parked on background
    /// work, or running a background shell.
    Busy,
    /// Rested, but for less than the requested duration.
    Clock,
    Owed(OwedWake),
    /// A message for this card has not reached a terminal status.
    Message,
    /// A run bound to the session is not terminal; stopping would cancel it.
    OpenRun,
    /// A team seat whose board does not read `Done` is owed a stage.
    Board,
    /// An agent the stop would close with this one is working or owed something.
    Tree,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Stop { idle_secs: u64 },
    Hold(Hold),
}

/// The terms the rollup alone answers: the producer's whole view of an agent.
fn resting(agent: &AgentState) -> Result<(), Hold> {
    if agent.ended_at.is_some() {
        return Err(Hold::Ended);
    }
    if agent.is_provider_subagent() {
        return Err(Hold::ProviderSubagent);
    }
    if agent.is_awaiting_input() {
        return Err(Hold::AwaitingInput);
    }
    if agent.budget_park.is_some() {
        return Err(Hold::BudgetPark);
    }
    if agent.compacting_since.is_some() {
        return Err(Hold::Compacting);
    }
    if !matches!(
        agent.effective_status(),
        AgentStatus::Idle | AgentStatus::Success
    ) || agent.phase == TurnPhase::Parked
        || !agent.background_shells.is_empty()
    {
        return Err(Hold::Busy);
    }
    Ok(())
}

/// Rest runs from the latest of the request, the turn end, the provider
/// marker that settled a turn no hook closed, and the session's last event:
/// a failed compaction returns to rest without a new turn end.
fn rested_since(agent: &AgentState, stop: &IdleStop) -> Timestamp {
    let context = agent.context.as_ref();
    let marker = settled_outcome(agent.status, context, agent.last_activity)
        .and_then(|_| context?.settle)
        .map(|settle| settle.at);
    [agent.turn_ended_at, marker, Some(agent.last_activity)]
        .into_iter()
        .flatten()
        .fold(stop.requested_at, Timestamp::max)
}

/// The live `rimz subagents` descendants a stop of `agent` closes with it.
pub fn closing_with<'a>(agents: &'a [AgentState], agent: &AgentState) -> Vec<&'a AgentState> {
    let mut closing: Vec<&AgentState> = Vec::new();
    let mut pending = crate::address::launched_children(agents, agent);
    while let Some(child) = pending.pop() {
        if child.ended_at.is_some()
            || (child.kind == agent.kind && child.agent_id == agent.agent_id)
            || closing.iter().any(|seen| std::ptr::eq(*seen, child))
        {
            continue;
        }
        closing.push(child);
        pending.extend(crate::address::launched_children(agents, child));
    }
    closing
}

/// What `agent` is still owed, beyond what its own rollup row shows.
fn debt(
    store: &Store,
    messages: &[MessageRecord],
    runs: &[RunRecord],
    agent: &AgentState,
) -> Result<Option<Hold>, StoreErr> {
    if let Some(owed) = super::owed::owed_wake(store, &agent.kind, &agent.agent_id)? {
        return Ok(Some(Hold::Owed(owed)));
    }
    if messages
        .iter()
        .any(|message| message.same_agent_card(agent) && !message.status.is_terminal())
    {
        return Ok(Some(Hold::Message));
    }
    if runs
        .iter()
        .any(|run| run.matches_agent(agent) && !run.status.is_terminal())
    {
        return Ok(Some(Hold::OpenRun));
    }
    Ok(None)
}

/// When the stop falls due while the agent keeps resting, or `None` while a
/// term the rollup shows is holding the clock.
pub fn due_at(agent: &AgentState, stop: &IdleStop) -> Option<Timestamp> {
    resting(agent).ok()?;
    let after = i64::try_from(stop.after_secs).unwrap_or(i64::MAX);
    rested_since(agent, stop)
        .checked_add(jiff::SignedDuration::from_secs(after))
        .ok()
}

/// The whole decision, read fresh: the helper calls it immediately before it
/// stops the agent. `agent` and `agents` come from one enriched snapshot. The
/// clock is `agent`'s alone; every agent closing with it must be at rest with
/// nothing owed.
pub fn decide(
    store: &Store,
    agents: &[AgentState],
    agent: &AgentState,
    stop: &IdleStop,
    now: Timestamp,
) -> Result<Verdict, StoreErr> {
    if let Err(hold) = resting(agent) {
        return Ok(Verdict::Hold(hold));
    }
    if due_at(agent, stop).is_none_or(|due| now < due) {
        return Ok(Verdict::Hold(Hold::Clock));
    }
    let messages = store.list_messages()?;
    let runs = super::run::list(store.paths())?;
    if let Some(hold) = debt(store, &messages, &runs, agent)? {
        return Ok(Verdict::Hold(hold));
    }
    for child in closing_with(agents, agent) {
        if resting(child).is_err() || debt(store, &messages, &runs, child)?.is_some() {
            return Ok(Verdict::Hold(Hold::Tree));
        }
    }
    if agent.is_team_seat() && !board_done(agent) {
        return Ok(Verdict::Hold(Hold::Board));
    }
    let idle = now.duration_since(rested_since(agent, stop)).as_secs();
    Ok(Verdict::Stop {
        idle_secs: u64::try_from(idle).unwrap_or(0),
    })
}

fn board_done(agent: &AgentState) -> bool {
    agent
        .worktree_path
        .as_deref()
        .and_then(|root| super::scratch::board_stage(Path::new(root)))
        .is_some_and(|stage| stage.name == crate::config::DONE_STAGE)
}

/// Attach each pending request, with its due time while the clock runs, to
/// its session's rollup row for the card and `agents show`. Enrich-only: the
/// durable record stays the truth.
pub(crate) fn project_requests(snapshot: &mut SidebarSnapshot, paths: &StatePaths) {
    let requests = crate::store::idle_stop::read(paths);
    for agent in &mut snapshot.agents {
        agent.idle_stop = requests
            .iter()
            .find(|request| {
                agent.ended_at.is_none()
                    && request.kind == agent.kind
                    && request.agent_id == agent.agent_id
            })
            .map(|request| PendingIdleStop {
                stop: request.stop.clone(),
                due_at: due_at(agent, &request.stop),
            });
    }
}

/// Ask a helper to stop each agent whose request is due on the rollup's terms.
pub(crate) fn stop_idle_agents(
    snapshot: &SidebarSnapshot,
    paths: &StatePaths,
    runtime: &RuntimePaths,
) {
    stop_idle_agents_with(snapshot, paths, runtime, |request| {
        spawn_idle_stop(runtime, request)
    });
}

fn stop_idle_agents_with(
    snapshot: &SidebarSnapshot,
    paths: &StatePaths,
    runtime: &RuntimePaths,
    mut spawn: impl FnMut(&IdleStopHelperRequest) -> bool,
) {
    for request in crate::store::idle_stop::read(paths) {
        let Some(agent) = find_agent(&snapshot.agents, &request.kind, &request.agent_id) else {
            continue;
        };
        if due_at(agent, &request.stop).is_none_or(|due| snapshot.now < due) {
            continue;
        }
        let record_path = fire_record_path(runtime, &agent.kind, &agent.agent_id);
        if read_fire_record(&record_path).is_some_and(|record| {
            snapshot.now.as_second() - record.fired_at.as_second()
                < IDLE_STOP_RESPAWN_THROTTLE.as_secs() as i64
        }) {
            continue;
        }
        let Some(pane_id) = snapshot.live_agent_pane(&agent.kind, &agent.agent_id) else {
            continue;
        };
        let peers = crate::address::addressable_agents(snapshot);
        let helper = IdleStopHelperRequest {
            workspace_id: runtime.workspace_id.clone(),
            kind: agent.kind.clone(),
            agent_id: agent.agent_id.clone(),
            pane_id,
            label: crate::address::agent_handle(agent, &peers, false),
        };
        if spawn(&helper)
            && let Err(err) = write_temp_then_rename_cache(
                &record_path,
                &FireRecord {
                    fired_at: snapshot.now,
                },
            )
        {
            tracing::warn!(
                tags.operation = "idle_stop.write_fire_record",
                error = &err as &dyn std::error::Error,
                "sidebar: failed to record idle-stop pacing",
            );
        }
    }
}

fn fire_record_path(
    runtime: &RuntimePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
) -> PathBuf {
    runtime.live_path("idle-stop").join(format!(
        "{}.json",
        crate::store::sidecar::digest(kind.as_str(), agent_id.as_str())
    ))
}

fn read_fire_record(path: &Path) -> Option<FireRecord> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

fn spawn_idle_stop(runtime: &RuntimePaths, request: &IdleStopHelperRequest) -> bool {
    let args = crate::child_process::agent_helper_argv("idle-stop", request);
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        workspace = %runtime.workspace_id,
        kind = %request.kind,
        "sidebar: stopping idle agent",
    );
    if let Err(err) = crate::child_process::spawn_detached_rimz(runtime, args, "agent-idle-stop") {
        tracing::debug!(
            workspace = %runtime.workspace_id,
            tags.operation = "idle_stop.spawn",
            error = &err as &dyn std::error::Error,
            "sidebar: failed to spawn agent idle stop",
        );
        return false;
    }
    true
}

#[cfg(test)]
mod tests;
