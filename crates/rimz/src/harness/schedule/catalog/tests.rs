use super::*;

#[test]
fn room_reaping_preserves_other_rooms() {
    let dir = tempfile::tempdir().unwrap();
    let root_a = dir.path().join("a");
    let root_b = dir.path().join("b");
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();
    let paths_a = StatePaths::for_project_root(&root_a).unwrap();
    let paths_b = StatePaths::for_project_root(&root_b).unwrap();
    for (root, paths) in [(&root_a, &paths_a), (&root_b, &paths_b)] {
        let workspace = crate::workspace::WorkspaceResolver::resolve(root, None).unwrap();
        crate::workspace::record::write(
            paths,
            &crate::workspace::record::WorkspaceRecord::from_resolved(&workspace),
        )
        .unwrap();
        instances::insert(
            paths,
            "actionless",
            &TaskEntry {
                root: root.clone(),
                when: Some(vec!["ci=failed".into()]),
                ..TaskEntry::default()
            },
        )
        .unwrap();
    }
    let before_b = std::fs::read(instances::path(&paths_b.root)).unwrap();
    assert_eq!(TaskCatalog::reap_dead_deliveries_for(&root_a).unwrap(), 1);
    assert!(instances::load_from(&paths_a.root).0.is_empty());
    assert_eq!(
        std::fs::read(instances::path(&paths_b.root)).unwrap(),
        before_b
    );
    assert_eq!(TaskCatalog::reap_dead_deliveries_for(&root_a).unwrap(), 0);
}

