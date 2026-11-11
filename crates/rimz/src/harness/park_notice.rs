//! A launched child parked on a provider limit: whether its run is held open
//! until the refill or failed, and the typed cause of a failure.
//!
//! This module only reads agent rows, run records and the provider usage
//! cache, and spawns a hidden CLI helper. The helper classifies again and makes
//! the locked hold or failure write, so the sidebar producer remains read-only
//! on the store.

use std::cell::OnceCell;
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::auto_continue::limit_marker_active;
use crate::RuntimePaths;
use crate::agents::{AgentState, AgentStatus, ProviderCapacity};
use crate::ids::{RunId, WorkspaceId};
use crate::store::run::{LimitFailure, LimitFailureCause, LimitHold, ReportTo, RunRecord};
use crate::store::snapshot::find_agent;
use crate::utils::time::format_local_timestamp;

/// How long a park that is not held stays open before its run fails, from the
/// marker: time for a paced usage read to publish the refill, and for the child
/// or a person to continue it.
const FAIL_WINDOW: Duration = Duration::from_secs(10 * 60);

/// The hold limit when `harness.cache_keepalive_max` is `off`.
const UNCAPPED_HORIZON: Duration = Duration::from_secs(6 * 3600);

/// How far past classification a refill may fall for the run to be held for
/// it: the keep-warm cap, whether or not keep-warm pings are on.
fn horizon(harness: &crate::config::HarnessConfig) -> Duration {
    harness.cache_keepalive_max.unwrap_or(UNCAPPED_HORIZON)
}

/// What continues a held child at its refill, read by the host pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Continuation {
    /// Auto-continue is off.
    #[default]
    Off,
    /// Auto-continue is on with retries left.
    On,
    /// Auto-continue is on and has spent its retries on this park.
    Spent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParkNoticeRequest {
    pub workspace_id: WorkspaceId,
    pub run_id: RunId,
    pub horizon_secs: u64,
    pub continuation: Continuation,
}

/// The provider usage reading for a parked child's account and model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitReading {
    /// A spent window refills at this instant, still ahead.
    Refill(Timestamp),
    /// No spent window ahead, and quota available in a reading taken after the park began.
    Refilled,
    /// Nothing tells.
    Unknown,
}

/// The child's limit reading at `now` for a park that began at `since`.
pub fn limit_reading(
    runtime: &RuntimePaths,
    child: &AgentState,
    now: Timestamp,
    since: Timestamp,
) -> LimitReading {
    let key = child.login_key();
    read_limit(
        ProviderCapacity::read(runtime, &key).as_ref(),
        crate::agents::account::cached_usage_read_at(runtime, &key),
        child.model.as_deref(),
        now,
        since,
    )
}

/// The refill ahead of `now` for `key`'s account and `model`, read from the usage cache.
pub fn refill_ahead(
    runtime: &RuntimePaths,
    key: &crate::ids::LoginKey,
    model: Option<&str>,
    now: Timestamp,
) -> Option<Timestamp> {
    ProviderCapacity::read(runtime, key)?.latest_spent_window_reset_for_model(now, model)
}

/// A refill ahead reads as it stands; quota available counts as refilled only
/// from a usage read newer than the park, since an older one predates the limit.
fn read_limit(
    capacity: Option<&ProviderCapacity>,
    read_at: Option<Timestamp>,
    model: Option<&str>,
    now: Timestamp,
    since: Timestamp,
) -> LimitReading {
    let Some(capacity) = capacity else {
        return LimitReading::Unknown;
    };
    if let Some(refill) = capacity.latest_spent_window_reset_for_model(now, model) {
        return LimitReading::Refill(refill);
    }
    if read_at.is_some_and(|at| at > since)
        && capacity.subscription_budget_available_for_model(now, model)
    {
        return LimitReading::Refilled;
    }
    LimitReading::Unknown
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Classification {
    Hold {
        until: Timestamp,
    },
    /// Inside the fail window; nothing is claimed, written or spawned.
    Wait,
    Fail(LimitFailureCause, Option<Timestamp>),
}

/// Classify a fresh park at `now`; `since` is the marker's time.
fn classify(
    now: Timestamp,
    since: Timestamp,
    reading: LimitReading,
    horizon: Duration,
    continuation: Continuation,
) -> Classification {
    use LimitFailureCause as Cause;
    if let LimitReading::Refill(refill) = reading {
        if refill > now + horizon {
            return Classification::Fail(Cause::BeyondHoldLimit, Some(refill));
        }
        if continuation == Continuation::On {
            return Classification::Hold { until: refill };
        }
    }
    if now < since + FAIL_WINDOW {
        return Classification::Wait;
    }
    match (continuation, reading) {
        (Continuation::Spent, _) => Classification::Fail(Cause::ContinueFailed, None),
        (_, LimitReading::Refill(refill)) => Classification::Fail(Cause::RefillKnown, Some(refill)),
        (Continuation::Off, LimitReading::Refilled) => Classification::Fail(Cause::Refilled, None),
        _ => Classification::Fail(Cause::RefillUnknown, None),
    }
}

/// A child parked on a provider limit whose run no hold has claimed since it last acted.
pub struct UnnoticedPark<'a> {
    pub child: &'a AgentState,
    pub parent: &'a AgentState,
}

