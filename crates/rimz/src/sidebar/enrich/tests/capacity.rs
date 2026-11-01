use super::*;

fn capacity_path(state: &StatePaths) -> PathBuf {
    StatePaths::class_path(
        &state.root,
        crate::disk::paths::Class::Records,
        "loop-capacity-waits.json",
    )
}

fn workflow_wait(state: &StatePaths, suffix: u32, status: &str, outcome: &str) -> PathBuf {
    let id = crate::ids::WorkflowRunId::parse(&format!("wf_0190b8172c00700080000000{suffix:08}"))
        .unwrap();
    let paths = state.workflow(&id);
    std::fs::create_dir_all(&paths.home).unwrap();
    let at = "2026-01-01T00:00:00Z";
    let record = serde_json::json!({
        "version": 1, "id": id, "workflow": "review",
        "spec_dir": "/definitions/review", "spec_digest": "abc",
        "scope": {"kind": "worktree", "path": "/repo"}, "project_root": "/repo",
        "created_at": at, "state": status, "node": "triage", "entered_at": at,
        "visits": {}, "visit_grant": 0, "deadline_fired": false, "next_seq": 1,
        "wait": {
            "node": "triage", "outcome": outcome, "detail": "free disk low", "hold": "disk",
            "kinds": ["codex"], "first": at, "last": at, "count": 2,
            "gives_up_at": "2026-01-01T06:00:00Z", "recheck_at": null
        }
    });
    std::fs::write(&paths.record, serde_json::to_vec(&record).unwrap()).unwrap();
    paths.record
}

#[test]
fn durable_capacity_waits_merge_sources_on_producer_and_consumer_folds() {
    let (_dir, runtime, snapshot) = runtime();
    let state = StatePaths::under(runtime.workspace_id.clone(), &runtime.root).unwrap();
    let path = capacity_path(&state);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut waits = serde_json::json!({
        "task": {
            "/repo/a": {
                "since": "2026-01-01T00:00:00Z", "first": "2026-01-01T00:02:00Z",
                "last": "2026-01-01T00:03:00Z", "count": 4, "kinds": ["claude"],
                "reason": "cannot count active agents: broken", "hold": "store", "gave_up": false
            }
        }
    });
    waits["task"]["/repo/b"] = waits["task"]["/repo/a"].clone();
    waits["task"]["/repo/b"]["hold"] = "disk".into();
    waits["task"]["/repo/b"]["reason"] = "oldest disk".into();
    waits["task"]["/repo/b"]["first"] = "2025-12-30T00:00:00Z".into();
    waits["task"]["/repo/c"] = waits["task"]["/repo/a"].clone();
    waits["task"]["/repo/c"]["gave_up"] = true.into();
    std::fs::write(&path, serde_json::to_vec(&waits).unwrap()).unwrap();
    let clocks: BTreeMap<_, _> = ["/repo/a", "/repo/b", "/repo/c"]
        .map(|checkout| {
            (
                serde_json::to_string(&("task", checkout)).unwrap(),
                serde_json::json!({"since": "2026-01-01T00:00:00Z", "fired": true}),
            )
        })
        .into();
    let when_path = runtime.lane_path("loop-when.json");
    std::fs::create_dir_all(when_path.parent().unwrap()).unwrap();
    std::fs::write(when_path, serde_json::to_vec(&clocks).unwrap()).unwrap();
    for (suffix, status, outcome) in [
        (1, "active", "capacity"),
        (2, "active", "capacity"),
        (3, "active", "occupied"),
        (4, "active", "window"),
        (5, "done", "capacity"),
        (6, "parked", "capacity"),
    ] {
        workflow_wait(&state, suffix, status, outcome);
    }

    let mut actual = Vec::new();
    for opts in [cached_opts(), producing_opts()] {
        let projected = fold(snapshot.clone(), None, &runtime, opts);
        actual.push(serde_json::to_value(projected).unwrap()["throttle_notices"].clone());
    }
    waits["task"]["/repo/a"]["first"] = "2025-12-31T23:59:00Z".into();
    std::fs::write(&path, serde_json::to_vec(&waits).unwrap()).unwrap();
    for opts in [cached_opts(), producing_opts()] {
        let projected = fold(snapshot.clone(), None, &runtime, opts);
        actual.push(serde_json::to_value(projected).unwrap()["throttle_notices"].clone());
    }
    let expected = serde_json::json!([
        {"kind": "disk", "reason": "oldest disk", "count": 3},
        {"kind": "store", "reason": "cannot count active agents: broken", "count": 1}
    ]);
    assert_eq!(
        actual,
        [
            expected.clone(),
            expected.clone(),
            expected.clone(),
            expected
        ],
        "count scopes/runs, not retry counts; ignore give-ups and non-capacity/inactive runs"
    );
}

