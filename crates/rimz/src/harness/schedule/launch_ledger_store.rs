//! Locked durable mutations of resident/workflow launch memory and judge declines.
//!
//! The catalog's per-task removals and renames (`remove`, `remove_workflows`,
//! `rename_workflows`, `remove_declines`, `rename_declines`) read their file first and return
//! before taking the lock when it holds nothing for the task: taking the lock creates room state
//! in a project with no room (`loop_schedule::loop_rename_without_a_room_does_not_create_state`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::launch_ledger::{self, DeclineRecord, LaunchRecord};

pub(super) fn record_workflow(
    paths: &crate::StatePaths,
    task: &str,
    checkout: &Path,
    record: launch_ledger::WorkflowLaunchRecord,
    replace_project: bool,
) -> Result<(), LedgerWriteErr> {
    mutate(
        paths,
        launch_ledger::workflow_path,
        "loop-workflow-launches.lock",
        launch_ledger::load_workflows,
        |ledger| &mut ledger.tasks,
        |tasks| {
            let launches = tasks.entry(task.to_owned()).or_default();
            if launches
                .get(checkout)
                .is_some_and(|previous| previous.run == record.run)
            {
                return;
            }
            if replace_project && record.scope == crate::store::workflow::ScopeKind::Worktree {
                launches.retain(|_, previous| {
                    previous.scope != crate::store::workflow::ScopeKind::Project
                });
            }
            launches.insert(checkout.to_owned(), record);
        },
        || Ok(()),
    )
}

pub(super) fn remove_workflows(
    paths: &crate::StatePaths,
    task: &str,
) -> Result<(), LedgerWriteErr> {
    if !launch_ledger::load_workflows(paths)?
        .tasks
        .contains_key(task)
    {
        return Ok(());
    }
    mutate(
        paths,
        launch_ledger::workflow_path,
        "loop-workflow-launches.lock",
        launch_ledger::load_workflows,
        |ledger| &mut ledger.tasks,
        |tasks| {
            tasks.remove(task);
        },
        || Ok(()),
    )
}

pub(super) fn rename_workflows(
    paths: &crate::StatePaths,
    task: &str,
    name: &str,
) -> Result<(), LedgerWriteErr> {
    if !launch_ledger::load_workflows(paths)?
        .tasks
        .contains_key(task)
    {
        return Ok(());
    }
    mutate(
        paths,
        launch_ledger::workflow_path,
        "loop-workflow-launches.lock",
        launch_ledger::load_workflows,
        |ledger| &mut ledger.tasks,
        |tasks| {
            if let Some(checkouts) = tasks.remove(task) {
                tasks.insert(name.to_owned(), checkouts);
            }
        },
        || Ok(()),
    )
}

