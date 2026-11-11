use super::*;
use crate::store::workflow::{RunRecord, RunState};

const RAIL: &str = include_str!("../../../harness/workflow/tests/fixtures/reached.toml");
// Captured verbatim from the engine tests in workflow::tests::attempts::reached.
const ORDINARY: &str = include_str!("fixtures/reached-ordinary.jsonl");
const SKIPPED: &str = include_str!("fixtures/reached-skipped.jsonl");
const LOOPED: &str = include_str!("fixtures/reached-looped.jsonl");

fn history_run(state: &StatePaths, journal: &str) -> RunRecord {
    let id = crate::ids::WorkflowRunId::new();
    let paths = state.workflow(&id);
    std::fs::create_dir_all(&paths.home).unwrap();
    std::fs::write(&paths.spec, RAIL).unwrap();
    std::fs::write(&paths.journal, journal).unwrap();
    serde_json::from_value(serde_json::json!({
        "version":1, "id":id, "workflow":"test", "spec_dir":"/unused-definition",
        "spec_digest":"test", "project_root":"/repo",
        "scope":{"kind":"worktree", "path":"/repo/lane"},
        "created_at":Timestamp::UNIX_EPOCH, "entered_at":Timestamp::UNIX_EPOCH,
        "state":"active", "node":"finish", "visits":{}, "visit_grant":0,
        "deadline_fired":false, "next_seq":1
    }))
    .unwrap()
}

#[test]
fn seeds_marks_ordinary_skipped_and_looped_history() {
    let (_dir, runtime, _) = runtime();
    let state = StatePaths::under(runtime.workspace_id.clone(), &runtime.root).unwrap();
    let actual = [
        (ORDINARY, "finish", RunState::Active),
        (SKIPPED, "finish", RunState::Active),
        (LOOPED, "build", RunState::Parked),
    ]
    .map(|(journal, node, status)| {
        let mut record = history_run(&state, journal);
        record.node = node.into();
        record.state = status;
        let runs = super::super::workflows::seeds(&[record], &state);
        assert_eq!(
            runs[0]
                .track
                .iter()
                .map(|node| node.name.as_str())
                .collect::<Vec<_>>(),
            ["build", "gate", "middle", "review", "finish"]
        );
        (
            runs[0]
                .track
                .iter()
                .map(|node| node.reached)
                .collect::<Vec<_>>(),
            runs[0].current.reached,
        )
    });
    assert_eq!(
        actual,
        [
            (vec![true, true, true, true, true], true),
            (vec![true, true, false, false, true], true),
            (vec![true, true, true, true, true], true),
        ]
    );
}

#[test]
fn seeds_caches_each_journal_and_reparses_only_the_appended_run() {
    let (_dir, runtime, _) = runtime();
    let state = StatePaths::under(runtime.workspace_id.clone(), &runtime.root).unwrap();
    let ordinary = history_run(&state, ORDINARY);
    let skipped = history_run(&state, SKIPPED);
    let mut terminal = skipped.clone();
    terminal.id = crate::ids::WorkflowRunId::new();
    terminal.state = RunState::Done;
    let records = [ordinary, skipped, terminal];
    let seed = || {
        let before = crate::testkit::parse_cache_read_counts();
        let _ = super::super::workflows::seeds(&records, &state);
        let after = crate::testkit::parse_cache_read_counts();
        (after.0 - before.0, after.1 - before.1)
    };
    let counts = seed();
    assert_eq!(counts, (4, 4), "one spec and journal read per live run");
    assert_eq!(seed(), (4, 0), "unchanged files only stat");

    use std::io::Write;
    let following = LOOPED
        .strip_prefix(ORDINARY)
        .unwrap()
        .lines()
        .next()
        .unwrap();
    writeln!(
        std::fs::OpenOptions::new()
            .append(true)
            .open(state.workflow(&records[0].id).journal)
            .unwrap(),
        "{following}"
    )
    .unwrap();
    assert_eq!(seed(), (4, 1), "only the appended run reparses");
}

#[test]
fn seeds_forgets_reached_nodes_when_the_journal_is_missing_or_unreadable() {
    let (_dir, runtime, _) = runtime();
    let state = StatePaths::under(runtime.workspace_id.clone(), &runtime.root).unwrap();
    let record = history_run(&state, SKIPPED);
    let records = [record];
    assert!(
        super::super::workflows::seeds(&records, &state)[0]
            .current
            .reached
    );
    let journal = state.workflow(&records[0].id).journal;
    std::fs::remove_file(&journal).unwrap();
    for unreadable in [false, true] {
        if unreadable {
            std::fs::create_dir(&journal).unwrap();
        }
        let runs = super::super::workflows::seeds(&records, &state);
        assert!(runs[0].track.iter().all(|node| !node.reached));
        assert!(!runs[0].current.reached);
    }
}

