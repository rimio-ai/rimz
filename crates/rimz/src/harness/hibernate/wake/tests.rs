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
        WakeTrigger::Helper {
            requested_at_ms: Some(requested),
            ready: &mut || Some(Dwell::Elapsed),
        },
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
    assert_eq!(record["dwell"], "elapsed");
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
        crate::StatePaths::for_project_root_under(&dir.path().join("project"), dir.path()).unwrap();
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
        crate::StatePaths::for_project_root_under(&dir.path().join("project"), dir.path()).unwrap();
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
        WakeTrigger::Helper {
            requested_at_ms: Some(0),
            ready: &mut || Some(Dwell::Elapsed),
        },
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
        crate::StatePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
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
    let log = std::fs::read_to_string(paths.audit_path("hibernate.log.jsonl")).unwrap_or_default();
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

struct TestClock(std::cell::Cell<u64>);

impl Clock for TestClock {
    fn elapsed_ms(&self) -> u64 {
        self.0.get()
    }
    fn sleep(&self, ms: u64) {
        self.0.set(self.0.get() + ms);
    }
}

fn dwell_case(
    queue: impl Fn(u64) -> Option<u64>,
    focused: impl Fn(u64) -> Option<bool>,
    expected: Option<(&str, u64)>,
    aborted_at: u64,
) {
    let (dir, paths, _, entry) = fixture();
    let runtime =
        crate::RuntimePaths::under(paths.workspace_id.clone(), &dir.path().join("runtime"))
            .unwrap();
    let stamp = wake_stamp_path(&runtime, Some(&entry.pane_id));
    crate::disk::atomic::write_temp_then_rename_cache(&stamp, &jiff::Timestamp::UNIX_EPOCH)
        .unwrap();
    let before_stamp = std::fs::read(&stamp).unwrap();
    let before_record = std::fs::read(&paths.hibernation).unwrap();
    let audit = paths.audit_path("hibernate.log.jsonl");
    std::fs::create_dir_all(audit.parent().unwrap()).unwrap();
    std::fs::write(&audit, "").unwrap();
    let clock = TestClock(std::cell::Cell::new(0));
    let request = WakeHelperRequest {
        workspace_id: paths.workspace_id.clone(),
        pane_id: entry.pane_id.clone(),
        requested_at_ms: Some(0),
    };
    let focus = || focused(clock.elapsed_ms());
    let result = wake_focus_with(
        &paths,
        &runtime,
        &request,
        dir.path(),
        || focus_dwell(&clock, || queue(clock.elapsed_ms()), focus),
        focus,
        |_| {},
    )
    .unwrap();
    if let Some((cause, elapsed)) = expected {
        assert_eq!(
            clock.elapsed_ms(),
            elapsed,
            "thaw must follow the dwell decision"
        );
        assert!(matches!(result, Woken::Thawed { .. }));
        assert_eq!(
            std::fs::read_to_string(entry.scope.join("cgroup.freeze")).unwrap(),
            "0"
        );
        assert!(hibernate::read(&paths).is_empty());
        let record: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&audit).unwrap()).unwrap();
        assert_eq!(record["trigger"], "helper");
        assert_eq!(record["dwell"], cause);
        assert!(matches!(
            read_wake_stamp(&runtime, Some(&entry.pane_id)),
            Some(WakeStamp::Focus { .. })
        ));
    } else {
        assert_eq!(
            clock.elapsed_ms(),
            aborted_at,
            "abort must follow the dwell decision"
        );
        assert_eq!(
            std::fs::read(&stamp).unwrap(),
            before_stamp,
            "aborted dwell must not stamp"
        );
        assert_eq!(
            std::fs::read(&paths.hibernation).unwrap(),
            before_record,
            "aborted dwell must not alter the record"
        );
        assert_eq!(
            std::fs::read(&audit).unwrap(),
            b"",
            "aborted dwell must not log"
        );
        assert_eq!(
            std::fs::read_to_string(entry.scope.join("cgroup.freeze")).unwrap(),
            "1"
        );
    }
}

