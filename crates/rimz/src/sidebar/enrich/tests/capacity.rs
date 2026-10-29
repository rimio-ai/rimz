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
            "node": "triage", "outcome": outcome, "detail": "no capacity",
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
                "reason": "no capacity", "gave_up": false
            }
        }
    });
    waits["task"]["/repo/b"] = waits["task"]["/repo/a"].clone();
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
        actual.push(serde_json::to_value(projected).unwrap()["capacity_wait"].clone());
    }
    waits["task"]["/repo/a"]["first"] = "2025-12-31T23:59:00Z".into();
    std::fs::write(&path, serde_json::to_vec(&waits).unwrap()).unwrap();
    for opts in [cached_opts(), producing_opts()] {
        let projected = fold(snapshot.clone(), None, &runtime, opts);
        actual.push(serde_json::to_value(projected).unwrap()["capacity_wait"].clone());
    }
    let workflow_oldest = serde_json::json!({
        "kinds": ["codex"], "since": "2026-01-01T00:00:00Z",
        "gives_up_at": "2026-01-01T06:00:00Z", "count": 4
    });
    let loop_oldest = serde_json::json!({
        "kinds": ["claude"], "since": "2025-12-31T23:59:00Z",
        "gives_up_at": null, "count": 4
    });
    assert_eq!(
        actual,
        [
            workflow_oldest.clone(),
            workflow_oldest,
            loop_oldest.clone(),
            loop_oldest
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
            "reason": "no capacity", "gave_up": false
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
        actual.push(!serde_json::to_value(projected).unwrap()["capacity_wait"].is_null());
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
    let before = serde_json::to_value(&projected).unwrap()["capacity_wait"].clone();
    std::fs::remove_file(record).unwrap();
    let cleared = serde_json::to_value(fold_cached(projected, None, &runtime)).unwrap();
    assert_eq!(
        (before, cleared["capacity_wait"].clone()),
        (
            serde_json::json!({
                "kinds": ["codex"], "since": "2026-01-01T00:00:00Z",
                "gives_up_at": "2026-01-01T06:00:00Z", "count": 1
            }),
            serde_json::Value::Null
        ),
        "an unreadable loop ledger cannot hide workflow waits or retain stale summaries"
    );
}
