//! Focus dwell, synchronous thaw and crash recovery, outside the sidebar's read-only graph.

use super::*;

#[derive(Debug, PartialEq, Eq)]
pub enum Woken {
    NotHibernating,
    Thawed {
        kind: AgentKind,
        agent_id: AgentSessionId,
    },
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub thawed: usize,
    pub withdrawn: usize,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RefaultHelperRequest {
    workspace_id: WorkspaceId,
    entry: crate::store::hibernate::Entry,
    thawed_at: jiff::Timestamp,
    baseline_anon: u64,
    baseline_file: u64,
}

enum WakeTrigger<'a> {
    Sync,
    Helper {
        requested_at_ms: Option<u64>,
        ready: &'a mut dyn FnMut() -> Option<Dwell>,
    },
}

const FOCUS_DWELL_MS: u64 = 1_000;
const FOCUS_SETTLE_MS: u64 = 250;
const INPUT_POLL_MS: u64 = 50;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dwell {
    Input,
    Elapsed,
}

trait Clock {
    fn elapsed_ms(&self) -> u64;
    fn sleep(&self, ms: u64);
}

struct SystemClock {
    initial_ms: u64,
    started: std::time::Instant,
}

impl Clock for SystemClock {
    fn elapsed_ms(&self) -> u64 {
        self.initial_ms
            .saturating_add(self.started.elapsed().as_millis() as u64)
    }

    fn sleep(&self, ms: u64) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
}

fn focus_dwell(
    clock: &impl Clock,
    mut queue: impl FnMut() -> Option<u64>,
    focused: impl Fn() -> Option<bool>,
) -> Option<Dwell> {
    if clock.elapsed_ms() >= FOCUS_DWELL_MS {
        return focused().unwrap_or(true).then_some(Dwell::Elapsed);
    }
    clock.sleep(FOCUS_SETTLE_MS.saturating_sub(clock.elapsed_ms()));
    let baseline = queue();
    loop {
        let remaining = FOCUS_DWELL_MS.saturating_sub(clock.elapsed_ms());
        if remaining == 0 {
            return focused().unwrap_or(true).then_some(Dwell::Elapsed);
        }
        clock.sleep(INPUT_POLL_MS.min(remaining));
        if clock.elapsed_ms() >= FOCUS_DWELL_MS {
            return focused().unwrap_or(true).then_some(Dwell::Elapsed);
        }
        if baseline
            .zip(queue())
            .is_some_and(|(before, after)| after > before)
        {
            return focused().unwrap_or(true).then_some(Dwell::Input);
        }
    }
}

fn wake_focus_with(
    paths: &crate::StatePaths,
    runtime: &crate::RuntimePaths,
    request: &WakeHelperRequest,
    root: &Path,
    mut wait: impl FnMut() -> Option<Dwell>,
    focused: impl Fn() -> Option<bool>,
    spawn: impl FnMut(RefaultHelperRequest),
) -> Result<Woken, HibernateErr> {
    let mut outcome = if crate::store::hibernate::read(paths)
        .iter()
        .any(|entry| entry.pane_id == request.pane_id)
    {
        let Some(outcome) = wait() else {
            return Ok(Woken::NotHibernating);
        };
        Some(outcome)
    } else {
        None
    };
    let mut ready = || {
        // A record may land while acquiring the lock; that wake still needs a dwell.
        let outcome = outcome.take().or_else(&mut wait)?;
        // Lock acquisition can outlast the decision's focus evidence.
        focused().unwrap_or(true).then_some(outcome)
    };
    wake_with(
        paths,
        runtime,
        &request.pane_id,
        root,
        crate::disk::lock::LOCK_TIMEOUT,
        WakeTrigger::Helper {
            requested_at_ms: request.requested_at_ms,
            ready: &mut ready,
        },
        spawn,
    )
}

pub fn wake_request(
    store: &Store,
    request: &WakeHelperRequest,
    backend: &dyn crate::mux::MuxBackend,
    session_name: &str,
) -> Result<Woken, HibernateErr> {
    let clock = SystemClock {
        initial_ms: request.requested_at_ms.map_or(0, |requested| {
            crate::utils::time::unix_now_ms().saturating_sub(requested)
        }),
        started: std::time::Instant::now(),
    };
    let tty = crate::store::hibernate::read(store.paths())
        .iter()
        .find(|entry| entry.pane_id == request.pane_id)
        .and_then(|entry| crate::proc::input_tty(entry.provider_pid));
    let focused = || {
        backend
            .client_view(crate::mux::ClientFocusOptions {
                session_name: Some(session_name.to_owned()),
                command_timeout: Some(std::time::Duration::from_millis(250)),
            })
            .ok()
            .map(|view| {
                if view.clients.is_empty() {
                    view.viewed_panes.contains(&request.pane_id)
                } else {
                    view.clients
                        .iter()
                        .any(|client| client.pane_id == request.pane_id)
                }
            })
    };
    wake_focus_with(
        store.paths(),
        store.runtime_paths(),
        request,
        Path::new("/sys/fs/cgroup"),
        || {
            focus_dwell(
                &clock,
                || {
                    tty.as_ref()
                        .and_then(|tty| rustix::io::ioctl_fionread(tty).ok())
                },
                focused,
            )
        },
        focused,
        |request| spawn_refault(store.runtime_paths(), request),
    )
}