/// The live child of an open subagent run reporting to its launcher, whose
/// live turn died on a limit marker. `agents` must carry joined context.
fn parked_child<'a>(
    run: &RunRecord,
    agents: &'a [AgentState],
    now: Timestamp,
) -> Option<&'a AgentState> {
    if !run.subagent || run.report_to != ReportTo::Launcher || run.status.is_terminal() {
        return None;
    }
    let child = find_agent(agents, run.kind.as_str(), run.agent_id.as_ref()?)?;
    (child.ended_at.is_none()
        && child.status == AgentStatus::Running
        && limit_marker_active(child, now))
    .then_some(child)
}

/// The park `run` is still to be classified for: a parked child with a live
/// parent, and no hold claimed since the child last acted.
pub fn unnoticed_park<'a>(
    run: &RunRecord,
    agents: &'a [AgentState],
    now: Timestamp,
) -> Option<UnnoticedPark<'a>> {
    let child = parked_child(run, agents, now)?;
    if run
        .park_noticed_activity
        .is_some_and(|noticed| noticed >= child.last_activity)
    {
        return None;
    }
    let parent = crate::address::launched_parent(agents, child)
        .filter(|parent| parent.ended_at.is_none())?;
    Some(UnnoticedPark { child, parent })
}

/// What a parked child's run is owed now.
pub enum ParkAction<'a> {
    /// Hold the run through `hold`; a hold already stored for this park is
    /// claimed again, never reclassified.
    Hold {
        child: &'a AgentState,
        hold: LimitHold,
    },
    /// Fail the run for the park named by `activity`.
    Fail {
        child: &'a AgentState,
        activity: Timestamp,
        failure: LimitFailure,
    },
}

/// Decide a run's park at `now`: a held run with nothing to continue it fails
/// once its refill and the fail window have both passed; an unclaimed park is
/// classified from the reading `reading` takes for the park's start.
pub fn park_action<'a>(
    run: &RunRecord,
    agents: &'a [AgentState],
    now: Timestamp,
    horizon: Duration,
    continuation: Continuation,
    reading: impl FnOnce(&AgentState, Timestamp) -> LimitReading,
) -> Option<ParkAction<'a>> {
    if let Some(hold) = run.limit_hold
        && continuation != Continuation::On
        && now >= hold.until.max(hold.since + FAIL_WINDOW)
        && let Some(child) = parked_child(run, agents, now)
        && child.last_activity == hold.activity
    {
        let (cause, refill_at) = match continuation {
            Continuation::Spent => (LimitFailureCause::ContinueFailed, None),
            Continuation::Off | Continuation::On => {
                (LimitFailureCause::RefillKnown, Some(hold.until))
            }
        };
        return Some(ParkAction::Fail {
            child,
            activity: hold.activity,
            failure: failure(child, cause, refill_at, now),
        });
    }
    let UnnoticedPark { child, .. } = unnoticed_park(run, agents, now)?;
    let activity = child.last_activity;
    if let Some(hold) = run.limit_hold.filter(|hold| hold.activity == activity) {
        return Some(ParkAction::Hold { child, hold });
    }
    let (_, marker) = child.displayed_turn_error(now)?;
    let since = marker.at.max(run.answer_started_at()).min(now);
    match classify(now, since, reading(child, since), horizon, continuation) {
        Classification::Hold { until } => Some(ParkAction::Hold {
            child,
            hold: LimitHold {
                activity,
                since,
                until,
            },
        }),
        Classification::Wait => None,
        Classification::Fail(cause, refill_at) => Some(ParkAction::Fail {
            child,
            activity,
            failure: failure(child, cause, refill_at, now),
        }),
    }
}

