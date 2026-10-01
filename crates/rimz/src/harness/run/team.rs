//! The team-long run an agent-launched team keeps on its leader.
//!
//! One run per stretch of work: opened by the launch prompt or by a flip out of
//! `Done`, fed the leader's final message at every turn end, and settled by
//! the flip into `Done` or the cohort's death, reporting the leader to the launcher.

use std::path::Path;

use crate::agents::{AgentDefinition, AgentState, PermissionMode};
use crate::disk::lock::WorkspaceLock;
use crate::disk::paths::StatePaths;
use crate::ids::MessageId;
use crate::store::run::{RunStatus, TeamRun};

use super::{RecordMutation, Result, RunRecord};

/// `<team>#<channel>` for a team seat, grouped as `address::team_cohorts` groups it.
fn team_instance(seat: &AgentState) -> Option<String> {
    let team = seat.team.as_deref().filter(|team| !team.is_empty())?;
    let channel = seat.channel().unwrap_or_else(|| "external".to_owned());
    Some(format!("{team}#{channel}"))
}

/// Open the leader's team run for an agent-launched team's launch prompt.
pub(super) fn create_team_run(
    paths: &StatePaths,
    leader: &AgentState,
    reader: Option<&str>,
    adapter: &AgentDefinition,
    prompt: &str,
    cwd: &Path,
) -> Result<Option<RunRecord>> {
    if leader.launched_by.is_none() || prompt.trim().is_empty() || !super::peer_can_report(adapter)
    {
        return Ok(None);
    }
    let (Some(launch_id), Some(instance)) = (leader.launch_id.as_ref(), team_instance(leader))
    else {
        return Ok(None);
    };
    let _guard = WorkspaceLock::acquire(&paths.workspace_lock)?;
    if let Some(record) = open_team_run_for(paths, &instance)? {
        return Ok(Some(record));
    }
    let mut record = RunRecord::new(
        paths.workspace_id.clone(),
        leader.kind.clone(),
        leader.mode.unwrap_or(PermissionMode::Auto),
        prompt.to_owned(),
        leader
            .worktree_path
            .as_deref()
            .map_or_else(|| cwd.to_path_buf(), Into::into),
    );
    record.agent_name = leader.name.clone();
    record.reader = reader.map(str::to_owned);
    record.team = Some(TeamRun {
        launch_id: launch_id.clone(),
        instance,
    });
    crate::store::run::write(&paths.runs_dir, &record)?;
    Ok(Some(record))
}

/// The open team run whose leader is `seat`'s launch.
pub fn open_team_run(paths: &StatePaths, seat: &AgentState) -> Result<Option<RunRecord>> {
    let launch_id = seat.launch_id.as_ref().unwrap_or(&seat.agent_id);
    Ok(super::list(paths)?.into_iter().find(|record| {
        !record.status.is_terminal()
            && record.kind == seat.kind
            && record
                .team
                .as_ref()
                .is_some_and(|team| &team.launch_id == launch_id)
    }))
}

/// The open team run of the cohort `instance` (`<team>#<channel>`).
pub fn open_team_run_for(paths: &StatePaths, instance: &str) -> Result<Option<RunRecord>> {
    Ok(team_runs(paths, instance)?
        .into_iter()
        .find(|record| !record.status.is_terminal()))
}

/// Settle the cohort's open run, stamping its report (absent when no launcher is left):
/// completed, or failed with `failure` as its tail.
/// Returns `None` when already settled so racing reporters cancel their own message.
pub fn settle_team_run(
    paths: &StatePaths,
    run_id: &crate::ids::RunId,
    report: Option<&MessageId>,
    failure: Option<&str>,
) -> Result<Option<RunRecord>> {
    let status = match failure {
        Some(_) => RunStatus::Failed,
        None => RunStatus::Completed,
    };
    let (record, settled) = super::update_record(paths, run_id, |record, now| {
        if !record.mark_terminal(status, now) {
            return Ok(RecordMutation::Keep(false));
        }
        record.report_message_id = report.cloned();
        if let Some(failure) = failure {
            record.failure_tail = Some(failure.into());
        }
        Ok(RecordMutation::Write(true))
    })?;
    Ok(settled.then_some(record))
}

/// Open a fresh team run for a board that left `Done`, carrying the task and
/// leader of the cohort's newest run. `None` when the cohort never had one
/// (a user-launched team) or already has one open.
pub fn reopen_team_run(paths: &StatePaths, instance: &str) -> Result<Option<RunRecord>> {
    let _guard = WorkspaceLock::acquire(&paths.workspace_lock)?;
    let runs = team_runs(paths, instance)?;
    let Some(newest) = runs.first() else {
        return Ok(None);
    };
    if !newest.status.is_terminal() {
        return Ok(None);
    }
    let mut record = RunRecord::new(
        paths.workspace_id.clone(),
        newest.kind.clone(),
        newest.permission_mode,
        newest.prompt.clone(),
        newest.worktree_path.clone(),
    );
    record.agent_id.clone_from(&newest.agent_id);
    record.agent_name.clone_from(&newest.agent_name);
    record.reader.clone_from(&newest.reader);
    record.transcript_path.clone_from(&newest.transcript_path);
    record.team.clone_from(&newest.team);
    record.status = RunStatus::Running;
    crate::store::run::write(&paths.runs_dir, &record)?;
    Ok(Some(record))
}

/// The cohort's team runs, newest first.
fn team_runs(paths: &StatePaths, instance: &str) -> Result<Vec<RunRecord>> {
    let mut runs = super::list(paths)?
        .into_iter()
        .filter(|record| {
            record
                .team
                .as_ref()
                .is_some_and(|team| team.instance == instance)
        })
        .collect::<Vec<_>>();
    runs.sort_by_key(|run| std::cmp::Reverse(run.started_at));
    Ok(runs)
}
