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
    if !paths.lock_path("hibernate.lock").exists() && !paths.hibernation.exists() {
        return Ok(Woken::NotHibernating);
    }
    let lock_path = paths.lock_path("hibernate.lock");
    let _guard = match crate::disk::lock::WorkspaceLock::acquire_with_timeout(&lock_path, timeout) {
        Ok(guard) => guard,
        Err(crate::disk::lock::LockErr::Timeout { .. }) => {
            return wake_workspace_unlocked(paths, runtime, pane, root, &lock_path);
        }
        Err(error) => return Err(StoreErr::from(error).into()),
    };
    crate::disk::atomic::write_temp_then_rename_cache(
        &wake_stamp_path(runtime, Some(pane)),
        &jiff::Timestamp::now(),
    )?;
    let mut result = Woken::NotHibernating;
    for entry in crate::store::hibernate::read(paths)
        .into_iter()
        .filter(|entry| &entry.pane_id == pane)
    {
        let scope = recorded_scope(root, &entry.scope)?;
        if scope
            .as_path()
            .try_exists()
            .map_err(crate::proc::cgroup::CgroupErr::from)?
        {
            crate::proc::cgroup::thaw(&scope)?;
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
) -> Result<Woken, HibernateErr> {
    crate::disk::atomic::write_temp_then_rename_cache(
        &wake_stamp_path(runtime, None),
        &jiff::Timestamp::now(),
    )?;
    crate::proc::cgroup::thaw_workspace_in(root, &paths.workspace_id)?;
    let mut result = Woken::NotHibernating;
    let mut withdrawn = 0;
    for entry in crate::store::hibernate::read(paths) {
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

fn recorded_scope(root: &Path, path: &Path) -> Result<ScopePath, HibernateErr> {
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

fn is_frozen(scope: &ScopePath) -> Result<bool, HibernateErr> {
    match std::fs::read_to_string(scope.as_path().join("cgroup.events")) {
        Ok(events) => Ok(events.lines().any(|line| line == "frozen 1")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !scope.as_path().exists() => {
            Ok(false)
        }
        Err(error) => Err(crate::proc::cgroup::CgroupErr::from(error).into()),
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::store::hibernate::{self, Entry};

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
        let result = wake_pane_with_timeout(
            &paths,
            &crate::RuntimePaths::for_workspace(paths.workspace_id.clone()).unwrap(),
            &entry.pane_id,
            dir.path(),
            std::time::Duration::ZERO,
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
        let diagnostic: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(paths.audit_path("hibernate.log.jsonl")).unwrap(),
        )
        .unwrap();
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
                kind: entry.kind,
                agent_id: entry.agent_id
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
        assert_eq!(hibernate::read(&paths), std::slice::from_ref(&entry));
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
