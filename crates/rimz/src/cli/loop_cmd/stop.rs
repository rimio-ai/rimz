//! Stop an active loop runner through durable cancellation and a SIGTERM backstop.

use super::*;
use rimz::harness::schedule::runner::StopOutcome;

pub(super) fn stop(name: &str, globals: &GlobalFlags) -> Result<()> {
    let root = match task_catalog(globals)?.for_run(name) {
        Some(task) => task.entry().resolved_root(),
        None => match in_flight_without_row(name, globals)? {
            Some((root, _)) => root,
            // A removed task's live run is still the task's to stop.
            None => project_root_for_globals(globals)
                .filter(|root| live_workflow_runs(name, root).is_ok_and(|runs| !runs.is_empty()))
                .ok_or_else(|| {
                    anyhow::anyhow!("no loop task named `{name}`; see `rimz loop list`")
                })?,
        },
    };
    let (outcome, pane_open) = stop_active_run(name, &root, None, globals)?;
    let (canceled, mut failures) = cancel_workflow_runs(name, &root)?;
    match outcome {
        StopOutcome::NoActiveRun if canceled > 0 || !failures.is_empty() => {}
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
    failures.extend(pane_open);
    if !failures.is_empty() {
        bail!(failures.join("\n"));
    }
    Ok(())
}

/// Cancel each live workflow run the task started, as `rimz workflow cancel` does; the run's
/// own report then records the end and starts the next waiting trigger. A run that fails to
/// cancel leaves the others to try, and its error is returned beside the count.
fn cancel_workflow_runs(name: &str, root: &Path) -> Result<(usize, Vec<String>)> {
    let mut canceled = 0;
    let mut failures = Vec::new();
    for run in live_workflow_runs(name, root)? {
        let output = std::process::Command::new(rimz::proc::rimz_exe())
            .args(rimz::harness::workflow::cancel_args(&run.id, "loop stop"))
            .current_dir(root)
            .stdin(std::process::Stdio::null())
            .output();
        let failure = match output {
            Err(error) => format!("canceling workflow run {}: {error}", run.id),
            Ok(output) if !output.status.success() => format!(
                "loop `{name}`: workflow run {} not canceled: {}",
                run.id,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            Ok(_) => {
                writeln!(ui::out(), "loop `{name}`: stopped · run {}", run.id)?;
                canceled += 1;
                continue;
            }
        };
        failures.push(failure);
    }
    Ok((canceled, failures))
}

fn live_workflow_runs(name: &str, root: &Path) -> Result<Vec<rimz::store::workflow::RunRecord>> {
    let state = StatePaths::for_project_root(root)?;
    if !state.root.exists() {
        return Ok(Vec::new());
    }
    let mut runs = rimz::store::workflow::live_runs(&state)?;
    runs.retain(|run| run.started_by(name));
    Ok(runs)
}