pub fn wake_pane(store: &Store, pane: &PaneId) -> Result<Woken, HibernateErr> {
    wake_pane_with_timeout(
        store.paths(),
        store.runtime_paths(),
        pane,
        Path::new("/sys/fs/cgroup"),
        crate::disk::lock::LOCK_TIMEOUT,
    )
}

pub fn wake_pane_at(paths: &crate::StatePaths, pane: &PaneId) -> Result<Woken, HibernateErr> {
    wake_pane_in(paths, pane, Path::new("/sys/fs/cgroup"))
}

pub(crate) fn wake_pane_in(
    paths: &crate::StatePaths,
    pane: &PaneId,
    root: &Path,
) -> Result<Woken, HibernateErr> {
    if !paths.lock_path("hibernate.lock").exists() && !paths.hibernation.exists() {
        return Ok(Woken::NotHibernating);
    }
    let runtime =
        crate::RuntimePaths::for_workspace(paths.workspace_id.clone()).map_err(StoreErr::from)?;
    wake_pane_with_timeout(paths, &runtime, pane, root, crate::disk::lock::LOCK_TIMEOUT)
}

pub(super) fn wake_pane_with_timeout(
    paths: &crate::StatePaths,
    runtime: &crate::RuntimePaths,
    pane: &PaneId,
    root: &Path,
    timeout: std::time::Duration,
) -> Result<Woken, HibernateErr> {
    wake_with(
        paths,
        runtime,
        pane,
        root,
        timeout,
        WakeTrigger::Sync,
        |request| spawn_refault(runtime, request),
    )
}

fn wake_with(
    paths: &crate::StatePaths,
    runtime: &crate::RuntimePaths,
    pane: &PaneId,
    root: &Path,
    timeout: std::time::Duration,
    mut trigger: WakeTrigger<'_>,
    mut spawn: impl FnMut(RefaultHelperRequest),
) -> Result<Woken, HibernateErr> {
    let started = std::time::Instant::now();
    if !paths.lock_path("hibernate.lock").exists() && !paths.hibernation.exists() {
        return Ok(Woken::NotHibernating);
    }
    let lock_path = paths.lock_path("hibernate.lock");
    let _guard = match crate::disk::lock::WorkspaceLock::acquire_with_timeout(&lock_path, timeout) {
        Ok(guard) => guard,
        Err(crate::disk::lock::LockErr::Timeout { .. }) => {
            if let WakeTrigger::Helper { ready, .. } = &mut trigger
                && ready().is_none()
            {
                return Ok(Woken::NotHibernating);
            }
            return wake_workspace_unlocked(paths, runtime, pane, root, &lock_path, started);
        }
        Err(error) => return Err(StoreErr::from(error).into()),
    };
    if matches!(trigger, WakeTrigger::Sync) {
        crate::disk::atomic::write_temp_then_rename_cache(
            &wake_stamp_path(runtime, Some(pane)),
            &WakeStamp::Sync(jiff::Timestamp::now()),
        )?;
    }
    let entries: Vec<_> = crate::store::hibernate::read(paths)
        .into_iter()
        .filter(|entry| &entry.pane_id == pane)
        .collect();
    if entries.is_empty() {
        return Ok(Woken::NotHibernating);
    }
    let dwell = match &mut trigger {
        WakeTrigger::Sync => None,
        WakeTrigger::Helper { ready, .. } => match ready() {
            Some(dwell) => Some(dwell),
            None => return Ok(Woken::NotHibernating),
        },
    };
    let mut result = Woken::NotHibernating;
    for mut entry in entries {
        let scope = recorded_scope(root, &entry.scope)?;
        let thaw = if scope
            .as_path()
            .try_exists()
            .map_err(crate::proc::cgroup::CgroupErr::from)?
        {
            crate::proc::cgroup::thaw(&scope)
        } else {
            Ok(())
        };
        let thawed_at = jiff::Timestamp::now();
        let thaw_ms = started.elapsed().as_millis() as u64;
        let (trigger, focus_to_thaw_ms) = match &trigger {
            WakeTrigger::Sync => ("sync", None),
            WakeTrigger::Helper {
                requested_at_ms, ..
            } => (
                "helper",
                requested_at_ms.map(|requested| {
                    (thawed_at.as_millisecond().max(0) as u64).saturating_sub(requested)
                }),
            ),
        };
        entry.wake_error = thaw.as_ref().err().map(ToString::to_string);
        record_wake(paths, &entry, trigger, thaw_ms, focus_to_thaw_ms, dwell);
        if let Err(error) = thaw {
            crate::store::hibernate::record(paths, entry)?;
            return Err(error.into());
        }
        if dwell.is_some() {
            crate::disk::atomic::write_temp_then_rename_cache(
                &wake_stamp_path(runtime, Some(pane)),
                &WakeStamp::Focus { focus: thawed_at },
            )?;
        }
        let baseline = crate::proc::cgroup::memory_stat(&scope).ok();
        if let Some(baseline) = baseline {
            spawn(RefaultHelperRequest {
                workspace_id: paths.workspace_id.clone(),
                entry: entry.clone(),
                thawed_at,
                baseline_anon: baseline.workingset_refault_anon,
                baseline_file: baseline.workingset_refault_file,
            });
        }
        crate::store::hibernate::withdraw(paths, &entry.kind, &entry.agent_id)?;
        result = Woken::Thawed {
            kind: entry.kind,
            agent_id: entry.agent_id,
        };
    }
    Ok(result)
}

