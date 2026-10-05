//! A launched child parked on a provider limit, and the one notice its parent gets per park.
//!
//! This module only reads agent rows and run records and spawns a hidden CLI
//! helper. The helper claims the park on the run record and queues the notice,
//! so the sidebar producer remains read-only on the store.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::auto_continue::limit_marker_active;
use crate::RuntimePaths;
use crate::agents::{AgentState, AgentStatus};
use crate::ids::{RunId, WorkspaceId};
use crate::store::run::RunRecord;
use crate::store::snapshot::find_agent;
use crate::utils::time::format_duration_coarse;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParkNoticeRequest {
    pub workspace_id: WorkspaceId,
    pub run_id: RunId,
}

/// A child parked on a provider limit whose parent has not been told about this park.
pub struct UnnoticedPark<'a> {
    pub child: &'a AgentState,
    pub parent: &'a AgentState,
}

/// The park `run` owes its parent a notice for: an open subagent run whose
/// child's live turn died on a limit marker, with a live parent, and no notice
/// claimed since the child last acted. `agents` must carry joined context.
pub fn unnoticed_park<'a>(run: &RunRecord, agents: &'a [AgentState]) -> Option<UnnoticedPark<'a>> {
    if !run.subagent || run.status.is_terminal() {
        return None;
    }
    let child = find_agent(agents, run.kind.as_str(), run.agent_id.as_ref()?)?;
    if child.ended_at.is_some()
        || child.status != AgentStatus::Running
        || !limit_marker_active(child)
        || run
            .park_noticed_activity
            .is_some_and(|noticed| noticed >= child.last_activity)
    {
        return None;
    }
    let parent = crate::address::launched_parent(agents, child)
        .filter(|parent| parent.ended_at.is_none())?;
    Some(UnnoticedPark { child, parent })
}

fn notify_parents_with(
    runs: &[RunRecord],
    agents: &[AgentState],
    workspace_id: &WorkspaceId,
    mut spawn: impl FnMut(&ParkNoticeRequest),
) {
    for run in runs {
        if unnoticed_park(run, agents).is_some() {
            spawn(&ParkNoticeRequest {
                workspace_id: workspace_id.clone(),
                run_id: run.run_id.clone(),
            });
        }
    }
}

/// Ask one short-lived helper per unnoticed park to tell the child's parent.
/// The helper's locked claim settles a race between ticks.
pub(crate) fn notify_parents(runs: &[RunRecord], agents: &[AgentState], runtime: &RuntimePaths) {
    notify_parents_with(runs, agents, &runtime.workspace_id, |request| {
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
    });
}

/// The notice text: who parked, on what, how long the run stays open, and the parent's three moves.
pub fn text(child: &AgentState, run: &RunRecord, now: Timestamp) -> String {
    let handle = format!(
        "@{}",
        child
            .name
            .clone()
            .unwrap_or_else(|| child.agent_id.to_string())
    );
    let reason = match child.displayed_turn_error() {
        Some((class, error)) => match &error.label {
            Some(label) => format!("\"{label}\""),
            None => format!("a {}", class.words()),
        },
        None => "no label recorded".to_owned(),
    };
    let deadline = match run.deadline_at {
        Some(at) if at > now => format!(
            " until its deadline in {}",
            format_duration_coarse(at.duration_since(now).as_secs())
        ),
        Some(_) => ", past its deadline, until it is stopped".to_owned(),
        None => String::new(),
    };
    format!(
        "{handle} stopped on a provider limit: {reason}. Its run stays open{deadline}. \
         Wait for the reset, stop it with `rimz subagents stop {handle}`, or relaunch the task elsewhere."
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
            class,
            at,
            label: label.map(str::to_owned),
        });
        child.context = Some(context);
    }

    fn eligible(run: &RunRecord, agents: &[AgentState]) -> bool {
        unnoticed_park(run, agents).is_some()
    }

    #[test]
    fn a_limit_park_is_owed_one_notice_until_the_child_acts_again() {
        for class in [
            TurnErrorClass::PausedRateLimit,
            TurnErrorClass::PausedSpendLimit,
        ] {
            let (mut agents, mut run) = parked(class, Some("Usage limit reached"));
            let park = unnoticed_park(&run, &agents).expect("a fresh park is owed a notice");
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

    #[test]
    fn the_detector_asks_for_one_helper_per_unnoticed_park() {
        let (agents, parked_run) =
            parked(TurnErrorClass::PausedRateLimit, Some("Usage limit reached"));
        let mut told = parked_run.clone();
        told.run_id = RunId::new();
        told.park_noticed_activity = Some(agents[1].last_activity);
        let mut settled = parked_run.clone();
        settled.run_id = RunId::new();
        settled.status = RunStatus::Completed;
        let workspace_id = parked_run.workspace_id.clone();

        let mut asked = Vec::new();
        notify_parents_with(
            &[told, parked_run.clone(), settled],
            &agents,
            &workspace_id,
            |request| asked.push(request.clone()),
        );
        assert_eq!(
            asked,
            [ParkNoticeRequest {
                workspace_id,
                run_id: parked_run.run_id,
            }]
        );
    }

    #[test]
    fn text_names_the_child_the_label_the_deadline_and_the_three_moves() {
        let (agents, mut run) =
            parked(TurnErrorClass::PausedRateLimit, Some("Usage limit reached"));
        let now = Timestamp::now();
        run.deadline_at = Some(now + Duration::from_secs(24 * 60 + 5));
        assert_eq!(
            text(&agents[1], &run, now),
            "@still-silver stopped on a provider limit: \"Usage limit reached\". \
             Its run stays open until its deadline in 24m. Wait for the reset, \
             stop it with `rimz subagents stop @still-silver`, or relaunch the task elsewhere."
        );

        let (agents, run) = parked(TurnErrorClass::PausedSpendLimit, None);
        assert_eq!(
            text(&agents[1], &run, now),
            "@still-silver stopped on a provider limit: a spend limit. Its run stays open. \
             Wait for the reset, stop it with `rimz subagents stop @still-silver`, \
             or relaunch the task elsewhere."
        );
    }
}
