//! The smart-compaction reminder: whether it is due mid-turn, and the pending file the host pass
//! arms for the agent's next root post-tool hook to claim.

use std::path::PathBuf;
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
use crate::store::event::SoftCompactClaim;
use crate::store::message::AutoCompact;
use crate::store::snapshot::SidebarSnapshot;

// A minute gives ordinary delivery time to settle, past one 30s lock or mux
// timeout, without spending the default 14k headroom on a ten-minute retry.
// Helpers have no overall deadline: a stalled helper may overlap a retry, so
// the helper's fresh ceiling_due recheck, not this cache, guards durable state.
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

/// The rung the host pass last armed for a row, kept past the hook's claim so a pass that runs
/// before the fold reads the claim does not arm the same rung again.
#[derive(Serialize, Deserialize)]
struct ArmedRecord {
    compactions: u32,
    rung: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CeilingCompactRequest {
    pub workspace_id: WorkspaceId,
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    pub pane_id: PaneId,
    pub label: String,
}

/// Whether the smart-compaction reminder is due for an agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReminderVerdict {
    /// Due now, at this rung: 1 at the seat's line, 2 at `second_line`.
    Due(u8),
    /// Not due for a normal reason (no threshold, below the line, rung spent,
    /// resting, compacting, owed continue, blocked login, etc.).
    NotDue,
    /// Every other condition passed and `compaction_count` moved past the
    /// spent mark, but this guarded kind has not shown a fold low-water fill
    /// under the seat's line since the last reminder.
    WithheldByGuard,
}

impl ReminderVerdict {
    pub const fn is_due(self) -> bool {
        matches!(self, Self::Due(_))
    }
}

/// The reminder's second line, 10% past `line` rounded up so it always sits above it (200k →
/// 220k, 70% → 77%, 5% → 6%), or `None` when it would reach a full window.
pub(crate) fn second_line(line: AutoCompact) -> Option<AutoCompact> {
    match line {
        AutoCompact::Tokens(tokens) => Some(AutoCompact::Tokens(
            tokens.saturating_add(tokens.div_ceil(10)),
        )),
        AutoCompact::Percent(pct) => {
            let second = u16::from(pct) + u16::from(pct).div_ceil(10);
            u8::try_from(second)
                .ok()
                .filter(|second| *second < 100)
                .map(AutoCompact::Percent)
        }
    }
}

/// Whether the smart-compaction reminder is due for `agent`, and at which rung: an open turn
/// above its line, each rung once per compaction epoch (a claimed rung 2 spends rung 1), with a
/// continue to follow a self-compaction (blank continue text withholds it) and no compaction
/// open, unprompted since, or owed a continue. A kind
/// [`reminder_skip`](crate::agents::reminder_skip) names never gets one. Rung 2 sits at
/// `second_line` and never fires at or past the row's auto-compact ceiling. On a kind whose
/// `Capabilities::lifecycle_reports_fill` is true, the first reminder past a spent mark's epoch
/// also requires a fold-observed low-water reading under the seat's line; when every other
/// condition holds and that guard alone says no, returns [`ReminderVerdict::WithheldByGuard`].
/// It is not due while the agent's login's usage holds a compaction back; `compaction_blocked`
/// answers that, as `RoomCapacities::compaction_blocked` in the host pass, only for a reminder
/// otherwise due. A compaction command waiting in the queue does not withhold it by itself: the
/// agent's request to compact joins that command.
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
    let mark = agent.soft_compact.as_ref();
    let spent = mark
        .filter(|mark| mark.compactions == Some(agent.compaction_count))
        .map_or(0, |mark| mark.rung.unwrap_or(1));
    let rung = if second_rung_reached(agent, threshold) {
        2
    } else {
        1
    };
    if crate::agents::reminder_skip(definition).is_some()
        || !threshold.triggered(agent)
        || rung <= spent
        || !agent.holds_open_turn()
        || agent.is_compacting(now)
        || agent.compaction_unprompted(now)
        || agent.soft_compact_in_flight()
    {
        return ReminderVerdict::NotDue;
    }
    if compaction_blocked() {
        return ReminderVerdict::NotDue;
    }
    if spent == 0
        && definition.spec().capabilities.lifecycle_reports_fill
        && let Some(mark) = mark
        && mark.compactions.is_some()
        && !low_water_below_threshold(agent, mark, threshold)
    {
        return ReminderVerdict::WithheldByGuard;
    }
    ReminderVerdict::Due(rung)
}