fn failure(
    child: &AgentState,
    cause: LimitFailureCause,
    refill_at: Option<Timestamp>,
    now: Timestamp,
) -> LimitFailure {
    LimitFailure {
        cause,
        refill_at,
        label: child
            .displayed_turn_error(now)
            .and_then(|(_, error)| error.label.clone()),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the host pass's readings and the spawn are injected so tests replace each"
)]
fn spawn_park_helpers_with(
    runs: &[RunRecord],
    agents: &[AgentState],
    workspace_id: &WorkspaceId,
    now: Timestamp,
    horizon: Duration,
    mut continuation: impl FnMut(&AgentState) -> Continuation,
    mut reading: impl FnMut(&AgentState, Timestamp) -> LimitReading,
    mut spawn: impl FnMut(&ParkNoticeRequest),
) {
    for run in runs {
        let Some(child) = parked_child(run, agents, now) else {
            continue;
        };
        let continuation = continuation(child);
        if park_action(run, agents, now, horizon, continuation, &mut reading).is_none() {
            continue;
        }
        spawn(&ParkNoticeRequest {
            workspace_id: workspace_id.clone(),
            run_id: run.run_id.clone(),
            horizon_secs: horizon.as_secs(),
            continuation,
        });
    }
}

/// Ask one short-lived helper per park owed a hold or a failure. A park inside
/// its fail window that is not held spawns nothing. The helper's locked write
/// settles a race between ticks.
pub(crate) fn spawn_park_helpers(
    runs: &[RunRecord],
    agents: &[AgentState],
    runtime: &RuntimePaths,
    now: Timestamp,
    config: &crate::config::MachineConfig,
    store: Option<&crate::store::Store>,
    outcomes: &[crate::store::snapshot::ResumeOutcome],
) {
    let exhausted = OnceCell::new();
    spawn_park_helpers_with(
        runs,
        agents,
        &runtime.workspace_id,
        now,
        horizon(&config.harness),
        |child| {
            if !config.resume.auto_continue {
                return Continuation::Off;
            }
            let exhausted = exhausted.get_or_init(|| {
                let messages =
                    super::auto_continue::read_resume_messages(store, &config.resume, outcomes);
                super::auto_continue::exhausted_parks(
                    |_| false,
                    agents,
                    runtime,
                    &config.resume,
                    &messages,
                )
            });
            if exhausted.contains(&(child.kind.clone(), child.agent_id.clone())) {
                Continuation::Spent
            } else {
                Continuation::On
            }
        },
        |child, since| limit_reading(runtime, child, now, since),
        |request| {
            let args = crate::child_process::agent_helper_argv("park-notice", request);
            if let Err(err) =
                crate::child_process::spawn_detached_rimz(runtime, args, "subagent-park-notice")
            {
                tracing::debug!(
                    workspace = %runtime.workspace_id,
                    run_id = %request.run_id,
                    error = &err as &dyn std::error::Error,
                    "sidebar: failed to spawn subagent park notice helper",
                );
            }
        },
    );
}

/// The run's stored failure sentence: the provider's label, and the refill
/// instant or the failed continue. No duration, which the report computes.
pub(super) fn failure_tail(failure: &LimitFailure) -> String {
    let mut tail = "stopped on a provider limit".to_owned();
    if let Some(label) = &failure.label {
        tail.push_str(&format!(": \"{label}\""));
    }
    match (failure.cause, failure.refill_at) {
        (_, Some(refill)) => {
            tail.push_str(&format!(
                "; it refills at {}",
                format_local_timestamp(refill)
            ));
        }
        (LimitFailureCause::ContinueFailed, None) => {
            tail.push_str("; continuing it after the refill failed");
        }
        _ => {}
    }
    tail
}

#[cfg(test)]
mod tests;
