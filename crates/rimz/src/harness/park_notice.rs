//! A launched child parked on a provider limit: whether its run is held open
//! until the reset or failed, and the one notice its parent gets per held park.
//!
//! This module only reads agent rows, run records and the provider usage
//! cache, and spawns a hidden CLI helper. The helper classifies again, makes
//! the locked hold or failure write and queues the notice, so the sidebar
//! producer remains read-only on the store.

use std::cell::OnceCell;
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::auto_continue::limit_marker_active;
use crate::RuntimePaths;
use crate::agents::{AgentState, AgentStatus};
use crate::ids::{RunId, WorkspaceId};
use crate::store::run::{LimitHold, ReportTo, RunRecord};
use crate::store::snapshot::find_agent;
use crate::utils::time::{format_duration_coarse, format_duration_compact, format_local_timestamp};

/// How long an unknown reset may stay unpublished before the run fails: one
/// paced usage read (300 s plus its 90 s claim) and slack, from the marker.
const SETTLE_WINDOW: Duration = Duration::from_secs(7 * 60);

/// The hold limit when `harness.cache_keepalive_max` is `off`.
const UNCAPPED_HORIZON: Duration = Duration::from_secs(6 * 3600);

/// How far past classification a reset may fall for the run to be held for it:
/// the keep-warm cap, whether or not keep-warm pings are on.
fn horizon(harness: &crate::config::HarnessConfig) -> Duration {
    harness.cache_keepalive_max.unwrap_or(UNCAPPED_HORIZON)
}

/// What continues a held child at its reset, read by the host pass.
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

/// Why a parked child's run was not kept open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailCause {
    BeyondHorizon {
        reset: Timestamp,
        horizon: Duration,
    },
    ResetUnknown,
    /// A held run reached its reset with nothing to continue it; never `On`.
    ResetReached {
        reset: Timestamp,
        continuation: Continuation,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Classification {
    Hold {
        until: Timestamp,
    },
    /// The reset is not published yet; nothing is claimed, written or spawned.
    Wait,
    Fail(FailCause),
}

/// Classify a fresh park from the reset reading at `now`; `since` is the marker's time.
fn classify(
    now: Timestamp,
    since: Timestamp,
    reset: Option<Timestamp>,
    horizon: Duration,
) -> Classification {
    match reset {
        Some(until) if until <= now + horizon => Classification::Hold { until },
        Some(reset) => Classification::Fail(FailCause::BeyondHorizon { reset, horizon }),
        None if now < since + SETTLE_WINDOW => Classification::Wait,
        None => Classification::Fail(FailCause::ResetUnknown),
    }
}

/// A child parked on a provider limit whose parent has not been told about this park.
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

/// The park `run` owes its parent a notice for: a parked child with a live
/// parent, and no notice claimed since the child last acted.
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
    /// Hold the run through `hold` and tell the parent; a hold already
    /// stored for this park is told again, never reclassified.
    Notice {
        child: &'a AgentState,
        parent: &'a AgentState,
        hold: LimitHold,
    },
    /// Fail the run for the park named by `activity`.
    Fail {
        child: &'a AgentState,
        activity: Timestamp,
        cause: FailCause,
    },
}

