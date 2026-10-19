use super::*;

fn entry() -> TaskEntry {
    TaskEntry {
        root: "/repo".into(),
        dir: Some("/trees/followup/subdir".into()),
        ..TaskEntry::default()
    }
}

fn paths(entry: TaskEntry, source: TaskSource, arming: Option<&Arming>) -> Vec<PathBuf> {
    worktree_pins(
        "nightly",
        &LoadedTask::new("nightly", entry, source),
        arming,
        Timestamp::UNIX_EPOCH,
    )
    .into_iter()
    .map(|pin| {
        assert_eq!(pin.name, "nightly");
        pin.path
    })
    .collect()
}

#[test]
fn plain_tasks_pin_root_and_run_directory_even_with_invalid_actions() {
    for source in [TaskSource::Config, TaskSource::Instance] {
        assert_eq!(
            paths(entry(), source, None),
            vec![
                PathBuf::from("/repo"),
                PathBuf::from("/trees/followup/subdir")
            ]
        );
    }
}

#[test]
fn fanout_and_bare_worktree_spawns_pin_only_root() {
    for task in [
        TaskEntry {
            each_worktree: true,
            ..entry()
        },
        TaskEntry {
            worktree: Some("new".into()),
            ..entry()
        },
    ] {
        assert_eq!(
            paths(task, TaskSource::Config, None),
            vec![PathBuf::from("/repo")]
        );
    }
}

#[test]
fn worktree_guards_conditions_and_watches_need_run_directory() {
    let spawn = TaskEntry {
        worktree: Some("new".into()),
        ..entry()
    };
    for task in [
        TaskEntry {
            check: Some(crate::config::TaskCheck::Shell("true".into())),
            ..spawn.clone()
        },
        TaskEntry {
            judge: vec![crate::config::Judge {
                agent: "claude".into(),
                prompt: None,
                prompt_file: None,
                recheck: None,
                timeout: None,
            }],
            ..spawn.clone()
        },
        TaskEntry {
            when: Some(vec!["ci=passed".into()]),
            ..spawn.clone()
        },
        TaskEntry {
            watch: Some(crate::config::WatchSpec::Command("true".into())),
            ..spawn
        },
    ] {
        assert_eq!(paths(task, TaskSource::Instance, None).len(), 2);
    }
}

#[test]
fn paused_tasks_pin_but_disabled_expired_and_project_tasks_do_not() {
    let paused = Arming {
        enabled: true,
        at: None,
        pause_until: Some(Timestamp::from_second(10).unwrap()),
        strikes: None,
    };
    assert_eq!(paths(entry(), TaskSource::Config, Some(&paused)).len(), 2);
    for strikes in [None, Some(3)] {
        let disabled = Arming {
            enabled: false,
            strikes,
            ..paused
        };
        assert!(paths(entry(), TaskSource::Config, Some(&disabled)).is_empty());
    }
    for arming in [None, Some(&paused)] {
        assert!(
            paths(
                entry(),
                TaskSource::Project {
                    state: TrustState::Trusted
                },
                arming
            )
            .is_empty()
        );
    }
    assert!(
        paths(
            TaskEntry {
                deadline: Some(Timestamp::UNIX_EPOCH),
                ..entry()
            },
            TaskSource::Instance,
            None
        )
        .is_empty()
    );
}

#[test]
fn strict_sources_read_all_rooms_and_allow_absent_instance_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("empty")).unwrap();
    let room = dir.path().join("room");
    let file = instances::path(&room);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(
        &file,
        serde_json::to_vec(&Tasks(BTreeMap::from([("nightly".into(), entry())]))).unwrap(),
    )
    .unwrap();
    let pins = pins_from_sources(
        dir.path(),
        Tasks::default(),
        &BTreeMap::new(),
        Timestamp::UNIX_EPOCH,
    )
    .unwrap();
    assert_eq!(pins.len(), 2);
    let broken = instances::path(&dir.path().join("other-room"));
    std::fs::create_dir_all(broken.parent().unwrap()).unwrap();
    std::fs::write(&broken, "not json").unwrap();
    let error = pins_from_sources(
        dir.path(),
        Tasks::default(),
        &BTreeMap::new(),
        Timestamp::UNIX_EPOCH,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains(&broken.display().to_string()));
}

#[test]
fn strict_sources_preserve_same_names_in_other_roots_and_machine_precedence() {
    let dir = tempfile::tempdir().unwrap();
    let file = instances::path(&dir.path().join("room"));
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(
        &file,
        serde_json::to_vec(&Tasks(BTreeMap::from([("nightly".into(), entry())]))).unwrap(),
    )
    .unwrap();
    let machine = Tasks(BTreeMap::from([(
        "nightly".into(),
        TaskEntry {
            dir: Some("/machine".into()),
            ..entry()
        },
    )]));
    let pins =
        pins_from_sources(dir.path(), machine, &BTreeMap::new(), Timestamp::UNIX_EPOCH).unwrap();
    assert_eq!(
        pins.iter()
            .map(|pin| pin.path.as_path())
            .collect::<Vec<_>>(),
        [Path::new("/repo"), Path::new("/machine")]
    );
    let machine = Tasks(BTreeMap::from([(
        "nightly".into(),
        TaskEntry {
            root: "/other".into(),
            dir: None,
            ..entry()
        },
    )]));
    let pins =
        pins_from_sources(dir.path(), machine, &BTreeMap::new(), Timestamp::UNIX_EPOCH).unwrap();
    assert_eq!(pins.len(), 3);
    assert!(
        pins.iter()
            .any(|pin| pin.path == Path::new("/trees/followup/subdir"))
    );
}

#[test]
fn strict_sources_reject_unreadable_workspace_listing() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("workspaces");
    std::fs::write(&file, "not a directory").unwrap();
    let error = pins_from_sources(
        &file,
        Tasks::default(),
        &BTreeMap::new(),
        Timestamp::UNIX_EPOCH,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains(&file.display().to_string()));
}
