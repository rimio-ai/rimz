//! Synchronous thaw and crash recovery, outside the sidebar's read-only graph.

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

enum WakeTrigger {
    Sync,
    Helper(Option<u64>),
}

pub fn wake_request(store: &Store, request: &WakeHelperRequest) -> Result<Woken, HibernateErr> {
    wake_with(
        store.paths(),
        store.runtime_paths(),
        &request.pane_id,
        Path::new("/sys/fs/cgroup"),
        crate::disk::lock::LOCK_TIMEOUT,
        WakeTrigger::Helper(request.requested_at_ms),
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
    trigger: WakeTrigger,
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
            return wake_workspace_unlocked(paths, runtime, pane, root, &lock_path, started);
        }
        Err(error) => return Err(StoreErr::from(error).into()),
    };
    crate::disk::atomic::write_temp_then_rename_cache(
        &wake_stamp_path(runtime, Some(pane)),
        &match trigger {
            WakeTrigger::Sync => WakeStamp::Sync(jiff::Timestamp::now()),
            WakeTrigger::Helper(_) => WakeStamp::Focus {
                focus: jiff::Timestamp::now(),
            },
        },
    )?;
    let mut result = Woken::NotHibernating;
    for mut entry in crate::store::hibernate::read(paths)
        .into_iter()
        .filter(|entry| &entry.pane_id == pane)
    {
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
        let (trigger, focus_to_thaw_ms) = match trigger {
            WakeTrigger::Sync => ("sync", None),
            WakeTrigger::Helper(requested_at_ms) => (
                "helper",
                requested_at_ms.map(|requested| {
                    (thawed_at.as_millisecond().max(0) as u64).saturating_sub(requested)
                }),
            ),
        };
        entry.wake_error = thaw.as_ref().err().map(ToString::to_string);
        record_wake(paths, &entry, trigger, thaw_ms, focus_to_thaw_ms);
        if let Err(error) = thaw {
            crate::store::hibernate::record(paths, entry)?;
            return Err(error.into());
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
        record_wake(paths, &entry, "lock_timeout", thaw_ms, None);
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
mod tests {
    use super::*;
    use crate::store::hibernate::{self, Entry};

    #[test]
    fn helper_wake_captures_baseline_and_focus_latency_before_withdrawal() {
        let (dir, paths, _, entry) = fixture();
        std::fs::write(
            entry.scope.join("memory.stat"),
            "anon 50\nfile 20\nworkingset_refault_anon 100\nworkingset_refault_file 200\n",
        )
        .unwrap();
        let requested = crate::utils::time::unix_now_ms().saturating_sub(100);
        let mut spawned = Vec::new();
        wake_with(
            &paths,
            &crate::RuntimePaths::for_workspace(paths.workspace_id.clone()).unwrap(),
            &entry.pane_id,
            dir.path(),
            std::time::Duration::ZERO,
            WakeTrigger::Helper(Some(requested)),
            |request| {
                assert_eq!(
                    std::fs::read_to_string(entry.scope.join("cgroup.freeze")).unwrap(),
                    "0"
                );
                assert_eq!(hibernate::read(&paths), std::slice::from_ref(&entry));
                spawned.push(request);
            },
        )
        .unwrap();
        assert_eq!(
            spawned.len(),
            1,
            "a thaw with readable counters spawns one sampler"
        );
        assert_eq!(spawned[0].baseline_anon, 100);
        assert_eq!(spawned[0].baseline_file, 200);
        assert_eq!(spawned[0].entry, entry);
        let log = std::fs::read_to_string(paths.audit_path("hibernate.log.jsonl")).unwrap();
        let record: serde_json::Value = serde_json::from_str(&log).unwrap();
        assert_eq!(record["trigger"], "helper");
        assert!(record["focus_to_thaw_ms"].as_u64().unwrap() >= 100);
        assert!(hibernate::read(&paths).is_empty());
    }

    #[test]
    fn refault_samples_are_cumulative_from_thaw_and_stop_when_scope_disappears() {
        let stat = |anon, file| crate::proc::cgroup::MemoryStat {
            anon: 0,
            file: 0,
            workingset_refault_anon: anon,
            workingset_refault_file: file,
        };
        let mut deadlines = Vec::new();
        let samples = sample_refaults(&stat(100, 200), |after_ms| {
            deadlines.push(after_ms);
            match after_ms {
                1_000 => Some((1_020, stat(110, 205))),
                10_000 => Some((10_040, stat(135, 211))),
                60_000 => Some((60_080, stat(90, 199))),
                _ => panic!("unexpected sample deadline"),
            }
        });
        assert_eq!(deadlines, [1_000, 10_000, 60_000]);
        let values: Vec<_> = samples
            .iter()
            .map(|s| (s.after_ms, s.refault_anon, s.refault_file))
            .collect();
        assert_eq!(values, [(1_020, 10, 5), (10_040, 35, 11), (60_080, 0, 0)]);
        let mut calls = 0;
        let samples = sample_refaults(&stat(100, 200), |after_ms| {
            calls += 1;
            (after_ms == 1_000).then(|| (after_ms, stat(101, 202)))
        });
        assert_eq!(calls, 2);
        assert_eq!(samples.len(), 1);
    }

    fn fixture() -> (tempfile::TempDir, crate::StatePaths, PathBuf, Entry) {
        let dir = tempfile::tempdir().unwrap();
        let id = WorkspaceId::from_project_root(dir.path());
        let paths = crate::StatePaths::under(id.clone(), &dir.path().join("state")).unwrap();
        let uid = nix::unistd::Uid::current().as_raw();
        let slice = dir.path().join(format!(
            "user.slice/user-{uid}.slice/user@{uid}.service/app.slice"
        ));
        let scope = slice.join(format!("{}a.scope", scope_prefix(&id)));
        std::fs::create_dir_all(&scope).unwrap();
        std::fs::write(scope.join("cgroup.freeze"), "1").unwrap();
        std::fs::write(scope.join("cgroup.events"), "populated 1\nfrozen 1\n").unwrap();
        let entry = Entry {
            kind: AgentKind::new_unchecked("claude"),
            agent_id: "session".into(),
            pane_id: "tmux:%3".parse().unwrap(),
            scope,
            provider_pid: 10,
            frozen_at: jiff::Timestamp::UNIX_EPOCH,
            anon_at_freeze: 600,
            step: 2,
            wake_error: None,
            reclaimed_bytes: 100,
        };
        hibernate::record(&paths, entry.clone()).unwrap();
        (dir, paths, slice, entry)
    }

    #[test]
    fn no_op_wake_does_not_create_an_unborn_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let paths =
            crate::StatePaths::for_project_root_under(&dir.path().join("project"), dir.path())
                .unwrap();
        wake_pane_in(&paths, &"tmux:%3".parse().unwrap(), dir.path()).unwrap();
        assert!(
            !paths.root.exists(),
            "a no-op wake must not materialize an unregistered room directory"
        );
    }

    #[test]
    fn no_op_reconcile_does_not_create_an_unborn_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let paths =
            crate::StatePaths::for_project_root_under(&dir.path().join("project"), dir.path())
                .unwrap();
        reconcile_in(&paths, &paths.workspace_id, dir.path()).unwrap();
        assert!(
            !paths.root.exists(),
            "a no-op reconcile must not materialize an unregistered room directory"
        );
    }

    #[test]
    fn lock_timeout_thaws_withdraws_and_records_diagnostic() {
        let (dir, paths, slice, entry) = fixture();
        let orphan = slice.join(format!(
            "{}unpublished.scope",
            scope_prefix(&paths.workspace_id)
        ));
        std::fs::create_dir_all(&orphan).unwrap();
        std::fs::write(orphan.join("cgroup.freeze"), "1").unwrap();
        let mut peer = entry.clone();
        peer.agent_id = "peer".into();
        peer.pane_id = "tmux:%4".parse().unwrap();
        peer.scope = slice.join(format!("{}peer.scope", scope_prefix(&paths.workspace_id)));
        std::fs::create_dir_all(&peer.scope).unwrap();
        std::fs::write(peer.scope.join("cgroup.freeze"), "1").unwrap();
        hibernate::record(&paths, peer.clone()).unwrap();
        let lock_path = paths.lock_path("hibernate.lock");
        let guard = crate::disk::lock::WorkspaceLock::acquire(&lock_path).unwrap();
        let result = wake_with(
            &paths,
            &crate::RuntimePaths::for_workspace(paths.workspace_id.clone()).unwrap(),
            &entry.pane_id,
            dir.path(),
            std::time::Duration::ZERO,
            WakeTrigger::Helper(Some(0)),
            |_| panic!("timeout recovery must not spawn a sampler"),
        );
        assert!(
            result.is_ok(),
            "lock timeout must not prevent thaw: {result:?}"
        );
        assert_eq!(
            std::fs::read_to_string(entry.scope.join("cgroup.freeze")).unwrap(),
            "0"
        );
        for scope in [&orphan, &peer.scope] {
            assert_eq!(
                std::fs::read_to_string(scope.join("cgroup.freeze")).unwrap(),
                "0"
            );
        }
        assert!(hibernate::read(&paths).is_empty());
        let log = std::fs::read_to_string(paths.audit_path("hibernate.log.jsonl")).unwrap();
        let records: Vec<serde_json::Value> = log
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let wakes: Vec<_> = records
            .iter()
            .filter(|record| record["event"] == "wake")
            .collect();
        assert_eq!(wakes.len(), 2, "each withdrawn entry needs wake evidence");
        for (record, entry) in wakes.iter().zip([&entry, &peer]) {
            assert_eq!(record["trigger"], "lock_timeout");
            assert_eq!(record["agent_id"], entry.agent_id.as_str());
            assert_eq!(record["step"], entry.step);
            assert_eq!(record["anon_at_freeze"], entry.anon_at_freeze);
            assert_eq!(record["reclaimed_bytes"], entry.reclaimed_bytes);
            assert!(record["thaw_ms"].is_number());
            assert!(record["focus_to_thaw_ms"].is_null());
        }
        let diagnostic = records.last().unwrap();
        assert_eq!(diagnostic["event"], "wake_lock_timeout");
        assert_eq!(diagnostic["workspace_id"], paths.workspace_id.as_str());
        assert_eq!(diagnostic["pane_id"], entry.pane_id.to_string());
        assert_eq!(diagnostic["lock_path"], lock_path.to_str().unwrap());
        assert_eq!(diagnostic["withdrawn"], 2);
        drop(guard);
        // A delayed publisher can restore a stale entry after the unlocked wake.
        hibernate::record(&paths, entry.clone()).unwrap();
        for scope in [&entry.scope, &orphan, &peer.scope] {
            std::fs::write(scope.join("cgroup.events"), "frozen 0\n").unwrap();
        }
        reconcile_in(&paths, &paths.workspace_id, dir.path()).unwrap();
        assert!(hibernate::read(&paths).is_empty());
    }

    #[test]
    fn room_without_hibernation_state_is_untouched_by_wake() {
        let dir = tempfile::tempdir().unwrap();
        let paths =
            crate::StatePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
                .unwrap();
        std::fs::create_dir_all(&paths.root).unwrap();
        wake_pane_in(&paths, &"tmux:%3".parse().unwrap(), dir.path()).unwrap();
        assert_eq!(
            std::fs::read_dir(&paths.root).unwrap().count(),
            0,
            "ordinary pane input must not create hibernation state"
        );
    }

    #[test]
    fn reconcile_thaws_an_unpublished_freeze_before_events_confirm_it() {
        let (dir, paths, _, entry) = fixture();
        hibernate::withdraw(&paths, &entry.kind, &entry.agent_id).unwrap();
        std::fs::write(entry.scope.join("cgroup.events"), "frozen 0\n").unwrap();
        let report = reconcile_in(&paths, &paths.workspace_id, dir.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(entry.scope.join("cgroup.freeze")).unwrap(),
            "0"
        );
        assert_eq!(report.thawed, 1);
    }

    #[test]
    fn wake_thaws_before_withdrawing_and_is_idempotent() {
        let (dir, paths, _, entry) = fixture();
        assert_eq!(
            wake_pane_in(&paths, &entry.pane_id, dir.path()).unwrap(),
            Woken::Thawed {
                kind: entry.kind.clone(),
                agent_id: entry.agent_id.clone()
            }
        );
        assert_eq!(
            std::fs::read_to_string(entry.scope.join("cgroup.freeze")).unwrap(),
            "0"
        );
        assert!(hibernate::read(&paths).is_empty());
        assert_eq!(
            wake_pane_in(&paths, &entry.pane_id, dir.path()).unwrap(),
            Woken::NotHibernating
        );
        let log =
            std::fs::read_to_string(paths.audit_path("hibernate.log.jsonl")).unwrap_or_default();
        assert_eq!(
            log.lines().count(),
            1,
            "one diagnostic per wake, none for a no-op"
        );
        let record: serde_json::Value = serde_json::from_str(&log).unwrap();
        assert_eq!(record["event"], "wake");
        assert_eq!(record["trigger"], "sync");
        assert_eq!(record["workspace_id"], paths.workspace_id.as_str());
        assert_eq!(record["pane_id"], entry.pane_id.to_string());
        assert_eq!(record["kind"], entry.kind.as_str());
        assert_eq!(record["agent_id"], entry.agent_id.as_str());
        assert_eq!(record["frozen_at"], entry.frozen_at.to_string());
        assert_eq!(record["step"], entry.step);
        assert_eq!(record["anon_at_freeze"], entry.anon_at_freeze);
        assert_eq!(record["reclaimed_bytes"], entry.reclaimed_bytes);
        assert!(record["thaw_ms"].is_number());
        assert!(record["focus_to_thaw_ms"].is_null());
    }

    #[test]
    fn wake_drops_gone_scopes_but_retains_failed_and_invalid_scopes() {
        let (dir, paths, _, mut entry) = fixture();
        std::fs::remove_dir_all(&entry.scope).unwrap();
        wake_pane_in(&paths, &entry.pane_id, dir.path()).unwrap();
        assert!(
            hibernate::read(&paths).is_empty(),
            "gone scope is withdrawn"
        );
        std::fs::create_dir_all(entry.scope.join("cgroup.freeze")).unwrap();
        hibernate::record(&paths, entry.clone()).unwrap();
        assert!(wake_pane_in(&paths, &entry.pane_id, dir.path()).is_err());
        let retained = hibernate::read(&paths).remove(0);
        let json = serde_json::to_value(&retained).unwrap();
        assert!(
            json["wake_error"]
                .as_str()
                .is_some_and(|error| !error.is_empty())
        );
        let log = std::fs::read_to_string(paths.audit_path("hibernate.log.jsonl")).unwrap();
        let event: serde_json::Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
        assert_eq!(event["error"], json["wake_error"]);
        std::fs::remove_dir(entry.scope.join("cgroup.freeze")).unwrap();
        std::fs::write(entry.scope.join("cgroup.freeze"), "1").unwrap();
        wake_pane_in(&paths, &entry.pane_id, dir.path()).unwrap();
        assert!(
            hibernate::read(&paths).is_empty(),
            "successful retry clears the failure"
        );
        entry.scope = dir.path().join("rimz-agent-foreign.scope");
        hibernate::record(&paths, entry.clone()).unwrap();
        assert!(wake_pane_in(&paths, &entry.pane_id, dir.path()).is_err());
        assert_eq!(hibernate::read(&paths), [entry]);
    }

    #[test]
    fn reconcile_repairs_orphans_and_stale_records_without_touching_other_rooms() {
        let (dir, paths, slice, entry) = fixture();
        let orphan = slice.join(format!("{}orphan.scope", scope_prefix(&paths.workspace_id)));
        let foreign = slice.join("rimz-agent-another-room.scope");
        for scope in [&orphan, &foreign] {
            std::fs::create_dir_all(scope).unwrap();
            std::fs::write(scope.join("cgroup.events"), "frozen 1\n").unwrap();
            std::fs::write(scope.join("cgroup.freeze"), "1").unwrap();
        }
        let mut missing = entry.clone();
        missing.agent_id = "missing".into();
        missing.scope = slice.join(format!(
            "{}missing.scope",
            scope_prefix(&paths.workspace_id)
        ));
        hibernate::record(&paths, missing).unwrap();
        let report = reconcile_in(&paths, &paths.workspace_id, dir.path()).unwrap();
        assert_eq!(
            report,
            ReconcileReport {
                thawed: 1,
                withdrawn: 1
            }
        );
        assert_eq!(
            std::fs::read_to_string(orphan.join("cgroup.freeze")).unwrap(),
            "0"
        );
        assert_eq!(
            std::fs::read_to_string(foreign.join("cgroup.freeze")).unwrap(),
            "1"
        );
        assert_eq!(hibernate::read(&paths), std::slice::from_ref(&entry));
        std::fs::write(entry.scope.join("cgroup.events"), "frozen 0\n").unwrap();
        reconcile_in(&paths, &paths.workspace_id, dir.path()).unwrap();
        assert!(hibernate::read(&paths).is_empty());
    }
}
