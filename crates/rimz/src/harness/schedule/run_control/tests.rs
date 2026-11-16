use std::fs::File;

use super::*;
use crate::disk::lease::assert_waits_for_gate;
use crate::disk::paths::logs_dir;
use crate::harness::schedule::task_record_store;
use crate::harness::schedule::test_homes::dead_mark;
use crate::ids::{FireId, WorkspaceId};

fn record_room(root: &Path) -> Result<()> {
    let workspace = crate::workspace::WorkspaceResolver::resolve(root, None)?;
    let paths = StatePaths::for_project_root(root)?;
    let runtime = RuntimePaths::for_state(&paths)?;
    crate::Store::open(paths, runtime)?.record_workspace(&workspace)?;
    Ok(())
}

#[test]
fn cold_runtime_holder_is_visible_without_a_room_record() {
    let root = tempfile::tempdir().unwrap();
    let paths = StatePaths::for_project_root(root.path()).unwrap();
    let runtime = RuntimePaths::for_state(&paths).unwrap();
    let path = runtime.lock_path(run_lock_file_name("pending", None));
    let RunLockAttempt::Acquired(_guard) =
        acquire_run_lock_file(&path, Some(TaskKind::Routine)).unwrap()
    else {
        panic!("fixture takes lock")
    };
    assert!(
        matches!(
            RunLocks::list(root.path())
                .unwrap()
                .state("pending")
                .unwrap(),
            RunLockState::Held(_)
        ),
        "stop must find a cold runtime-only holder"
    );
    assert!(in_flight_run("pending", root.path()).unwrap().is_some());
    assert!(
        !paths.workspace_record.exists(),
        "lookup must not birth a room"
    );
}

#[test]
fn task_lookup_reports_the_earliest_holder_among_its_held_locks() {
    let root = tempfile::tempdir().unwrap();
    record_room(root.path()).unwrap();
    let locks = RuntimePaths::for_project_root(root.path())
        .unwrap()
        .locks_dir;
    std::fs::create_dir_all(&locks).unwrap();
    let write = |file: &str, pid: u32, started_at: i64| {
        let path = locks.join(file);
        let info = RunLockInfo {
            pid,
            started_at: Timestamp::from_second(started_at).unwrap(),
        };
        std::fs::write(&path, serde_json::to_vec(&info).unwrap()).unwrap();
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap()
    };
    let later = write("loop-run-fixer-ws_0123456789abcdef01234567.lock", 11, 200);
    let earlier = write("loop-run-fixer-ws_89abcdef0123456789abcdef.lock", 22, 100);
    let _free = write("loop-run-fixer.lock", 33, 50);
    let other_task = write("loop-run-fixer-nightly.lock", 44, 10);
    other_task.try_lock().unwrap();
    let holder = || match RunLocks::list(root.path()).unwrap().state("fixer").unwrap() {
        RunLockState::Held(holder) => holder.map(|info| info.pid),
        RunLockState::Available => None,
    };

    assert_eq!(holder(), None, "a free lock file is no run");
    later.try_lock().unwrap();
    assert_eq!(holder(), Some(11));
    earlier.try_lock().unwrap();
    assert_eq!(holder(), Some(22));
    let holderless = locks.join("loop-run-fixer-ws_ffffffffffffffffffffffff.lock");
    std::fs::write(&holderless, "").unwrap();
    let holderless = std::fs::File::open(holderless).unwrap();
    holderless.try_lock().unwrap();
    assert_eq!(holder(), Some(22), "a holderless lock sorts last");
}

#[test]
fn held_locks_no_row_claims_are_listed_under_their_whole_stem() {
    let root = tempfile::tempdir().unwrap();
    record_room(root.path()).unwrap();
    let locks = RuntimePaths::for_project_root(root.path())
        .unwrap()
        .locks_dir;
    std::fs::create_dir_all(&locks).unwrap();
    let hold = |file: &str, pid: u32| {
        let path = locks.join(file);
        let info = RunLockInfo {
            pid,
            started_at: Timestamp::from_second(100).unwrap(),
        };
        std::fs::write(&path, serde_json::to_vec(&info).unwrap()).unwrap();
        let file = File::open(path).unwrap();
        file.try_lock().unwrap();
        file
    };
    let fan_out = "fan-ws_0123456789abcdef01234567";
    assert_eq!(
        run_lock_stem(&run_lock_file_name(fan_out, None)),
        Some(fan_out)
    );
    assert_eq!(run_lock_stem("loop-watch-x.lock"), None);
    let _later = hold(&run_lock_file_name("later", None), 11);
    let _fan = hold(&run_lock_file_name(fan_out, None), 22);
    let _watcher = hold("loop-watch-x.lock", 33);
    std::fs::write(locks.join(run_lock_file_name("free", None)), "").unwrap();
    std::fs::create_dir(locks.join(run_lock_file_name("ghost", None))).unwrap();
    let rowless = |rows: &[&str]| {
        RunLocks::list(root.path())
            .unwrap()
            .rowless(rows)
            .into_iter()
            .map(|(name, holder)| (name, holder.map(|info| info.pid)))
            .collect::<Vec<_>>()
    };

    assert_eq!(
        rowless(&[]),
        [
            (fan_out.to_owned(), Some(22)),
            ("later".to_owned(), Some(11))
        ]
    );
    assert_eq!(rowless(&["fan"]), [("later".to_owned(), Some(11))]);
    assert_eq!(
        rowless(&["later", "ghost"]),
        [(fan_out.to_owned(), Some(22))]
    );

    let snapshot = RunLocks::list(root.path()).unwrap();
    assert!(snapshot.state("ghost").is_err());
    assert!(matches!(
        snapshot.state("later").unwrap(),
        RunLockState::Held(Some(info)) if info.pid == 11
    ));
    assert!(
        in_flight_run("later", root.path()).unwrap().is_some(),
        "another name's unopenable lock does not fail the lookup"
    );
}

