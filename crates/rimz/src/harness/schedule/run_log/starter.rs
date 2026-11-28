//! The report a run owes the agent that started it without `--detach`: one `WORKFLOW_REPORT`
//! notice, queued for the starter's next turn boundary while the starter lives, and the run's
//! report mark set sent either way. The message id is written into the record before the notice
//! is queued, so a collection that dies between the two queues the same message on its retry and
//! the queue takes it once.
//!
//! A run a `runs` step started reports to that run instead: its ending is written onto the
//! starting run's record, which the engine wakes with an advance, and its own mark set sent
//! under the same lock.

use crate::disk::lock::WorkspaceLock;
use crate::ids::{MessageId, WorkflowRunId};
use crate::store::message::{DeliveryGate, HarnessNotice, MessageRecord, MessageSender};
use crate::store::workflow::{self, RunEnding, RunHeader, RunStarter, RunState};

/// Report the finished run `run` to its agent starter, once, then mark its report sent. Takes
/// the workspace lock the caller read `header` under, and releases it while the notice is
/// queued, since the queue takes that lock itself.
pub(super) fn report(
    state: &crate::StatePaths,
    run: &WorkflowRunId,
    header: &RunHeader,
    guard: WorkspaceLock,
) -> anyhow::Result<()> {
    let paths = state.workflow(run);
    if let Some(RunStarter::Run { .. }) = &header.starter {
        crate::harness::workflow::join_report(state, run, header, &guard)?;
        return Ok(workflow::mark_header_report_sent(&paths, &guard)?);
    }
    let message = workflow::stamp_report_message(&paths, &guard)?;
    drop(guard);
    #[cfg(feature = "testkit")]
    crate::testkit::rendezvous("RIMZ_TEST_WORKFLOW_REPORT_GATE");
    queue(state, run, header, &message)?;
    let guard = WorkspaceLock::acquire(&state.workspace_lock)?;
    // Another collection may have settled the run while the lock was down.
    if workflow::classify(&paths)?
        .header()
        .is_some_and(|header| header.owes_report())
    {
        workflow::mark_header_report_sent(&paths, &guard)?;
    }
    Ok(())
}

/// Queue the report for the starter's live row; a starter that ended, or a room without a
/// record, is told nothing.
fn queue(
    state: &crate::StatePaths,
    run: &WorkflowRunId,
    header: &RunHeader,
    message: &MessageId,
) -> anyhow::Result<()> {
    let Some(RunStarter::Agent { kind, launch }) = &header.starter else {
        return Ok(());
    };
    let runtime = crate::disk::paths::RuntimePaths::for_state(state)?;
    let Some(store) = crate::store::Store::open_existing(state.clone(), runtime) else {
        return Ok(());
    };
    let mut projection = store.runtime_projection(crate::store::runtime::RuntimeScope::Audit)?;
    crate::store::agent_context::attach_rest_certificates(
        store.runtime_paths(),
        projection.agents.iter_mut(),
    );
    let Some(agent) = crate::address::launch_row(&projection.agents, kind, launch)
        .filter(|agent| agent.ended_at.is_none())
    else {
        return Ok(());
    };
    let room = crate::workspace::record::read(&state.workspace_record)?;
    let mut record = MessageRecord::new(
        room.workspace_id,
        agent,
        text(state, run, header),
        DeliveryGate::Done,
    )
    .with_channel(agent.lane_name())
    .with_sender(MessageSender::Harness {
        notice: HarnessNotice::WorkflowReport,
    })
    .with_automated(true);
    record.message_id = message.clone();
    store.queue_message_once(&record, &room.session_name)?;
    Ok(())
}

/// How the run ended, where it worked, its notes folder, and the command that shows its history.
fn text(state: &crate::StatePaths, run: &WorkflowRunId, header: &RunHeader) -> String {
    let (ending, cause) = match &header.ending {
        Some(RunEnding::Done | RunEnding::Tried) => ("done", None),
        Some(RunEnding::Canceled) => ("canceled", None),
        Some(RunEnding::Failed) => ("failed", None),
        Some(RunEnding::CapacityGaveUp { reason, .. }) => ("failed", reason.as_deref()),
        None => match header.state {
            Some(RunState::Done) => ("done", None),
            Some(RunState::Canceled) => ("canceled", None),
            _ => ("failed", None),
        },
    };
    let workflow = header.workflow.as_deref().unwrap_or("unnamed workflow");
    let shown = crate::store::workflow::RunNames::new(state).shown(run);
    let mut lines = vec![format!("workflow run {shown} ({workflow}) ended {ending}")];
    if let Some(reason) = header.reason.as_deref().or(cause) {
        lines.push(format!("reason {reason}"));
    }
    if let Some(checkout) = &header.scope_path {
        let branch = crate::worktree::current_branch(checkout)
            .map(|branch| format!(" on branch {branch}"))
            .unwrap_or_default();
        lines.push(format!("checkout {}{branch}", checkout.display()));
    }
    if let Some(notes) = &header.notes {
        let notes = state.notes_run(crate::disk::paths::Class::AutomationNotes, notes);
        lines.push(format!("notes {}", notes.display()));
    }
    lines.push(format!(
        "history: rimz {}",
        crate::store::workflow::workflow_command("show", &shown, &["--history"])
    ));
    lines.join("\n")
}
