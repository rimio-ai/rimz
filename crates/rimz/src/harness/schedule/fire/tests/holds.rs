use super::*;

#[test]
fn workflow_tick_resamples_a_hold_before_its_due_time() {
    let root = tempfile::tempdir().unwrap();
    let state = StatePaths::for_project_root(root.path()).unwrap();
    let runtime = RuntimePaths::for_state(&state).unwrap();
    let record: crate::store::workflow::RunRecord = serde_json::from_value(serde_json::json!({
        "version":1,"id":"wf_0190b8172c0070008000000000000001","workflow":"review",
        "spec_dir":"/definitions/review","spec_digest":"abc","scope":{"kind":"worktree","path":root.path()},
        "project_root":root.path(),"created_at":"2026-01-01T00:00:00Z","state":"active","node":"pr",
        "entered_at":"2026-01-01T00:00:00Z","visits":{},"visit_grant":0,"deadline_fired":false,"next_seq":1,
        "recheck_at":"1970-01-01T00:06:40Z",
        "wait":{"node":"pr","outcome":"hold","case":0,"detail":"ready","first":"1970-01-01T00:00:00Z","last":"1970-01-01T00:00:00Z","count":1,"recheck_at":"1970-01-01T00:06:40Z"}
    })).unwrap();
    let paths = state.workflow(&record.id);
    std::fs::create_dir_all(&paths.home).unwrap();
    std::fs::write(&paths.record, serde_json::to_vec(&record).unwrap()).unwrap();
    for (now, expected) in [(120, 120), (149, 120), (180, 180), (390, 390), (400, 400)] {
        tick_workflows(
            &runtime,
            root.path(),
            Timestamp::from_second(now).unwrap(),
            LoopRunHost::Detached,
        );
        let stamps: BTreeMap<crate::ids::WorkflowRunId, Timestamp> =
            crate::disk::atomic::read_json_cache(&runtime.lane_path("workflow-tick.json"));
        assert_eq!(
            stamps.get(&record.id),
            Some(&Timestamp::from_second(expected).unwrap()),
            "hold tick at {now}"
        );
    }
}
