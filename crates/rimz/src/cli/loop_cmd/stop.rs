//! Stop an active loop runner through durable cancellation and a SIGTERM backstop.

use super::*;
use crate::cli::supervised::StopRunErr;
use rimz::harness::schedule::runner::{StopOutcome, stop_task};

pub(super) fn stop(name: &str, globals: &GlobalFlags) -> Result<()> {
    let root = match task_catalog(globals)?.for_run(name) {
        Some(task) => task.entry().resolved_root(),
        None => in_flight_without_row(name, globals)?
            .map(|(root, _)| root)
            .ok_or_else(|| anyhow::anyhow!("no loop task named `{name}`; see `rimz loop list`"))?,
    };
    let mut pane_open = None;
    let outcome = stop_task(name, &root, |workspace, paths, record| {
        let id = paths.workspace_id.clone();
        let store = crate::cli::open_existing_store_at(paths)?
            .with_context(|| crate::cli::no_room(format_args!("for workspace {id}")))?;
        match (workspace, record) {
            (_, None) => Ok(()),
            (Some(workspace), Some(record)) => {
                match crate::cli::supervised::stop_supervised_run(
                    workspace, &store, globals, record,
                ) {
                    // The run did stop: the lock wait and the SIGTERM backstop
                    // still apply, and the open pane fails the command after them.
                    // A second `loop stop` finds the lock released and never
                    // reaches the pane, so the fix names the run's own stop.
                    Err(StopRunErr::PaneOpen(open)) => {
                        pane_open = Some(format!(
                            "loop `{name}`: {open}; run `rimz agents stop {}` to close it",
                            record.run_id
                        ));
                        Ok(())
                    }
                    Err(StopRunErr::NotCanceled(err) | StopRunErr::NotEnded(err)) => Err(err),
                    Ok(()) => Ok(()),
                }
            }
            (None, Some(record)) => crate::cli::supervised::cancel_supervised_run(&store, record),
        }
    });
    let outcome = match (outcome, &pane_open) {
        (Err(err), Some(open)) => return Err(err.context(open.clone())),
        (outcome, _) => outcome?,
    };
    match outcome {
        StopOutcome::NoActiveRun => writeln!(ui::out(), "loop `{name}`: no active run")?,
        StopOutcome::Stopped { run_id, signaled } => {
            let run_id = run_id.map(|id| format!(" · run {id}")).unwrap_or_default();
            let backstop = if signaled { " · SIGTERM" } else { "" };
            writeln!(ui::out(), "loop `{name}`: stopped{run_id}{backstop}")?;
        }
    }
    if let Some(open) = pane_open {
        bail!(open);
    }
    Ok(())
}