/// Holds task `pile`'s run lock under a recorded room, with a live `-p` run an earlier fire
/// of the task started a minute before the holder took the lock.
fn holder_beside_an_earlier_fire_run(root: &Path) -> (RunLockGuard, StatePaths) {
    record_room(root).unwrap();
    let paths = StatePaths::for_project_root(root).unwrap();
    let path = RuntimePaths::for_state(&paths)
        .unwrap()
        .lock_path(run_lock_file_name("pile", None));
    let RunLockAttempt::Acquired(guard) =
        acquire_run_lock_file(&path, Some(TaskKind::Routine)).unwrap()
    else {
        panic!("fixture takes lock")
    };
    let earlier = supervised_run(
        root,
        guard.info.started_at - jiff::SignedDuration::from_mins(1),
    );
    crate::harness::run::create(&paths, &earlier).unwrap();
    (guard, paths)
}

fn supervised_run(root: &Path, started_at: Timestamp) -> RunRecord {
    let mut run = RunRecord::new(
        crate::ids::WorkspaceId::from_project_root(root),
        crate::ids::AgentKind::new_unchecked("codex"),
        crate::agents::PermissionMode::Auto,
        "task".into(),
        root.to_path_buf(),
    );
    run.loop_task = Some("pile".to_owned());
    run.started_at = started_at;
    run
}

#[test]
fn in_flight_run_names_only_a_run_the_holder_started() {
    let root = tempfile::tempdir().unwrap();
    let (guard, paths) = holder_beside_an_earlier_fire_run(root.path());
    let in_flight = in_flight_run("pile", root.path()).unwrap().unwrap();
    assert_eq!(in_flight.holder, Some(guard.info));
    assert_eq!(
        in_flight.run.map(|run| run.run_id),
        None,
        "an earlier fire's run is not the holder's"
    );

    let own = supervised_run(root.path(), guard.info.started_at);
    crate::harness::run::create(&paths, &own).unwrap();
    let in_flight = in_flight_run("pile", root.path()).unwrap().unwrap();
    assert_eq!(in_flight.run.map(|run| run.run_id), Some(own.run_id));
}

#[test]
fn stop_task_cancels_and_records_no_earlier_fire_run_as_the_holders() {
    let root = tempfile::tempdir().unwrap();
    let (guard, _) = holder_beside_an_earlier_fire_run(root.path());
    let mut handed = Some(None);
    let (outcome, _) = stop_task("pile", root.path(), |_, _, run| {
        handed = Some(run.map(|run| run.run_id.clone()));
        // The holder leaves at the stop, as a fire that takes the mark does.
        drop(guard);
        Ok(())
    })
    .unwrap();
    assert_eq!(handed, Some(None), "the cancel is handed no run");
    assert!(
        matches!(
            outcome,
            StopOutcome::Stopped {
                run_id: None,
                signaled: false,
                ..
            }
        ),
        "the stopped row names no run"
    );
}

#[test]
fn run_lock_probes_never_defeat_a_starting_run() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("task.lock");
    std::fs::write(&path, b"").unwrap();
    let state = assert_waits_for_gate(&path, || probe_run_lock_path(&path)).unwrap();
    assert!(matches!(state, RunLockState::Available));
    let claim = assert_waits_for_gate(&path, || acquire_run_lock_file(&path, None)).unwrap();
    assert!(matches!(claim, RunLockAttempt::Acquired(_)));
}

