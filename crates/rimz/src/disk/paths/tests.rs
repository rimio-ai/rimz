use super::*;
use crate::ids::WorkspaceId;

impl StatePaths {
    /// Room-local state paths.
    fn all_paths(&self) -> Vec<PathBuf> {
        vec![
            self.workspace_lock.clone(),
            self.publish_lock.clone(),
            self.tmp_dir.clone(),
            self.agents_dir.clone(),
            self.room_shared_dir.clone(),
            self.out_dir.clone(),
            self.skills_dir.clone(),
            self.events_log.clone(),
            self.events_archive_dir.clone(),
            self.agents_carryover.clone(),
            self.snapshots_dir.clone(),
            self.latest_snapshot.clone(),
            self.rollup_cache.clone(),
            self.messages_dir.clone(),
            self.message_history_dir.clone(),
            self.fleet_budget_record.clone(),
            self.transcript_dir.clone(),
            self.runs_dir.clone(),
            self.cache_dir.clone(),
            self.workspace_record.clone(),
            self.room_bin.clone(),
            self.boot_marker.clone(),
            self.live_roster.clone(),
            self.pending_recovery.clone(),
            self.idle_stop_requests.clone(),
            self.last_death_marker.clone(),
            self.doctor_watermark.clone(),
            self.auto_gc_stamp.clone(),
            self.crashes_dir.clone(),
            Class::Audit.path_under(&self.root),
            Class::Records.path_under(&self.root),
        ]
    }
}

impl RuntimePaths {
    /// Room-local paths only; account-shared paths are outside the class model.
    /// Keyed file families are represented by their containing directory.
    fn all_paths(&self) -> Vec<PathBuf> {
        vec![
            self.sock_dir.clone(),
            self.live_dir.clone(),
            self.lanes_dir.clone(),
            self.locks_dir.clone(),
            self.lock_path("workspace.lock"),
            self.lock_path("publish.lock"),
            self.lock_path("subagent-zone.lock"),
            self.lock_path("loop-instances.lock"),
            self.lock_path("message-sweep.lock"),
            self.lock_path("sidebar-launch.lock"),
            self.lock_path("snapshot.lock"),
            self.lock_path("pr-state.lock"),
            self.lock_path("diff-stats.lock"),
            self.lock_path("budget.fleet.lock"),
            self.lock_path("pane-write"),
            self.heartbeat_dir.clone(),
            self.read_marks_dir.clone(),
            self.agent_context_dir.clone(),
            self.subagent_context_dir.clone(),
            self.agent_telemetry_dir.clone(),
            self.agent_activity_dir.clone(),
            self.active_time_dir.clone(),
            self.prompt_dir(),
            self.copilot_otel_path(),
            self.sidebar_width_path(),
            self.sidebar_filter_path(),
            self.unread_path(),
            self.pane_frame_path(),
            self.agent_projection_path(),
            self.topology_writer_lock(),
            self.authoritative_pane_probe_path(),
            self.authoritative_pane_probe_lock(),
            self.diff_stats_path(),
            self.cohort_spend_path(),
            self.pipeline_path(),
            self.pr_state_path(),
            self.trunk_state_path(),
            self.focus_anchor_path(),
            self.focus_anchor_lock(),
            self.codex_app_server_socket_path(),
        ]
    }
}

#[test]
fn spending_service_names_differ_only_by_extension() {
    let runtime = RuntimePaths::under(
        WorkspaceId::from_project_root(Path::new("/tmp/project-a")),
        Path::new("/tmp/rimz-runtime-test"),
    )
    .unwrap();
    assert_eq!(
        runtime.shared_spending_service_socket_path(1, 18, 10, 7, "0123456789abcdef01234567"),
        runtime
            .shared_root
            .join("spending.v1.c18.p10.w7.n0123456789abcdef01234567.sock")
    );
    assert_eq!(
        runtime.shared_spending_service_owner_lock(1, 18, 10, 7, "0123456789abcdef01234567"),
        runtime
            .shared_root
            .join("spending.v1.c18.p10.w7.n0123456789abcdef01234567.lock")
    );
}

#[test]
fn room_tree_removal_tolerates_absence_and_names_failures() {
    let dir = tempfile::tempdir().unwrap();
    let id = WorkspaceId::from_project_root(dir.path());
    let state = StatePaths::under(id.clone(), dir.path()).unwrap();
    let runtime = RuntimePaths::under(id, dir.path()).unwrap();
    state.remove_root().unwrap();
    runtime.remove_root().unwrap();

    fs::create_dir_all(state.temp_unit_dir(None)).unwrap();
    fs::create_dir_all(&runtime.live_dir).unwrap();
    state.remove_root().unwrap();
    assert!(!state.root.exists());
    runtime.remove_root().unwrap();
    assert!(!runtime.root.exists());

    fs::create_dir_all(state.root.parent().unwrap()).unwrap();
    fs::write(&state.root, b"blocker").unwrap();
    assert!(matches!(state.remove_root(), Err(PathErr::Io { path, .. }) if path == state.root));
}

#[test]
fn path_io_errors_name_the_path_that_failed() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("blocker");
    fs::write(&blocker, b"blocker").unwrap();
    let runtime = blocker.join("runtime");
    assert!(
        matches!(ensure_private_runtime_dir(&runtime), Err(PathErr::Io { path, .. }) if path == runtime)
    );
    let id = WorkspaceId::from_project_root(dir.path());
    let state = StatePaths::under_named(id.clone(), WorkspaceDirName::fallback(&id), &blocker);
    assert!(
        matches!(state.ensure_dirs(), Err(PathErr::Io { path, .. }) if path == state.snapshots_dir)
    );
}

