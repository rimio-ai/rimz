//! Stop an active loop runner through durable cancellation and a SIGTERM backstop.

use super::*;
use rimz::harness::schedule::runner::StopOutcome;

pub(super) fn stop(name: &str, globals: &GlobalFlags) -> Result<()> {
    let root = match task_catalog(globals)?.for_run(name) {
        Some(task) => task.entry().resolved_root(),
        None => in_flight_without_row(name, globals)?
            .map(|(root, _)| root)
            .ok_or_else(|| anyhow::anyhow!("no loop task named `{name}`; see `rimz loop list`"))?,
    };
    let (outcome, pane_open) = stop_active_run(
        name,
        &root,
        schedule::backlog::StopCause::Stop,
        None,
        globals,
    )?;
    match outcome {
        StopOutcome::NoActiveRun => writeln!(ui::out(), "loop `{name}`: no active run")?,
        StopOutcome::Stopped {
            run_id,
            signaled,
            waiting,
        } => {
            let run_id = run_id.map(|id| format!(" · run {id}")).unwrap_or_default();
            let backstop = if signaled { " · SIGTERM" } else { "" };
            writeln!(ui::out(), "loop `{name}`: stopped{run_id}{backstop}")?;
            if waiting > 0 {
                let noun = if waiting == 1 { "trigger" } else { "triggers" };
                writeln!(
                    ui::out(),
                    "  {waiting} {noun} waiting · next starts now; `rimz loop pause {name}` drops them"
                )?;
            }
        }
    }
    if let Some(open) = pane_open {
        bail!(open);
    }
    Ok(())
}
