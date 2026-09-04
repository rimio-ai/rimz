use anyhow::{Context, Result};
use serde_json::Map;

use rimz::harness::schedule::catalog::TaskCatalog;
use rimz::harness::schedule::runner::{
    CheckEcho, SCHEDULED_RUN_DEFAULT_TIMEOUT, check_record, check_timeout, parse_task_timeout,
    run_check,
};
use rimz::harness::schedule::signal::{
    Signal, SignalSource, WatchOutcome, acquire_watch_lock, fire_signal,
};

use super::*;

pub(super) fn run(name: &str, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let catalog = TaskCatalog::load(Some(&ctx.workspace.project_root))?;
    let Some(task) = catalog.for_run(name) else {
        return Ok(());
    };
    if task.entry().resolved_root() != ctx.workspace.project_root {
        return Ok(());
    }
    let Some(command) = task.entry().watch.as_deref() else {
        return Ok(());
    };
    let Some(_guard) = acquire_watch_lock(ctx.runtime(), name).context("locking wake watcher")?
    else {
        return Ok(());
    };
    let configured_timeout = rimz::config::MachineConfig::load_lenient()
        .r#loop
        .default_timeout
        .as_deref()
        .map(parse_task_timeout)
        .transpose()
        .map_err(anyhow::Error::msg)?;
    let timeout = check_timeout(task.entry())?
        .or(configured_timeout)
        .unwrap_or(SCHEDULED_RUN_DEFAULT_TIMEOUT);
    let outcome = run_check(
        &ctx.workspace.project_root,
        command,
        timeout,
        CheckEcho::Capture,
    )?;
    let check = check_record(&outcome);
    let watch = if check.timed_out {
        WatchOutcome::TimedOut {
            code: check.code,
            output: check.output,
        }
    } else {
        WatchOutcome::Exited {
            code: check.code,
            output: check.output,
        }
    };
    let signal = Signal {
        name: format!("wake.{name}")
            .parse()
            .expect("generated wake signal name is valid"),
        payload: Map::new(),
        source: SignalSource::Watch,
        watch: Some(watch),
    };
    ctx.store
        .append_signal(&ctx.workspace.session_name, &signal)
        .context("appending wake signal")?;
    fire_signal(ctx.runtime(), Some(&ctx.workspace.project_root), &signal)
        .context("firing watched wake")?;
    Ok(())
}
