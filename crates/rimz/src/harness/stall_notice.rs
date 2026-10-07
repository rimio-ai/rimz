//! A silent launched child and the one notice its parent gets per run.
//!
//! Evaluation reads the producer's heartbeat-folded agents. Durable claims,
//! messages and assists belong to the detached helper, not the sidebar graph.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::agents::AgentState;
use crate::ids::{RunId, WorkspaceId};
use crate::store::run::{ReportTo, RunRecord};
use crate::store::snapshot::find_agent;
use crate::utils::time::format_duration_coarse;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StallNoticeRequest {
    pub workspace_id: WorkspaceId,
    pub run_id: RunId,
    pub stalled_after_secs: u32,
}

/// An open child whose silence has not yet been reported to its live parent.
pub struct UnnoticedStall<'a> {
    pub child: &'a AgentState,
    pub parent: &'a AgentState,
    pub silent_secs: u64,
}

/// Select a launcher-reporting run's unnoticed stall from heartbeat-folded agents.
pub fn unnoticed_stall<'a>(
    run: &RunRecord,
    agents: &'a [AgentState],
    now: Timestamp,
    stalled_after_secs: u32,
) -> Option<UnnoticedStall<'a>> {
    if !run.subagent
        || run.report_to != ReportTo::Launcher
        || run.status.is_terminal()
        || run.stall_noticed_at.is_some()
    {
        return None;
    }
    let child = find_agent(agents, run.kind.as_str(), run.agent_id.as_ref()?)?;
    let silent_secs = child.silent_child_for(now, stalled_after_secs)?;
    let parent = crate::address::launched_parent(agents, child)
        .filter(|parent| parent.ended_at.is_none())?;
    Some(UnnoticedStall {
        child,
        parent,
        silent_secs,
    })
}

fn notify_parents_with(
    runs: &[RunRecord],
    agents: &[AgentState],
    workspace_id: &WorkspaceId,
    now: Timestamp,
    stalled_after_secs: u32,
    mut spawn: impl FnMut(&StallNoticeRequest),
) {
    for run in runs {
        if unnoticed_stall(run, agents, now, stalled_after_secs).is_some() {
            spawn(&StallNoticeRequest {
                workspace_id: workspace_id.clone(),
                run_id: run.run_id.clone(),
                stalled_after_secs,
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
    stalled_after_secs: u32,
) {
    notify_parents_with(
        runs,
        agents,
        &runtime.workspace_id,
        now,
        stalled_after_secs,
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

/// Name the silent child, the still-open run, and the parent's three moves.
pub fn text(child: &AgentState, run: &RunRecord, silent_secs: u64, now: Timestamp) -> String {
    let handle = format!(
        "@{}",
        child
            .name
            .clone()
            .unwrap_or_else(|| child.agent_id.to_string())
    );
    let silence = format_duration_coarse(i64::try_from(silent_secs).unwrap_or(i64::MAX));
    let deadline = match run.deadline_at {
        Some(at) if at > now => format!(
            " until its deadline in {}",
            format_duration_coarse(at.duration_since(now).as_secs())
        ),
        Some(_) => ", past its deadline, until it is stopped".to_owned(),
        None => String::new(),
    };
    format!(
        "{handle} has been silent for {silence} and is still running; its pane is open. Its run stays open{deadline}. RimZ will not stop it: look with `rimz pane capture {handle}`, stop it with `rimz subagents stop {handle}`, or relaunch the task elsewhere."
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
        (vec![parent, child], run)
    }

    fn eligible(run: &RunRecord, agents: &[AgentState]) -> bool {
        unnoticed_stall(run, agents, now(), 1_800).is_some()
    }

    #[test]
    fn only_a_silent_live_child_run_with_a_live_launcher_is_owed_a_notice() {
        let (agents, run) = silent();
        let stall =
            unnoticed_stall(&run, &agents, now(), 1_800).expect("silent child owes a notice");
        assert_eq!(stall.child.agent_id.as_str(), "child");
        assert_eq!(stall.parent.agent_id.as_str(), "parent");
        assert_eq!(stall.silent_secs, 1_920);
        assert!(unnoticed_stall(&run, &agents, now(), 1_921).is_none());
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
                2 => run.agent_id = None,
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
            AgentStatus::Idle,
            AgentStatus::Success,
            AgentStatus::Paused,
            AgentStatus::Failed,
            AgentStatus::Waiting,
        ] {
            let mut agents = agents.clone();
            agents[1].status = status;
            assert!(!eligible(&run, &agents), "{status:?}");
        }
        let mut sleeping = agents.clone();
        sleeping[1].status = AgentStatus::Idle;
        sleeping[1].pending_waits.push(PendingWait {
            name: "wait-command".to_owned(),
            trigger: PendingWaitTrigger::Command {
                command: "cargo test".to_owned(),
            },
            armed_at: Some(now()),
        });
        assert!(!eligible(&run, &sleeping), "an armed wait is not a stall");
        for class in [
            TurnErrorClass::PausedRateLimit,
            TurnErrorClass::PausedSpendLimit,
            TurnErrorClass::PausedOverloaded,
            TurnErrorClass::Failed,
            TurnErrorClass::Unknown,
        ] {
            let mut errored = agents.clone();
            let mut context = AgentContext::new("codex", now());
            context.turn_error = Some(AgentTurnError {
                class,
                at: now(),
                label: None,
            });
            errored[1].context = Some(context);
            assert!(!eligible(&run, &errored), "{class:?}");
        }
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
            |request| asked.push(request.clone()),
        );
        assert_eq!(
            asked,
            [StallNoticeRequest {
                workspace_id: run.workspace_id,
                run_id: run.run_id,
                stalled_after_secs: 1_800
            }]
        );
    }

    #[test]
    fn text_names_the_silence_deadline_and_the_parents_moves() {
        let (mut agents, mut run) = silent();
        run.deadline_at = Some(now() + Duration::from_secs(24 * 60 + 5));
        assert_eq!(
            text(&agents[1], &run, 1_920, now()),
            "@still-silver has been silent for 32m and is still running; its pane is open. Its run stays open until its deadline in 24m. RimZ will not stop it: look with `rimz pane capture @still-silver`, stop it with `rimz subagents stop @still-silver`, or relaunch the task elsewhere."
        );
        run.deadline_at = Some(now());
        assert!(
            text(&agents[1], &run, 1_920, now())
                .contains("Its run stays open, past its deadline, until it is stopped.")
        );
        run.deadline_at = None;
        agents[1].name = None;
        assert!(
            text(&agents[1], &run, 9 * 3_600, now()).starts_with("@child has been silent for 9h")
        );
        assert!(
            text(&agents[1], &run, 1_920, now())
                .contains("Its run stays open. RimZ will not stop it:")
        );
    }
}