fn wake_workspace_unlocked(
    paths: &crate::StatePaths,
    runtime: &crate::RuntimePaths,
    pane: &PaneId,
    root: &Path,
    lock_path: &Path,
    started: std::time::Instant,
) -> Result<Woken, HibernateErr> {
    crate::disk::atomic::write_temp_then_rename_cache(
        &wake_stamp_path(runtime, None),
        &jiff::Timestamp::now(),
    )?;
    crate::proc::cgroup::thaw_workspace_in(root, &paths.workspace_id)?;
    let thaw_ms = started.elapsed().as_millis() as u64;
    let mut result = Woken::NotHibernating;
    let mut withdrawn = 0;
    for mut entry in crate::store::hibernate::read(paths) {
        entry.wake_error = None;
        record_wake(paths, &entry, "lock_timeout", thaw_ms, None, None);
        withdrawn += usize::from(crate::store::hibernate::withdraw(
            paths,
            &entry.kind,
            &entry.agent_id,
        )?);
        if &entry.pane_id == pane {
            result = Woken::Thawed {
                kind: entry.kind,
                agent_id: entry.agent_id,
            };
        }
    }
    crate::diag::hibernate::wake_lock_timeout(paths, pane, lock_path, withdrawn);
    Ok(result)
}

pub(super) fn recorded_scope(root: &Path, path: &Path) -> Result<ScopePath, HibernateErr> {
    let invalid = || crate::proc::cgroup::CgroupErr::InvalidScope(path.to_owned());
    let relative = path.strip_prefix(root).map_err(|_| invalid())?;
    ScopePath::from_cgroup(root, &format!("0::/{}", relative.display()))
        .ok_or_else(|| invalid().into())
}

pub fn reconcile(
    store: &Store,
    workspace_id: &WorkspaceId,
) -> Result<ReconcileReport, HibernateErr> {
    reconcile_at(store.paths(), workspace_id)
}

pub fn reconcile_at(
    paths: &crate::StatePaths,
    workspace_id: &WorkspaceId,
) -> Result<ReconcileReport, HibernateErr> {
    if !cfg!(target_os = "linux") {
        return Ok(ReconcileReport::default());
    }
    reconcile_in(paths, workspace_id, Path::new("/sys/fs/cgroup"))
}

fn reconcile_in(
    paths: &crate::StatePaths,
    workspace_id: &WorkspaceId,
    root: &Path,
) -> Result<ReconcileReport, HibernateErr> {
    // A freeze requires a scope, so an unborn room with neither scope nor record has no transaction to join.
    if !paths.hibernation.exists()
        && crate::proc::cgroup::workspace_scopes_in(root, workspace_id)?.is_empty()
    {
        return Ok(ReconcileReport::default());
    }
    let _guard = crate::disk::lock::WorkspaceLock::acquire(&paths.lock_path("hibernate.lock"))
        .map_err(StoreErr::from)?;
    let entries = crate::store::hibernate::read(paths);
    let mut report = ReconcileReport::default();
    for entry in &entries {
        let scope = recorded_scope(root, &entry.scope)?;
        if !is_frozen(&scope)? {
            report.withdrawn += usize::from(crate::store::hibernate::withdraw(
                paths,
                &entry.kind,
                &entry.agent_id,
            )?);
        }
    }
    for scope in crate::proc::cgroup::workspace_scopes_in(root, workspace_id)? {
        if entries.iter().any(|entry| entry.scope == scope.as_path()) {
            continue;
        }
        if freeze_requested(&scope)? || is_frozen(&scope)? {
            crate::proc::cgroup::thaw(&scope)?;
            report.thawed += 1;
        }
    }
    Ok(report)
}

