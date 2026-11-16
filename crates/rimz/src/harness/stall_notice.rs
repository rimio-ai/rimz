//! A launched child silent mid-turn, never started, or stopped on a turn error
//! nothing will retry, and its one parent notice per run.
//!
//! Evaluation reads the producer's heartbeat-folded agents. Durable claims,
//! messages and assists belong to the detached helper, not the sidebar graph.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::park_notice::Continuation;
use crate::RuntimePaths;
use crate::agents::state::ChildStall;
use crate::agents::{AgentState, TurnErrorClass};
use crate::ids::{RunId, WorkspaceId};
use crate::store::run::{ReportTo, RunRecord};
use crate::store::snapshot::find_agent;
use crate::utils::time::format_duration_coarse;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StallNoticeRequest {
    pub workspace_id: WorkspaceId,
    pub run_id: RunId,
    pub stalled_after_secs: u32,
    /// What continues the child's overload pause, read by the host pass.
    pub continuation: Continuation,
}

/// An open child whose silence has not yet been reported to its live parent.
pub struct UnnoticedStall<'a> {
    pub child: &'a AgentState,
    pub parent: &'a AgentState,
    pub silent_secs: u64,
    cause: Cause,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cause {
    Silent(ChildStall),
    /// A displayed turn error that nothing will retry.
    Stopped,
}

/// Select a launcher-reporting run's unnoticed stall from heartbeat-folded
/// agents. `continuation` is asked only for a child paused on an overload.
pub fn unnoticed_stall<'a>(
    run: &RunRecord,
    agents: &'a [AgentState],
    now: Timestamp,
    stalled_after_secs: u32,
    continuation: impl FnOnce(&AgentState) -> Continuation,
) -> Option<UnnoticedStall<'a>> {
    if !run.subagent
        || run.report_to != ReportTo::Launcher
        || run.status.is_terminal()
        || run.stall_noticed_at.is_some()
    {
        return None;
    }
    let child = match &run.agent_id {
        Some(id) => find_agent(agents, run.kind.as_str(), id)?,
        None => {
            let name = run.agent_name.as_deref()?;
            agents.iter().find(|child| {
                child.kind == run.kind
                    && child.name.as_deref() == Some(name)
                    && child.is_launched_child()
                    && child.ended_at.is_none()
            })?
        }
    };
    let (silent_secs, cause) = match stopped_for(child, now, stalled_after_secs, continuation) {
        Some(silent_secs) => (silent_secs, Cause::Stopped),
        None => {
            let (silent_secs, cause) = child.silent_child_for(now, stalled_after_secs)?;
            let provisional = run.agent_id.is_none() && child.agent_id.is_provisional();
            let cause = if provisional {
                ChildStall::NeverStarted
            } else {
                cause
            };
            (silent_secs, Cause::Silent(cause))
        }
    };
    let parent = crate::address::launched_parent(agents, child)
        .filter(|parent| parent.ended_at.is_none())?;
    Some(UnnoticedStall {
        child,
        parent,
        silent_secs,
        cause,
    })
}

/// Silence seconds at the stall window for an open child whose displayed turn
/// error is not a provider limit and that nothing will retry: one displayed as
/// failed, or paused on an overload whose auto-continue is off or spent. The
/// park notice owns limit pauses, and auto-continue owns an overload pause
/// with retries left.
fn stopped_for(
    child: &AgentState,
    now: Timestamp,
    stalled_after_secs: u32,
    continuation: impl FnOnce(&AgentState) -> Continuation,
) -> Option<u64> {
    if child.parent_agent_id.is_none() || child.ended_at.is_some() {
        return None;
    }
    let stopped = match child.displayed_turn_error(now)?.0 {
        TurnErrorClass::PausedRateLimit | TurnErrorClass::PausedSpendLimit => false,
        TurnErrorClass::PausedOverloaded => continuation(child) != Continuation::On,
        TurnErrorClass::Failed | TurnErrorClass::Unknown | TurnErrorClass::NotEntitled => true,
    };
    let silent_secs = now.duration_since(child.last_activity).as_secs();
    (stopped && silent_secs >= i64::from(stalled_after_secs)).then_some(silent_secs as u64)
}

