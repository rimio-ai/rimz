//! Stop an active loop runner through durable cancellation and a SIGTERM backstop, then
//! forward a stop to everything alive the task's fires launched.

use super::*;
use rimz::harness::schedule::launches::Launch;
use rimz::harness::schedule::runner::StopOutcome;

pub(super) fn stop(name: &str, globals: &GlobalFlags) -> Result<()> {
    let root = match task_catalog(globals)?.for_run(name) {
        Some(task) => task.entry().resolved_root(),
        None => match in_flight_without_row(name, globals)? {
            Some((root, _)) => root,
            // A removed task's live launch is still the task's to stop.
            None => project_root_for_globals(globals)
                .filter(|root| {
                    task_id(name, None, Some(root))
                        .and_then(|id| alive_launches(&id, root).ok())
                        .is_some_and(|(alive, _)| !alive.is_empty())
                })
                .ok_or_else(|| {
                    anyhow::anyhow!("no loop task named `{name}`; see `rimz loop list`")
                })?,
        },
    };
    let (outcome, pane_open) = stop_active_run(name, &root, None, globals)?;
    let (stopped, mut failures) = stop_alive_launches(name, &root, globals)?;
    match outcome {
        StopOutcome::NoActiveRun if stopped > 0 || !failures.is_empty() => {}
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

/// Forward a stop to each launch alive under one of the task's fires, through its own public
/// stop: `rimz workflow cancel` for a workflow run, the supervised stop for a `-p` run, and
/// `rimz agents stop` for an agent (a team is its agents). A launch that fails to stop leaves
/// the others to try, and its error is returned beside the count stopped.
fn stop_alive_launches(
    name: &str,
    root: &Path,
    globals: &GlobalFlags,
) -> Result<(usize, Vec<String>)> {
    let Some(id) = task_id(name, None, Some(root)) else {
        return Ok((0, Vec::new()));
    };
    let (alive, rows) = alive_launches(&id, root)?;
    let mut stopped = 0;
    let mut failures = Vec::new();
    for launch in &alive {
        let address = launch_address(&launch.launch, &rows);
        let result = match &launch.launch {
            Launch::Workflow(run) => forward_stop(
                root,
                globals,
                rimz::harness::workflow::cancel_args(&run.id, "loop stop"),
            )
            .map(|()| None),
            Launch::Supervised(run) => stop_supervised(name, root, run, globals),
            Launch::Agent(_) => {
                forward_stop(root, globals, ["agents", "stop", address.as_str()]).map(|()| None)
            }
        };
        match result {
            Ok(pane_open) => {
                stopped += 1;
                let label = match &launch.launch {
                    Launch::Agent(_) => address.clone(),
                    Launch::Workflow(_) | Launch::Supervised(_) => format!("run {address}"),
                };
                writeln!(ui::out(), "loop `{name}`: stopped · {label}")?;
                failures.extend(pane_open);
            }
            Err(err) => failures.push(match &launch.launch {
                Launch::Workflow(_) => {
                    format!("loop `{name}`: workflow run {address} not canceled: {err:#}")
                }
                Launch::Supervised(_) => format!("loop `{name}`: run {address}: {err:#}"),
                Launch::Agent(_) => format!("loop `{name}`: {address} not stopped: {err:#}"),
            }),
        }
    }
    Ok((stopped, failures))
}

fn stop_supervised(
    name: &str,
    root: &Path,
    run: &rimz::store::run::RunRecord,
    globals: &GlobalFlags,
) -> Result<Option<String>> {
    let workspace = rimz::WorkspaceResolver::resolve(root, None)
        .with_context(|| format!("resolving project root at {}", root.display()))?;
    let paths = StatePaths::for_project_root(&workspace.project_root)?;
    stop_owned_run(name, Some(&workspace), paths, run, globals)
}

/// Run one child `rimz` stop command in the task's room, under this command's `--mux`, failing
/// with its stderr.
fn forward_stop<I, S>(root: &Path, globals: &GlobalFlags, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut command = std::process::Command::new(rimz::proc::rimz_exe());
    if let Some(mux) = globals.mux {
        command.args(["--mux", mux.as_str()]);
    }
    let output = command
        .args(args)
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .output()?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}
