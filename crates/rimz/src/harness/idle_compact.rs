//! Producer-side idle compaction: condense an idle seat's context, once it
//! reaches the seat's token floor, before its provider prompt cache expires.
//!
//! The room host and the detached `rimz agents idle-compact` helper
//! share one eligibility decision. The helper owns the durable message write;
//! this module writes only a disposable spawn-pacing record.

use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::agents::{AgentState, AgentStatus};
use crate::config::{HarnessConfig, MachineConfig};
use crate::disk::atomic::write_temp_then_rename_cache;
use crate::harness::cache_keepalive::SeatPolicy;
use crate::ids::{AgentKind, AgentSessionId, PaneId, WorkspaceId};
use crate::store::snapshot::SidebarSnapshot;

/// Bounds duplicate helper spawns while a frame or context reading catches up.
const IDLE_COMPACT_RESPAWN_THROTTLE: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdleCompactRequest {
    pub workspace_id: WorkspaceId,
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    pub pane_id: PaneId,
    pub command: String,
    pub occupied_tokens: u64,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct FireRecord {
    fired_at: Timestamp,
    fired_for_request: Timestamp,
}

/// When an idle seat compacts: at or above `floor` tokens, once idle for
/// `fire_after` and before its prompt cache expires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdleWindow {
    pub floor: u64,
    pub fire_after: Duration,
    pub cache_expires: Duration,
}

/// The seat's token floor (`None` is off) and resolved cache lifetime less the margin.
/// Lifetimes at or below the margin never compact; the ping floor does not apply.
pub fn fire_point(
    agent: &AgentState,
    floor: Option<u64>,
    harness: &HarnessConfig,
) -> Option<IdleWindow> {
    let ttl = harness
        .prompt_cache_lifetime(agent)
        .filter(|ttl| *ttl > harness.prompt_cache_margin)?;
    Some(IdleWindow {
        floor: floor?,
        fire_after: ttl.checked_sub(harness.prompt_cache_margin)?,
        cache_expires: ttl,
    })
}

fn eligible_seat(agent: &AgentState, floor: u64) -> bool {
    !agent.is_provider_subagent()
        && !agent.agent_id.is_empty()
        && agent.compacting_since.is_none()
        && !agent.compacted_since_prompt()
        && agent.budget_park.is_none()
        && agent.hibernation.is_none()
        && !agent.is_awaiting_input()
        && matches!(
            agent.effective_status(),
            AgentStatus::Idle | AgentStatus::Success
        )
        && agent.occupied_context_tokens().is_some_and(|tokens| {
            tokens >= floor && agent.last_compact_command_tokens != Some(tokens)
        })
}

/// Shared producer/helper decision; spawn pacing is separate from delivery.
pub fn should_compact(
    agent: &AgentState,
    command: Option<&str>,
    window: Option<IdleWindow>,
    now: Timestamp,
) -> bool {
    let (Some(window), Some(last_request_at)) = (window, agent.last_request_at()) else {
        return false;
    };
    let idle = now.as_second() - last_request_at.as_second();
    eligible_seat(agent, window.floor)
        && command.is_some()
        && idle >= window.fire_after.as_secs().min(i64::MAX as u64) as i64
        && idle < window.cache_expires.as_secs().min(i64::MAX as u64) as i64
}

/// Compact each eligible seat whose floor and idle window are due; a keep-warm hold defers it until its maximum or stage condition lets go.
pub(crate) fn compact_idle_agents(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    config: &MachineConfig,
    policy: &SeatPolicy,
) {
    compact_idle_agents_with(snapshot, runtime, config, policy, |request| {
        spawn_idle_compact(runtime, request)
    });
}

fn compact_idle_agents_with(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    config: &MachineConfig,
    policy: &SeatPolicy,
    mut spawn: impl FnMut(&IdleCompactRequest) -> bool,
) {
    for agent in snapshot.agents.iter() {
        let floor = policy.idle_compact_floor(agent, &config.harness);
        let Some(window) = fire_point(agent, floor, &config.harness) else {
            continue;
        };
        let command = crate::agents::compact_command(agent, &config.harness);
        if !should_compact(agent, command.as_deref(), Some(window), snapshot.now)
            || policy
                .holding(agent, &config.harness, snapshot.now)
                .is_some()
        {
            continue;
        }
        let record_path = fire_record_path(runtime, &agent.kind, &agent.agent_id);
        if !spawn_due(agent, snapshot.now, read_fire_record(&record_path).as_ref()) {
            continue;
        }
        let Some(pane_id) = snapshot.live_agent_pane(&agent.kind, &agent.agent_id) else {
            continue;
        };
        let (Some(command), Some(occupied_tokens), Some(last_request_at)) = (
            command,
            agent.occupied_context_tokens(),
            agent.last_request_at(),
        ) else {
            continue;
        };
        let peers = crate::address::addressable_agents(snapshot);
        let request = IdleCompactRequest {
            workspace_id: runtime.workspace_id.clone(),
            kind: agent.kind.clone(),
            agent_id: agent.agent_id.clone(),
            pane_id,
            command,
            occupied_tokens,
            label: crate::address::agent_handle(agent, &peers, false),
        };
        if spawn(&request) {
            write_fire_record(
                &record_path,
                &FireRecord {
                    fired_at: snapshot.now,
                    fired_for_request: last_request_at,
                },
            );
        }
    }
}

fn spawn_due(agent: &AgentState, now: Timestamp, record: Option<&FireRecord>) -> bool {
    record.is_none_or(|record| {
        Some(record.fired_for_request) != agent.last_request_at()
            && now.as_second() - record.fired_at.as_second()
                >= IDLE_COMPACT_RESPAWN_THROTTLE.as_secs() as i64
    })
}

fn fire_record_path(
    runtime: &RuntimePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
) -> PathBuf {
    runtime.live_path("idle-compact").join(format!(
        "{}.json",
        crate::store::sidecar::digest(kind.as_str(), agent_id.as_str())
    ))
}

fn read_fire_record(path: &Path) -> Option<FireRecord> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

fn write_fire_record(path: &Path, record: &FireRecord) {
    if let Err(err) = write_temp_then_rename_cache(path, record) {
        tracing::warn!(
            tags.operation = "idle_compact.write_fire_record",
            error = &err as &dyn std::error::Error,
            "sidebar: failed to record idle-compaction pacing",
        );
    }
}

fn spawn_idle_compact(runtime: &RuntimePaths, request: &IdleCompactRequest) -> bool {
    let args = crate::child_process::agent_helper_argv("idle-compact", request);
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        workspace = %runtime.workspace_id,
        kind = %request.kind,
        occupied = request.occupied_tokens,
        "sidebar: compacting idle agent",
    );
    if let Err(err) = crate::child_process::spawn_detached_rimz(runtime, args, "agent-idle-compact")
    {
        tracing::debug!(
            workspace = %runtime.workspace_id,
            tags.operation = "idle_compact.spawn",
            error = &err as &dyn std::error::Error,
            "sidebar: failed to spawn agent idle-compaction",
        );
        return false;
    }
    true
}

#[cfg(test)]
mod tests;
