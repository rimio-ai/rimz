use super::*;

fn workflow_wait(state: &StatePaths, suffix: u32, status: &str, outcome: &str) -> PathBuf {
    let id = crate::ids::WorkflowRunId::parse(&format!("wf_0190b8172c00700080000000{suffix:08}"))
        .unwrap();
    let paths = state.workflow(&id);
    std::fs::create_dir_all(&paths.home).unwrap();
    let at = "2026-01-01T00:00:00Z";
    let record = serde_json::json!({
        "version": crate::store::workflow::VERSION, "id": id, "workflow": "review",
        "spec_dir": "/definitions/review",
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
fn workflow_capacity_waits_fold_alike_on_producer_and_consumer_folds() {
    let (_dir, runtime, snapshot) = runtime();
    let state = StatePaths::under(runtime.workspace_id.clone(), &runtime.root).unwrap();
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
    let expected = serde_json::json!([
        {"kind": "disk", "reason": "free disk low", "count": 2}
    ]);
    assert_eq!(
        actual,
        [expected.clone(), expected],
        "count runs, not retry counts; ignore non-capacity and inactive runs"
    );
}

#[test]
fn removed_workflow_waits_clear() {
    let (_dir, runtime, snapshot) = runtime();
    let state = StatePaths::under(runtime.workspace_id.clone(), &runtime.root).unwrap();
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
        "a removed workflow wait leaves no stale summary"
    );
}

#[test]
fn self_clearing_and_legacy_holds_do_not_notify() {
    let (_dir, runtime, snapshot) = runtime();
    let state = StatePaths::under(runtime.workspace_id.clone(), &runtime.root).unwrap();
    for hold in [serde_json::json!("capacity"), serde_json::Value::Null] {
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