#[test]
fn removing_a_task_keeping_runs_leaves_its_unacted_run_live_and_names_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("room");
    std::fs::create_dir_all(&root).unwrap();
    let paths = StatePaths::for_project_root(&root).unwrap();
    let workspace = crate::workspace::WorkspaceResolver::resolve(&root, None).unwrap();
    crate::workspace::record::write(
        &paths,
        &crate::workspace::record::WorkspaceRecord::from_resolved(&workspace),
    )
    .unwrap();
    instances::insert(
        &paths,
        "review",
        &TaskEntry {
            root: root.clone(),
            ..task("review")
        },
    )
    .unwrap();
    let catalog = TaskCatalog::load_room(&root).unwrap();
    let binding = catalog.visible["review"].binding("review");
    mint(&binding, None).unwrap();
    let id = super::super::task_record::load()
        .unwrap()
        .live(&binding.key())
        .map(|(id, _)| id.clone())
        .unwrap();
    let record: crate::store::workflow::RunRecord = serde_json::from_value(serde_json::json!({
        "version": crate::store::workflow::VERSION, "id": crate::ids::WorkflowRunId::new(),
        "workflow": "review", "spec_dir": root, "prompt": null,
        "scope": {"kind": "worktree", "path": root}, "project_root": root,
        "created_at": "2000-01-01T00:00:00Z", "state": "active", "park": null, "node": "pr",
        "entered_at": "2000-01-01T00:00:00Z", "input": null, "key": null,
        "recheck_at": null, "wait": null, "visits": {}, "deadline": null,
        "deadline_fired": false, "next_seq": 1, "attempt": null, "prev_attempt": null,
        "terminal_at": null, "task": "review", "loop_fire": crate::ids::FireId::new(&id),
    }))
    .unwrap();
    {
        let guard = crate::disk::lock::WorkspaceLock::acquire(&paths.workspace_lock).unwrap();
        crate::store::workflow::initialize(&paths, &record, &guard).unwrap();
    }
    assert_eq!(
        crate::harness::workflow::unacted_task_runs(&paths, &id).unwrap(),
        vec![record.id.clone()],
        "the fixture run is unacted"
    );

    // A booking that no longer is the one the caller was checked against stays.
    let other = crate::agents::LaunchedBy {
        kind: crate::ids::AgentKind::new_unchecked("claude"),
        agent_id: "launch-other".into(),
    };
    let (mutation, alive) = catalog
        .remove_keeping_runs("review", &[other], std::iter::empty())
        .unwrap();
    assert!(!mutation.changed());
    assert!(alive.is_empty());
    assert!(instances::load_from(&paths.root).0.contains_key("review"));

    let (mutation, alive) = catalog
        .remove_keeping_runs("review", &[], std::iter::empty())
        .unwrap();

    assert!(mutation.changed());
    let alive = alive
        .iter()
        .map(|alive| match &alive.launch {
            super::super::launches::Launch::Workflow(run) => run.id.clone(),
            other => panic!("unexpected launch {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(alive, vec![record.id.clone()]);
    let after = crate::store::workflow::load(&paths.workflow(&record.id)).unwrap();
    assert_eq!(after.state, crate::store::workflow::RunState::Active);
    assert_eq!(after.task, None, "the run is released from the task");
    assert!(instances::load_from(&paths.root).0.is_empty());
}

fn task(label: &str) -> TaskEntry {
    TaskEntry {
        args: vec!["true".to_owned()],
        timeout: Some("1m".to_owned()),
        label: Some(label.to_owned()),
        root: PathBuf::from("/repo"),
        cron: Some("0 * * * *".to_owned()),
        ..TaskEntry::default()
    }
}

#[test]
fn pending_deliveries_filter_scope_and_sort_one_shot_triggers() {
    use super::super::pending::{pending_waits_by_session, session_deliveries};
    use crate::config::TaskTarget;
    use crate::ids::{AgentKind, AgentSessionId};

    let target = TaskTarget {
        kind: AgentKind::new_unchecked("claude"),
        session: AgentSessionId::from("session"),
        handle: "coder".into(),
    };
    let command = TaskEntry {
        root: PathBuf::from("/repo"),
        wait: Some(target.clone()),
        watch: Some(crate::config::WatchSpec::Command("cargo test".to_owned())),
        ..TaskEntry::default()
    };
    let signal = TaskEntry {
        watch: None,
        signal: Some("pr.merged".into()),
        ..command.clone()
    };
    let timer = TaskEntry {
        watch: None,
        fire_at: Some("2026-06-01T12:00:00Z".parse().unwrap()),
        wait_meta: Some(crate::config::WaitMeta {
            armed_at: "2026-06-01T10:00:00Z".parse().unwrap(),
            delay: None,
            reader: None,
            joined_until: None,
        }),
        ..command.clone()
    };
    let catalog = TaskCatalog::from_layers(
        Tasks(BTreeMap::from([
            ("z-command".into(), command.clone()),
            ("a-command".into(), command.clone()),
            (
                "m-make".into(),
                TaskEntry {
                    watch: Some(crate::config::WatchSpec::Command("make".to_owned())),
                    ..command.clone()
                },
            ),
            ("a-later-timer".into(), timer.clone()),
            (
                "z-sooner-timer".into(),
                TaskEntry {
                    fire_at: Some("2026-06-01T11:00:00Z".parse().unwrap()),
                    ..timer
                },
            ),
            ("signal".into(), signal.clone()),
            (
                "standing".into(),
                TaskEntry {
                    repeat: true,
                    ..signal
                },
            ),
            (
                "other-root".into(),
                TaskEntry {
                    root: PathBuf::from("/elsewhere"),
                    ..command.clone()
                },
            ),
            (
                "other-session".into(),
                TaskEntry {
                    wait: Some(TaskTarget {
                        session: AgentSessionId::from("other"),
                        ..target.clone()
                    }),
                    ..command.clone()
                },
            ),
            (
                "no-delivery".into(),
                TaskEntry {
                    wait: None,
                    args: vec!["true".to_owned()],
                    timeout: Some("1m".to_owned()),
                    ..command.clone()
                },
            ),
        ])),
        Tasks(BTreeMap::from([("machine".into(), command)])),
        None,
        Some(Path::new("/repo")),
    );
    let agent = crate::testkit::agent_state(
        target.kind.as_str(),
        target.session.as_str(),
        jiff::Timestamp::UNIX_EPOCH,
    );
    let session = (target.kind, target.session);
    assert_eq!(
        session_deliveries(&catalog, Path::new("/repo"), Some(&agent), &[])
            .map(|(name, _)| name)
            .collect::<Vec<_>>(),
        vec![
            "a-command",
            "a-later-timer",
            "m-make",
            "signal",
            "standing",
            "z-command",
            "z-sooner-timer"
        ]
    );
    let waits = pending_waits_by_session(&catalog, Path::new("/repo"));
    assert_eq!(
        waits[&session]
            .iter()
            .map(|wait| wait.name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "z-sooner-timer",
            "a-later-timer",
            "a-command",
            "m-make",
            "z-command",
            "signal"
        ]
    );
    assert_eq!(waits.len(), 2);
}

#[test]
fn foreign_machine_preserves_instance_and_both_bindings() {
    let instance = task("instance");
    let mut machine = task("machine");
    machine.root = PathBuf::from("/other");
    let catalog = TaskCatalog::from_layers(
        Tasks(BTreeMap::from([("same".to_owned(), instance.clone())])),
        Tasks(BTreeMap::from([("same".to_owned(), machine.clone())])),
        None,
        Some(Path::new("/repo")),
    );

    for loaded in [&catalog.visible()["same"], catalog.for_run("same").unwrap()] {
        assert_eq!(loaded.source(), TaskSource::Instance);
        assert_eq!(loaded.entry(), &instance);
    }
    for (source, entry) in [
        (TaskSource::Instance, instance),
        (TaskSource::Config, machine),
    ] {
        assert!(catalog.row_bindings.contains(&Binding::for_task(
            "same",
            source,
            &entry.resolved_root(),
        )));
    }
}

#[test]
fn skipped_machine_rows_keep_their_bindings_for_gc() {
    let skipped = Tasks(BTreeMap::from([
        ("bare".to_owned(), TaskEntry::default()),
        (
            "relative".to_owned(),
            TaskEntry {
                root: PathBuf::from("nowhere/x"),
                ..TaskEntry::default()
            },
        ),
    ]));
    let catalog = TaskCatalog::from_layers(
        Tasks::default(),
        Tasks::default(),
        None,
        Some(Path::new("/repo")),
    )
    .with_skipped_machine(skipped);

    for name in ["bare", "relative"] {
        assert!(
            catalog.row_bindings.contains(&Binding::for_task(
                name,
                TaskSource::Config,
                Path::new("/repo"),
            )),
            "{name}: gc would prune the record it keeps once its root is fixed"
        );
    }
}

#[test]
fn visible_and_runnable_precedence_diverge_for_untrusted_project() {
    let catalog = TaskCatalog::from_layers(
        Tasks(BTreeMap::from([("same".to_owned(), task("instance"))])),
        Tasks(BTreeMap::from([("same".to_owned(), task("machine"))])),
        Some((
            Tasks(BTreeMap::from([
                ("same".to_owned(), task("project")),
                ("blocked".to_owned(), task("blocked")),
            ])),
            TrustState::Untrusted,
        )),
        Some(Path::new("/repo")),
    );

    assert_eq!(
        catalog.visible()["same"].entry.label.as_deref(),
        Some("project")
    );
    assert_eq!(
        catalog.for_run("same").unwrap().entry.label.as_deref(),
        Some("machine")
    );
    assert!(matches!(
        catalog.for_run("blocked").unwrap().source,
        TaskSource::Project {
            state: TrustState::Untrusted
        }
    ));
}

#[test]
fn trusted_project_wins_both_merges() {
    let catalog = TaskCatalog::from_layers(
        Tasks(BTreeMap::from([("same".to_owned(), task("instance"))])),
        Tasks(BTreeMap::from([("same".to_owned(), task("machine"))])),
        Some((
            Tasks(BTreeMap::from([("same".to_owned(), task("project"))])),
            TrustState::Trusted,
        )),
        Some(Path::new("/repo")),
    );

    assert_eq!(
        catalog.visible()["same"].entry.label.as_deref(),
        Some("project")
    );
    assert_eq!(
        catalog.for_run("same").unwrap().entry.label.as_deref(),
        Some("project")
    );
    assert!(
        catalog
            .row_bindings
            .iter()
            .any(|binding| binding.key() == "machine::same")
    );
    assert!(catalog.row_bindings.contains(&Binding::for_task(
        "same",
        TaskSource::Project {
            state: TrustState::Trusted,
        },
        Path::new("/repo"),
    )));
}

#[test]
fn empty_project_layer_keeps_its_project_root() {
    let catalog = TaskCatalog::from_layers(
        Tasks::default(),
        Tasks::default(),
        Some((Tasks::default(), TrustState::Trusted)),
        Some(Path::new("/repo")),
    );

    assert_eq!(catalog.project_root.as_deref(), Some(Path::new("/repo")));
}

#[test]
fn malformed_schedule_stays_visible_with_valid_action() {
    let mut entry = task("still runnable manually");
    entry.fire_at = Some(jiff::Timestamp::UNIX_EPOCH);
    let catalog = TaskCatalog::from_layers(
        Tasks::default(),
        Tasks(BTreeMap::from([("broken".to_owned(), entry)])),
        None,
        None,
    );
    let loaded = &catalog.visible()["broken"];

    assert!(matches!(loaded.action(), Ok(TaskAction::Shell(_))));
    assert!(matches!(
        loaded.trigger(),
        Err(ScheduleErr::TimeConflict { .. })
    ));
    assert!(catalog.for_run("broken").is_some());
}

#[test]
fn ephemeral_tasks_are_one_shot_or_deadline_bound() {
    let mut entry = task("wait");
    assert!(!super::super::TaskShape::compile("task", &entry).is_ephemeral());

    entry.cron = None;
    assert!(super::super::TaskShape::compile("task", &entry).is_ephemeral());

    entry.cron = Some("*/15 * * * *".to_owned());
    entry.deadline = Some(jiff::Timestamp::UNIX_EPOCH);
    assert!(super::super::TaskShape::compile("task", &entry).is_ephemeral());

    entry.cron = None;
    entry.signal = Some("ci.failed".to_owned());
    entry.wait_meta = Some(crate::config::WaitMeta {
        armed_at: jiff::Timestamp::UNIX_EPOCH,
        delay: None,
        reader: None,
        joined_until: None,
    });
    assert!(super::super::TaskShape::compile("task", &entry).is_ephemeral());
    assert_eq!(TaskSource::from_entry(&entry), TaskSource::Instance);

    entry.signal = None;
    entry.deadline = None;
    entry.when = Some(vec!["team.stage=Done".to_owned()]);
    assert!(super::super::TaskShape::compile("task", &entry).is_ephemeral());
    entry.repeat = true;
    assert!(!super::super::TaskShape::compile("task", &entry).is_ephemeral());
}

#[test]
fn when_and_signal_rows_fire_once_unless_repeat_and_clocks_always_repeat() {
    let base = TaskEntry {
        cron: None,
        ..task("row")
    };
    let rows = [
        (
            "every",
            TaskEntry {
                cron: Some("0 * * * *".into()),
                ..base.clone()
            },
            false,
        ),
        (
            "cron",
            TaskEntry {
                cron: Some("0 * * * *".into()),
                ..base.clone()
            },
            false,
        ),
        (
            "signal",
            TaskEntry {
                signal: Some("ci.failed".into()),
                ..base.clone()
            },
            true,
        ),
        (
            "when",
            TaskEntry {
                when: Some(vec!["pr=open".into()]),
                ..base.clone()
            },
            true,
        ),
    ];
    for (trigger, entry, spent) in rows {
        let once = super::super::TaskShape::compile("row", &entry).is_ephemeral();
        assert_eq!(once, spent, "{trigger} without repeat");
        let repeating = TaskEntry {
            repeat: true,
            ..entry
        };
        assert!(
            !super::super::TaskShape::compile("row", &repeating).is_ephemeral(),
            "{trigger} with repeat"
        );
    }
}

#[test]
fn a_team_binding_is_standing_without_repeat() {
    let entry: TaskEntry = serde_json::from_value(serde_json::json!({
        "wait": {"kind": "claude", "session": "s", "handle": "@coder"},
        "team": "forge#feature",
        "signal": "ci.failed",
        "match": {"path": "/r/feature"},
        "root": "/r",
        "dir": "/r/feature",
    }))
    .unwrap();
    let shape = super::super::TaskShape::compile("binding", &entry);
    assert!(!shape.is_ephemeral());
    assert!(!shape.trigger().as_ref().unwrap().once);
}

#[test]
fn vanished_delivery_root_is_not_alive() {
    let dir = tempfile::tempdir().expect("tempdir");
    let entry = TaskEntry {
        root: dir.path().join("vanished"),
        ..TaskEntry::default()
    };
    let target = crate::config::TaskTarget {
        kind: crate::ids::AgentKind::new_unchecked("claude"),
        session: "missing-session".into(),
        handle: "@claude".to_owned(),
    };

    assert!(
        delivery_holder(&entry.resolved_root(), &target)
            .expect("check vanished target")
            .is_none()
    );
}
