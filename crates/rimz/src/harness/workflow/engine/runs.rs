//! The `runs` step's join: the ending of a run the step started, carried onto the starting run's
//! record by the child's report.

use super::Result;
use crate::disk::lock::WorkspaceLock;
use crate::disk::paths::StatePaths;
use crate::ids::WorkflowRunId;
use crate::store::workflow::{
    self, AttemptKind, JoinedEnding, RunClass, RunEnding, RunHeader, RunStarter, RunState,
};

/// Store how the finished run `child` ended on the run whose `runs` step started it, under the
/// workspace lock the caller holds, and wake that run with an advance, which waits for the lock.
/// Only a run still active at the attempt that reserved `child`, with no ending stored for it
/// yet, takes the ending, so a second report, or one that arrives after the parent left the
/// attempt, changes nothing. A wake that cannot spawn leaves the parent to the tick's advance.
pub fn join_report(
    state: &StatePaths,
    child: &WorkflowRunId,
    header: &RunHeader,
    guard: &WorkspaceLock,
) -> Result<()> {
    let Some(RunStarter::Run { run: parent, .. }) = &header.starter else {
        return Ok(());
    };
    let paths = state.workflow(parent);
    let RunClass::Current(mut record) = workflow::classify(&paths)? else {
        return Ok(());
    };
    if record.state != RunState::Active {
        return Ok(());
    }
    let Some(started) = record
        .attempt
        .as_mut()
        .filter(|attempt| attempt.kind == AttemptKind::Workflow)
        .and_then(|attempt| attempt.runs.as_mut())
        .and_then(|runs| runs.runs.iter_mut().find(|run| &run.id == child))
        .filter(|run| run.ended.is_none())
    else {
        return Ok(());
    };
    started.ended = Some(ended(state, header));
    workflow::write(&paths, &record, guard)?;
    if let Err(error) = super::spawn(&record) {
        tracing::warn!(run = %record.id, %error, "joined run's advance could not be spawned");
    }
    Ok(())
}

/// The child's ending as its record's header names it, with where it worked and its notes.
fn ended(state: &StatePaths, header: &RunHeader) -> JoinedEnding {
    let ending = header.ending.clone().unwrap_or(match header.state {
        Some(RunState::Done) => RunEnding::Done,
        Some(RunState::Canceled) => RunEnding::Canceled,
        _ => RunEnding::Failed,
    });
    let reason = match &ending {
        RunEnding::CapacityGaveUp { reason, .. } => header.reason.clone().or(reason.clone()),
        _ => header.reason.clone(),
    };
    JoinedEnding {
        ending,
        reason,
        branch: header
            .scope_path
            .as_deref()
            .and_then(crate::worktree::current_branch),
        checkout: header.scope_path.clone(),
        notes: header
            .notes
            .as_ref()
            .map(|notes| state.notes_run(crate::disk::paths::Class::AutomationNotes, notes)),
    }
}
