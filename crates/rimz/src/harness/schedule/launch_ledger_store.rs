//! Locked durable mutations of resident launch and decline ledgers.

use std::path::Path;

use super::launch_ledger::{self, DeclineRecord, Declines, LaunchRecord, Ledger};

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

fn mutate(
    paths: &crate::StatePaths,
    edit: impl FnOnce(&mut Ledger),
    after_write: impl FnOnce() -> std::io::Result<()>,
) -> Result<(), LedgerWriteErr> {
    let _guard = crate::disk::lock::WorkspaceLock::acquire(&paths.lock_path("loop-launches.lock"))?;
    let mut ledger = launch_ledger::load(paths)?;
    let before = ledger.clone();
    ledger.retain(|_, launches| {
        launches.retain(|checkout, _| checkout.is_dir());
        !launches.is_empty()
    });
    edit(&mut ledger);
    if ledger != before {
        crate::disk::atomic::write_temp_then_rename(&launch_ledger::path(paths), &ledger)?;
    }
    after_write().map_err(LedgerWriteErr::Completion)
}

fn mutate_declines(
    paths: &crate::StatePaths,
    edit: impl FnOnce(&mut Declines),
    after_write: impl FnOnce() -> std::io::Result<()>,
) -> Result<(), LedgerWriteErr> {
    let _guard = crate::disk::lock::WorkspaceLock::acquire(&paths.lock_path("loop-declines.lock"))?;
    let mut declines = launch_ledger::load_declines(paths)?;
    let before = declines.clone();
    declines.retain(|_, checkouts| {
        checkouts.retain(|checkout, _| checkout.is_dir());
        !checkouts.is_empty()
    });
    edit(&mut declines);
    if declines != before {
        crate::disk::atomic::write_temp_then_rename(
            &launch_ledger::decline_path(paths),
            &declines,
        )?;
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
    mutate_declines(
        paths,
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
    mutate_declines(
        paths,
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
    mutate_declines(
        paths,
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
    mutate_declines(
        paths,
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
            let decline = serde_json::json!({"at": "2026-06-01T00:00:00Z", "since": null, "profile": "codex", "reason": "nothing to do"});
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
    fn launches_survive_reload_and_remove_only_the_named_task() {
        let home = tempfile::tempdir().unwrap();
        let paths = crate::StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
        let checkout = home.path().join("lane");
        std::fs::create_dir(&checkout).unwrap();
        let launch = LaunchRecord {
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
