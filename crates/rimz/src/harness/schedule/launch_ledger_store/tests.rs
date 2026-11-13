use super::super::launch_ledger;
use super::*;

fn workflow_launch(
    scope: crate::store::workflow::ScopeKind,
) -> launch_ledger::WorkflowLaunchRecord {
    launch_ledger::WorkflowLaunchRecord {
        at: "2026-10-27T16:44:26Z".parse().unwrap(),
        run: Some(crate::ids::WorkflowRunId::new()),
        scope,
        leader: None,
        destination: None,
    }
}

/// Reruns `test` in a child process with a home of its own, since the machine loop config it
/// edits is shared by every test process of a run and another one's edit can restore a task
/// this test removed. True in the parent, which then returns.
fn in_private_machine_home(test: &str) -> bool {
    if std::env::var_os("RIMZ_TEST_LEDGER_MACHINE_HOME").is_some() {
        return false;
    }
    let home = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("harness::schedule::launch_ledger_store::tests::{test}"),
            "--nocapture",
        ])
        .env("RIMZ_UNIT_TEST_RIMZ_HOME", home.path())
        .env("RIMZ_TEST_LEDGER_MACHINE_HOME", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

#[test]
fn workflow_launches_round_trip_at_version_two_without_rewriting_the_same_run() {
    let home = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
    assert!(
        launch_ledger::load_workflows(&paths)
            .unwrap()
            .tasks
            .is_empty()
    );
    let record = workflow_launch(crate::store::workflow::ScopeKind::Worktree);
    record_workflow(&paths, "land", home.path(), record.clone(), false).unwrap();
    let ledger = launch_ledger::load_workflows(&paths).unwrap();
    assert_eq!(
        ledger
            .tasks
            .get("land")
            .and_then(|rows| rows.get(home.path())),
        Some(&record)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(launch_ledger::workflow_path(&paths)).unwrap())
            .unwrap();
    assert_eq!(value["version"], 2);
    assert_eq!(
        value["tasks"]["land"][home.path().to_str().unwrap()]["scope"],
        "worktree"
    );
    let before = crate::disk::atomic::testkit::fsync_count();
    let repeated = launch_ledger::WorkflowLaunchRecord {
        at: jiff::Timestamp::now(),
        ..record.clone()
    };
    record_workflow(&paths, "land", home.path(), repeated, false).unwrap();
    assert_eq!(crate::disk::atomic::testkit::fsync_count(), before);
    assert_eq!(
        launch_ledger::load_workflows(&paths).unwrap().tasks["land"][home.path()],
        record
    );
}

#[test]
fn workflow_future_versions_and_malformed_files_fail_closed_without_writes() {
    let home = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
    std::fs::create_dir_all(launch_ledger::workflow_path(&paths).parent().unwrap()).unwrap();
    let mut outcomes = Vec::new();
    for contents in ["{\"version\":3,\"tasks\":{}}", "malformed"] {
        std::fs::write(launch_ledger::workflow_path(&paths), contents).unwrap();
        let read_failed = launch_ledger::load_workflows(&paths).is_err();
        let write_failed = record_workflow(
            &paths,
            "land",
            home.path(),
            workflow_launch(crate::store::workflow::ScopeKind::Worktree),
            false,
        )
        .is_err();
        outcomes.push((
            read_failed,
            write_failed,
            std::fs::read_to_string(launch_ledger::workflow_path(&paths)).unwrap() == contents,
        ));
    }
    assert_eq!(outcomes, vec![(true, true, true); 2]);
}

#[test]
fn workflow_mutations_prune_vanished_checkouts_but_keep_project_roots_and_other_tasks() {
    use crate::store::workflow::ScopeKind;
    let home = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
    let gone = home.path().join("gone");
    std::fs::create_dir(&gone).unwrap();
    record_workflow(
        &paths,
        "land",
        &gone,
        workflow_launch(ScopeKind::Worktree),
        false,
    )
    .unwrap();
    let project = workflow_launch(ScopeKind::Project);
    record_workflow(&paths, "land", home.path(), project.clone(), false).unwrap();
    let other = workflow_launch(ScopeKind::Worktree);
    record_workflow(&paths, "other", home.path(), other.clone(), false).unwrap();
    std::fs::remove_dir(&gone).unwrap();
    rename_workflows(&paths, "land", "renamed").unwrap();
    let ledger = launch_ledger::load_workflows(&paths).unwrap();
    assert_eq!(
        ledger.tasks.get("renamed"),
        Some(&std::collections::BTreeMap::from([(
            home.path().to_owned(),
            project
        )]))
    );
    assert!(!ledger.tasks.contains_key("land"));
    remove_workflows(&paths, "renamed").unwrap();
    assert_eq!(
        launch_ledger::load_workflows(&paths).unwrap().tasks,
        std::collections::BTreeMap::from([(
            "other".into(),
            std::collections::BTreeMap::from([(home.path().to_owned(), other)])
        )])
    );
}

fn write_residents(paths: &crate::StatePaths, residents: serde_json::Value) {
    crate::disk::atomic::write_temp_then_rename(&launch_ledger::resident_path(paths), &residents)
        .unwrap();
}

#[test]
fn resident_launches_fold_into_workflow_memory_once_and_keep_their_leader() {
    use crate::store::workflow::ScopeKind;
    let home = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
    let lane = home.path().join("lane");
    std::fs::create_dir(&lane).unwrap();
    let (root, lane_key) = (
        home.path().to_str().unwrap().to_owned(),
        lane.to_str().unwrap().to_owned(),
    );
    write_residents(
        &paths,
        serde_json::json!({
            "fixer": {
                root.clone(): {"at": "2026-06-01T00:00:00Z", "leader": "otter"},
                lane_key.clone(): {"at": "2026-06-02T00:00:00Z", "leader": "fox",
                    "destination": "/worktrees/lane"}
            },
            "land": {root.clone(): {"at": "2026-06-03T00:00:00Z", "leader": "owl"}}
        }),
    );
    let run = workflow_launch(ScopeKind::Worktree);
    crate::disk::atomic::write_temp_then_rename(
        &launch_ledger::workflow_path(&paths),
        &serde_json::json!({"version": 1, "tasks": {"land": {root.clone(): run}}}),
    )
    .unwrap();
    let resident =
        |at: &str, leader: &str, destination: Option<&str>| launch_ledger::WorkflowLaunchRecord {
            at: at.parse().unwrap(),
            run: None,
            scope: ScopeKind::Worktree,
            leader: Some(leader.into()),
            destination: destination.map(PathBuf::from),
        };
    let expected = BTreeMap::from([
        (
            "fixer".to_owned(),
            BTreeMap::from([
                (
                    home.path().to_owned(),
                    resident("2026-06-01T00:00:00Z", "otter", None),
                ),
                (
                    lane.clone(),
                    resident("2026-06-02T00:00:00Z", "fox", Some("/worktrees/lane")),
                ),
            ]),
        ),
        (
            "land".to_owned(),
            BTreeMap::from([(home.path().to_owned(), run.clone())]),
        ),
    ]);
    assert_eq!(
        launch_ledger::load_workflows(&paths).unwrap().tasks,
        expected
    );
    // The first write persists the fold; the resident file is not read again after it.
    remove_workflows(&paths, "land").unwrap();
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(launch_ledger::workflow_path(&paths)).unwrap())
            .unwrap();
    assert_eq!(value["version"], 2);
    assert!(value["tasks"].get("land").is_none());
    assert_eq!(
        value["tasks"]["fixer"].as_object().map(|rows| rows.len()),
        Some(2)
    );
    remove_workflows(&paths, "fixer").unwrap();
    assert!(
        launch_ledger::load_workflows(&paths)
            .unwrap()
            .tasks
            .is_empty()
    );
    assert!(launch_ledger::resident_path(&paths).exists());
}

