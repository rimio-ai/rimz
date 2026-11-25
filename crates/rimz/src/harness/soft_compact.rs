//! The smart-compaction reminder: whether it is due mid-turn, its detached helper, and
//! disposable spawn pacing.

use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::agents::AgentState;
use crate::agents::state::SoftCompactMark;
use crate::config::MachineConfig;
use crate::diag::DiagSink;
use crate::diag::record::DiagEvent;
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

/// Cool-down after a row's last compaction before RimZ's ceiling watch may fire again on a
/// reading still at or over the ceiling.
const CEILING_COOLDOWN: Duration = Duration::from_secs(600);

#[derive(Serialize, Deserialize)]
struct FireRecord {
    fired_at: Timestamp,
    compactions: u32,
}

#[derive(Serialize, Deserialize)]
struct WithheldRecord {
    compactions: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CeilingCompactRequest {
    pub workspace_id: WorkspaceId,
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    pub pane_id: PaneId,
    pub label: String,
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

/// Whether the smart-compaction reminder is due for an agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReminderVerdict {
    /// Due now.
    Due,
    /// Not due for a normal reason (no threshold, below the line, same epoch,
    /// resting, compacting, owed continue, blocked login, etc.).
    NotDue,
    /// Every other condition passed and `compaction_count` moved past the
    /// spent mark, but this guarded kind has not shown a fold low-water fill
    /// under the seat's line since the last reminder.
    WithheldByGuard,
}

impl ReminderVerdict {
    pub const fn is_due(self) -> bool {
        matches!(self, Self::Due)
    }
}

/// Whether the smart-compaction reminder is due for `agent`: an open turn above its line, once
/// per compaction epoch, with a continue to follow a self-compaction (blank continue text
/// withholds it) and no compaction open, unprompted since, or owed a continue. On a kind whose
/// `Capabilities::lifecycle_reports_fill` is true, a repeat past a spent mark also requires a
/// fold-observed low-water reading under the seat's line; when every other condition holds and
/// that guard alone says no, returns [`ReminderVerdict::WithheldByGuard`]. It is not due while
/// the agent's login's usage holds a compaction back; `compaction_blocked` answers that, as
/// `RoomCapacities::compaction_blocked` in the host pass and as
/// [`login_compaction_blocked`](crate::agents::ProviderCapacity::login_compaction_blocked) in
/// the helper, only for a reminder otherwise due. A compaction command waiting in the queue
/// does not withhold it by itself: the agent's request to compact joins that command.
pub fn reminder_due(
    snapshot: &SidebarSnapshot,
    agent: &AgentState,
    threshold: Option<AutoCompact>,
    continue_text: &str,
    now: Timestamp,
    compaction_blocked: impl FnOnce() -> bool,
) -> ReminderVerdict {
    if continue_text.trim().is_empty()
        || agent.ended_at.is_some()
        || agent.is_provider_subagent()
        || agent.is_awaiting_input()
        || snapshot
            .live_agent_pane(&agent.kind, &agent.agent_id)
            .is_none()
    {
        return ReminderVerdict::NotDue;
    }
    let Ok(definition) = crate::agents::definition_by_kind(agent.kind.as_str()) else {
        return ReminderVerdict::NotDue;
    };
    let Some(threshold) = threshold else {
        return ReminderVerdict::NotDue;
    };
    if crate::agents::soft_compact_skip(definition).is_some()
        || !threshold.triggered(agent)
        || !agent.holds_open_turn()
        || agent.is_compacting(now)
        || agent.compaction_unprompted(now)
        || agent.soft_compact_in_flight()
        || agent
            .soft_compact
            .as_ref()
            .is_some_and(|mark| mark.compactions == Some(agent.compaction_count))
    {
        return ReminderVerdict::NotDue;
    }
    if compaction_blocked() {
        return ReminderVerdict::NotDue;
    }
    if definition.spec().capabilities.lifecycle_reports_fill
        && let Some(mark) = agent.soft_compact.as_ref()
        && mark.compactions.is_some()
        && !low_water_below_threshold(agent, mark, threshold)
    {
        return ReminderVerdict::WithheldByGuard;
    }
    ReminderVerdict::Due
}

fn low_water_below_threshold(
    agent: &AgentState,
    mark: &SoftCompactMark,
    threshold: AutoCompact,
) -> bool {
    match threshold {
        AutoCompact::Tokens(_) => mark
            .low_water_tokens
            .is_some_and(|tokens| !threshold.reached(tokens, agent.resolved_context_window())),
        AutoCompact::Percent(pct) => {
            let token_low = mark
                .low_water_tokens
                .zip(agent.resolved_context_window().filter(|window| *window > 0))
                .is_some_and(|(tokens, window)| !threshold.reached(tokens, Some(window)));
            let pct_low = mark.low_water_pct.is_some_and(|low| low < pct);
            token_low || pct_low
        }
    }
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
    diag: &DiagSink,
) {
    compact_agents_with(
        snapshot,
        runtime,
        capacities,
        config,
        policy,
        diag,
        Timestamp::now,
        |request| spawn_soft_compact(runtime, request),
    );
}

fn record_withheld(
    runtime: &RuntimePaths,
    diag: &DiagSink,
    agent: &AgentState,
    threshold: AutoCompact,
) {
    if !diag.is_enabled() {
        return;
    }
    let path = runtime.live_path("soft-compact").join(format!(
        "{}-withheld.json",
        crate::store::sidecar::digest(agent.kind.as_str(), agent.agent_id.as_str())
    ));
    let recorded = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<WithheldRecord>(&bytes).ok());
    if recorded.is_some_and(|record| record.compactions == agent.compaction_count) {
        return;
    }
    let mark = agent.soft_compact.as_ref();
    diag.emit(DiagEvent::SoftCompactReminderWithheld {
        agent_kind: agent.kind.clone(),
        agent_id: agent.agent_id.clone(),
        agent_name: agent.name.clone(),
        compaction_count: agent.compaction_count,
        low_water_tokens: mark.and_then(|mark| mark.low_water_tokens),
        low_water_pct: mark.and_then(|mark| mark.low_water_pct),
        threshold: threshold.to_string(),
    });
    if let Err(error) = write_temp_then_rename_cache(
        &path,
        &WithheldRecord {
            compactions: agent.compaction_count,
        },
    ) {
        tracing::debug!(
            tags.operation = "soft_compact.write_withheld_record",
            %error,
            "sidebar: failed to record withheld soft-compaction dedupe",
        );
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "tests inject clock and spawn hooks beside the host pass inputs"
)]
fn compact_agents_with(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    capacities: &RoomCapacities<'_>,
    config: &MachineConfig,
    policy: &SeatPolicy,
    diag: &DiagSink,
    mut now: impl FnMut() -> Timestamp,
    mut spawn: impl FnMut(&SoftCompactRequest) -> bool,
) {
    for agent in snapshot.agents.iter() {
        let threshold = policy.smart_compact(agent, &config.harness);
        match reminder_due(
            snapshot,
            agent,
            threshold,
            &config.resume.auto_continue_text,
            snapshot.now,
            || capacities.compaction_blocked(agent, snapshot.now),
        ) {
            ReminderVerdict::NotDue => continue,
            ReminderVerdict::WithheldByGuard => {
                if let Some(threshold) = threshold {
                    record_withheld(runtime, diag, agent, threshold);
                }
                continue;
            }
            ReminderVerdict::Due => {}
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

/// Whether RimZ's auto-compact ceiling watch is due for `agent`, returning the recorded ceiling
/// token count. The helper rechecks this plus `message::compact::refuse_repeat` on fresh state
/// before queueing.
pub fn ceiling_due(
    snapshot: &SidebarSnapshot,
    agent: &AgentState,
    continue_text: &str,
    now: Timestamp,
    compaction_blocked: impl FnOnce() -> bool,
) -> Option<u64> {
    if continue_text.trim().is_empty()
        || agent.ended_at.is_some()
        || agent.is_provider_subagent()
        || agent.is_awaiting_input()
        || snapshot
            .live_agent_pane(&agent.kind, &agent.agent_id)
            .is_none()
    {
        return None;
    }
    let definition = crate::agents::definition_by_kind(agent.kind.as_str()).ok()?;
    if crate::agents::auto_compact_enforcer(definition)
        != Some(crate::agents::AutoCompactEnforcer::Rimz)
    {
        return None;
    }
    let ceiling = agent
        .record
        .as_ref()
        .and_then(|record| record.auto_compact)?;
    if !agent
        .occupied_context_tokens()
        .is_some_and(|tokens| tokens >= ceiling)
        || !agent.holds_open_turn()
        || agent.is_compacting(now)
        || agent.compaction_unprompted(now)
        || agent
            .compacted_at
            .is_some_and(|at| now.duration_since(at).as_secs() < CEILING_COOLDOWN.as_secs() as i64)
    {
        return None;
    }
    (!compaction_blocked()).then_some(ceiling)
}

fn spawn_ceiling_compact(runtime: &RuntimePaths, request: &CeilingCompactRequest) -> bool {
    let args = crate::child_process::agent_helper_argv("ceiling-compact", request);
    if let Err(error) =
        crate::child_process::spawn_detached_rimz(runtime, args, "agent-ceiling-compact")
    {
        tracing::debug!(
            workspace = %runtime.workspace_id,
            tags.operation = "ceiling_compact.spawn",
            %error,
            "sidebar: failed to spawn ceiling-compaction helper",
        );
        return false;
    }
    true
}

pub(crate) fn enforce_ceiling_agents(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    capacities: &RoomCapacities<'_>,
    config: &MachineConfig,
) {
    enforce_ceiling_agents_with(
        snapshot,
        runtime,
        capacities,
        config,
        Timestamp::now,
        |request| spawn_ceiling_compact(runtime, request),
    );
}

fn enforce_ceiling_agents_with(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    capacities: &RoomCapacities<'_>,
    config: &MachineConfig,
    mut now: impl FnMut() -> Timestamp,
    mut spawn: impl FnMut(&CeilingCompactRequest) -> bool,
) {
    for agent in snapshot.agents.iter() {
        if ceiling_due(
            snapshot,
            agent,
            &config.resume.auto_continue_text,
            snapshot.now,
            || capacities.compaction_blocked(agent, snapshot.now),
        )
        .is_none()
        {
            continue;
        }
        // One replaceable pacing file per card, keyed by compaction_count like the reminder.
        let path = runtime.live_path("ceiling-compact").join(format!(
            "{}.json",
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
        let request = CeilingCompactRequest {
            workspace_id: runtime.workspace_id.clone(),
            kind: agent.kind.clone(),
            agent_id: agent.agent_id.clone(),
            pane_id,
            label: crate::address::agent_handle(agent, &peers, false),
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
                tags.operation = "ceiling_compact.write_fire_record",
                %error,
                "sidebar: failed to record ceiling-compaction pacing",
            );
        }
    }
}

#[cfg(test)]
mod tests;
