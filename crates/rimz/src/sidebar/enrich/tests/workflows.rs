use super::*;

#[test]
fn every_frame_fold_seeds_live_runs_and_clears_terminal_runs() {
    let (dir, runtime, snapshot) = runtime();
    let state = StatePaths::under(runtime.workspace_id.clone(), &runtime.root).unwrap();
    let id = crate::ids::WorkflowRunId::new();
    let paths = state.workflow(&id);
    std::fs::create_dir_all(&paths.home).unwrap();
    std::fs::write(
        &paths.spec,
        "[workflow]\n\
         scope='worktree'\n\
         start='watch'\n\
         [checks.watch]\n\
         shell='true'\n\
         outcomes=['queued']\n\
         [checks.watch.on]\n\
         queued='@wait'\n",
    )
    .unwrap();
    let checkout = dir.path().join("trees/lane");
    let mut record = serde_json::json!({
        "version":1, "id":id, "workflow":"land", "spec_dir":"/unused-definition",
        "spec_digest":"digest", "project_root":dir.path(),
        "scope":{"kind":"worktree", "path":checkout},
        "created_at":snapshot.now, "entered_at":snapshot.now, "state":"active",
        "node":"watch", "visits":{}, "visit_grant":0, "deadline_fired":false, "next_seq":1
    });
    let snapshot = snapshot.with_project_root(Some(dir.path().to_path_buf()));
    let frame = crate::sidebar::frame::assemble_frame(Vec::new(), unix_now_ms(), "rimz-test");
    for status in ["active", "parked", "done", "failed", "canceled"] {
        record["state"] = status.into();
        std::fs::write(&paths.record, serde_json::to_vec(&record).unwrap()).unwrap();
        for opts in [cached_opts(), producing_opts()] {
            let projected = fold(snapshot.clone(), Some(&frame), &runtime, opts);
            let live = matches!(status, "active" | "parked");
            assert_eq!(
                projected.worktree_groups.len(),
                usize::from(live),
                "{status}: both fold paths project rowless scopes"
            );
            if live {
                let workflow = projected.worktree_groups[0].workflow.as_ref().unwrap();
                assert_eq!(
                    workflow.track[0].name, "watch",
                    "read the sealed run spec, not spec_dir"
                );
                assert_eq!(workflow.run, id);
                assert!(
                    fold_cached(snapshot.clone(), None, &runtime)
                        .worktree_groups
                        .is_empty(),
                    "frameless rollup folds have no groups"
                );
            }
        }
    }
}