#[test]
fn unreadable_resident_launches_fail_workflow_memory_closed_until_folded() {
    let home = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
    std::fs::create_dir_all(launch_ledger::resident_path(&paths).parent().unwrap()).unwrap();
    std::fs::write(launch_ledger::resident_path(&paths), "malformed").unwrap();
    assert!(launch_ledger::load_workflows(&paths).is_err());
    crate::disk::atomic::write_temp_then_rename(
        &launch_ledger::workflow_path(&paths),
        &serde_json::json!({"version": 2, "tasks": {}}),
    )
    .unwrap();
    assert!(
        launch_ledger::load_workflows(&paths)
            .unwrap()
            .tasks
            .is_empty()
    );
}

#[test]
fn catalog_moves_capacity_on_rename_and_never_reads_the_obsolete_decline_file() {
    if in_private_machine_home(
        "catalog_moves_capacity_on_rename_and_never_reads_the_obsolete_decline_file",
    ) {
        return;
    }
    use super::super::{catalog::TaskCatalog, instances};
    for operation in ["rename", "replace", "remove"] {
        let root = tempfile::tempdir().unwrap();
        let paths = crate::StatePaths::for_project_root(root.path()).unwrap();
        let workspace = crate::workspace::WorkspaceResolver::resolve(root.path(), None).unwrap();
        crate::workspace::record::write(
            &paths,
            &crate::workspace::record::WorkspaceRecord::from_resolved(&workspace),
        )
        .unwrap();
        let entry = crate::config::TaskEntry {
            root: root.path().to_owned(),
            stay: true,
            once: Some(true),
            agent: Some("codex".into()),
            every: Some("1h".into()),
            ..Default::default()
        };
        instances::insert(&paths, "guard", &entry).unwrap();
        // A decline cache an older binary left, now unreadable, decides nothing.
        let declines = crate::StatePaths::class_path(
            &paths.root,
            crate::disk::paths::Class::Records,
            "loop-declines.json",
        );
        std::fs::create_dir_all(declines.parent().unwrap()).unwrap();
        std::fs::write(&declines, b"broken").unwrap();
        let catalog = TaskCatalog::load(Some(root.path())).unwrap();
        let capacity = serde_json::json!({
            "since": null, "first": "2026-06-01T00:00:00Z", "last": "2026-06-01T00:00:00Z",
            "count": 1, "kinds": ["claude"], "reason": "no capacity", "gave_up": false
        });
        crate::disk::atomic::write_temp_then_rename(
            &launch_ledger::capacity_path(&paths),
            &serde_json::json!({"guard": {root.path().display().to_string(): capacity}}),
        )
        .unwrap();
        match operation {
            "rename" => {
                catalog.rename("guard", "renamed").unwrap();
            }
            "replace" => {
                catalog.replace_machine("guard", &entry).unwrap();
            }
            _ => {
                catalog.remove("guard").unwrap();
            }
        }
        let capacity: serde_json::Value =
            serde_json::from_slice(&std::fs::read(launch_ledger::capacity_path(&paths)).unwrap())
                .unwrap();
        assert!(capacity.get("guard").is_none(), "{operation}: {capacity}");
        assert_eq!(capacity.get("renamed").is_some(), operation == "rename");
        assert_eq!(std::fs::read(&declines).unwrap(), b"broken", "{operation}");
    }
}