#[test]
fn handleless_skills_are_outside_writable_tmp() {
    let dir = tempfile::tempdir().unwrap();
    let paths = StatePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    assert!(!paths.agent_skills_dir(None).starts_with(&paths.tmp_dir));
    assert!(paths.agent_skills_dir(None).starts_with(&paths.cache_dir));
}
#[test]
fn runtime_cleanup_does_not_race_canonical_path_writers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime = dir.path().join("runtime");
    fs::create_dir(&runtime).expect("runtime directory");
    fs::write(runtime.join("old-hint"), b"old").expect("old runtime hint");

    let removed = remove_runtime_dir_with(&runtime, |detached| {
        // Force the late writer into the recursive-cleanup window.
        fs::create_dir_all(&runtime).expect("late writer recreates runtime");
        fs::write(runtime.join("late-hint"), b"late").expect("late runtime hint");
        assert!(detached.join("old-hint").exists());
        fs::remove_dir_all(detached).unwrap();
        assert!(!detached.exists());
        Ok(true)
    })
    .expect("runtime cleanup succeeds despite late writer");

    assert!(removed);
    assert!(!runtime.join("old-hint").exists());
    assert_eq!(fs::read(runtime.join("late-hint")).unwrap(), b"late");
}

#[test]
fn runtime_cleanup_accepts_an_absent_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(
        !remove_runtime_dir_with(&dir.path().join("absent"), |_| {
            panic!("an absent runtime must not need recursive cleanup")
        })
        .expect("absent runtime is already clean")
    );
}

#[test]
fn state_cleanup_detaches_before_inspecting_and_removing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = dir.path().join("cache");
    fs::create_dir(&cache).expect("state directory");
    fs::write(cache.join("old"), b"old").expect("old entry");

    let inspected = remove_state_dir_with(&cache, |detached| {
        assert_eq!(detached, dir.path().join("cache.reset"));
        // Force the late writer into the window between detach and removal.
        fs::create_dir_all(&cache).expect("late writer recreates the directory");
        fs::write(cache.join("late"), b"late").expect("late entry");
        assert!(detached.join("old").exists());
        Ok::<_, PathErr>(7)
    })
    .expect("state cleanup succeeds despite late writer");

    assert_eq!(inspected, Some(7));
    assert!(!cache.join("old").exists());
    assert!(!dir.path().join("cache.reset").exists());
    assert_eq!(fs::read(cache.join("late")).unwrap(), b"late");
}

#[test]
fn state_cleanup_removes_an_interrupted_resets_leftover_uninspected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = dir.path().join("cache");
    let leftover = dir.path().join("cache.reset");
    fs::create_dir_all(leftover.join("nested")).expect("leftover tree");

    // Leftover alone: nothing to detach, and the leftover still goes.
    let absent = remove_state_dir_with(&cache, |_| -> Result<()> {
        panic!("an absent directory must not be inspected")
    })
    .expect("absent directory is already clean");
    assert_eq!(absent, None);
    assert!(!leftover.exists());

    // Leftover beside a live directory: only the live tree is inspected.
    fs::create_dir_all(leftover.join("nested")).expect("leftover tree");
    fs::create_dir(&cache).expect("state directory");
    fs::write(cache.join("old"), b"old").expect("old entry");
    remove_state_dir_with(&cache, |detached| {
        assert!(detached.join("old").exists());
        assert!(!detached.join("nested").exists());
        Ok::<_, PathErr>(())
    })
    .expect("state cleanup replaces the leftover");
    assert!(!leftover.exists() && !cache.exists());

    // Handed the leftover itself, as a parent listing does: removed, uninspected.
    fs::create_dir_all(leftover.join("nested")).expect("leftover tree");
    let listed = remove_state_dir_with(&leftover, |_| -> Result<()> {
        panic!("a leftover must not be inspected")
    })
    .expect("leftover is removed");
    assert_eq!(listed, None);
    assert!(!leftover.exists());
    assert!(!dir.path().join("cache.reset.reset").exists());
}

#[test]
fn state_cleanup_appends_to_a_dotted_name_and_reports_failures() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dotted = dir.path().join("unit.v2");
    let stem_sibling = dir.path().join("unit.reset");
    fs::create_dir(&dotted).expect("dotted directory");
    fs::create_dir(&stem_sibling).expect("unrelated sibling");
    fs::write(stem_sibling.join("keep"), b"keep").expect("sibling entry");

    remove_state_dir_with(&dotted, |detached| {
        assert_eq!(detached, dir.path().join("unit.v2.reset"));
        Ok::<_, PathErr>(())
    })
    .expect("dotted directory is removed");
    assert!(!dotted.exists());
    assert!(stem_sibling.join("keep").exists());

    fs::create_dir(&dotted).expect("dotted directory");
    let err = remove_state_dir_with(&dotted, |detached| {
        Err::<(), _>(PathErr::io(detached)(io::Error::other("inspect failed")))
    })
    .expect_err("an inspection failure is returned");
    assert!(err.to_string().contains("inspect failed"));

    let file = dir.path().join("file");
    fs::write(&file, b"not a directory").expect("file");
    remove_state_dir_with(&file.join("child"), |_| Ok::<_, PathErr>(()))
        .expect_err("an error other than an absent source is returned");
}

