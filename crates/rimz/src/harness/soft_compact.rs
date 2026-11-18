//! The smart-compaction reminder: whether it is due mid-turn, its detached helper, and
//! disposable spawn pacing.

use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::agents::AgentState;
use crate::config::MachineConfig;
use crate::disk::atomic::write_temp_then_rename_cache;
use crate::harness::cache_keepalive::{RoomCapacities, SeatPolicy};
use crate::ids::{AgentKind, AgentSessionId, PaneId, WorkspaceId};
use crate::store::message::{AutoCompact, SoftCompactStep};
use crate::store::snapshot::SidebarSnapshot;

// A minute gives ordinary delivery time to settle, past one 30s lock or mux
// timeout, without spending the default 14k headroom on a ten-minute retry.
// Helpers have no overall deadline: a stalled helper may overlap a retry, so
// the helper's fresh reminder_due recheck, not this cache, guards durable state.
const RESPAWN_THROTTLE: Duration = Duration::from_secs(60);

#[derive(Serialize, Deserialize)]
struct FireRecord {
    fired_at: Timestamp,
    compactions: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SoftCompactRequest {
    pub workspace_id: WorkspaceId,
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    pub pane_id: PaneId,
    pub label: String,
    /// Always `WrapUp`, the reminder's stored tag; kept so a request an older binary
    /// spawned still decodes.
    pub step: SoftCompactStep,
}

/// Whether the smart-compaction reminder is due for `agent`: an open turn above its line, once
/// per compaction epoch, with a continue to follow a self-compaction (blank continue text
/// withholds it) and no compaction open, pending, or owed a continue. It is not due while the
/// agent's login's usage holds a compaction back; `compaction_blocked` answers that, as
/// `RoomCapacities::compaction_blocked` in the host pass and as
/// [`login_compaction_blocked`](crate::agents::ProviderCapacity::login_compaction_blocked) in
/// the helper, only for a reminder otherwise due. The helper also refuses one while a
/// compaction command is still pending in the queue, which this row-only read cannot see.
pub fn reminder_due(
    snapshot: &SidebarSnapshot,
    agent: &AgentState,
    threshold: Option<AutoCompact>,
    continue_text: &str,
    now: Timestamp,
    compaction_blocked: impl FnOnce() -> bool,
) -> bool {
    if continue_text.trim().is_empty()
        || agent.ended_at.is_some()
        || agent.is_provider_subagent()
        || agent.is_awaiting_input()
        || snapshot
            .live_agent_pane(&agent.kind, &agent.agent_id)
            .is_none()
    {
        return false;
    }
    let Ok(definition) = crate::agents::definition_by_kind(agent.kind.as_str()) else {
        return false;
    };
    if crate::agents::soft_compact_skip(definition).is_some()
        || !threshold.is_some_and(|threshold| threshold.triggered(agent))
        || !agent.holds_open_turn()
        || agent.is_compacting(now)
        || agent.compaction_unprompted(now)
        || agent.soft_compact_in_flight()
        || agent
            .soft_compact
            .as_ref()
            .is_some_and(|mark| mark.compactions == Some(agent.compaction_count))
    {
        return false;
    }
    !compaction_blocked()
}

pub fn spawn_soft_compact(runtime: &RuntimePaths, request: &SoftCompactRequest) -> bool {
    let args = crate::child_process::agent_helper_argv("soft-compact", request);
    if let Err(error) =
        crate::child_process::spawn_detached_rimz(runtime, args, "agent-soft-compact")
    {
        tracing::debug!(
            workspace = %runtime.workspace_id,
            tags.operation = "soft_compact.spawn",
            %error,
            "sidebar: failed to spawn soft-compaction helper",
        );
        return false;
    }
    true
}

pub(crate) fn compact_agents(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    capacities: &RoomCapacities<'_>,
    config: &MachineConfig,
    policy: &SeatPolicy,
) {
    compact_agents_with(
        snapshot,
        runtime,
        capacities,
        config,
        policy,
        Timestamp::now,
        |request| spawn_soft_compact(runtime, request),
    );
}

fn compact_agents_with(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    capacities: &RoomCapacities<'_>,
    config: &MachineConfig,
    policy: &SeatPolicy,
    mut now: impl FnMut() -> Timestamp,
    mut spawn: impl FnMut(&SoftCompactRequest) -> bool,
) {
    for agent in snapshot.agents.iter() {
        if !reminder_due(
            snapshot,
            agent,
            policy.smart_compact(agent, &config.harness),
            &config.resume.auto_continue_text,
            snapshot.now,
            || capacities.compaction_blocked(agent, snapshot.now),
        ) {
            continue;
        }
        // One file per card, replaced on each epoch rather than accumulated.
        let path = runtime.live_path("soft-compact").join(format!(
            "{}-wrap-up.json",
            crate::store::sidecar::digest(agent.kind.as_str(), agent.agent_id.as_str())
        ));
        let record = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<FireRecord>(&bytes).ok());
        let fired_at = now();
        if record.is_some_and(|record| {
            record.compactions == agent.compaction_count
                && fired_at.duration_since(record.fired_at).as_secs()
                    < RESPAWN_THROTTLE.as_secs() as i64
        }) {
            continue;
        }
        let Some(pane_id) = snapshot.live_agent_pane(&agent.kind, &agent.agent_id) else {
            continue;
        };
        let peers = crate::address::addressable_agents(snapshot);
        let request = SoftCompactRequest {
            workspace_id: runtime.workspace_id.clone(),
            kind: agent.kind.clone(),
            agent_id: agent.agent_id.clone(),
            pane_id,
            label: crate::address::agent_handle(agent, &peers, false),
            step: SoftCompactStep::WrapUp,
        };
        if !spawn(&request) {
            continue;
        }
        if let Err(error) = write_temp_then_rename_cache(
            &path,
            &FireRecord {
                fired_at,
                compactions: agent.compaction_count,
            },
        ) {
            tracing::warn!(
                tags.operation = "soft_compact.write_fire_record",
                %error,
                "sidebar: failed to record soft-compaction pacing",
            );
        }
    }
}

#[cfg(test)]
mod tests;