fn notify_parents_with(
    runs: &[RunRecord],
    agents: &[AgentState],
    workspace_id: &WorkspaceId,
    now: Timestamp,
    stalled_after_secs: u32,
    mut continuation: impl FnMut(&AgentState) -> Continuation,
    mut spawn: impl FnMut(&StallNoticeRequest),
) {
    for run in runs {
        if let Some(stall) =
            unnoticed_stall(run, agents, now, stalled_after_secs, &mut continuation)
        {
            spawn(&StallNoticeRequest {
                workspace_id: workspace_id.clone(),
                run_id: run.run_id.clone(),
                stalled_after_secs,
                continuation: continuation(stall.child),
            });
        }
    }
}

/// Ask one helper per unnoticed stall; the helper's locked claim deduplicates ticks.
pub(crate) fn notify_parents(
    runs: &[RunRecord],
    agents: &[AgentState],
    runtime: &RuntimePaths,
    now: Timestamp,
    config: &crate::config::MachineConfig,
    store: Option<&crate::store::Store>,
    outcomes: &[crate::store::snapshot::ResumeOutcome],
) {
    notify_parents_with(
        runs,
        agents,
        &runtime.workspace_id,
        now,
        config.agents.attention.stalled_after_secs.get(),
        super::park_notice::continuation_reader(agents, runtime, config, store, outcomes),
        |request| {
            let args = crate::child_process::agent_helper_argv("stall-notice", request);
            if let Err(err) =
                crate::child_process::spawn_detached_rimz(runtime, args, "subagent-stall-notice")
            {
                tracing::debug!(workspace = %runtime.workspace_id, run_id = %request.run_id, error = &err as &dyn std::error::Error, "sidebar: failed to spawn subagent stall notice helper");
            }
        },
    );
}