#[test]
fn seeds_skips_the_journal_when_the_spec_is_missing_or_unparseable() {
    let (_dir, runtime, _) = runtime();
    let state = StatePaths::under(runtime.workspace_id.clone(), &runtime.root).unwrap();
    let record = history_run(&state, SKIPPED);
    let spec = state.workflow(&record.id).spec;
    std::fs::write(&spec, "not TOML").unwrap();
    for missing in [false, true] {
        if missing {
            std::fs::remove_file(&spec).unwrap();
        }
        let before = crate::testkit::parse_cache_read_counts();
        let runs = super::super::workflows::seeds(std::slice::from_ref(&record), &state);
        let after = crate::testkit::parse_cache_read_counts();
        assert!(runs[0].track.is_empty());
        assert!(!runs[0].current.reached);
        assert_eq!(after.0 - before.0, 1, "stat only the spec");
        assert_eq!(after.1 - before.1, usize::from(!missing));
    }
}

/// Writes a one-check run at `checkout` and returns its id and record JSON, unwritten.
fn live_run(
    state: &StatePaths,
    root: &Path,
    checkout: &Path,
    now: Timestamp,
) -> (crate::ids::WorkflowRunId, serde_json::Value) {
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
    let record = serde_json::json!({
        "version":1, "id":id, "workflow":"land", "spec_dir":"/unused-definition",
        "spec_digest":"digest", "project_root":root,
        "scope":{"kind":"worktree", "path":checkout},
        "created_at":now, "entered_at":now, "state":"active",
        "node":"watch", "visits":{}, "visit_grant":0, "deadline_fired":false, "next_seq":1
    });
    (id, record)
}

#[test]
fn every_frame_fold_seeds_live_deployed_runs_and_clears_terminal_and_loop_task_runs() {
    let (dir, runtime, snapshot) = runtime();
    let state = StatePaths::under(runtime.workspace_id.clone(), &runtime.root).unwrap();
    let checkout = dir.path().join("trees/lane");
    let (id, mut record) = live_run(&state, dir.path(), &checkout, snapshot.now);
    let paths = state.workflow(&id);
    let snapshot = snapshot.with_project_root(Some(dir.path().to_path_buf()));
    let frame = crate::sidebar::frame::assemble_frame(Vec::new(), unix_now_ms(), "rimz-test");
    for origin in [None, Some("deployed"), Some("inline")] {
        match origin {
            Some(origin) => record["origin"] = origin.into(),
            None => assert!(record.get("origin").is_none()),
        }
        for status in ["active", "parked", "done", "failed", "canceled"] {
            record["state"] = status.into();
            std::fs::write(&paths.record, serde_json::to_vec(&record).unwrap()).unwrap();
            for opts in [cached_opts(), producing_opts()] {
                let projected = fold(snapshot.clone(), Some(&frame), &runtime, opts);
                let live = matches!(status, "active" | "parked") && origin != Some("inline");
                assert_eq!(
                    projected.worktree_groups.len(),
                    usize::from(live),
                    "{origin:?} {status}: both fold paths project rowless deployed scopes only"
                );
                if live {
                    let workflow = projected.worktree_groups[0].workflow.as_ref().unwrap();
                    assert_eq!(
                        workflow.track[0].name, "watch",
                        "read the sealed run spec, not spec_dir"
                    );
                    assert_eq!(workflow.run, id);
                    assert!(
                        !workflow.current.reached,
                        "a missing journal reaches nothing"
                    );
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
}

#[test]
fn a_loop_task_runs_step_agent_stays_an_ordinary_card_in_its_lane() {
    let (dir, runtime, _) = runtime();
    let state = StatePaths::under(runtime.workspace_id.clone(), &runtime.root).unwrap();
    let checkout = dir.path().join("trees/lane");
    let worktree = checkout.to_string_lossy().into_owned();
    let now = Timestamp::now();
    let (id, mut record) = live_run(&state, dir.path(), &checkout, now);
    record["node"] = "work".into();
    record["attempt"] = serde_json::json!({
        "kind":"agent", "lifecycle":"until", "seq":1, "node":"work",
        "started_at":now, "host":{"pid":1, "start":null}
    });
    let mut agent = codex_root("step", &worktree, "terminal_1");
    agent.workflow = Some(crate::agents::WorkflowAttemptRef {
        run: id.clone(),
        seq: 1,
        role: crate::agents::WorkflowAttemptRole::Step,
    });
    let snapshot =
        SidebarSnapshot::build_with_agents(runtime.workspace_id.clone(), vec![agent], now)
            .with_project_root(Some(dir.path().to_path_buf()));
    let frame = crate::sidebar::frame::assemble_frame(
        vec![pane("terminal_1", "codex", &worktree)],
        unix_now_ms(),
        "rimz-test",
    );
    for origin in ["deployed", "inline"] {
        record["origin"] = origin.into();
        std::fs::write(
            state.workflow(&id).record,
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        for opts in [cached_opts(), producing_opts()] {
            let projected = fold(snapshot.clone(), Some(&frame), &runtime, opts);
            assert_eq!(projected.worktree_groups.len(), 1, "{origin}");
            let group = &projected.worktree_groups[0];
            assert_eq!(
                group
                    .rows
                    .iter()
                    .filter(|row| row.is_agent())
                    .map(|row| row.id.as_str())
                    .collect::<Vec<_>>(),
                ["step"],
                "{origin}: the step agent keeps its card in its lane's group"
            );
            assert_eq!(
                group
                    .workflow
                    .as_ref()
                    .and_then(|run| run.agent.as_ref())
                    .map(|(_, session)| session.as_str()),
                (origin == "deployed").then_some("step"),
                "{origin}: only a deployed run binds its step agent to a strip"
            );
        }
    }
}
