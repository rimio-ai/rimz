//! Stop an active loop runner through durable cancellation and a SIGTERM backstop.

use super::*;
use rimz::harness::schedule::runner::{StopOutcome, stop_task};

pub(super) fn stop(name: &str, globals: &GlobalFlags) -> Result<()> {
    let task = task_catalog(globals)?
        .for_run(name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no loop task named `{name}`; see `rimz loop list`"))?;
    match stop_task(name, &task, |workspace, id, record| {
        let store = rimz::Store::open(
            StatePaths::for_workspace(id.clone())?,
            RuntimePaths::for_workspace(id)?,
        )?;
        match (workspace, record) {
            (_, None) => Ok(()),
            (Some(workspace), Some(record)) => {
                crate::cli::supervised::stop_supervised_run(workspace, &store, globals, record)
            }
            (None, Some(record)) => crate::cli::supervised::cancel_supervised_run(&store, record),
        }
    })? {
        StopOutcome::NoActiveRun => writeln!(ui::out(), "loop `{name}`: no active run")?,
        StopOutcome::Stopped { run_id, signaled } => {
            let run_id = run_id.map(|id| format!(" · run {id}")).unwrap_or_default();
            let backstop = if signaled { " · SIGTERM" } else { "" };
            writeln!(ui::out(), "loop `{name}`: stopped{run_id}{backstop}")?;
        }
    }
    Ok(())
}