#[test]
fn resident_rename_preserves_launched_task_identity() {
    use super::super::{catalog::TaskCatalog, instances};
    let home = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::for_project_root(home.path()).unwrap();
    let workspace = crate::workspace::WorkspaceResolver::resolve(home.path(), None).unwrap();
    crate::workspace::record::write(
        &paths,
        &crate::workspace::record::WorkspaceRecord::from_resolved(&workspace),
    )
    .unwrap();
    let entry = crate::config::TaskEntry {
        root: home.path().to_owned(),
        stay: true,
        agent: Some("codex".into()),
        every: Some("1h".into()),
        ..Default::default()
    };
    instances::insert(&paths, "unlaunched", &entry).unwrap();
    TaskCatalog::load(Some(home.path()))
        .unwrap()
        .rename("unlaunched", "fixer")
        .unwrap();
    write_residents(
        &paths,
        serde_json::json!({"fixer": {home.path().to_str().unwrap():
            {"at": "2026-06-01T00:00:00Z", "leader": "otter"}}}),
    );
    let before = launch_ledger::load_workflows(&paths).unwrap();
    let error = TaskCatalog::load(Some(home.path()))
        .unwrap()
        .rename("fixer", "renamed")
        .unwrap_err();
    assert!(error.to_string().contains("resident leaders"), "{error}");
    let catalog = TaskCatalog::load(Some(home.path())).unwrap();
    assert!(catalog.visible().contains_key("fixer"));
    assert!(!catalog.visible().contains_key("renamed"));
    assert_eq!(launch_ledger::load_workflows(&paths).unwrap(), before);
}

#[test]
fn nonresident_removal_ignores_malformed_launch_ledger() {
    use super::super::{catalog::TaskCatalog, instances};
    let home = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::for_project_root(home.path()).unwrap();
    let workspace = crate::workspace::WorkspaceResolver::resolve(home.path(), None).unwrap();
    crate::workspace::record::write(
        &paths,
        &crate::workspace::record::WorkspaceRecord::from_resolved(&workspace),
    )
    .unwrap();
    instances::insert(
        &paths,
        "check",
        &crate::config::TaskEntry {
            root: home.path().to_owned(),
            check: Some("true".into()),
            every: Some("1h".into()),
            ..Default::default()
        },
    )
    .unwrap();
    std::fs::write(launch_ledger::resident_path(&paths), "malformed").unwrap();
    assert!(
        TaskCatalog::load(Some(home.path()))
            .unwrap()
            .remove("check")
            .is_ok()
    );
    assert!(
        !TaskCatalog::load(Some(home.path()))
            .unwrap()
            .visible()
            .contains_key("check")
    );
}