#[test]
fn temp_unit_dirs_lists_canonical_and_reset_detached_units() {
    let dir = tempfile::tempdir().unwrap();
    let paths = StatePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    assert!(paths.temp_unit_dirs().is_empty());

    let live = paths.ensure_temp_unit(Some("otter")).unwrap();
    fs::write(paths.tmp_dir.join("stray-file"), b"not a unit").unwrap();
    let detached = paths.root.join("tmp.reset/fox");
    fs::create_dir_all(&detached).unwrap();
    fs::create_dir_all(paths.root.join("out.reset/fox")).unwrap();

    let mut units = paths.temp_unit_dirs();
    units.sort();
    assert_eq!(units, [live, detached]);
}

#[test]
fn runtime_paths_follow_lifetime_classes() {
    let dir = tempfile::tempdir().unwrap();
    let id = WorkspaceId::from_project_root(dir.path());
    let paths = RuntimePaths::under(id, dir.path()).unwrap();
    assert_eq!(paths.heartbeat_dir, paths.root.join("live/heartbeat"));
    assert_eq!(paths.prompt_dir(), paths.root.join("live/prompt"));
    assert_eq!(
        paths.pane_frame_path(),
        paths.root.join("lanes/snapshot.json")
    );
    assert_eq!(
        paths.topology_writer_lock(),
        paths.locks_dir.join("topology-writer.lock")
    );
    for path in paths.all_paths() {
        if path.starts_with(&paths.locks_dir) {
            assert!(
                path.starts_with(
                    dir.path()
                        .join("ws")
                        .join(paths.dir_name.as_str())
                        .join("locks")
                )
            );
            continue;
        }
        let relative = path.strip_prefix(&paths.root).unwrap();
        assert!(
            Class::RUNTIME
                .iter()
                .any(|class| relative.starts_with(class.dir_name())),
            "unclassified runtime path: {}",
            path.display()
        );
    }
}

#[test]
fn state_locks_do_not_depend_on_runtime_root() {
    let dir = tempfile::tempdir().unwrap();
    let id = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(id.clone(), &dir.path().join("runtime")).unwrap();
    let paths = StatePaths::under_named(id, runtime.dir_name.clone(), &dir.path().join("state"));
    assert_eq!(
        paths.workspace_lock,
        paths.root.join("locks/workspace.lock")
    );
    assert_eq!(paths.publish_lock, paths.root.join("locks/publish.lock"));
    let paired = RuntimePaths::for_state(&paths).unwrap();
    assert_eq!(paired.lock_path("workspace.lock"), paths.workspace_lock);
    let other = RuntimePaths::for_state_under(&paths, &dir.path().join("other-runtime"));
    assert_ne!(paired.root, other.root);
    assert_eq!(paired.locks_dir, other.locks_dir);
    let store = crate::Store::open(paths.clone(), runtime).unwrap();
    assert_eq!(store.runtime_paths().locks_dir, paired.locks_dir);
    for path in paired.all_paths() {
        if path.extension().is_some_and(|ext| ext == "lock") || path == paired.locks_dir {
            assert!(
                path.starts_with(paths.root.join("locks")),
                "{}",
                path.display()
            );
        }
    }
    assert!(
        paths
            .fleet_budget_record
            .starts_with(dir.path().join("state"))
    );
}

#[test]
fn agents_home_environment_precedence() {
    let mut env = BTreeMap::new();
    assert_eq!(agents_home_in(&env), None);
    assert_eq!(skills_library_in(&env), None);
    env.insert("HOME".to_owned(), "/home/user".to_owned());
    assert_eq!(
        agents_home_in(&env),
        Some(PathBuf::from("/home/user/.rimz"))
    );
    env.insert("XDG_CONFIG_HOME".to_owned(), "/config".to_owned());
    assert_eq!(
        agents_home_in(&env),
        Some(PathBuf::from("/home/user/.rimz"))
    );
    env.insert("RIMZ_HOME".to_owned(), "/rimz".to_owned());
    assert_eq!(agents_home_in(&env), Some(PathBuf::from("/rimz")));
    env.insert("RIMZ_AGENTS_HOME".to_owned(), "/library".to_owned());
    assert_eq!(agents_home_in(&env), Some(PathBuf::from("/library")));
    assert_eq!(
        skills_library_in(&env),
        Some(PathBuf::from("/library/skills"))
    );
    env.insert("RIMZ_AGENTS_HOME".to_owned(), String::new());
    assert_eq!(agents_home_in(&env), Some(PathBuf::from("/rimz")));
    env.insert("RIMZ_HOME".to_owned(), String::new());
    env.insert("HOME".to_owned(), String::new());
    assert_eq!(agents_home_in(&env), None);
}

#[test]
fn rimz_home_prefers_the_override_then_home() {
    let tmp = Path::new("/tmp");
    let home = Some(Path::new("/home/user"));
    assert_eq!(
        rimz_home_from(Some(Path::new("/elsewhere")), home, tmp),
        Path::new("/elsewhere")
    );
    assert_eq!(
        rimz_home_from(None, home, tmp),
        Path::new("/home/user/.rimz")
    );
    assert_eq!(rimz_home_from(None, None, tmp), Path::new("/tmp/rimz-home"));
}

#[test]
fn runtime_domain_env_stamps_the_resolved_home() {
    let env = runtime_domain_env();
    assert_eq!(env.get("RIMZ_HOME").map(PathBuf::from), Some(rimz_home()));
    for key in [
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "XDG_STATE_HOME",
        "XDG_RUNTIME_DIR",
    ] {
        assert!(env.contains_key(key), "{key} stamped");
    }
}