/// Whether `agent` sits at the second line past `line`, which counts only below the row's
/// auto-compact ceiling, or its window where it records none.
fn second_rung_reached(agent: &AgentState, line: AutoCompact) -> bool {
    let Some(second) = second_line(line) else {
        return false;
    };
    let window = agent.resolved_context_window();
    let second_tokens = match second {
        AutoCompact::Tokens(tokens) => Some(tokens),
        AutoCompact::Percent(pct) => window.map(|window| window * u64::from(pct) / 100),
    };
    let ceiling = agent
        .record
        .as_ref()
        .and_then(|record| record.auto_compact)
        .or(window);
    second.triggered(agent)
        && !second_tokens
            .zip(ceiling)
            .is_some_and(|(second, ceiling)| second >= ceiling)
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

/// The host pass's files for one row, under the runtime's `soft-compact` directory: the pending
/// copy a hook claims by the row's session key and by its launch id, and the armed record.
fn soft_compact_path(runtime: &RuntimePaths, kind: &AgentKind, key: &str, suffix: &str) -> PathBuf {
    runtime.live_path("soft-compact").join(format!(
        "{}-{suffix}.json",
        crate::store::sidecar::digest(kind.as_str(), key)
    ))
}

const SESSION_PENDING: &str = "pending";
const LAUNCH_PENDING: &str = "launch-pending";
const ARMED: &str = "armed";

/// Arms the reminder rung due for each row in `snapshot` as a pending file its next root
/// post-tool hook claims through [`claim_pending`], and removes what a row armed once its
/// reminder is no longer due (claimed and folded, a new epoch, a turn that ended).
pub(crate) fn compact_agents(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    capacities: &RoomCapacities<'_>,
    config: &MachineConfig,
    policy: &SeatPolicy,
    diag: &DiagSink,
) {
    let mut peers = None;
    for agent in snapshot.agents.iter() {
        let threshold = policy.smart_compact(agent, &config.harness);
        let verdict = reminder_due(
            snapshot,
            agent,
            threshold,
            &config.resume.auto_continue_text,
            snapshot.now,
            || capacities.compaction_blocked(agent, snapshot.now),
        );
        let armed_path = soft_compact_path(runtime, &agent.kind, agent.agent_id.as_str(), ARMED);
        let (ReminderVerdict::Due(rung), Some(threshold)) = (verdict, threshold) else {
            if verdict == ReminderVerdict::WithheldByGuard
                && let Some(threshold) = threshold
            {
                record_withheld(runtime, diag, agent, threshold);
            }
            disarm(runtime, agent, &armed_path);
            continue;
        };
        let armed = std::fs::read(&armed_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ArmedRecord>(&bytes).ok());
        if armed
            .is_some_and(|armed| armed.compactions == agent.compaction_count && armed.rung >= rung)
        {
            continue;
        }
        let peers = peers.get_or_insert_with(|| crate::address::addressable_agents(snapshot));
        let claim = SoftCompactClaim {
            agent_id: agent.agent_id.clone(),
            compactions: agent.compaction_count,
            rung,
            threshold,
            label: Some(crate::address::agent_handle(agent, peers, false)),
            occupied_tokens: agent.occupied_context_tokens(),
        };
        if !arm(runtime, &agent.kind, agent.launch_id.as_ref(), &claim) {
            continue;
        }
        let armed = ArmedRecord {
            compactions: agent.compaction_count,
            rung,
        };
        if let Err(error) = write_temp_then_rename_cache(&armed_path, &armed) {
            tracing::debug!(
                tags.operation = "soft_compact.write_armed_record",
                %error,
                "sidebar: failed to record the armed smart-compaction rung",
            );
        }
    }
}

/// Removes a row's pending copies and armed record once its reminder is no longer due. A row the
/// host never armed costs one failed open.
fn disarm(runtime: &RuntimePaths, agent: &AgentState, armed_path: &std::path::Path) {
    if std::fs::metadata(armed_path).is_err() {
        return;
    }
    let mut pending = vec![soft_compact_path(
        runtime,
        &agent.kind,
        agent.agent_id.as_str(),
        SESSION_PENDING,
    )];
    if let Some(launch_id) = &agent.launch_id {
        pending.push(soft_compact_path(
            runtime,
            &agent.kind,
            launch_id.as_str(),
            LAUNCH_PENDING,
        ));
    }
    for path in pending.iter().chain([&armed_path.to_path_buf()]) {
        if let Err(error) = std::fs::remove_file(path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::debug!(
                tags.operation = "soft_compact.disarm",
                %error,
                "sidebar: failed to remove an armed smart-compaction reminder",
            );
        }
    }
}

/// Writes `claim` as the reminder pending for its row's next root post-tool hook, under the row's
/// launch id (when it has one) and under its session key, so a hook finds it whether or not its
/// env carries `RIMZ_AGENT_ID`. Returns whether every copy was written.
pub fn arm(
    runtime: &RuntimePaths,
    kind: &AgentKind,
    launch_id: Option<&AgentSessionId>,
    claim: &SoftCompactClaim,
) -> bool {
    let session = soft_compact_path(runtime, kind, claim.agent_id.as_str(), SESSION_PENDING);
    let launch = launch_id.map(|id| soft_compact_path(runtime, kind, id.as_str(), LAUNCH_PENDING));
    [Some(session), launch].into_iter().flatten().all(|path| {
        write_temp_then_rename_cache(&path, claim)
            .inspect_err(|error| {
                tracing::debug!(
                    tags.operation = "soft_compact.arm",
                    %error,
                    "sidebar: failed to arm the smart-compaction reminder",
                );
            })
            .is_ok()
    })
}

/// Takes the reminder the host pass armed for the hook's agent, at most once across concurrent
/// hooks: by `launch_id` (an agent-owned hook's `RIMZ_AGENT_ID`) when it is non-empty, else by
/// the payload's session key. By launch id it takes only a reminder for the payload's session, when
/// the payload names one: a native child shares its parent's env but not its session. The rename
/// to a name only this process uses is the take. A pending file that cannot be taken or read
/// yields `None`, and one taken but unreadable is gone.
pub fn claim_pending(
    runtime: &RuntimePaths,
    kind: &AgentKind,
    launch_id: Option<&str>,
    session: Option<&str>,
) -> Option<SoftCompactClaim> {
    let pending = match launch_id.filter(|id| !id.is_empty()) {
        Some(launch_id) => {
            let pending = soft_compact_path(runtime, kind, launch_id, LAUNCH_PENDING);
            if let Some(session) = session {
                let owner = std::fs::read(&pending)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<SoftCompactClaim>(&bytes).ok())?;
                if owner.agent_id.as_str() != session {
                    return None;
                }
            }
            pending
        }
        None => soft_compact_path(runtime, kind, session?, SESSION_PENDING),
    };
    let taken = pending.with_extension(format!("{}.claim", std::process::id()));
    std::fs::rename(&pending, &taken).ok()?;
    let claim = std::fs::read(&taken)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let _ = std::fs::remove_file(&taken);
    claim
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