#[test]
fn catalog_routes_workflow_memory_through_rename_remove_and_replacement() {
    if in_private_machine_home(
        "catalog_routes_workflow_memory_through_rename_remove_and_replacement",
    ) {
        return;
    }
    use super::super::{catalog::TaskCatalog, instances};
    let mut outcomes = Vec::new();
    let cases = ["workflow", "stay"].into_iter().flat_map(|kind| {
        ["rename", "replace", "remove", "replace-remove"].map(|operation| (kind, operation))
    });
    for (kind, operation) in cases {
        let home = tempfile::tempdir().unwrap();
        let paths = crate::StatePaths::for_project_root(home.path()).unwrap();
        let workspace = crate::workspace::WorkspaceResolver::resolve(home.path(), None).unwrap();
        crate::workspace::record::write(
            &paths,
            &crate::workspace::record::WorkspaceRecord::from_resolved(&workspace),
        )
        .unwrap();
        let entry = crate::config::TaskEntry {
            root: home.path().to_owned(),
            workflow: (kind == "workflow").then(|| "review".into()),
            agent: (kind == "stay").then(|| "claude".into()),
            prompt: (kind == "stay").then(|| "fix".into()),
            stay: kind == "stay",
            every: Some("1h".into()),
            ..Default::default()
        };
        instances::insert(&paths, "land", &entry).unwrap();
        let record = workflow_launch(crate::store::workflow::ScopeKind::Worktree);
        let original = serde_json::json!({"version":1,"tasks":{
            "land":{home.path().to_str().unwrap():record},
            "other":{home.path().to_str().unwrap():record}
        }});
        crate::disk::atomic::write_temp_then_rename(
            &launch_ledger::workflow_path(&paths),
            &original,
        )
        .unwrap();
        let catalog = TaskCatalog::load(Some(home.path())).unwrap();
        match operation {
            "rename" => {
                catalog.rename("land", "renamed").unwrap();
            }
            "replace" => {
                catalog.replace_machine("land", &entry).unwrap();
            }
            "replace-remove" => {
                catalog
                    .replace_machine(
                        "land",
                        &crate::config::TaskEntry {
                            workflow: None,
                            agent: None,
                            prompt: None,
                            stay: false,
                            check: Some("true".into()),
                            ..entry
                        },
                    )
                    .unwrap();
                TaskCatalog::load(Some(home.path()))
                    .unwrap()
                    .remove("land")
                    .unwrap();
            }
            _ => {
                catalog.remove("land").unwrap();
            }
        }
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(launch_ledger::workflow_path(&paths)).unwrap())
                .unwrap();
        outcomes.push((
            kind,
            value["tasks"].get("land").is_some(),
            value["tasks"].get("renamed").is_some(),
            value["tasks"]["other"] == original["tasks"]["other"],
        ));
        if operation == "replace" {
            TaskCatalog::load(Some(home.path()))
                .unwrap()
                .remove("land")
                .unwrap();
        }
    }
    let expected = ["workflow", "stay"].into_iter().flat_map(|kind| {
        [
            (kind, false, true, true),
            (kind, true, false, true),
            (kind, false, false, true),
            (kind, false, false, true),
        ]
    });
    assert_eq!(outcomes, expected.collect::<Vec<_>>());
}