#[test]
fn capacity_waits_disappear_when_the_condition_clock_changes_or_is_absent() {
    let (_dir, runtime, snapshot) = runtime();
    let state = StatePaths::under(runtime.workspace_id.clone(), &runtime.root).unwrap();
    let path = capacity_path(&state);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        serde_json::to_vec(&serde_json::json!({"task": {"/repo/a": {
            "since": "2026-01-01T00:00:00Z", "first": "2026-01-01T00:00:00Z",
            "last": "2026-01-01T00:00:00Z", "count": 1, "kinds": ["claude"],
            "reason": "free disk low", "hold": "disk", "gave_up": false
        }}}))
        .unwrap(),
    )
    .unwrap();
    let matching = serde_json::json!({"since": "2026-01-01T00:00:00Z", "fired": true});
    let changed = serde_json::json!({"since": "2026-01-01T00:01:00Z", "fired": true});
    let scoped = serde_json::to_string(&("task", "/repo/a")).unwrap();
    let mut actual = Vec::new();
    let when_path = runtime.lane_path("loop-when.json");
    std::fs::create_dir_all(when_path.parent().unwrap()).unwrap();
    for clocks in [
        serde_json::json!({"task": matching}),
        serde_json::json!({"task": changed}),
        serde_json::json!({}),
        serde_json::json!({&scoped: matching}),
        serde_json::json!({&scoped: changed, "task": matching}),
    ] {
        std::fs::write(&when_path, serde_json::to_vec(&clocks).unwrap()).unwrap();
        let projected = fold_cached(snapshot.clone(), None, &runtime);
        actual.push(
            serde_json::to_value(projected).unwrap()["throttle_notices"]
                .as_array()
                .is_some_and(|notices| !notices.is_empty()),
        );
    }
    assert_eq!(
        actual,
        [true, false, false, true, false],
        "only a wait matching its current condition clock may show"
    );
}

#[test]
fn unreadable_capacity_records_preserve_workflow_waits_and_removed_waits_clear() {
    let (_dir, runtime, snapshot) = runtime();
    let state = StatePaths::under(runtime.workspace_id.clone(), &runtime.root).unwrap();
    let path = capacity_path(&state);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"broken").unwrap();
    let record = workflow_wait(&state, 1, "active", "capacity");
    let projected = fold_cached(snapshot, None, &runtime);
    let before = serde_json::to_value(&projected).unwrap()["throttle_notices"].clone();
    std::fs::remove_file(record).unwrap();
    let cleared = serde_json::to_value(fold_cached(projected, None, &runtime)).unwrap();
    assert_eq!(
        (before, cleared["throttle_notices"].clone()),
        (
            serde_json::json!([{"kind": "disk", "reason": "free disk low", "count": 1}]),
            serde_json::json!([])
        ),
        "an unreadable loop ledger cannot hide workflow waits or retain stale summaries"
    );
}

#[test]
fn notices_merge_live_room_tickets_and_use_the_oldest_reason_per_cause() {
    let (dir, runtime, mut snapshot) = runtime();
    snapshot.project_root = Some(dir.path().to_owned());
    let queue = crate::disk::paths::loop_throttle_queue_dir();
    std::fs::create_dir_all(&queue).unwrap();
    let mut tickets = Vec::new();
    let owner = nix::unistd::getpid().as_raw() as u32;
    for (offset, root, pid, hold, text) in [
        (
            1,
            dir.path(),
            owner,
            "check_failed",
            "capacity check failing: first",
        ),
        (
            2,
            dir.path(),
            owner,
            "check_failed",
            "capacity check failing: later",
        ),
        (3, Path::new("/another-room"), owner, "disk", "other room"),
        (4, dir.path(), u32::MAX, "store", "dead owner"),
        (5, dir.path(), owner, "capacity", "no capacity (your check)"),
    ] {
        let at = unix_now_ms() + offset;
        let path = crate::disk::paths::loop_throttle_ticket_in(&queue, at);
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "owner": {"pid": pid, "start": null}, "task": "triage", "root": root,
                "checkout": root, "enqueued_ms": at, "state": "waiting",
                "reason": {"kind": hold, "text": text}
            }))
            .unwrap(),
        )
        .unwrap();
        tickets.push(path);
    }
    let projected = fold_cached(snapshot, None, &runtime);
    for path in tickets {
        std::fs::remove_file(path).unwrap();
    }
    assert_eq!(
        serde_json::to_value(projected).unwrap()["throttle_notices"],
        serde_json::json!([
            {"kind": "check_failed", "reason": "capacity check failing: first", "count": 2}
        ])
    );
}

#[test]
fn self_clearing_and_legacy_holds_do_not_notify() {
    let (_dir, runtime, snapshot) = runtime();
    let state = StatePaths::under(runtime.workspace_id.clone(), &runtime.root).unwrap();
    let path = capacity_path(&state);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    for hold in [serde_json::json!("capacity"), serde_json::Value::Null] {
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({"task": {"/repo": {
                "since": "2026-01-01T00:00:00Z", "first": "2026-01-01T00:00:00Z",
                "last": "2026-01-01T00:00:00Z", "count": 4, "kinds": [],
                "reason": "capacity check failing: text is not classification", "hold": hold,
                "gave_up": false
            }}}))
            .unwrap(),
        )
        .unwrap();
        let when = runtime.lane_path("loop-when.json");
        std::fs::create_dir_all(when.parent().unwrap()).unwrap();
        std::fs::write(
            when,
            br#"{"task":{"since":"2026-01-01T00:00:00Z","fired":true}}"#,
        )
        .unwrap();
        let workflow = workflow_wait(&state, 1, "active", "capacity");
        let mut record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&workflow).unwrap()).unwrap();
        record["wait"]["hold"] = hold;
        std::fs::write(workflow, serde_json::to_vec(&record).unwrap()).unwrap();
        let projected = fold_cached(snapshot.clone(), None, &runtime);
        assert_eq!(
            serde_json::to_value(projected).unwrap()["throttle_notices"],
            serde_json::json!([])
        );
    }
}