fn freeze_requested(scope: &ScopePath) -> Result<bool, HibernateErr> {
    match std::fs::read_to_string(scope.as_path().join("cgroup.freeze")) {
        Ok(value) => Ok(value.trim() == "1"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !scope.as_path().exists() => {
            Ok(false)
        }
        Err(error) => Err(crate::proc::cgroup::CgroupErr::from(error).into()),
    }
}

pub(super) fn is_frozen(scope: &ScopePath) -> Result<bool, HibernateErr> {
    match std::fs::read_to_string(scope.as_path().join("cgroup.events")) {
        Ok(events) => Ok(events.lines().any(|line| line == "frozen 1")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !scope.as_path().exists() => {
            Ok(false)
        }
        Err(error) => Err(crate::proc::cgroup::CgroupErr::from(error).into()),
    }
}

fn record_wake(
    paths: &crate::StatePaths,
    entry: &crate::store::hibernate::Entry,
    trigger: &str,
    thaw_ms: u64,
    focus_to_thaw_ms: Option<u64>,
    dwell: Option<Dwell>,
) {
    crate::diag::hibernate::record(
        paths,
        &entry.pane_id,
        &entry.kind,
        &entry.agent_id,
        crate::diag::hibernate::WakeEvent::Wake {
            frozen_at: entry.frozen_at,
            step: entry.step,
            anon_at_freeze: entry.anon_at_freeze,
            reclaimed_bytes: entry.reclaimed_bytes,
            trigger,
            thaw_ms,
            focus_to_thaw_ms,
            dwell: dwell.map(|dwell| match dwell {
                Dwell::Input => "input",
                Dwell::Elapsed => "elapsed",
            }),
            error: entry.wake_error.as_deref(),
        },
    );
}

fn spawn_refault(runtime: &crate::RuntimePaths, request: RefaultHelperRequest) {
    let args = crate::child_process::agent_helper_argv("wake-refault", &request);
    if let Err(error) =
        crate::child_process::spawn_detached_rimz(runtime, args, "agent-wake-refault")
    {
        tracing::debug!(%error, "failed to spawn wake refault sampler");
    }
}

pub fn sample_wake_refault(request: RefaultHelperRequest) {
    let Ok(paths) = crate::StatePaths::for_workspace(request.workspace_id.clone()) else {
        return;
    };
    let Ok(scope) = recorded_scope(Path::new("/sys/fs/cgroup"), &request.entry.scope) else {
        return;
    };
    let baseline = crate::proc::cgroup::MemoryStat {
        anon: 0,
        file: 0,
        workingset_refault_anon: request.baseline_anon,
        workingset_refault_file: request.baseline_file,
    };
    let initial_ms = crate::utils::time::unix_now_ms()
        .saturating_sub(request.thawed_at.as_millisecond().max(0) as u64);
    let started = std::time::Instant::now();
    let elapsed_ms = || initial_ms.saturating_add(started.elapsed().as_millis() as u64);
    let samples = sample_refaults(&baseline, |deadline_ms| {
        std::thread::sleep(std::time::Duration::from_millis(
            deadline_ms.saturating_sub(elapsed_ms()),
        ));
        crate::proc::cgroup::memory_stat(&scope)
            .ok()
            .map(|stat| (elapsed_ms(), stat))
    });
    crate::diag::hibernate::record(
        &paths,
        &request.entry.pane_id,
        &request.entry.kind,
        &request.entry.agent_id,
        crate::diag::hibernate::WakeEvent::WakeRefault {
            thawed_at: request.thawed_at,
            samples: &samples,
        },
    );
}

fn sample_refaults(
    baseline: &crate::proc::cgroup::MemoryStat,
    mut sample: impl FnMut(u64) -> Option<(u64, crate::proc::cgroup::MemoryStat)>,
) -> Vec<crate::diag::hibernate::RefaultSample> {
    let mut samples = Vec::new();
    for deadline_ms in [1_000, 10_000, 60_000] {
        let Some((after_ms, stat)) = sample(deadline_ms) else {
            break;
        };
        samples.push(crate::diag::hibernate::RefaultSample {
            after_ms,
            refault_anon: stat
                .workingset_refault_anon
                .saturating_sub(baseline.workingset_refault_anon),
            refault_file: stat
                .workingset_refault_file
                .saturating_sub(baseline.workingset_refault_file),
        });
    }
    samples
}

#[cfg(all(test, target_os = "linux"))]
mod tests;