#[derive(Debug, thiserror::Error)]
pub(super) enum LedgerWriteErr {
    #[error(transparent)]
    Read(#[from] launch_ledger::LedgerErr),
    #[error(transparent)]
    Lock(#[from] crate::disk::lock::LockErr),
    #[error(transparent)]
    Write(#[from] crate::disk::atomic::AtomicErr),
    #[error("recording resident loop assist: {0}")]
    Completion(#[source] std::io::Error),
}

fn mutate<T: Clone + PartialEq + serde::Serialize, R>(
    paths: &crate::StatePaths,
    path: fn(&crate::StatePaths) -> PathBuf,
    lock: &str,
    load: fn(&crate::StatePaths) -> Result<T, launch_ledger::LedgerErr>,
    records: fn(&mut T) -> &mut BTreeMap<String, BTreeMap<std::path::PathBuf, R>>,
    edit: impl FnOnce(&mut BTreeMap<String, BTreeMap<std::path::PathBuf, R>>),
    after_write: impl FnOnce() -> std::io::Result<()>,
) -> Result<(), LedgerWriteErr> {
    let _guard = crate::disk::lock::WorkspaceLock::acquire(&paths.lock_path(lock))?;
    let mut ledger = load(paths)?;
    let before = ledger.clone();
    let checkouts = records(&mut ledger);
    checkouts.retain(|_, launches| {
        launches.retain(|checkout, _| checkout.is_dir());
        !launches.is_empty()
    });
    edit(checkouts);
    if ledger != before {
        crate::disk::atomic::write_temp_then_rename(&path(paths), &ledger)?;
    }
    after_write().map_err(LedgerWriteErr::Completion)
}

pub(super) fn record_decline(
    paths: &crate::StatePaths,
    task: &str,
    checkout: &Path,
    record: DeclineRecord,
    after_write: impl FnOnce() -> std::io::Result<()>,
) -> Result<(), LedgerWriteErr> {
    mutate(
        paths,
        launch_ledger::decline_path,
        "loop-declines.lock",
        launch_ledger::load_declines,
        |declines| declines,
        |declines| {
            declines
                .entry(task.to_owned())
                .or_default()
                .insert(checkout.to_owned(), record);
        },
        after_write,
    )
}

pub(super) fn remove_declines(paths: &crate::StatePaths, task: &str) -> Result<(), LedgerWriteErr> {
    if !launch_ledger::load_declines(paths)?.contains_key(task) {
        return Ok(());
    }
    mutate(
        paths,
        launch_ledger::decline_path,
        "loop-declines.lock",
        launch_ledger::load_declines,
        |declines| declines,
        |declines| {
            declines.remove(task);
        },
        || Ok(()),
    )
}

pub(super) fn remove_checkout_decline(
    paths: &crate::StatePaths,
    task: &str,
    checkout: &Path,
) -> Result<(), LedgerWriteErr> {
    mutate(
        paths,
        launch_ledger::decline_path,
        "loop-declines.lock",
        launch_ledger::load_declines,
        |declines| declines,
        |declines| {
            if let Some(checkouts) = declines.get_mut(task) {
                checkouts.remove(checkout);
                if checkouts.is_empty() {
                    declines.remove(task);
                }
            }
        },
        || Ok(()),
    )
}

pub(super) fn rename_declines(
    paths: &crate::StatePaths,
    task: &str,
    name: &str,
) -> Result<(), LedgerWriteErr> {
    if !launch_ledger::load_declines(paths)?.contains_key(task) {
        return Ok(());
    }
    mutate(
        paths,
        launch_ledger::decline_path,
        "loop-declines.lock",
        launch_ledger::load_declines,
        |declines| declines,
        |declines| {
            if let Some(checkouts) = declines.remove(task) {
                declines.insert(name.to_owned(), checkouts);
            }
        },
        || Ok(()),
    )
}

pub(super) fn record(
    paths: &crate::StatePaths,
    task: &str,
    checkout: &Path,
    record: LaunchRecord,
    after_write: impl FnOnce() -> std::io::Result<()>,
) -> Result<(), LedgerWriteErr> {
    mutate(
        paths,
        launch_ledger::path,
        "loop-launches.lock",
        launch_ledger::load,
        |ledger| ledger,
        |ledger| {
            ledger
                .entry(task.to_owned())
                .or_default()
                .insert(checkout.to_owned(), record);
        },
        after_write,
    )
}

pub(super) fn remove(paths: &crate::StatePaths, task: &str) -> Result<(), LedgerWriteErr> {
    if !launch_ledger::load(paths)?.contains_key(task) {
        return Ok(());
    }
    mutate(
        paths,
        launch_ledger::path,
        "loop-launches.lock",
        launch_ledger::load,
        |ledger| ledger,
        |ledger| {
            ledger.remove(task);
        },
        || Ok(()),
    )
}

#[cfg(test)]
mod tests {
    use super::super::launch_ledger;
    use super::*;
    use launch_ledger::Ledger;

    fn workflow_launch(
        scope: crate::store::workflow::ScopeKind,
    ) -> launch_ledger::WorkflowLaunchRecord {
        launch_ledger::WorkflowLaunchRecord {
            at: "2026-10-27T16:44:26Z".parse().unwrap(),
            run: crate::ids::WorkflowRunId::new(),
            scope,
        }
    }

    #[test]
    fn workflow_launches_round_trip_at_version_one_without_rewriting_the_same_run() {
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
        assert_eq!(value["version"], 1);
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
        for contents in ["{\"version\":2,\"tasks\":{}}", "malformed"] {
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

    #[test]
    fn launch_records_round_trip_old_rows_and_named_destinations() {
        let old = serde_json::json!({"at": "2026-06-01T00:00:00Z", "leader": "otter"});
        let mut named = old.clone();
        named["destination"] = serde_json::json!("/worktrees/lane");
        for value in [old, named] {
            let record: LaunchRecord = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(record).unwrap(), value);
        }
    }

    #[test]
    fn catalog_moves_declines_on_rename_and_clears_on_redefinition_and_removal() {
        use super::super::{catalog::TaskCatalog, instances};
        for operation in ["rename", "replace", "remove"] {
            let root = tempfile::tempdir().unwrap();
            let paths = crate::StatePaths::for_project_root(root.path()).unwrap();
            let workspace =
                crate::workspace::WorkspaceResolver::resolve(root.path(), None).unwrap();
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
            let mut decline = serde_json::json!({"at": "2026-06-01T00:00:00Z", "since": null, "profile": "codex", "reason": "nothing to do"});
            crate::disk::atomic::write_temp_then_rename(
                &launch_ledger::decline_path(&paths),
                &serde_json::json!({
                    "guard": {root.path().display().to_string(): decline.clone()},
                    "other": {root.path().display().to_string(): decline.clone()}
                }),
            )
            .unwrap();
            let catalog = TaskCatalog::load(Some(root.path())).unwrap();
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
            let memory: serde_json::Value = serde_json::from_slice(
                &std::fs::read(launch_ledger::decline_path(&paths)).unwrap(),
            )
            .unwrap();
            assert!(
                memory.get("guard").is_none(),
                "{operation} must clear the old key: {memory}"
            );
            decline["tier"] = serde_json::json!(0);
            assert_eq!(memory["other"][root.path().display().to_string()], decline);
            assert_eq!(memory.get("renamed").is_some(), operation == "rename");
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
        crate::disk::atomic::write_temp_then_rename(
            &launch_ledger::path(&paths),
            &Ledger::from([(
                "fixer".to_owned(),
                std::collections::BTreeMap::from([(
                    home.path().to_owned(),
                    LaunchRecord {
                        destination: None,
                        at: jiff::Timestamp::now(),
                        leader: "otter".into(),
                    },
                )]),
            )]),
        )
        .unwrap();
        let before = launch_ledger::load(&paths).unwrap();
        let error = TaskCatalog::load(Some(home.path()))
            .unwrap()
            .rename("fixer", "renamed")
            .unwrap_err();
        assert!(error.to_string().contains("resident leaders"), "{error}");
        let catalog = TaskCatalog::load(Some(home.path())).unwrap();
        assert!(catalog.visible().contains_key("fixer"));
        assert!(!catalog.visible().contains_key("renamed"));
        assert_eq!(launch_ledger::load(&paths).unwrap(), before);
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
        std::fs::write(launch_ledger::path(&paths), "malformed").unwrap();
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
        use super::super::{catalog::TaskCatalog, instances};
        let mut outcomes = Vec::new();
        for operation in ["rename", "replace", "remove", "replace-remove"] {
            let home = tempfile::tempdir().unwrap();
            let paths = crate::StatePaths::for_project_root(home.path()).unwrap();
            let workspace =
                crate::workspace::WorkspaceResolver::resolve(home.path(), None).unwrap();
            crate::workspace::record::write(
                &paths,
                &crate::workspace::record::WorkspaceRecord::from_resolved(&workspace),
            )
            .unwrap();
            let entry = crate::config::TaskEntry {
                root: home.path().to_owned(),
                workflow: Some("review".into()),
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
            let value: serde_json::Value = serde_json::from_slice(
                &std::fs::read(launch_ledger::workflow_path(&paths)).unwrap(),
            )
            .unwrap();
            outcomes.push((
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
        assert_eq!(
            outcomes,
            vec![
                (false, true, true),
                (true, false, true),
                (false, false, true),
                (false, false, true)
            ]
        );
    }

    #[test]
    fn malformed_workflow_memory_blocks_only_workflow_catalog_edits() {
        use super::super::{catalog::TaskCatalog, instances};
        let mut outcomes = Vec::new();
        for workflow in [false, true] {
            for rename in [false, true] {
                let home = tempfile::tempdir().unwrap();
                let paths = crate::StatePaths::for_project_root(home.path()).unwrap();
                let workspace =
                    crate::workspace::WorkspaceResolver::resolve(home.path(), None).unwrap();
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
                        workflow: workflow.then(|| "review".into()),
                        check: (!workflow).then(|| "true".into()),
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
        }
        assert_eq!(outcomes, vec![false, false, true, true]);
    }

    #[test]
    fn launches_survive_reload_and_remove_only_the_named_task() {
        let home = tempfile::tempdir().unwrap();
        let paths = crate::StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
        let checkout = home.path().join("lane");
        std::fs::create_dir(&checkout).unwrap();
        let launch = LaunchRecord {
            destination: None,
            at: jiff::Timestamp::now(),
            leader: "otter".into(),
        };
        record(&paths, "fixer", &checkout, launch.clone(), || Ok(())).unwrap();
        record(&paths, "yagni", &checkout, launch.clone(), || Ok(())).unwrap();
        assert_eq!(
            launch_ledger::load(&paths).unwrap()["fixer"][&checkout],
            launch
        );
        remove(&paths, "fixer").unwrap();
        let ledger = launch_ledger::load(&paths).unwrap();
        assert!(!ledger.contains_key("fixer"));
        assert_eq!(ledger["yagni"][&checkout], launch);
    }

    #[test]
    fn next_write_prunes_gone_checkouts_and_rewrites_manual_launches() {
        let home = tempfile::tempdir().unwrap();
        let paths = crate::StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
        let gone = home.path().join("gone");
        std::fs::create_dir(&gone).unwrap();
        let launch = LaunchRecord {
            destination: None,
            at: jiff::Timestamp::now(),
            leader: "otter".into(),
        };
        record(&paths, "fixer", &gone, launch.clone(), || Ok(())).unwrap();
        std::fs::remove_dir(&gone).unwrap();
        record(&paths, "fixer", home.path(), launch.clone(), || Ok(())).unwrap();
        let replacement = LaunchRecord {
            leader: "fox".into(),
            ..launch
        };
        record(&paths, "fixer", home.path(), replacement.clone(), || Ok(())).unwrap();
        let ledger = launch_ledger::load(&paths).unwrap();
        assert_eq!(ledger["fixer"].len(), 1);
        assert_eq!(ledger["fixer"][home.path()], replacement);
    }

    #[test]
    fn failed_completion_keeps_the_attempted_launch() {
        let home = tempfile::tempdir().unwrap();
        let paths = crate::StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
        let launch = LaunchRecord {
            destination: None,
            at: jiff::Timestamp::now(),
            leader: "otter".into(),
        };
        record(&paths, "fixer", home.path(), launch.clone(), || Ok(())).unwrap();
        for task in ["yagni", "fixer"] {
            let replacement = LaunchRecord {
                leader: "fox".into(),
                ..launch.clone()
            };
            let result = record(&paths, task, home.path(), replacement.clone(), || {
                Err(std::io::Error::other("assist append failed"))
            });
            assert!(matches!(result, Err(LedgerWriteErr::Completion(_))));
            assert_eq!(
                launch_ledger::load(&paths).unwrap()[task][home.path()],
                replacement
            );
        }
    }
}
