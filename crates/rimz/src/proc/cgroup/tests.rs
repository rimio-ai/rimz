use super::*;
use nix::libc;

#[test]
fn login_session_membership_does_not_change_user_manager_slice() {
    let root = Path::new("/fake-cgroup");
    assert!(
        scope_in(
            root,
            "0::/user.slice/user-1000.slice/session-c5.scope",
            1000
        )
        .is_none()
    );
    assert_eq!(
        user_app_slice_in(root, 1000),
        root.join("user.slice/user-1000.slice/user@1000.service/app.slice")
    );
}

#[test]
fn workspace_sweep_thaws_only_this_rooms_scopes() {
    let root = tempfile::tempdir().unwrap();
    let workspace = crate::ids::WorkspaceId::from_project_root(root.path());
    let uid = nix::unistd::Uid::current().as_raw();
    let slice = root.path().join(format!(
        "user.slice/user-{uid}.slice/user@{uid}.service/app.slice"
    ));
    let own = slice.join(format!("rimz-agent-{}-a.scope", workspace.as_str()));
    let other = slice.join("rimz-agent-another-room.scope");
    let helper = slice.join("rimz-helper-a.scope");
    for scope in [&own, &other, &helper] {
        std::fs::create_dir_all(scope).unwrap();
        std::fs::write(scope.join("cgroup.freeze"), "1").unwrap();
    }
    thaw_workspace_in(root.path(), &workspace).unwrap();
    assert_eq!(
        std::fs::read_to_string(own.join("cgroup.freeze")).unwrap(),
        "0"
    );
    for scope in [&other, &helper] {
        assert_eq!(
            std::fs::read_to_string(scope.join("cgroup.freeze")).unwrap(),
            "1"
        );
    }
}

#[test]
fn scope_resolution_requires_own_app_slice_and_agent_unit() {
    let root = Path::new("/fake-cgroup");
    let group = "/user.slice/user-1000.slice/user@1000.service/app.slice/rimz-agent-w-n.scope";
    assert_eq!(
        scope_in(root, &format!("0::{group}\n"), 1000).map(|scope| scope.0),
        Some(root.join(&group[1..]))
    );
    for text in [
        format!("0::{group}/../escape"),
        format!("1:memory:{group}"),
        format!("0::{}", group.replace("rimz-agent", "rimz-helper")),
        format!("0::{}", group.replace("1000", "1001")),
    ] {
        assert!(scope_in(root, &text, 1000).is_none(), "{text}");
    }
}

#[test]
fn freeze_waits_for_confirmation_and_timeout_thaws() {
    let root = tempfile::tempdir().unwrap();
    let scope = ScopePath(root.path().to_owned());
    std::fs::write(root.path().join("cgroup.freeze"), "0").unwrap();
    std::fs::write(root.path().join("cgroup.events"), "populated 1\nfrozen 0\n").unwrap();
    assert!(matches!(
        freeze_with(&scope, Duration::ZERO),
        Err(CgroupErr::FreezeTimeout)
    ));
    assert_eq!(
        std::fs::read_to_string(root.path().join("cgroup.freeze")).unwrap(),
        "0"
    );
    std::thread::scope(|threads| {
        threads.spawn(|| {
            std::thread::sleep(Duration::from_millis(20));
            std::fs::write(root.path().join("cgroup.events"), "populated 1\nfrozen 1\n").unwrap();
        });
        freeze(&scope).unwrap();
        assert!(
            std::fs::read_to_string(root.path().join("cgroup.events"))
                .unwrap()
                .contains("frozen 1")
        );
    });
    assert_eq!(
        std::fs::read_to_string(root.path().join("cgroup.freeze")).unwrap(),
        "1"
    );
    thaw(&scope).unwrap();
    assert_eq!(
        std::fs::read_to_string(root.path().join("cgroup.freeze")).unwrap(),
        "0"
    );
}

#[test]
fn reclaim_retries_old_kernels_and_accepts_partial_reclaim() {
    let mut calls = Vec::new();
    let result = reclaim_with(4096, |value| {
        calls.push(value.to_owned());
        Err(io::Error::from_raw_os_error(if calls.len() == 1 {
            libc::EINVAL
        } else {
            libc::EAGAIN
        }))
    });
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(calls, ["4096 swappiness=200", "4096"]);
    assert!(reclaim_with(1, |_| Err(io::Error::from_raw_os_error(libc::EACCES))).is_err());
}

#[test]
fn reclaim_measures_memory_and_reads_scope_processes() {
    let root = tempfile::tempdir().unwrap();
    let scope = ScopePath(root.path().to_owned());
    std::fs::write(root.path().join("memory.current"), "4096\n").unwrap();
    std::fs::write(root.path().join("memory.reclaim"), "").unwrap();
    std::fs::write(root.path().join("cgroup.procs"), "123\n456\n").unwrap();
    let result = reclaim(&scope, 1024);
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(
        result.unwrap(),
        ReclaimReport {
            before: 4096,
            after: 4096
        }
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("memory.reclaim")).unwrap(),
        "1024 swappiness=200"
    );
    assert_eq!(procs(&scope), [123, 456]);
    reclaim(&scope, 0).unwrap();
    assert_eq!(
        std::fs::read_to_string(root.path().join("memory.reclaim")).unwrap(),
        "1024 swappiness=200"
    );
}

#[test]
fn memory_stat_reads_named_counters_and_rejects_missing_or_invalid_values() {
    let root = tempfile::tempdir().unwrap();
    let scope = ScopePath(root.path().to_owned());
    std::fs::write(
        root.path().join("memory.stat"),
        "file 20\nanon 60\nworkingset_refault_file 3\nworkingset_refault_anon 2\nunknown 9\n",
    )
    .unwrap();
    assert_eq!(
        memory_stat(&scope).unwrap(),
        MemoryStat {
            anon: 60,
            file: 20,
            workingset_refault_anon: 2,
            workingset_refault_file: 3
        }
    );
    for text in [
        "file 20\n",
        "anon nope\nfile 20\nworkingset_refault_anon 0\nworkingset_refault_file 0\n",
    ] {
        std::fs::write(root.path().join("memory.stat"), text).unwrap();
        assert!(memory_stat(&scope).is_err());
    }
}