#[test]
fn run_lock_reports_holder_metadata_and_accepts_empty_legacy_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing_path = dir.path().join("missing.lock");
    assert!(matches!(
        probe_run_lock_path(&missing_path).expect("probe missing lock"),
        RunLockState::Available
    ));
    assert!(
        !missing_path.exists(),
        "probing should not create a lock file"
    );

    let path = dir.path().join("task.lock");
    let guard = match acquire_run_lock_file(&path, None).expect("acquire lock") {
        RunLockAttempt::Acquired(guard) => guard,
        RunLockAttempt::Held(_) => panic!("fresh lock should be acquired"),
    };
    let written: RunLockInfo =
        serde_json::from_slice(&std::fs::read(&path).expect("read lock")).expect("parse lock info");
    assert_eq!(written.pid, std::process::id());

    match acquire_run_lock_file(&path, None).expect("contend for lock") {
        RunLockAttempt::Held(Some(info)) => assert_eq!(info, written),
        RunLockAttempt::Held(None) => panic!("holder metadata should be readable"),
        RunLockAttempt::Acquired(_) => panic!("held lock should reject contender"),
    }

    drop(guard);
    let before_probe = std::fs::read(&path).expect("read lock before probe");
    assert!(matches!(
        probe_run_lock_path(&path).expect("probe available lock"),
        RunLockState::Available
    ));
    assert_eq!(
        std::fs::read(&path).expect("read lock after probe"),
        before_probe,
        "probing an available lock should not rewrite its metadata"
    );

    let empty_path = dir.path().join("legacy.lock");
    let holder = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&empty_path)
        .expect("open lock");
    holder.try_lock().expect("hold empty lock");
    assert!(
        matches!(
            probe_run_lock_path(&empty_path).expect("probe empty lock"),
            RunLockState::Held(None)
        ),
        "a held lock with no metadata is still held"
    );
}

#[test]
fn wait_for_run_lock_release_observes_guard_drop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("task.lock");
    let guard = match acquire_run_lock_file(&path, None).expect("acquire lock") {
        RunLockAttempt::Acquired(guard) => guard,
        RunLockAttempt::Held(_) => panic!("fresh lock should be acquired"),
    };
    let holder = Some(guard.info);
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        drop(guard);
    });

    assert!(
        wait_for_run_lock_release_path(&path, holder, Duration::from_secs(1))
            .expect("wait for release")
    );
    releaser.join().expect("release thread");
}

#[test]
fn wait_for_process_exit_reads_the_holder_dead_within_the_grace() {
    let mut exited = std::process::Command::new("true")
        .spawn()
        .expect("spawn true");
    exited.wait().expect("reap true");
    assert!(wait_for_process_exit(exited.id(), Duration::ZERO));

    let mut live = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep");
    let waited = wait_for_process_exit(live.id(), Duration::from_millis(50));
    live.kill().expect("kill sleep");
    live.wait().expect("reap sleep");
    assert!(!waited, "a live process outlasts the grace");
}

#[test]
fn vanished_task_root_keeps_its_persisted_workspace_identity() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("vanished");

    let (workspace, project_root) = stop_workspace(&root).expect("resolve stop workspace");

    assert!(workspace.is_none());
    assert_eq!(
        WorkspaceId::from_project_root(&project_root),
        WorkspaceId::from_project_root(&root)
    );
}

#[test]
fn a_stop_row_carries_its_task_id_and_settles_that_tasks_mark() {
    for target in 0..2 {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        record_room(&root).unwrap();
        let paths = StatePaths::for_project_root(&root).unwrap();
        let name = format!("same-{}", TaskId::new().short());
        let scopes = [
            task_record::Scope::Machine,
            task_record::Scope::Workspace(WorkspaceId::from_project_root(&root)),
        ];
        let ids = scopes.map(|scope| {
            task_record_store::mutate(|records| {
                let (id, record) = records.bind(&task_record::Binding {
                    scope,
                    name: name.clone(),
                });
                record
                    .marks
                    .insert(root.clone(), dead_mark(FireId::new(&id), true));
                id
            })
            .unwrap()
        });
        let info = RunLockInfo {
            pid: 4242,
            started_at: Timestamp::now(),
        };
        let mut backlog = LockedBacklog::acquire(&paths).unwrap();
        backlog
            .record
            .tasks
            .entry(ids[target].to_string())
            .or_default()
            .stopping = Some(StopMark {
            pid: info.pid,
            started_at: info.started_at,
            cause: StopCause::Stop,
        });
        append_stopped_record(
            ids[target].as_str(),
            &name,
            &root,
            info,
            None,
            None,
            &mut backlog,
        )
        .unwrap();
        drop(backlog);
        let records = task_record::load().unwrap();
        for (index, id) in ids.iter().enumerate() {
            let mark = &records.get(id).unwrap().marks[&root];
            assert_eq!(mark.result.is_some(), index == target, "{target}: {mark:?}");
        }
        let rows = run_log::task_records(&logs_dir(), &ids[target], Some(&root));
        assert!(
            rows.iter().any(|row| row.result == LoopRunResult::Canceled),
            "{target}: {rows:?}"
        );
    }
}