/// Decide a run's park at `now`: a held run at its reset with nothing to
/// continue it fails; an unnoticed park is classified from `reset`.
pub fn park_action<'a>(
    run: &RunRecord,
    agents: &'a [AgentState],
    now: Timestamp,
    horizon: Duration,
    continuation: Continuation,
    reset: impl FnOnce(&AgentState) -> Option<Timestamp>,
) -> Option<ParkAction<'a>> {
    if let Some(hold) = run.limit_hold
        && hold.until <= now
        && continuation != Continuation::On
        && let Some(child) = parked_child(run, agents, now)
        && child.last_activity == hold.activity
    {
        let reset = hold.until;
        let cause = FailCause::ResetReached {
            reset,
            continuation,
        };
        let activity = hold.activity;
        return Some(ParkAction::Fail {
            child,
            activity,
            cause,
        });
    }
    let UnnoticedPark { child, parent } = unnoticed_park(run, agents, now)?;
    let activity = child.last_activity;
    if let Some(hold) = run.limit_hold.filter(|hold| hold.activity == activity) {
        return Some(ParkAction::Notice {
            child,
            parent,
            hold,
        });
    }
    let (_, marker) = child.displayed_turn_error(now)?;
    let since = marker.at.max(run.answer_started_at()).min(now);
    match classify(now, since, reset(child), horizon) {
        Classification::Hold { until } => Some(ParkAction::Notice {
            child,
            parent,
            hold: LimitHold {
                activity,
                since,
                until,
            },
        }),
        Classification::Wait => None,
        Classification::Fail(cause) => Some(ParkAction::Fail {
            child,
            activity,
            cause,
        }),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the host pass's readings and the spawn are injected so tests replace each"
)]
fn notify_parents_with(
    runs: &[RunRecord],
    agents: &[AgentState],
    workspace_id: &WorkspaceId,
    now: Timestamp,
    horizon: Duration,
    mut continuation: impl FnMut(&AgentState) -> Continuation,
    mut reset: impl FnMut(&AgentState) -> Option<Timestamp>,
    mut spawn: impl FnMut(&ParkNoticeRequest),
) {
    for run in runs {
        let Some(child) = parked_child(run, agents, now) else {
            continue;
        };
        let continuation = continuation(child);
        if park_action(run, agents, now, horizon, continuation, &mut reset).is_none() {
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

/// Ask one short-lived helper per park owed a hold or a failure. A park
/// whose reset is not yet published spawns nothing. The helper's locked
/// write settles a race between ticks.
pub(crate) fn notify_parents(
    runs: &[RunRecord],
    agents: &[AgentState],
    runtime: &RuntimePaths,
    now: Timestamp,
    config: &crate::config::MachineConfig,
    store: Option<&crate::store::Store>,
    outcomes: &[crate::store::snapshot::ResumeOutcome],
) {
    let exhausted = OnceCell::new();
    notify_parents_with(
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
        |child| reset_reading(runtime, child, now),
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

/// The latest reset of the child's spent windows, from the provider usage cache.
pub fn reset_reading(
    runtime: &RuntimePaths,
    child: &AgentState,
    now: Timestamp,
) -> Option<Timestamp> {
    crate::agents::ProviderCapacity::read(runtime, &child.login_key())?
        .latest_spent_window_reset_for_model(now, child.model.as_deref())
}

fn handle(child: &AgentState) -> String {
    format!(
        "@{}",
        child
            .name
            .clone()
            .unwrap_or_else(|| child.agent_id.to_string())
    )
}

fn limit_reason(child: &AgentState, now: Timestamp) -> String {
    match child.displayed_turn_error(now) {
        Some((class, error)) => match &error.label {
            Some(label) => format!("\"{label}\""),
            None => format!("a {}", class.words()),
        },
        None => "no label recorded".to_owned(),
    }
}

/// The notice text for a held child: the reset, what happens at it, and how to stop the child.
pub fn text(
    child: &AgentState,
    hold: &LimitHold,
    continuation: Continuation,
    now: Timestamp,
) -> String {
    let handle = handle(child);
    let reset = reset_words(hold.until, now);
    let then = match continuation {
        Continuation::On => {
            "; RimZ will continue it then. Its run stays open, its deadline moved by the pause."
        }
        Continuation::Off | Continuation::Spent => {
            ". Nothing continues it then, so its run fails at the reset; a message to it after \
             that resumes it."
        }
    };
    format!(
        "{handle} stopped on a provider limit: {}. It resets at {reset}{then} \
         Stop it with `rimz subagents stop {handle}`.",
        limit_reason(child, now)
    )
}

/// The reset as a local time and its coarse distance from `now`.
fn reset_words(reset: Timestamp, now: Timestamp) -> String {
    let distance = format_duration_coarse(reset.duration_since(now).as_secs());
    format!("{} (in {distance})", format_local_timestamp(reset))
}

/// The run's one-line failure reason: the provider's label, the reset when
/// known, and why the run was not kept open.
pub fn failure_reason(child: &AgentState, cause: FailCause, now: Timestamp) -> String {
    let why = match cause {
        FailCause::BeyondHorizon { reset, horizon } => format!(
            "it resets at {}, past the {} hold limit (harness.cache_keepalive_max), so the run \
             was not kept open",
            format_local_timestamp(reset),
            format_duration_compact(horizon)
        ),
        FailCause::ResetUnknown => "its reset time is unknown, so the run was not kept open".into(),
        FailCause::ResetReached {
            reset,
            continuation,
        } => format!(
            "it reset at {} with nothing to continue it ({})",
            format_local_timestamp(reset),
            match continuation {
                Continuation::Spent => "auto-continue retries are spent",
                Continuation::Off | Continuation::On => "auto-continue is off",
            }
        ),
    };
    format!(
        "stopped on a provider limit: {}; {why}",
        limit_reason(child, now)
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;
    use crate::agents::{AgentContext, AgentTurnError, PermissionMode, TurnErrorClass};
    use crate::store::run::RunStatus;

    fn parked(class: TurnErrorClass, label: Option<&str>) -> (Vec<AgentState>, RunRecord) {
        let parent = AgentState::stub("claude", "parent", AgentStatus::Sleeping);
        let mut child = AgentState::stub("codex", "child", AgentStatus::Running);
        child.name = Some("still-silver".to_owned());
        child.parent_agent_id = Some(parent.agent_id.clone());
        child.parent_agent_kind = Some(parent.kind.clone());
        child.launch_depth = Some(1);
        mark(&mut child, class, label);
        let mut run = RunRecord::new(
            WorkspaceId::from_project_root(std::path::Path::new("/tmp/park-notice")),
            child.kind.clone(),
            PermissionMode::Auto,
            "gate".to_owned(),
            PathBuf::from("/tmp/park-notice"),
        );
        run.subagent = true;
        run.status = RunStatus::Running;
        run.agent_id = Some(child.agent_id.clone());
        (vec![parent, child], run)
    }

    /// A turn error one second after the child's last activity.
    fn mark(child: &mut AgentState, class: TurnErrorClass, label: Option<&str>) {
        let at = child.last_activity + Duration::from_secs(1);
        let mut context = AgentContext::new(child.kind.as_str(), at);
        context.turn_error = Some(AgentTurnError {
            ended_turn: false,
            class,
            at,
            label: label.map(str::to_owned),
        });
        child.context = Some(context);
    }

    fn eligible(run: &RunRecord, agents: &[AgentState]) -> bool {
        unnoticed_park(run, agents, agents[0].last_activity).is_some()
    }

    #[test]
    fn a_limit_park_is_owed_one_notice_until_the_child_acts_again() {
        for class in [
            TurnErrorClass::PausedRateLimit,
            TurnErrorClass::PausedSpendLimit,
        ] {
            let (mut agents, mut run) = parked(class, Some("Usage limit reached"));
            let park = unnoticed_park(&run, &agents, agents[0].last_activity)
                .expect("a fresh park is owed a notice");
            assert_eq!(park.child.agent_id.as_str(), "child");
            assert_eq!(park.parent.agent_id.as_str(), "parent");

            run.park_noticed_activity = Some(agents[1].last_activity);
            assert!(!eligible(&run, &agents), "told about this park already");

            agents[1].last_activity += Duration::from_secs(600);
            assert!(
                !eligible(&run, &agents),
                "the old marker predates the resume"
            );
            mark(&mut agents[1], class, None);
            assert!(eligible(&run, &agents), "a second park after a resume");
        }
    }

    #[test]
    fn only_a_live_limit_park_with_a_live_parent_is_owed_a_notice() {
        let fresh = || parked(TurnErrorClass::PausedRateLimit, Some("Usage limit reached"));
        let (agents, run) = fresh();
        assert!(eligible(&run, &agents));

        for class in [
            TurnErrorClass::PausedOverloaded,
            TurnErrorClass::Unknown,
            TurnErrorClass::Failed,
        ] {
            let (agents, run) = parked(class, Some("API Error: Overloaded"));
            assert!(!eligible(&run, &agents), "{class:?}");
        }
        for status in [
            RunStatus::Completed,
            RunStatus::Failed,
            RunStatus::Canceled,
            RunStatus::TimedOut,
        ] {
            let (agents, mut run) = fresh();
            run.status = status;
            assert!(!eligible(&run, &agents), "{status:?}");
        }
        let (agents, mut run) = fresh();
        run.subagent = false;
        assert!(!eligible(&run, &agents), "not a subagent run");
        let (agents, mut run) = fresh();
        run.report_to = ReportTo::Nobody;
        assert!(!eligible(&run, &agents), "a detached run reports to nobody");
        let (agents, mut run) = fresh();
        run.agent_id = None;
        assert!(!eligible(&run, &agents), "no session yet");
        let (mut agents, run) = fresh();
        agents[1].ended_at = Some(Timestamp::now());
        assert!(!eligible(&run, &agents), "ended child");
        let (mut agents, run) = fresh();
        agents[1].status = AgentStatus::Failed;
        agents[1].turn_started_at = Some(agents[1].last_activity);
        assert!(
            !eligible(&run, &agents),
            "a failed row is settled, not parked"
        );
        let (mut agents, run) = fresh();
        agents[0].ended_at = Some(Timestamp::now());
        assert!(!eligible(&run, &agents), "ended parent");
        let (mut agents, run) = fresh();
        agents.remove(0);
        assert!(!eligible(&run, &agents), "missing parent");
    }

    const HOUR: Duration = Duration::from_secs(3600);

    fn hour() -> Duration {
        HOUR
    }

    #[test]
    fn the_horizon_is_the_keep_warm_cap_whatever_keep_warm_says() {
        let mut harness = crate::config::HarnessConfig::default();
        assert_eq!(horizon(&harness), 6 * HOUR, "the default cap");
        harness.cache_keepalive_max = Some(2 * HOUR);
        assert_eq!(horizon(&harness), 2 * HOUR);
        harness.cache_keepalive = false;
        assert_eq!(horizon(&harness), 2 * HOUR, "pings off keep the cap");
        harness.cache_keepalive_max = None;
        assert_eq!(horizon(&harness), 6 * HOUR, "`off` holds up to 6h");
    }

    #[test]
    fn classification_holds_a_near_reset_and_fails_a_far_or_unknown_one() {
        let now: Timestamp = "2026-01-01T12:00:00Z".parse().unwrap();
        let since = now - Duration::from_secs(60);
        let horizon = 6 * HOUR;
        for reset in [now + Duration::from_secs(1), now + horizon] {
            assert_eq!(
                classify(now, since, Some(reset), horizon),
                Classification::Hold { until: reset }
            );
        }
        let far = now + horizon + Duration::from_secs(1);
        assert_eq!(
            classify(now, since, Some(far), horizon),
            Classification::Fail(FailCause::BeyondHorizon {
                reset: far,
                horizon
            })
        );
        let settling = now - SETTLE_WINDOW + Duration::from_secs(1);
        assert_eq!(classify(now, settling, None, horizon), Classification::Wait);
        assert_eq!(
            classify(now, now - SETTLE_WINDOW, None, horizon),
            Classification::Fail(FailCause::ResetUnknown)
        );
    }

    /// The child's marker time and the instant `secs` after it.
    fn marker_plus(agents: &[AgentState], secs: u64) -> (Timestamp, Timestamp) {
        let at = agents[1]
            .context
            .as_ref()
            .unwrap()
            .turn_error
            .as_ref()
            .unwrap()
            .at;
        (at, at + Duration::from_secs(secs))
    }

    fn action<'a>(
        run: &RunRecord,
        agents: &'a [AgentState],
        now: Timestamp,
        continuation: Continuation,
        reset: Option<Timestamp>,
    ) -> Option<ParkAction<'a>> {
        park_action(run, agents, now, 6 * HOUR, continuation, |_| reset)
    }

    #[test]
    fn a_near_reset_holds_from_the_marker_and_a_stored_hold_is_never_reclassified() {
        let (agents, mut run) =
            parked(TurnErrorClass::PausedRateLimit, Some("Usage limit reached"));
        run.started_at = agents[1].last_activity - hour();
        let (marker, now) = marker_plus(&agents, 30);
        let reset = now + 2 * HOUR;
        let Some(ParkAction::Notice {
            child,
            parent,
            hold,
        }) = action(&run, &agents, now, Continuation::On, Some(reset))
        else {
            panic!("a reset inside the horizon holds the run");
        };
        assert_eq!(child.agent_id.as_str(), "child");
        assert_eq!(parent.agent_id.as_str(), "parent");
        assert_eq!(
            hold,
            LimitHold {
                activity: agents[1].last_activity,
                since: marker,
                until: reset,
            }
        );

        // A later reading moves nothing once the hold is stored.
        run.limit_hold = Some(hold);
        let Some(ParkAction::Notice { hold: again, .. }) =
            action(&run, &agents, now, Continuation::On, Some(now + 30 * HOUR))
        else {
            panic!("a released notice is told again");
        };
        assert_eq!(again, hold);
    }

    #[test]
    fn the_hold_starts_no_earlier_than_the_answer_it_pauses() {
        let (agents, mut run) = parked(TurnErrorClass::PausedRateLimit, None);
        let (marker, now) = marker_plus(&agents, 30);
        run.started_at = marker + Duration::from_secs(10);
        let Some(ParkAction::Notice { hold, .. }) =
            action(&run, &agents, now, Continuation::On, Some(now + HOUR))
        else {
            panic!("held");
        };
        assert_eq!(hold.since, run.started_at);
    }

    #[test]
    fn a_far_or_unsettled_reset_fails_or_waits() {
        let (agents, run) = parked(TurnErrorClass::PausedSpendLimit, Some("Spend cap"));
        let (_, now) = marker_plus(&agents, 30);
        let far = now + 7 * HOUR;
        assert!(matches!(
            action(&run, &agents, now, Continuation::On, Some(far)),
            Some(ParkAction::Fail {
                cause: FailCause::BeyondHorizon { reset, .. },
                ..
            }) if reset == far
        ));
        assert!(action(&run, &agents, now, Continuation::On, None).is_none());
        let (_, settled) = marker_plus(&agents, SETTLE_WINDOW.as_secs());
        assert!(matches!(
            action(&run, &agents, settled, Continuation::On, None),
            Some(ParkAction::Fail {
                cause: FailCause::ResetUnknown,
                ..
            })
        ));
    }

    #[test]
    fn an_overload_pause_is_never_classified() {
        let (agents, run) = parked(
            TurnErrorClass::PausedOverloaded,
            Some("API Error: Overloaded"),
        );
        let (_, now) = marker_plus(&agents, 30);
        for reset in [Some(now + HOUR), Some(now + 30 * HOUR), None] {
            assert!(action(&run, &agents, now, Continuation::Off, reset).is_none());
        }
        let (_, settled) = marker_plus(&agents, SETTLE_WINDOW.as_secs() + 60);
        assert!(action(&run, &agents, settled, Continuation::Off, None).is_none());
    }

    #[test]
    fn a_held_run_fails_at_its_reset_only_with_nothing_to_continue_it() {
        let (agents, mut run) =
            parked(TurnErrorClass::PausedRateLimit, Some("Usage limit reached"));
        let (marker, now) = marker_plus(&agents, 30);
        let hold = LimitHold {
            activity: agents[1].last_activity,
            since: marker,
            until: now + HOUR,
        };
        run.limit_hold = Some(hold);
        run.park_noticed_activity = Some(hold.activity);
        let at_reset = hold.until;
        for continuation in [Continuation::Off, Continuation::Spent] {
            assert!(
                action(
                    &run,
                    &agents,
                    at_reset - Duration::from_secs(1),
                    continuation,
                    None
                )
                .is_none(),
                "before the reset"
            );
            let Some(ParkAction::Fail {
                activity, cause, ..
            }) = action(&run, &agents, at_reset, continuation, None)
            else {
                panic!("{continuation:?} fails at the reset");
            };
            assert_eq!(activity, hold.activity);
            assert_eq!(
                cause,
                FailCause::ResetReached {
                    reset: hold.until,
                    continuation
                }
            );
        }
        assert!(
            action(&run, &agents, at_reset, Continuation::On, None).is_none(),
            "auto-continue with retries left keeps the run open"
        );

        let mut moved = agents.clone();
        moved[1].last_activity += Duration::from_secs(5);
        assert!(
            action(&run, &moved, at_reset, Continuation::Off, None).is_none(),
            "the child left that park"
        );
        let mut settled = run.clone();
        settled.status = RunStatus::Failed;
        assert!(action(&settled, &agents, at_reset, Continuation::Off, None).is_none());
    }

    #[test]
    fn the_detector_asks_for_one_helper_per_held_or_failed_park() {
        let (agents, parked_run) =
            parked(TurnErrorClass::PausedRateLimit, Some("Usage limit reached"));
        let (_, now) = marker_plus(&agents, 30);
        let mut told = parked_run.clone();
        told.run_id = RunId::new();
        told.park_noticed_activity = Some(agents[1].last_activity);
        let mut settled = parked_run.clone();
        settled.run_id = RunId::new();
        settled.status = RunStatus::Completed;
        let workspace_id = parked_run.workspace_id.clone();

        let ask = |reset: Option<Timestamp>, continuation: Continuation| {
            let mut asked = Vec::new();
            notify_parents_with(
                &[told.clone(), parked_run.clone(), settled.clone()],
                &agents,
                &workspace_id,
                now,
                2 * HOUR,
                |_| continuation,
                |_| reset,
                |request| asked.push(request.clone()),
            );
            asked
        };
        let expected = |continuation| {
            vec![ParkNoticeRequest {
                workspace_id: workspace_id.clone(),
                run_id: parked_run.run_id.clone(),
                horizon_secs: 2 * 3600,
                continuation,
            }]
        };
        assert_eq!(
            ask(Some(now + HOUR), Continuation::On),
            expected(Continuation::On)
        );
        assert_eq!(
            ask(Some(now + 3 * HOUR), Continuation::Off),
            expected(Continuation::Off),
            "a failure is the helper's too"
        );
        assert!(
            ask(None, Continuation::On).is_empty(),
            "an unpublished reset spawns nothing"
        );
    }

    #[test]
    fn text_names_the_reset_what_happens_then_and_the_stop() {
        let (agents, _) = parked(TurnErrorClass::PausedRateLimit, Some("Usage limit reached"));
        let now = Timestamp::now();
        let until = now + 2 * HOUR + Duration::from_secs(5);
        let hold = LimitHold {
            activity: agents[1].last_activity,
            since: now,
            until,
        };
        let at = format_local_timestamp(until);
        assert_eq!(
            text(&agents[1], &hold, Continuation::On, now),
            format!(
                "@still-silver stopped on a provider limit: \"Usage limit reached\". \
                 It resets at {at} (in 2h); RimZ will continue it then. Its run stays open, \
                 its deadline moved by the pause. Stop it with `rimz subagents stop @still-silver`."
            )
        );
        for continuation in [Continuation::Off, Continuation::Spent] {
            assert_eq!(
                text(&agents[1], &hold, continuation, now),
                format!(
                    "@still-silver stopped on a provider limit: \"Usage limit reached\". \
                     It resets at {at} (in 2h). Nothing continues it then, so its run fails at \
                     the reset; a message to it after that resumes it. \
                     Stop it with `rimz subagents stop @still-silver`."
                ),
                "{continuation:?}"
            );
        }
    }

    #[test]
    fn the_failure_reason_names_the_label_the_reset_and_why_the_run_closed() {
        let (agents, _) = parked(TurnErrorClass::PausedRateLimit, Some("Usage limit reached"));
        let now = Timestamp::now();
        let reset = now + 9 * HOUR;
        let at = format_local_timestamp(reset);
        let lead = "stopped on a provider limit: \"Usage limit reached\"";
        for (cause, expected) in [
            (
                FailCause::BeyondHorizon {
                    reset,
                    horizon: 6 * HOUR,
                },
                format!(
                    "{lead}; it resets at {at}, past the 6h hold limit \
                     (harness.cache_keepalive_max), so the run was not kept open"
                ),
            ),
            (
                FailCause::ResetUnknown,
                format!("{lead}; its reset time is unknown, so the run was not kept open"),
            ),
            (
                FailCause::ResetReached {
                    reset,
                    continuation: Continuation::Off,
                },
                format!(
                    "{lead}; it reset at {at} with nothing to continue it (auto-continue is off)"
                ),
            ),
            (
                FailCause::ResetReached {
                    reset,
                    continuation: Continuation::Spent,
                },
                format!(
                    "{lead}; it reset at {at} with nothing to continue it \
                     (auto-continue retries are spent)"
                ),
            ),
        ] {
            assert_eq!(failure_reason(&agents[1], cause, now), expected);
        }
    }
}