/// Name the silent or stopped child, the still-open run, and the parent's moves.
pub fn text(stall: &UnnoticedStall<'_>, run: &RunRecord, now: Timestamp) -> String {
    let child = stall.child;
    let silent_secs = stall.silent_secs;
    let handle = format!(
        "@{}",
        child
            .name
            .clone()
            .unwrap_or_else(|| child.agent_id.to_string())
    );
    let silence = format_duration_coarse(i64::try_from(silent_secs).unwrap_or(i64::MAX));
    let activity = match stall.cause {
        Cause::Silent(ChildStall::MidTurn) => {
            format!("has been silent for {silence} and is still running")
        }
        Cause::Silent(ChildStall::NeverStarted) => {
            format!("has not started its first turn for {silence}")
        }
        Cause::Stopped => {
            let reason = child.displayed_turn_error_label(now).unwrap_or_default();
            format!("stopped on a turn error {silence} ago: \"{reason}\"")
        }
    };
    let deadline = match run.deadline_at {
        Some(at) if at > now => format!(
            " until its deadline in {}",
            format_duration_coarse(at.duration_since(now).as_secs())
        ),
        Some(_) => ", past its deadline, until it is stopped".to_owned(),
        None => String::new(),
    };
    let moves = match stall.cause {
        Cause::Silent(_) => "RimZ will not stop it: look with",
        Cause::Stopped => "RimZ will not continue it: look with",
    };
    let resume = match stall.cause {
        Cause::Silent(_) => String::new(),
        Cause::Stopped => format!(" continue it with `rimz message {handle} \"continue\"`,"),
    };
    format!(
        "{handle} {activity}; its pane is open. Its run stays open{deadline}. {moves} `rimz pane capture {handle}`,{resume} stop it with `rimz subagents stop {handle}`, or relaunch the task elsewhere."
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::agents::{
        AgentContext, AgentStatus, AgentTurnError, PendingWait, PendingWaitTrigger, PermissionMode,
        TurnErrorClass,
    };
    use crate::store::run::{ReportTo, RunStatus};

    fn now() -> Timestamp {
        "2026-01-01T01:00:00Z".parse().unwrap()
    }

    fn silent() -> (Vec<AgentState>, RunRecord) {
        let parent = AgentState::stub("claude", "parent", AgentStatus::Idle);
        let mut child = AgentState::stub("codex", "child", AgentStatus::Running);
        child.name = Some("still-silver".to_owned());
        child.parent_agent_id = Some(parent.agent_id.clone());
        child.parent_agent_kind = Some(parent.kind.clone());
        child.launch_depth = Some(1);
        child.prompt = Some("gate".to_owned());
        child.last_activity = now() - Duration::from_secs(1_920);
        let mut run = RunRecord::new(
            WorkspaceId::from_project_root(std::path::Path::new("/tmp/stall-notice")),
            child.kind.clone(),
            PermissionMode::Auto,
            "gate".to_owned(),
            "/tmp/stall-notice".into(),
        );
        run.subagent = true;
        run.status = RunStatus::Running;
        run.agent_id = Some(child.agent_id.clone());
        run.agent_name = Some("still-silver".to_owned());
        (vec![parent, child], run)
    }

    fn eligible(run: &RunRecord, agents: &[AgentState]) -> bool {
        unnoticed_stall(run, agents, now(), 1_800, |_| Continuation::Off).is_some()
    }

    #[test]
    fn only_a_silent_live_child_run_with_a_live_launcher_is_owed_a_notice() {
        let (agents, run) = silent();
        let stall = unnoticed_stall(&run, &agents, now(), 1_800, |_| Continuation::Off)
            .expect("silent child owes a notice");
        assert_eq!(stall.child.agent_id.as_str(), "child");
        assert_eq!(stall.parent.agent_id.as_str(), "parent");
        assert_eq!(stall.silent_secs, 1_920);
        assert!(unnoticed_stall(&run, &agents, now(), 1_921, |_| Continuation::Off).is_none());
        for status in [
            RunStatus::Completed,
            RunStatus::Failed,
            RunStatus::Canceled,
            RunStatus::TimedOut,
        ] {
            let mut run = run.clone();
            run.status = status;
            assert!(!eligible(&run, &agents), "{status:?}");
        }
        for change in 0..6 {
            let mut run = run.clone();
            match change {
                0 => run.subagent = false,
                1 => run.report_to = ReportTo::Nobody,
                2 => {
                    run.agent_id = None;
                    run.agent_name = None;
                }
                3 => run.agent_id = Some("missing".into()),
                4 => run.kind = crate::ids::AgentKind::new_unchecked("pi"),
                _ => run.stall_noticed_at = Some(now()),
            }
            assert!(!eligible(&run, &agents), "run condition {change}");
        }
        for change in 0..6 {
            let mut agents = agents.clone();
            match change {
                0 => agents[1].ended_at = Some(now()),
                1 => agents[1].last_activity = now() - Duration::from_secs(1_799),
                2 => agents[1].launch_depth = None,
                3 => agents[0].ended_at = Some(now()),
                4 => agents[1].parent_agent_id = None,
                _ => {
                    agents.remove(0);
                }
            }
            assert!(!eligible(&run, &agents), "agent condition {change}");
        }
        for status in [
            AgentStatus::Success,
            AgentStatus::Paused,
            AgentStatus::Failed,
            AgentStatus::Waiting,
        ] {
            let mut agents = agents.clone();
            agents[1].status = status;
            assert!(!eligible(&run, &agents), "{status:?}");
        }
        let mut interrupted = agents.clone();
        interrupted[1].status = AgentStatus::Idle;
        interrupted[1].turn_ended_at = Some(now());
        assert!(!eligible(&run, &interrupted));
        let mut sleeping = agents.clone();
        sleeping[1].status = AgentStatus::Idle;
        sleeping[1].pending_waits.push(PendingWait {
            class: crate::agents::PendingWaitClass::Working,
            name: "wait-command".to_owned(),
            trigger: PendingWaitTrigger::Command {
                command: "cargo test".to_owned(),
            },
            armed_at: Some(now()),
        });
        assert!(!eligible(&run, &sleeping), "an armed wait is not a stall");
        // A displayed error is never silence: a limit pause belongs to the
        // park notice, and an overload pause with retries left to
        // auto-continue.
        for class in [
            TurnErrorClass::PausedRateLimit,
            TurnErrorClass::PausedSpendLimit,
        ] {
            let errored = with_turn_error(&agents, class);
            assert!(!eligible(&run, &errored), "{class:?}");
        }
        let overloaded = with_turn_error(&agents, TurnErrorClass::PausedOverloaded);
        assert!(unnoticed_stall(&run, &overloaded, now(), 1_800, |_| Continuation::On).is_none());
    }

    fn with_turn_error(agents: &[AgentState], class: TurnErrorClass) -> Vec<AgentState> {
        let mut errored = agents.to_vec();
        let mut context = AgentContext::new("codex", now());
        context.turn_error = Some(AgentTurnError {
            ended_turn: false,
            class,
            at: now() - Duration::from_secs(1_919),
            label: Some("Auto mode is unavailable".to_owned()),
        });
        errored[1].context = Some(context);
        errored
    }

    #[test]
    fn a_stopped_child_nothing_will_retry_is_owed_a_notice() {
        let (agents, run) = silent();
        let owed = |class, continuation| {
            let agents = with_turn_error(&agents, class);
            let stall = unnoticed_stall(&run, &agents, now(), 1_800, |_| continuation)?;
            assert_eq!(stall.silent_secs, 1_920);
            Some(stall.cause)
        };
        for class in [
            TurnErrorClass::Failed,
            TurnErrorClass::Unknown,
            TurnErrorClass::NotEntitled,
        ] {
            assert_eq!(
                owed(class, Continuation::On),
                Some(Cause::Stopped),
                "{class:?}"
            );
        }
        let overloaded = TurnErrorClass::PausedOverloaded;
        assert_eq!(owed(overloaded, Continuation::Off), Some(Cause::Stopped));
        assert_eq!(owed(overloaded, Continuation::Spent), Some(Cause::Stopped));
        assert_eq!(
            owed(overloaded, Continuation::On),
            None,
            "auto-continue retries it"
        );
        for class in [
            TurnErrorClass::PausedRateLimit,
            TurnErrorClass::PausedSpendLimit,
        ] {
            for continuation in [Continuation::Off, Continuation::On, Continuation::Spent] {
                assert_eq!(
                    owed(class, continuation),
                    None,
                    "{class:?} {continuation:?}"
                );
            }
        }

        let failed = with_turn_error(&agents, TurnErrorClass::Failed);
        assert!(unnoticed_stall(&run, &failed, now(), 1_921, |_| Continuation::Off).is_none());
        let mut told = run.clone();
        told.stall_noticed_at = Some(now());
        assert!(unnoticed_stall(&told, &failed, now(), 1_800, |_| Continuation::Off).is_none());
        let mut orphaned = failed.clone();
        orphaned[0].ended_at = Some(now());
        assert!(unnoticed_stall(&run, &orphaned, now(), 1_800, |_| Continuation::Off).is_none());
        // A newer hook clears the marker, and with it the stop.
        let mut resumed = failed;
        resumed[1].last_activity = now();
        assert!(unnoticed_stall(&run, &resumed, now(), 1_800, |_| Continuation::Off).is_none());
    }

    #[test]
    fn text_names_the_stop_reason_deadline_and_the_parents_moves() {
        let (agents, mut run) = silent();
        run.deadline_at = Some(now() + Duration::from_secs(24 * 60 + 5));
        let agents = with_turn_error(&agents, TurnErrorClass::PausedOverloaded);
        let stall = unnoticed_stall(&run, &agents, now(), 1_800, |_| Continuation::Off)
            .expect("a stopped child owes a notice");
        assert_eq!(
            text(&stall, &run, now()),
            "@still-silver stopped on a turn error 32m ago: \"Auto mode is unavailable\"; its pane is open. Its run stays open until its deadline in 24m. RimZ will not continue it: look with `rimz pane capture @still-silver`, continue it with `rimz message @still-silver \"continue\"`, stop it with `rimz subagents stop @still-silver`, or relaunch the task elsewhere."
        );
    }

    #[test]
    fn detector_carries_the_continuation_it_read_for_a_stopped_child() {
        let (agents, run) = silent();
        let agents = with_turn_error(&agents, TurnErrorClass::PausedOverloaded);
        let mut asked = Vec::new();
        notify_parents_with(
            std::slice::from_ref(&run),
            &agents,
            &run.workspace_id,
            now(),
            1_800,
            |_| Continuation::Spent,
            |request| asked.push(request.continuation),
        );
        assert_eq!(asked, [Continuation::Spent]);
    }

    #[test]
    fn a_prompted_child_resting_before_its_first_turn_owes_a_notice() {
        let (mut agents, run) = silent();
        agents[1].status = AgentStatus::Idle;
        agents[1].turn_started_at = Some(agents[1].last_activity);
        assert!(unnoticed_stall(&run, &agents, now(), 1_921, |_| Continuation::Off).is_none());
        let stall = unnoticed_stall(&run, &agents, now(), 1_800, |_| Continuation::Off)
            .expect("first turn never started");
        assert_eq!(stall.cause, Cause::Silent(ChildStall::NeverStarted));
        assert_eq!(stall.silent_secs, 1_920);
    }

    #[test]
    fn an_unbound_run_finds_its_silent_provisional_child_by_kind_and_name() {
        let (mut agents, mut run) = silent();
        agents[1].agent_id = "launch_child".into();
        run.agent_id = None;
        assert!(unnoticed_stall(&run, &agents, now(), 1_921, |_| Continuation::Off).is_none());
        let stall = unnoticed_stall(&run, &agents, now(), 1_800, |_| Continuation::Off)
            .expect("provisional child");
        assert_eq!(stall.cause, Cause::Silent(ChildStall::NeverStarted));
        assert_eq!(stall.child.agent_id.as_str(), "launch_child");
        assert_eq!(stall.silent_secs, 1_920);
    }

    #[test]
    fn an_unbound_run_keeps_a_registered_childs_mid_turn_verdict() {
        let (agents, mut run) = silent();
        run.agent_id = None;
        let stall = unnoticed_stall(&run, &agents, now(), 1_800, |_| Continuation::Off)
            .expect("registered child");
        assert_eq!(stall.child.agent_id.as_str(), "child");
        assert_eq!(stall.cause, Cause::Silent(ChildStall::MidTurn));
    }

    #[test]
    fn provisional_lookup_requires_the_launched_childs_kind_name_and_open_lifetime() {
        let (agents, mut run) = silent();
        run.agent_id = None;
        for change in 0..5 {
            let mut agents = agents.clone();
            let mut run = run.clone();
            match change {
                0 => run.agent_name = Some("missing".to_owned()),
                1 => run.kind = crate::ids::AgentKind::new_unchecked("pi"),
                2 => agents[1].launch_depth = None,
                3 => agents[1].ended_at = Some(now()),
                _ => run.agent_id = Some("missing".into()),
            }
            assert!(!eligible(&run, &agents), "condition {change}");
        }
    }

    #[test]
    fn text_names_the_first_turn_silence_deadline_and_the_parents_moves() {
        let (agents, mut run) = silent();
        run.deadline_at = Some(now() + Duration::from_secs(24 * 60 + 5));
        let stall = UnnoticedStall {
            child: &agents[1],
            parent: &agents[0],
            silent_secs: 1_920,
            cause: Cause::Silent(ChildStall::NeverStarted),
        };
        assert_eq!(
            text(&stall, &run, now()),
            "@still-silver has not started its first turn for 32m; its pane is open. Its run stays open until its deadline in 24m. RimZ will not stop it: look with `rimz pane capture @still-silver`, stop it with `rimz subagents stop @still-silver`, or relaunch the task elsewhere."
        );
    }

    #[test]
    fn detector_asks_for_one_helper_per_unnoticed_stall() {
        let (agents, run) = silent();
        let mut told = run.clone();
        told.stall_noticed_at = Some(now());
        let mut settled = run.clone();
        settled.status = RunStatus::Completed;
        let mut asked = Vec::new();
        notify_parents_with(
            &[told, run.clone(), settled],
            &agents,
            &run.workspace_id,
            now(),
            1_800,
            |_| Continuation::Off,
            |request| asked.push(request.clone()),
        );
        assert_eq!(
            asked,
            [StallNoticeRequest {
                workspace_id: run.workspace_id,
                run_id: run.run_id,
                stalled_after_secs: 1_800,
                continuation: Continuation::Off,
            }]
        );
    }

    #[test]
    fn text_names_the_silence_deadline_and_the_parents_moves() {
        let (mut agents, mut run) = silent();
        run.deadline_at = Some(now() + Duration::from_secs(24 * 60 + 5));
        assert_eq!(
            text(
                &unnoticed_stall(&run, &agents, now(), 1_800, |_| Continuation::Off).unwrap(),
                &run,
                now()
            ),
            "@still-silver has been silent for 32m and is still running; its pane is open. Its run stays open until its deadline in 24m. RimZ will not stop it: look with `rimz pane capture @still-silver`, stop it with `rimz subagents stop @still-silver`, or relaunch the task elsewhere."
        );
        run.deadline_at = Some(now());
        assert!(
            text(
                &unnoticed_stall(&run, &agents, now(), 1_800, |_| Continuation::Off).unwrap(),
                &run,
                now()
            )
            .contains("Its run stays open, past its deadline, until it is stopped.")
        );
        run.deadline_at = None;
        agents[1].name = None;
        agents[1].last_activity = now() - Duration::from_secs(9 * 3_600);
        assert!(
            text(
                &unnoticed_stall(&run, &agents, now(), 1_800, |_| Continuation::Off).unwrap(),
                &run,
                now()
            )
            .starts_with("@child has been silent for 9h")
        );
        assert!(
            text(
                &unnoticed_stall(&run, &agents, now(), 1_800, |_| Continuation::Off).unwrap(),
                &run,
                now()
            )
            .contains("Its run stays open. RimZ will not stop it:")
        );
    }
}
