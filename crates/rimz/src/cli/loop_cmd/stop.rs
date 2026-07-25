//! Stop an active loop runner through durable cancellation and a SIGTERM backstop.

use super::*;

const STOP_GRACE: Duration = Duration::from_secs(5);

pub(super) fn stop(name: &str, globals: &GlobalFlags) -> Result<()> {
    let task = task_catalog(globals)?
        .for_run(name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no loop task named `{name}`; see `rimz loop list`"))?;
    let entry = task.entry();
    let lock_state = probe_run_lock(name, entry)?;
    if next_stop_action(&lock_state, false, false, false) == StopAction::Done {
        writeln!(ui::out(), "loop `{name}`: no active run")?;
        return Ok(());
    }

    let root = entry.resolved_root();
    let workspace = WorkspaceResolver::resolve(&root, None)
        .with_context(|| format!("resolving project root at {}", root.display()))?;
    let store = crate::cli::open_store(&workspace)?;
    let run = newest_active_run(store.paths(), name)?;
    if next_stop_action(&lock_state, run.is_some(), false, false) == StopAction::CancelRun
        && let Some(record) = &run
    {
        crate::cli::supervised::stop_supervised_run(&workspace, &store, globals, record)?;
    }

    if wait_for_run_lock_release(name, entry, STOP_GRACE)? {
        write_stopped(name, run.as_ref(), false)?;
        return Ok(());
    }

    let action = next_stop_action(&lock_state, run.is_some(), true, false);
    let (holder, signal_error) = match action {
        StopAction::Signal(info) => match signal_run_lock_holder(&info) {
            Ok(()) => (Some(info), None),
            Err(err) => (Some(info), Some(err)),
        },
        StopAction::Done | StopAction::CancelRun | StopAction::Manual => {
            (lock_info(&lock_state), None)
        }
    };

    if signal_error.is_none()
        && let Some(info) = holder
        && wait_for_run_lock_release(name, entry, STOP_GRACE)?
    {
        append_stopped_record(name, &task, info, run.as_ref());
        write_stopped(name, run.as_ref(), true)?;
        return Ok(());
    }

    let lock = run_lock_path(name, entry)?;
    let holder = holder
        .map(|info| format!(" (pid {})", info.pid))
        .unwrap_or_default();
    let signal = signal_error
        .map(|err| format!("; SIGTERM failed: {err:#}"))
        .unwrap_or_default();
    bail!(
        "loop `{name}` is still active{holder}; lock {}; stop the holder manually and retry{signal}",
        lock.display()
    )
}

fn lock_info(state: &RunLockState) -> Option<RunLockInfo> {
    match state {
        RunLockState::Held(info) => *info,
        RunLockState::Available => None,
    }
}

fn append_stopped_record(
    name: &str,
    task: &LoadedTask,
    info: RunLockInfo,
    run: Option<&RunRecord>,
) {
    let elapsed = Timestamp::now()
        .as_millisecond()
        .saturating_sub(info.started_at.as_millisecond());
    let duration_ms = u64::try_from(elapsed).unwrap_or(0);
    let mut record = LoopRunRecord::new(
        name,
        LoopRunResult::Canceled,
        LoopRunMode::Scheduled,
        duration_ms,
    );
    record.mode = None;
    record.error = Some("stopped by rimz loop stop".to_owned());
    record.run_id = run.map(|record| record.run_id.to_string());
    run_log::record_transition(task, &record);
}

fn write_stopped(name: &str, run: Option<&RunRecord>, signaled: bool) -> std::io::Result<()> {
    let run_id = run
        .map(|record| format!(" · run {}", record.run_id))
        .unwrap_or_default();
    let backstop = if signaled { " · SIGTERM" } else { "" };
    writeln!(ui::out(), "loop `{name}`: stopped{run_id}{backstop}")
}