#[test]
fn dwell_elapsed_thaws_while_focused() {
    dwell_case(|_| Some(3), |_| Some(true), Some(("elapsed", 1_000)), 0);
}

#[test]
fn dwell_input_growth_thaws_while_focused() {
    dwell_case(
        |ms| Some(if ms >= 500 { 4 } else { 3 }),
        |_| Some(true),
        Some(("input", 500)),
        0,
    );
}

#[test]
fn dwell_focus_out_growth_aborts_without_writes() {
    dwell_case(
        |ms| Some(if ms >= 500 { 6 } else { 3 }),
        |ms| Some(ms < 500),
        None,
        500,
    );
}

#[test]
fn dwell_departure_at_expiry_aborts_without_writes() {
    dwell_case(|_| Some(3), |ms| Some(ms < 800), None, 1_000);
}

#[test]
fn dwell_settle_growth_is_only_baseline() {
    dwell_case(
        |ms| Some(if ms >= 100 { 4 } else { 3 }),
        |_| Some(true),
        Some(("elapsed", 1_000)),
        0,
    );
}

#[test]
fn dwell_unavailable_tty_still_thaws_at_expiry() {
    dwell_case(|_| None, |_| Some(true), Some(("elapsed", 1_000)), 0);
}

#[test]
fn dwell_failed_focus_sample_thaws() {
    dwell_case(
        |ms| Some(if ms >= 500 { 4 } else { 3 }),
        |_| None,
        Some(("input", 500)),
        0,
    );
}

#[test]
fn dwell_counts_time_before_helper_start() {
    let clock = TestClock(std::cell::Cell::new(600));
    assert_eq!(
        focus_dwell(&clock, || Some(3), || Some(true)),
        Some(Dwell::Elapsed)
    );
    assert_eq!(clock.elapsed_ms(), 1_000);
    let clock = TestClock(std::cell::Cell::new(1_200));
    assert_eq!(
        focus_dwell(
            &clock,
            || panic!("expired dwell needs no queue"),
            || Some(true)
        ),
        Some(Dwell::Elapsed)
    );
    assert_eq!(clock.elapsed_ms(), 1_200);
}

#[test]
fn focus_unrecorded_pane_writes_no_stamp() {
    let (dir, paths, _, entry) = fixture();
    hibernate::withdraw(&paths, &entry.kind, &entry.agent_id).unwrap();
    let runtime =
        crate::RuntimePaths::under(paths.workspace_id.clone(), &dir.path().join("runtime"))
            .unwrap();
    let request = WakeHelperRequest {
        workspace_id: paths.workspace_id.clone(),
        pane_id: entry.pane_id.clone(),
        requested_at_ms: None,
    };
    wake_focus_with(
        &paths,
        &runtime,
        &request,
        dir.path(),
        || panic!("unrecorded pane needs no dwell"),
        || panic!("unrecorded pane needs no focus sample"),
        |_| {},
    )
    .unwrap();
    assert!(!wake_stamp_path(&runtime, Some(&entry.pane_id)).exists());
    assert!(!paths.audit_path("hibernate.log.jsonl").exists());
}

#[test]
fn focus_rechecked_after_lock_before_thaw() {
    let (dir, paths, _, entry) = fixture();
    let runtime =
        crate::RuntimePaths::under(paths.workspace_id.clone(), &dir.path().join("runtime"))
            .unwrap();
    let request = WakeHelperRequest {
        workspace_id: paths.workspace_id.clone(),
        pane_id: entry.pane_id.clone(),
        requested_at_ms: None,
    };
    wake_focus_with(
        &paths,
        &runtime,
        &request,
        dir.path(),
        || Some(Dwell::Elapsed),
        || Some(false),
        |_| {},
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(entry.scope.join("cgroup.freeze")).unwrap(),
        "1",
        "focus lost after dwell must veto the locked thaw"
    );
    assert!(!wake_stamp_path(&runtime, Some(&entry.pane_id)).exists());
    assert_eq!(hibernate::read(&paths), [entry]);
    assert!(!paths.audit_path("hibernate.log.jsonl").exists());
}