#[test]
fn redefinition_forgets_only_checkouts_whose_runs_have_not_reached_an_agent() {
    if in_private_machine_home(
        "redefinition_forgets_only_checkouts_whose_runs_have_not_reached_an_agent",
    ) {
        return;
    }
    use super::super::{catalog::TaskCatalog, instances};
    use crate::store::workflow::ScopeKind;
    let spec = "[workflow]\nscope='worktree'\nstart='pick'\n[script.pick]\nrun='true'\non.go='fix'\n[agent.fix]\nlifecycle='turn'\nagent='claude'\nprompt-file='prompt.md'\nthen='@done'\n";
    let mut remembered = Vec::new();
    for operation in ["replace", "remove"] {
        let home = tempfile::tempdir().unwrap();
        let paths = crate::StatePaths::for_project_root(home.path()).unwrap();
        let workspace = crate::workspace::WorkspaceResolver::resolve(home.path(), None).unwrap();
        crate::workspace::record::write(
            &paths,
            &crate::workspace::record::WorkspaceRecord::from_resolved(&workspace),
        )
        .unwrap();
        let entry = crate::config::TaskEntry {
            root: home.path().to_owned(),
            agent: Some("claude".into()),
            prompt: Some("fix".into()),
            stay: true,
            each_worktree: true,
            when: Some(vec!["pr=open".into()]),
            ..Default::default()
        };
        instances::insert(&paths, "redefined", &entry).unwrap();
        let mut launches = BTreeMap::new();
        for (checkout, visited) in [("waiting", &["pick"][..]), ("acted", &["pick", "fix"])] {
            let checkout = home.path().join(checkout);
            std::fs::create_dir(&checkout).unwrap();
            let id = crate::ids::WorkflowRunId::new();
            let record: crate::store::workflow::RunRecord =
                serde_json::from_value(serde_json::json!({
                    "version":crate::store::workflow::VERSION, "id":id, "workflow":"redefined", "task":"redefined",
                    "spec_dir":home.path(), "spec_digest":"abc", "prompt":null,
                    "scope":{"kind":"worktree","path":checkout},
                    "project_root":home.path(), "created_at":"2000-01-01T00:00:00Z",
                    "state":"active", "park":null, "node":visited[visited.len() - 1],
                    "entered_at":"2000-01-01T00:00:00Z", "input":null, "key":null,
                    "recheck_at":null, "wait":null,
                    "visits":visited.iter().map(|step| (*step, serde_json::json!(1))).collect::<BTreeMap<_, _>>(),
                    "visit_grant":0, "deadline":null, "deadline_fired":false,
                    "next_seq":1, "attempt":null, "prev_attempt":null, "terminal_at":null
                }))
                .unwrap();
            let guard = crate::disk::lock::WorkspaceLock::acquire(&paths.workspace_lock).unwrap();
            crate::store::workflow::initialize(&paths, &record, spec.as_bytes(), &guard).unwrap();
            launches.insert(
                checkout,
                launch_ledger::WorkflowLaunchRecord {
                    run: Some(id),
                    ..workflow_launch(ScopeKind::Worktree)
                },
            );
        }
        crate::disk::atomic::write_temp_then_rename(
            &launch_ledger::workflow_path(&paths),
            &serde_json::json!({"version": 2, "tasks": {"redefined": launches}}),
        )
        .unwrap();
        let catalog = TaskCatalog::load(Some(home.path())).unwrap();
        if operation == "replace" {
            catalog.replace_machine("redefined", &entry).unwrap();
        } else {
            catalog.remove("redefined").unwrap();
        }
        let ledger = launch_ledger::load_workflows(&paths).unwrap();
        if operation == "replace" {
            // The next case's root would collide with this machine entry.
            TaskCatalog::load(Some(home.path()))
                .unwrap()
                .remove("redefined")
                .unwrap();
        }
        remembered.push(
            ledger
                .tasks
                .get("redefined")
                .map(|launches| {
                    launches
                        .keys()
                        .map(|checkout| checkout.strip_prefix(home.path()).unwrap().to_owned())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        );
    }
    assert_eq!(remembered, vec![vec![PathBuf::from("acted")], vec![]]);
}

#[test]
fn malformed_workflow_memory_blocks_edits_of_tasks_that_record_there() {
    use super::super::{catalog::TaskCatalog, instances};
    let mut outcomes = Vec::new();
    for (kind, rename) in [
        ("check", false),
        ("check", true),
        ("stay", false),
        ("stay", true),
        ("workflow", false),
        ("workflow", true),
    ] {
        let home = tempfile::tempdir().unwrap();
        let paths = crate::StatePaths::for_project_root(home.path()).unwrap();
        let workspace = crate::workspace::WorkspaceResolver::resolve(home.path(), None).unwrap();
        crate::workspace::record::write(
            &paths,
            &crate::workspace::record::WorkspaceRecord::from_resolved(&workspace),
        )
        .unwrap();
        instances::insert(
            &paths,
            "land",
            &crate::config::TaskEntry {
                root: home.path().to_owned(),
                workflow: (kind == "workflow").then(|| "review".into()),
                check: (kind == "check").then(|| "true".into()),
                agent: (kind == "stay").then(|| "claude".into()),
                prompt: (kind == "stay").then(|| "fix".into()),
                stay: kind == "stay",
                every: Some("1h".into()),
                ..Default::default()
            },
        )
        .unwrap();
        std::fs::write(launch_ledger::workflow_path(&paths), "malformed").unwrap();
        let catalog = TaskCatalog::load(Some(home.path())).unwrap();
        let result = if rename {
            catalog.rename("land", "renamed")
        } else {
            catalog.remove("land")
        };
        outcomes.push(result.is_err());
    }
    assert_eq!(outcomes, vec![false, false, true, true, true, true]);
}