fn write_record(home: &Path, name: &str, id: &WorkspaceId) {
    let dir = workspaces_dir_under(home).join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("workspace.json"),
        format!(r#"{{"layout":2,"workspace_id":"{id}","project_root":"/x"}}"#),
    )
    .unwrap();
}

#[test]
fn old_layout_resolves_without_mutating_it() {
    let home = tempfile::tempdir().unwrap();
    let project = home.path().join("project");
    let paths = StatePaths::for_project_root_under(&project, home.path()).unwrap();
    fs::create_dir_all(&paths.root).unwrap();
    let bytes = format!(r#"{{"workspace_id":"{}"}}"#, paths.workspace_id);
    fs::write(&paths.workspace_record, &bytes).unwrap();
    let resolved = StatePaths::for_project_root_under(&project, home.path());
    assert!(resolved.is_ok(), "{resolved:?}");
    assert_eq!(resolved.unwrap().root, paths.root);
    assert_eq!(fs::read_to_string(&paths.workspace_record).unwrap(), bytes);
    assert!(!paths.cache_dir.exists());
}

/// Two project roots under `parent` whose ids share their first four hex.
fn colliding_roots(parent: &Path) -> (PathBuf, PathBuf) {
    let mut seen = std::collections::HashMap::new();
    for n in 0.. {
        let root = parent.join(format!("p{n}")).join("repo");
        let id = WorkspaceId::from_project_root(&root);
        if let Some(first) = seen.insert(id.hex()[..4].to_owned(), root.clone()) {
            return (first, root);
        }
    }
    unreachable!("the pigeonhole bounds the search at 65 537 roots")
}

#[test]
fn new_slugs_fit_both_socket_budgets_and_existing_long_slugs_are_adopted() {
    let home = tempfile::tempdir().unwrap();
    let root = Path::new("/src/abcdefghijklmnopqrstuvwxyz1234");
    let id = WorkspaceId::from_project_root(root);
    let paths = StatePaths::for_project_root_under(root, home.path()).unwrap();
    assert_eq!(
        paths.dir_name.as_str(),
        format!("abcdefghijklmnopqrst-{}", &id.hex()[..4])
    );
    let old_name = WorkspaceDirName::mint("abcdefghijklmnopqrstuvwxyz123456", &id, 4);
    write_record(home.path(), old_name.as_str(), &id);
    assert_eq!(
        StatePaths::for_project_root_under(root, home.path())
            .unwrap()
            .dir_name,
        old_name
    );
}

#[test]
fn project_root_mints_a_basename_name_and_finds_it_again() {
    let home = tempfile::tempdir().unwrap();
    let root = Path::new("/src/My Repo");
    let id = WorkspaceId::from_project_root(root);

    let minted = StatePaths::for_project_root_under(root, home.path()).unwrap();
    assert_eq!(
        minted.dir_name.as_str(),
        format!("My-Repo-{}", &id.hex()[..4])
    );
    assert_eq!(
        minted.root,
        home.path().join("ws").join(minted.dir_name.as_str())
    );
    assert!(!minted.root.exists(), "constructors create nothing");

    minted.ensure_dirs().unwrap();
    assert_eq!(
        StatePaths::for_project_root_under(root, home.path())
            .unwrap()
            .root,
        minted.root
    );
    assert_eq!(
        StatePaths::under(id.clone(), home.path()).unwrap().dir_name,
        WorkspaceDirName::fallback(&id)
    );
    write_record(home.path(), minted.dir_name.as_str(), &id);
    let by_id = StatePaths::under(id, home.path()).unwrap();
    assert_eq!(by_id.root, minted.root);
}

#[test]
fn mint_lengthens_past_a_taken_prefix_and_records_decide_lookup() {
    let home = tempfile::tempdir().unwrap();
    let (first_root, second_root) = colliding_roots(Path::new("/src"));
    let first = WorkspaceId::from_project_root(&first_root);
    let second = WorkspaceId::from_project_root(&second_root);
    let first_name = StatePaths::for_project_root_under(&first_root, home.path())
        .unwrap()
        .dir_name;
    write_record(home.path(), first_name.as_str(), &first);

    let second_paths = StatePaths::for_project_root_under(&second_root, home.path()).unwrap();
    assert_eq!(second_paths.dir_name.hex().len(), 6);
    assert_ne!(
        second_paths.root,
        workspaces_dir_under(home.path()).join(first_name.as_str())
    );

    // The first dir's hex prefixes the second id, and before the second is
    // born an id-only lookup must not adopt the first workspace's store.
    assert_eq!(
        StatePaths::under(second.clone(), home.path())
            .unwrap()
            .dir_name,
        WorkspaceDirName::fallback(&second)
    );
    write_record(home.path(), second_paths.dir_name.as_str(), &second);
    assert_eq!(
        StatePaths::under(second, home.path()).unwrap().dir_name,
        second_paths.dir_name
    );
    assert_eq!(
        StatePaths::under(first, home.path()).unwrap().dir_name,
        first_name
    );

    // An unrecorded dir under another basename (an unroomed project's loop
    // instances) is not adopted by a root whose id shares its hex.
    let other = tempfile::tempdir().unwrap();
    let neighbour = format!("elsewhere-{}", first_name.hex());
    fs::create_dir_all(workspaces_dir_under(other.path()).join(&neighbour)).unwrap();
    let minted = StatePaths::for_project_root_under(&second_root, other.path()).unwrap();
    assert_ne!(minted.dir_name.as_str(), neighbour);
    assert!(minted.dir_name.as_str().starts_with("repo-"));
}

#[test]
fn unrecorded_candidates_resolve_alone_and_refuse_together() {
    let home = tempfile::tempdir().unwrap();
    let root = Path::new("/src/repo");
    let id = WorkspaceId::from_project_root(root);
    let one = WorkspaceDirName::mint("repo", &id, 4);
    let two = WorkspaceDirName::mint("repo", &id, 6);
    let ws = workspaces_dir_under(home.path());
    fs::create_dir_all(ws.join(one.as_str())).unwrap();
    assert_eq!(
        StatePaths::for_project_root_under(root, home.path())
            .unwrap()
            .dir_name,
        one
    );

    fs::create_dir_all(ws.join(two.as_str())).unwrap();
    match StatePaths::for_project_root_under(root, home.path()) {
        Err(PathErr::AmbiguousWorkspaceDir { candidates, .. }) => {
            assert_eq!(candidates.len(), 2)
        }
        other => panic!("expected AmbiguousWorkspaceDir, got {other:?}"),
    }

    write_record(home.path(), two.as_str(), &id);
    assert_eq!(
        StatePaths::for_project_root_under(root, home.path())
            .unwrap()
            .dir_name,
        two
    );
    let foreign = WorkspaceDirName::mint("elsewhere", &id, 6);
    fs::rename(ws.join(two.as_str()), ws.join(foreign.as_str())).unwrap();
    assert_eq!(
        StatePaths::under(id, home.path()).unwrap().dir_name,
        foreign
    );
}

#[test]
fn id_only_lookup_ignores_foreign_unrecorded_directories() {
    let home = tempfile::tempdir().unwrap();
    let id = WorkspaceId::parse("ws_abcdef0123456789abcdef01").unwrap();
    let ws = workspaces_dir_under(home.path());
    for name in ["other-abcd", "another-abcdef"] {
        fs::create_dir_all(ws.join(name)).unwrap();
        let state = StatePaths::under(id.clone(), home.path()).unwrap();
        assert_eq!(state.dir_name, WorkspaceDirName::fallback(&id));
        let runtime = RuntimePaths::under(id.clone(), home.path()).unwrap();
        assert_eq!(runtime.dir_name, state.dir_name);
        assert!(!state.root.exists());
        assert!(!runtime.root.exists());
    }
    for name in ["other-abcd", "another-abcdef"] {
        assert_eq!(fs::read_dir(ws.join(name)).unwrap().count(), 0);
    }
}

#[test]
fn runtime_under_follows_recorded_state_without_a_runtime_tree() {
    let home = tempfile::tempdir().unwrap();
    let state = StatePaths::for_project_root_under(Path::new("/src/repo"), home.path()).unwrap();
    write_record(home.path(), state.dir_name.as_str(), &state.workspace_id);
    assert!(!home.path().join("rimz").exists());
    let runtime = RuntimePaths::under(state.workspace_id.clone(), home.path()).unwrap();
    assert_eq!(runtime.dir_name, state.dir_name);
    assert_eq!(runtime.lock_path("workspace.lock"), state.workspace_lock);
    assert!(!runtime.root.exists());
}

#[test]
fn id_only_paths_fall_back_to_the_full_hex_in_both_trees() {
    let dir = tempfile::tempdir().unwrap();
    let id = WorkspaceId::parse("ws_abcdef0123456789abcdef01").unwrap();
    let state = StatePaths::under(id.clone(), dir.path()).unwrap();
    let runtime = RuntimePaths::under(id, dir.path()).unwrap();
    assert_eq!(state.dir_name.as_str(), "ws-abcdef0123456789abcdef01");
    assert_eq!(runtime.dir_name, state.dir_name);
    assert_eq!(state.workspace_lock, runtime.lock_path("workspace.lock"));
    assert_eq!(state.publish_lock, runtime.lock_path("publish.lock"));
    assert_eq!(
        runtime.root,
        dir.path().join("rimz/ws/ws-abcdef0123456789abcdef01")
    );
}

#[test]
fn disposable_cleanup_preserves_held_lock_inodes() {
    use crate::disk::lock::WorkspaceLock;

    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let lock_path = runtime.lock_path("loop-watch-test.lock");
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let thread_path = lock_path.clone();
    let holder = std::thread::spawn(move || {
        let _guard = WorkspaceLock::acquire(&thread_path).unwrap();
        held_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    held_rx.recv().unwrap();
    assert!(runtime.remove_disposable_dirs().unwrap());
    assert!(lock_path.exists());
    assert!(WorkspaceLock::try_acquire(&lock_path).unwrap().is_none());
    for path in [&runtime.sock_dir, &runtime.live_dir, &runtime.lanes_dir] {
        assert!(!path.exists());
    }
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    assert!(WorkspaceLock::try_acquire(&lock_path).unwrap().is_some());
}

#[test]
fn workspace_spending_file_names() {
    for name in ["workspace-spending.abc.json", "workspace-spending..json"] {
        assert!(is_workspace_spending_file(name));
    }
    for name in [
        "workspace-spending.json",
        "budget.json",
        "workspace-spending.abc.json.tmp",
    ] {
        assert!(!is_workspace_spending_file(name));
    }
}

fn short_tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("r")
        .tempdir_in("/tmp")
        .expect("short tempdir")
}

#[test]
fn unit_tests_resolve_implicit_home_to_an_uncreated_temp_path() {
    if env_path("RIMZ_HOME").is_some() {
        return;
    }

    let resolved = rimz_home();
    if let Some(home) = env_path("HOME") {
        assert_ne!(
            resolved,
            home.join(".rimz"),
            "unit test escaped to real home"
        );
    }
    assert!(
        resolved.starts_with(env::temp_dir()),
        "unit-test home must be a temp root"
    );
    assert!(
        !resolved.exists(),
        "resolving the unit-test home must not create residue"
    );
}

#[test]
fn state_paths_resolve_under_the_home() {
    let id = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let paths = StatePaths::for_workspace(id.clone()).unwrap();
    assert_eq!(
        paths.root,
        rimz_home()
            .join("ws")
            .join(WorkspaceDirName::fallback(&id).as_str())
    );
    assert_eq!(paths.events_log.file_name().unwrap(), "events.log.jsonl");
    assert_eq!(paths.latest_snapshot.file_name().unwrap(), "latest.json");
    assert_eq!(paths.rollup_cache.file_name().unwrap(), "rollup.json");
    assert!(paths.rollup_cache.starts_with(&paths.snapshots_dir));
    assert_eq!(paths.runs_dir.file_name().unwrap(), "runs");
    assert_eq!(paths.room_shared_dir, paths.root.join("shared"));
    assert_eq!(paths.out_dir, paths.root.join("out"));
    assert_eq!(
        paths.temp_unit_dir(Some("otter")),
        paths.tmp_dir.join("otter")
    );
    assert_eq!(paths.temp_unit_dir(None), paths.tmp_dir.join("_unnamed"));
    assert_eq!(
        paths.out_reader_dir(Some("otter")),
        paths.out_dir.join("otter")
    );
    assert_eq!(paths.out_reader_dir(None), paths.out_dir.join("_unnamed"));
    assert_eq!(paths.agents_dir, paths.root.join("owned/agents"));
    assert_eq!(paths.transcript_dir.file_name().unwrap(), "transcript");
    assert_eq!(
        paths.workspace_record.file_name().unwrap(),
        "workspace.json"
    );
    assert_eq!(paths.room_bin.file_name().unwrap(), "rimz");
    assert_eq!(
        paths.live_roster,
        paths.root.join("records/live-roster.json")
    );
    assert_eq!(
        paths.pending_recovery,
        paths.root.join("records/pending-recovery.json")
    );
    assert_eq!(
        paths.idle_stop_requests,
        paths.root.join("records/idle-stop.json")
    );
    assert_eq!(paths.workspace_lock.file_name().unwrap(), "workspace.lock");
    assert_eq!(paths.events_log, paths.root.join("log/events.log.jsonl"));
    assert_eq!(paths.messages_dir, paths.root.join("records/messages"));
    assert_eq!(paths.transcript_dir, paths.root.join("audit/transcript"));
    assert_eq!(paths.runs_dir, paths.root.join("owned/runs"));
    assert_eq!(
        paths.agent_skills_dir(Some("otter")),
        paths.root.join("owned/agents/otter/skills")
    );
    assert_eq!(
        paths.agent_skills_dir(None),
        paths.root.join("cache/skills")
    );
    for path in paths.all_paths() {
        let relative = path.strip_prefix(&paths.root).unwrap();
        assert!(
            relative == Path::new("workspace.json")
                || relative == Path::new("rimz")
                || Class::STATE
                    .iter()
                    .any(|class| relative.starts_with(class.dir_name())),
            "unclassified state path: {}",
            path.display()
        );
    }
}

#[test]
fn runtime_paths_share_user_scoped_cache_files() {
    let root = Path::new("/tmp/rimz-runtime-test");
    let first = WorkspaceId::from_project_root(Path::new("/tmp/project-a"));
    let second = WorkspaceId::from_project_root(Path::new("/tmp/project-b"));
    let first_paths = RuntimePaths::under(first, root).unwrap();
    let second_paths = RuntimePaths::under(second, root).unwrap();

    assert_ne!(first_paths.root, second_paths.root);
    assert_eq!(
        first_paths.copilot_otel_path(),
        first_paths
            .root
            .join("live")
            .join("agent-telemetry")
            .join("copilot-otel.jsonl")
    );
    assert_eq!(first_paths.shared_root, second_paths.shared_root);
    assert_eq!(
        first_paths.shared_accounts_path(),
        second_paths.shared_accounts_path()
    );
    assert_eq!(
        first_paths.shared_rate_limits_path(),
        second_paths.shared_rate_limits_path()
    );
    assert_eq!(
        first_paths.shared_provider_spending_path(),
        second_paths.shared_provider_spending_path()
    );
    assert_eq!(
        first_paths.shared_spending_cursor_path(),
        second_paths.shared_spending_cursor_path()
    );
    assert_eq!(
        first_paths.shared_pricing_cache_path(),
        second_paths.shared_pricing_cache_path()
    );
}

#[test]
fn spending_service_paths_are_user_shared_versioned_and_socket_safe() {
    let root = Path::new("/tmp/rimz-service-path-test");
    let first = RuntimePaths::under(
        WorkspaceId::from_project_root(Path::new("/tmp/project-a")),
        root,
    )
    .unwrap();
    let second = RuntimePaths::under(
        WorkspaceId::from_project_root(Path::new("/tmp/project-b")),
        root,
    )
    .unwrap();

    let socket =
        first.shared_spending_service_socket_path(1, 18, 10, 7, "0123456789abcdef01234567");
    assert_eq!(
        socket,
        second.shared_spending_service_socket_path(1, 18, 10, 7, "0123456789abcdef01234567")
    );
    assert_eq!(
        first.shared_spending_service_owner_lock(1, 18, 10, 7, "0123456789abcdef01234567"),
        second.shared_spending_service_owner_lock(1, 18, 10, 7, "0123456789abcdef01234567")
    );
    assert_ne!(
        socket,
        first.shared_spending_service_socket_path(2, 18, 10, 7, "0123456789abcdef01234567")
    );
    assert_ne!(
        socket,
        first.shared_spending_service_socket_path(1, 19, 10, 7, "0123456789abcdef01234567")
    );
    assert_ne!(
        socket,
        first.shared_spending_service_socket_path(1, 18, 11, 7, "0123456789abcdef01234567")
    );
    assert_ne!(
        socket,
        first.shared_spending_service_socket_path(1, 18, 10, 8, "0123456789abcdef01234567")
    );
    assert_ne!(
        socket,
        first.shared_spending_service_socket_path(1, 18, 10, 7, "fedcba9876543210fedcba98")
    );
    crate::sock::validate_socket_path(&socket).unwrap();
}

#[test]
fn shared_directory_preparation_leaves_workspace_tree_absent() {
    let temp = short_tempdir();
    let paths = RuntimePaths::under(
        WorkspaceId::parse("ws_000000000000000000000000").unwrap(),
        temp.path(),
    )
    .unwrap();

    paths.ensure_shared_dirs().unwrap();

    assert!(paths.shared_root.is_dir());
    assert!(paths.persistent_shared_root.is_dir());
    assert!(!paths.root.exists());
}

#[test]
fn production_runtime_paths_persist_shared_data_and_keep_locks_runtime() {
    let temp = short_tempdir();
    let state_root = temp.path().join("home");
    let runtime_root = temp.path().join("runtime");
    let persistent_shared_root = state_root.join("cache").join("providers");
    let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/x"));

    let mut paths = RuntimePaths::under(workspace_id, &runtime_root).unwrap();
    paths.persistent_shared_root = persistent_shared_root.clone();

    assert_eq!(paths.shared_root, runtime_root.join("rimz").join("shared"));
    assert_eq!(paths.persistent_shared_root, persistent_shared_root);
    let board_lock = paths.board_lock(Path::new("/tmp/team"));
    assert!(board_lock.starts_with(paths.shared_root.join("board-write")));
    assert_ne!(board_lock, paths.board_lock(Path::new("/tmp/other-team")));
    assert_eq!(
        paths.shared_provider_spending_path(),
        persistent_shared_root.join("provider-spending.json")
    );
    assert_eq!(
        paths.shared_accounts_path(),
        persistent_shared_root.join("accounts.json")
    );
    assert_eq!(
        paths.shared_spending_cursor_path(),
        persistent_shared_root.join("spending.json")
    );
    assert_eq!(
        paths.shared_spending_lock(),
        runtime_root
            .join("rimz")
            .join("shared")
            .join("spending.lock")
    );
    assert_eq!(
        paths.shared_auto_redeem_path(&crate::ids::LoginKey::default_for(
            crate::ids::AgentKind::new_unchecked("codex")
        )),
        persistent_shared_root.join("auto_redeem.codex@default.json")
    );
    assert_eq!(
        paths.shared_auto_redeem_rate_path(&crate::ids::LoginKey::default_for(
            crate::ids::AgentKind::new_unchecked("codex")
        )),
        persistent_shared_root.join("auto_redeem_rate.codex@default.json")
    );
    assert_eq!(
        paths.shared_auto_redeem_lock(&crate::ids::LoginKey::default_for(
            crate::ids::AgentKind::new_unchecked("codex")
        )),
        runtime_root
            .join("rimz")
            .join("shared")
            .join("auto_redeem.codex@default.lock")
    );

    paths.ensure_dirs().unwrap();

    assert!(paths.persistent_shared_root.is_dir());
    assert!(paths.shared_root.is_dir());
    assert!(paths.agent_telemetry_dir.is_dir());
}

#[test]
fn ensure_dirs_keeps_shared_cache_when_roots_match() {
    let temp = short_tempdir();
    let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let paths = RuntimePaths::under(workspace_id, temp.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let cache = paths.shared_root.join("spending.json");
    fs::write(&cache, b"live").unwrap();

    paths.ensure_dirs().unwrap();

    assert!(cache.exists());
}

#[test]
fn runtime_fallback_uses_short_tmp_root() {
    let fallback = runtime_fallback_home();
    let expected = format!("rimz-{}", current_uid());
    assert_eq!(fallback.parent(), Some(Path::new("/tmp")));
    assert_eq!(
        fallback.file_name().and_then(|name| name.to_str()),
        Some(expected.as_str())
    );
}

#[test]
fn validated_under_fails_fast_with_the_xdg_remedy() {
    let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let deep_root = Path::new("/tmp").join("d".repeat(crate::sock::AF_UNIX_PATH_LIMIT));

    let dir_name = WorkspaceDirName::fallback(&workspace_id);
    let err =
        RuntimePaths::validated(workspace_id, dir_name, &deep_root).expect_err("overlong root");
    let rendered = err.to_string();

    match err {
        PathErr::SocketBudgetExceeded(source) => {
            assert!(source.path.starts_with(&deep_root));
            assert!(source.used > source.limit);
            assert!(rendered.contains(crate::sock::XDG_REMEDY));
        }
        other => panic!("expected SocketBudgetExceeded, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn ensure_dirs_hardens_runtime_root_before_workspace_children() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let runtime_root = dir.path().join("runtime");
    let rimz_root = runtime_root.join("rimz");
    fs::create_dir_all(&rimz_root).unwrap();
    for path in [&runtime_root, &rimz_root] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o777)).unwrap();
    }
    let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let runtime = RuntimePaths::under(workspace_id, &runtime_root).unwrap();

    runtime.ensure_dirs().unwrap();

    for path in [
        runtime_root.as_path(),
        rimz_root.as_path(),
        rimz_root.join("ws").as_path(),
        runtime.root.as_path(),
        runtime.shared_root.as_path(),
    ] {
        let mode = fs::metadata(path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{} is private", path.display());
    }
}

#[cfg(unix)]
#[test]
fn ensure_dirs_rejects_symlinked_runtime_root() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let real_root = dir.path().join("real");
    fs::create_dir(&real_root).unwrap();
    let runtime_root = dir.path().join("runtime");
    symlink(&real_root, &runtime_root).unwrap();
    let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let runtime = RuntimePaths::under(workspace_id, &runtime_root).unwrap();

    let err = runtime.ensure_dirs().expect_err("symlinked root");

    match err {
        PathErr::RuntimeDirSymlink { path } => assert_eq!(path, runtime_root),
        other => panic!("expected RuntimeDirSymlink, got {other:?}"),
    }
}

/// The fix sentence is the only advice a reader gets for a legacy root, so it
/// has to match what the root actually holds: config, state, and data carry
/// files RimZ still wants, and the old cache carries nothing worth moving.
#[test]
fn legacy_roots_fix_moves_config_roots_and_deletes_the_old_cache() {
    let home = Path::new("/home/u/.rimz");
    let cache = cache_home().join("rimz");
    let config = config_home().join("rimz");

    let config_only = legacy_roots_fix(std::slice::from_ref(&config), home);
    assert!(
        config_only.contains("move their contents into /home/u/.rimz"),
        "{config_only}"
    );
    assert!(config_only.contains("or set RIMZ_HOME"), "{config_only}");
    assert!(!config_only.contains("delete"), "{config_only}");

    let mixed = legacy_roots_fix(&[config.clone(), cache.clone()], home);
    assert!(
        mixed.contains(&format!("move the contents of {}", config.display())),
        "{mixed}"
    );
    assert!(
        mixed.contains(&format!("delete the obsolete cache at {}", cache.display())),
        "{mixed}"
    );

    let cache_only = legacy_roots_fix(std::slice::from_ref(&cache), home);
    assert!(
        cache_only.contains(&format!("delete the obsolete cache at {}", cache.display())),
        "{cache_only}"
    );
    assert!(!cache_only.contains("move"), "{cache_only}");
    // Relocating the home does not make a stale cache read again, so offering
    // RIMZ_HOME as an alternative would be advice that does not work.
    assert!(!cache_only.contains("RIMZ_HOME"), "{cache_only}");

    for sentence in [&config_only, &mixed, &cache_only] {
        assert!(
            sentence.contains("moving-from-the-xdg-roots"),
            "every form keeps the migration link: {sentence}"
        );
    }
}

/// Doctor reports what is on disk; the room guard asks the narrower question of
/// whether this host's config could be lost, and a cache holds no config.
#[test]
fn legacy_config_roots_excludes_the_cache_doctor_reports() {
    let dir = tempfile::tempdir().unwrap();
    let seeded = dir.path().join("rimz");
    fs::create_dir_all(&seeded).unwrap();

    assert_eq!(
        existing_legacy_roots([dir.path().to_path_buf()]),
        vec![seeded]
    );
    assert!(existing_legacy_roots([dir.path().join("absent")]).is_empty());
}

#[test]
fn store_catalog_documents_every_lifetime_class() {
    let catalog = include_str!("../../../../../docs/internals/store.md");
    for class in Class::STATE.into_iter().chain(Class::RUNTIME) {
        assert!(
            catalog.contains(&format!("| `{}/` |", class.dir_name())),
            "missing catalog rule for {class:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn ensure_temp_unit_creates_the_unit_and_room_shared_dir_private() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o775)).unwrap();
    let paths = StatePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();

    let unit = paths.ensure_temp_unit(Some("otter")).unwrap();

    for path in [&unit, &paths.room_shared_dir] {
        let mode = fs::metadata(path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{} is private", path.display());
    }
}

#[test]
fn sidebar_host_paths_are_per_session_and_fit_the_socket_budget() {
    let dir = tempfile::tempdir().unwrap();
    let id = WorkspaceId::from_project_root(Path::new("/src/host-paths"));
    let runtime = RuntimePaths::under(id.clone(), dir.path()).unwrap();
    let session = "a session name far longer than any socket file name may be";
    let socket = runtime.sidebar_host_socket_path(MuxName::Tmux, session);

    assert_eq!(socket.parent(), Some(runtime.sock_dir.as_path()));
    let instance = runtime.sidebar_socket_path(&SidebarInstanceId::new());
    assert!(
        socket.file_name().unwrap().len() <= instance.file_name().unwrap().len(),
        "the host socket stays inside the budget the pane sockets are checked against"
    );
    assert_ne!(
        socket,
        runtime.sidebar_host_socket_path(MuxName::Zellij, session)
    );
    assert_ne!(
        socket,
        runtime.sidebar_host_socket_path(MuxName::Tmux, "another session")
    );
    assert_ne!(
        runtime.sidebar_host_lock(MuxName::Tmux, session),
        runtime.sidebar_host_spawn_lock(MuxName::Tmux, session)
    );
}
