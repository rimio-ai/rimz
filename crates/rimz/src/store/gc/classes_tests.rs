use super::*;
use std::fs;
use std::time::{Duration, SystemTime};

#[path = "classes_notes_tests.rs"]
mod notes;

#[test]
fn never_swept_tracks_state_classes_and_team_notes_retention() {
    let classes = Class::STATE.into_iter().chain(Class::RUNTIME);
    let actual: Vec<_> = classes
        .map(|class| {
            (
                class,
                never_swept(class, HistoryRetention::Forever),
                never_swept(
                    class,
                    HistoryRetention::Duration(Duration::from_secs(86_400)),
                ),
            )
        })
        .collect();
    assert_eq!(
        actual,
        vec![
            (Class::Log, false, false),
            (Class::Records, false, false),
            (Class::Audit, false, false),
            (Class::Cache, true, true),
            (Class::Owned, false, false),
            (Class::Tmp, false, false),
            (Class::Shared, true, true),
            (Class::TeamNotes, true, false),
            (Class::AutomationNotes, true, false),
            (Class::Out, false, false),
            (Class::Locks, false, false),
            (Class::Sock, false, false),
            (Class::Live, false, false),
            (Class::Lanes, false, false),
            (Class::Snapshots, false, false),
        ]
    );
}

#[test]
fn never_swept_classes_keep_stale_entries_under_default_sweep() {
    let temp = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::under(
        crate::WorkspaceId::from_project_root(temp.path()),
        temp.path(),
    )
    .unwrap();
    paths.ensure_dirs().unwrap();
    let classes: Vec<_> = Class::STATE
        .into_iter()
        .filter(|class| never_swept(*class, HistoryRetention::Forever))
        .collect();
    assert_eq!(
        classes,
        [
            Class::Cache,
            Class::Shared,
            Class::TeamNotes,
            Class::AutomationNotes
        ]
    );
    let old = SystemTime::now() - Duration::from_secs(365 * 86_400);
    let files: Vec<_> = classes
        .into_iter()
        .map(|class| {
            let dir = class.path_under(&paths.root).join("stale-run");
            fs::create_dir_all(&dir).unwrap();
            let file = dir.join("keep");
            fs::write(&file, "keep").unwrap();
            for path in [&file, &dir] {
                fs::File::open(path).unwrap().set_modified(old).unwrap();
            }
            file
        })
        .collect();
    collect_state(
        &paths,
        GcPolicy {
            older_than: retention::DEFAULT_OLDER_THAN,
            tmp_grace: retention::DEFAULT_TMP_GRACE,
            transcript_retention: HistoryRetention::Forever,
            team_notes_retention: HistoryRetention::Forever,
            automation_notes_retention: HistoryRetention::Forever,
            team_notes_open: |_| false,
        },
        false,
        &|_| Ok(false),
        &|_| temp_owner,
    )
    .unwrap();
    let survivors: Vec<_> = files.iter().map(|file| file.exists()).collect();
    assert_eq!(survivors, [true, true, true, true], "{files:?}");
}

#[test]
fn notes_sweep_keeps_open_and_unreadable_runs_in_both_layouts_and_prunes_empty_dates() {
    let temp = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::under(
        crate::WorkspaceId::from_project_root(temp.path()),
        temp.path(),
    )
    .unwrap();
    let old = SystemTime::now() - Duration::from_secs(60);
    let mut runs = Vec::new();
    for layout in ["", "20261107"] {
        for name in ["open", "closed", "broken"] {
            let run = paths.team_notes_dir.join(layout).join(name);
            fs::create_dir_all(&run).unwrap();
            let manifest = run.join("run.json");
            let bytes = if name == "broken" {
                b"not json".as_slice()
            } else {
                br#"{"version":1,"team":"forge","created_at":"2026-11-07T00:00:00Z","checkout":"/repo"}"#
            };
            fs::write(&manifest, bytes).unwrap();
            for path in [&manifest, &run] {
                fs::File::open(path).unwrap().set_modified(old).unwrap();
            }
            runs.push(run);
        }
    }
    let empty = paths.team_notes_dir.join("20261108");
    fs::create_dir(&empty).unwrap();
    collect_team_notes(
        &paths,
        HistoryRetention::Duration(Duration::from_secs(1)),
        &mut Sweep::new(false),
        &mut GcReport::default(),
        &mut newest_entry_modified,
        &|run| run.file_name().unwrap() == "open",
    )
    .unwrap();
    let actual: Vec<_> = runs
        .iter()
        .chain([&empty])
        .map(|run| run.exists())
        .collect();
    assert_eq!(actual, [true, false, true, true, false, true, false]);
}

#[test]
fn team_notes_walk_ignores_disappeared_entries() {
    let temp = tempfile::tempdir().unwrap();
    assert!(
        newest_entry_modified(&temp.path().join("renamed-away.tmp")).is_ok(),
        "a disappearing atomic temp sibling must not abort the sweep"
    );
}

#[test]
fn forever_automation_notes_reports_bytes_without_opening_the_workspace_lock() {
    let home = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
    paths.ensure_dirs().unwrap();
    fs::create_dir_all(&paths.workspace_lock).unwrap();
    fs::create_dir_all(&paths.automation_notes_dir).unwrap();
    fs::write(paths.automation_notes_dir.join("kept.md"), "working memory").unwrap();
    let bytes = collect_automation_notes(
        &paths,
        HistoryRetention::Forever,
        &mut Sweep::new(false),
        &mut GcReport::default(),
    );
    assert!(
        bytes.is_ok_and(|bytes| bytes == "working memory".len() as u64),
        "forever retention must measure notes without the workspace lock"
    );
}

#[test]
fn team_notes_checks_and_removal_hold_the_checkout_board_lock() {
    use crate::{disk::lock::WorkspaceLock, store::notes};

    let home = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
    let relative = notes::allocate(
        &paths,
        notes::Kind::Team {
            team: "forge".into(),
            worktree: None,
        },
        home.path(),
        jiff::Timestamp::now(),
    )
    .unwrap();
    let run = paths.notes_run(Class::TeamNotes, &relative);
    let old = SystemTime::now() - Duration::from_secs(8 * 86_400);
    for path in [&run, &run.join("run.json")] {
        fs::File::open(path).unwrap().set_modified(old).unwrap();
    }
    let runtime = crate::RuntimePaths::for_state(&paths).unwrap();
    let lock = runtime.board_lock(home.path());
    let checks = std::cell::RefCell::new(Vec::new());
    collect_team_notes(
        &paths,
        HistoryRetention::Duration(Duration::from_secs(1)),
        &mut Sweep::new(false),
        &mut GcReport::default(),
        &mut newest_entry_modified,
        &|_| {
            checks
                .borrow_mut()
                .push(WorkspaceLock::try_acquire(&lock).unwrap().is_none());
            false
        },
    )
    .unwrap();
    assert_eq!(
        *checks.borrow(),
        [true, true],
        "GC must exclude board writers at each closed check"
    );
    assert!(!run.exists());
    assert!(WorkspaceLock::try_acquire(&lock).unwrap().is_some());
}

#[test]
fn team_notes_sweep_keeps_a_run_written_after_the_first_expiry_read() {
    for dry_run in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::StatePaths::under(
            crate::WorkspaceId::from_project_root(temp.path()),
            temp.path(),
        )
        .unwrap();
        let run = paths.team_notes_dir.join("active");
        fs::create_dir_all(&run).unwrap();
        let board = run.join("blackboard.md");
        fs::write(&board, "old").unwrap();
        let manifest = run.join("run.json");
        let record = br#"{"version":1,"team":"forge","created_at":"2026-11-07T00:00:00Z","checkout":"/repo"}"#;
        fs::write(&manifest, record).unwrap();
        let old = SystemTime::now() - Duration::from_secs(8 * 86_400);
        for path in [&board, &manifest, &run] {
            fs::File::open(path).unwrap().set_modified(old).unwrap();
        }
        let mut reads = 0;
        let mut newest = |_: &Path| {
            let modified = fs::metadata(&board).unwrap().modified().unwrap();
            reads += 1;
            if reads == 1 {
                fs::write(&board, "new").unwrap();
            }
            Ok(Some(modified))
        };
        let mut report = GcReport::default();
        let bytes = collect_team_notes(
            &paths,
            HistoryRetention::Duration(Duration::from_secs(86_400)),
            &mut Sweep::new(dry_run),
            &mut report,
            &mut newest,
            &|_| false,
        )
        .unwrap();
        assert!(
            board.exists(),
            "a write after the expiry read must keep the run"
        );
        assert_eq!(report.sidecar_files_removed, 0);
        assert_eq!(bytes, 3 + record.len() as u64);
        assert_eq!(reads, 2, "expiry must be read again at removal");
    }
}

fn temp_owner(agent: &crate::agents::AgentState) -> Option<String> {
    match agent.name.as_deref() {
        Some("child") => Some("parent".to_owned()),
        _ => agent.name.clone(),
    }
}

#[test]
fn empty_dir_gc_does_not_follow_parent_symlinks() {
    for machine_level in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::StatePaths::under(
            crate::WorkspaceId::from_project_root(temp.path()),
            temp.path(),
        )
        .unwrap();
        let target = temp.path().join("elsewhere");
        let foreign = if machine_level {
            target.join(paths.dir_name.as_str()).join("saved")
        } else {
            target.join("saved")
        };
        fs::create_dir_all(&foreign).unwrap();
        fs::write(foreign.join("report"), "keep").unwrap();
        fs::File::open(&foreign)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(8 * 86_400))
            .unwrap();
        let link = if machine_level {
            paths.empty_dir_root()
        } else {
            paths.empty_room_dir()
        };
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, link).unwrap();
        collect_empty(
            &paths,
            &Vec::new().into(),
            &mut Sweep::new(false),
            &mut GcReport::default(),
        )
        .unwrap();
        assert!(
            foreign.join("report").exists(),
            "GC must not follow a symlinked empty-dir parent"
        );
    }
}

#[test]
fn empty_dirs_wait_for_record_pruning_and_use_cwd_not_handle() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    fs::create_dir(&home).unwrap();
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&home, &link).unwrap();
    let paths = crate::StatePaths::under(crate::WorkspaceId::from_project_root(temp.path()), &link)
        .unwrap();
    let dir = home
        .join("empty")
        .join(paths.dir_name.as_str())
        .join("original-name");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("report"), "saved").unwrap();
    fs::File::open(&dir)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(8 * 86_400))
        .unwrap();
    let now = jiff::Timestamp::from_second(1_700_000_000).unwrap();
    let mut retained = crate::testkit::agent_state("claude", "ended", now);
    retained.name = Some("new-handle".into());
    retained.worktree_path = Some(dir.display().to_string());
    retained.ended_at = Some(now);
    collect_empty(
        &paths,
        &vec![retained].into(),
        &mut Sweep::new(false),
        &mut GcReport::default(),
    )
    .unwrap();
    assert!(dir.exists());
    let recent = dir.with_file_name("recent");
    fs::create_dir(&recent).unwrap();
    let mut preview = GcReport::default();
    collect_empty(
        &paths,
        &Vec::new().into(),
        &mut Sweep::new(true),
        &mut preview,
    )
    .unwrap();
    assert_eq!(preview.sidecar_files_removed, 1);
    assert!(dir.exists());
    let mut actual = GcReport::default();
    collect_empty(
        &paths,
        &Vec::new().into(),
        &mut Sweep::new(false),
        &mut actual,
    )
    .unwrap();
    assert_eq!(preview, actual);
    assert!(!dir.exists());
    assert!(recent.exists() && dir.parent().unwrap().exists());
}

#[test]
fn output_sweep_does_not_hold_the_store_lock() {
    let temp = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::under(
        crate::WorkspaceId::from_project_root(temp.path()),
        temp.path(),
    )
    .unwrap();
    paths.ensure_dirs().unwrap();
    let reader = paths.out_reader_dir(Some("armer"));
    fs::create_dir_all(&reader).unwrap();
    let pane_lock = paths.lock_path("pane-write/free.lock");
    let held_path = paths.lock_path("sidebar-launch.lock");
    let held = crate::disk::lock::WorkspaceLock::acquire(&held_path).unwrap();
    drop(crate::disk::lock::WorkspaceLock::acquire(&pane_lock).unwrap());
    let output = reader.join("wait-old.output");
    fs::File::create(&output)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(8 * 86_400))
        .unwrap();
    collect_state(
        &paths,
        GcPolicy {
            older_than: retention::DEFAULT_OLDER_THAN,
            tmp_grace: retention::DEFAULT_TMP_GRACE,
            transcript_retention: HistoryRetention::Forever,
            team_notes_retention: HistoryRetention::Forever,
            automation_notes_retention: HistoryRetention::Forever,
            team_notes_open: |_| false,
        },
        false,
        &|_| {
            assert!(
                crate::disk::lock::WorkspaceLock::try_acquire(&paths.workspace_lock)
                    .unwrap()
                    .is_some(),
                "a hook writer can acquire the store lock during output collection"
            );
            Ok(false)
        },
        &|_| temp_owner,
    )
    .unwrap();
    assert!(!output.exists());
    assert!(
        !paths.workspace_lock.exists(),
        "idle workspace lock has no exemption"
    );
    assert!(!pane_lock.exists(), "nested room locks are swept too");
    assert!(held_path.exists(), "busy state lock survives");
    drop(held);
}

#[test]
fn out_files_keep_recent_live_and_recorded_owners_and_preview_expiry() {
    let temp = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::under(
        crate::WorkspaceId::from_project_root(temp.path()),
        temp.path(),
    )
    .unwrap();
    let old = SystemTime::now() - Duration::from_secs(8 * 86_400);
    let reader = paths.out_reader_dir(Some("parent"));
    let emptied = paths.out_reader_dir(Some("gone"));
    fs::create_dir_all(&reader).unwrap();
    fs::create_dir_all(&emptied).unwrap();
    let file = |dir: &Path, name: &str, aged: bool| {
        let path = dir.join(name);
        let file = fs::File::create(&path).unwrap();
        if aged {
            file.set_modified(old).unwrap();
        }
        path
    };
    let kept = [
        file(&reader, "wait-recent.output", false),
        file(&reader, "wait-live.output", true),
        file(&reader, "fresh.output", false),
        file(&reader, "recorded.output", true),
        file(&reader, "recorded.2.output", true),
        file(&reader, "unrelated.txt", true),
    ];
    let removed = [
        file(&reader, "wait-old.output", true),
        file(&reader, "orphan.output", true),
        file(&emptied, "orphan.1234.output", true),
    ];
    let mut run = crate::store::run::RunRecord::new(
        paths.workspace_id.clone(),
        crate::ids::AgentKind::new_unchecked("claude"),
        crate::agents::PermissionMode::Auto,
        String::new(),
        temp.path().to_owned(),
    );
    run.agent_name = Some("recorded".to_owned());
    run.status = crate::store::run::RunStatus::Completed;
    crate::store::run::write(&paths.runs_dir, &run).unwrap();
    let watcher = |name: &str| Ok(name == "wait-live");
    let mut preview = GcReport::default();
    collect_out(&paths, &watcher, &mut Sweep::new(true), &mut preview).unwrap();
    assert_eq!(preview.sidecar_files_removed, 3);
    assert_eq!(preview.wait_outputs_removed, 1);
    assert_eq!(preview.dirs_removed, 1);
    assert!(removed.iter().all(|path| path.exists()));
    let mut actual = GcReport::default();
    collect_out(&paths, &watcher, &mut Sweep::new(false), &mut actual).unwrap();
    assert_eq!(preview, actual);
    assert!(kept.iter().all(|path| path.exists()));
    assert!(removed.iter().all(|path| !path.exists()));
    assert!(!emptied.exists(), "an emptied reader dir goes");
    assert!(reader.exists());
}

#[test]
fn tmp_units_follow_the_grace_rule_and_keep_the_unnamed_unit() {
    let temp = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::under(
        crate::WorkspaceId::from_project_root(temp.path()),
        temp.path(),
    )
    .unwrap();
    let old = SystemTime::now() - Duration::from_secs(25 * 3_600);
    let unit = |owner: Option<&str>| {
        let dir = paths.temp_unit_dir(owner);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("note"), b"temp").unwrap();
        fs::File::open(dir.join("note"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        fs::File::open(&dir).unwrap().set_modified(old).unwrap();
        dir
    };
    let (ended, live, orphan, unnamed) = (
        unit(Some("ended")),
        unit(Some("live")),
        unit(Some("orphan")),
        unit(None),
    );
    let recent = paths.temp_unit_dir(Some("recent"));
    fs::create_dir_all(&recent).unwrap();
    fs::File::open(&recent)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(2 * 3_600))
        .unwrap();
    let mut ended_row = crate::testkit::agent_state("claude", "ended", jiff::Timestamp::now());
    ended_row.name = Some("ended".to_owned());
    ended_row.ended_at = Some(jiff::Timestamp::try_from(old).unwrap());
    let mut live_row = ended_row.clone();
    live_row.name = Some("live".to_owned());
    live_row.runtime_owner = Some(crate::store::runtime::current_process_owner(
        crate::pane::RuntimeOwnerKind::Agent,
        "live",
    ));
    let mut report = GcReport::default();
    collect_tmp(
        &paths,
        &vec![ended_row, live_row].into(),
        retention::DEFAULT_TMP_GRACE,
        &|agent, _| temp_owner(agent),
        &mut Sweep::new(false),
        &mut report,
    )
    .unwrap();
    assert!(!ended.exists() && !orphan.exists());
    assert!(live.exists(), "a live owner keeps its unit past the grace");
    assert!(unnamed.exists() && recent.exists());
}

#[test]
fn tmp_shared_unit_keeps_live_or_open_children() {
    for live in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::StatePaths::under(
            crate::WorkspaceId::from_project_root(temp.path()),
            temp.path(),
        )
        .unwrap();
        let dir = paths.temp_unit_dir(Some("parent"));
        fs::create_dir_all(&dir).unwrap();
        let old = SystemTime::now() - Duration::from_secs(8 * 86_400);
        fs::File::open(&dir).unwrap().set_modified(old).unwrap();
        let mut parent = crate::testkit::agent_state("claude", "parent-id", jiff::Timestamp::now());
        parent.name = Some("parent".into());
        parent.ended_at = Some(old.try_into().unwrap());
        let mut child = crate::testkit::agent_state("claude", "child-id", jiff::Timestamp::now());
        child.name = Some("child".into());
        child.parent_agent_id = Some(parent.agent_id.clone());
        child.launch_depth = Some(1);
        if live {
            child.ended_at = parent.ended_at;
            child.runtime_owner = Some(crate::store::runtime::current_process_owner(
                crate::pane::RuntimeOwnerKind::Agent,
                "child",
            ));
        }
        collect_tmp(
            &paths,
            &vec![parent, child].into(),
            retention::DEFAULT_TMP_GRACE,
            &|agent, _| temp_owner(agent),
            &mut Sweep::new(false),
            &mut GcReport::default(),
        )
        .unwrap();
        assert!(
            dir.exists(),
            "child hold must keep the shared unit (live={live})"
        );
    }
}

#[cfg(unix)]
#[test]
fn tmp_errors_do_not_abort_other_units() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::under(
        crate::WorkspaceId::from_project_root(temp.path()),
        temp.path(),
    )
    .unwrap();
    let old = SystemTime::now() - Duration::from_secs(8 * 86_400);
    let blocked = paths.temp_unit_dir(Some("blocked"));
    let readable = blocked.join("nested");
    let unreadable = paths.temp_unit_dir(Some("unreadable"));
    let removable = paths.temp_unit_dir(Some("removable"));
    for dir in [&readable, &unreadable, &removable] {
        fs::create_dir_all(dir).unwrap();
        fs::File::open(dir).unwrap().set_modified(old).unwrap();
    }
    fs::write(readable.join("file"), b"blocked").unwrap();
    for dir in [&readable, &blocked] {
        fs::File::open(dir).unwrap().set_modified(old).unwrap();
    }
    fs::set_permissions(&readable, fs::Permissions::from_mode(0o500)).unwrap();
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();
    // Permissions do not bind root, so there is nothing to prove.
    if fs::write(readable.join("probe"), b"").is_ok() {
        return;
    }
    let result = collect_tmp(
        &paths,
        &Vec::new().into(),
        retention::DEFAULT_TMP_GRACE,
        &|agent, _| temp_owner(agent),
        &mut Sweep::new(false),
        &mut GcReport::default(),
    );
    fs::set_permissions(&readable, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        result.is_ok(),
        "one bad unit must not abort tmp: {result:?}"
    );
    assert!(!removable.exists());
    assert!(blocked.exists() && unreadable.exists());
}

#[test]
fn owned_units_and_terminal_runs_expire_together() {
    let temp = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::under(
        crate::WorkspaceId::from_project_root(temp.path()),
        temp.path(),
    )
    .unwrap();
    paths.ensure_dirs().unwrap();
    let old = SystemTime::now() - Duration::from_secs(8 * 86_400);
    let orphan = paths.agents_dir.join("orphan");
    fs::create_dir_all(orphan.join("scratch")).unwrap();
    fs::write(orphan.join("scratch/note"), b"keep together").unwrap();
    fs::File::open(orphan.join("scratch"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    fs::File::open(&orphan).unwrap().set_modified(old).unwrap();
    let mut run = crate::store::run::RunRecord::new(
        paths.workspace_id.clone(),
        crate::ids::AgentKind::new_unchecked("claude"),
        crate::agents::PermissionMode::Auto,
        String::new(),
        temp.path().to_owned(),
    );
    run.status = crate::store::run::RunStatus::Completed;
    crate::store::run::write(&paths.runs_dir, &run).unwrap();
    let terminal = paths.runs_dir.join(format!("{}.json", run.run_id));
    fs::File::open(&terminal)
        .unwrap()
        .set_modified(old)
        .unwrap();
    run.run_id = crate::ids::RunId::new();
    run.status = crate::store::run::RunStatus::Running;
    crate::store::run::write(&paths.runs_dir, &run).unwrap();
    let running = paths.runs_dir.join(format!("{}.json", run.run_id));
    fs::File::open(&running).unwrap().set_modified(old).unwrap();
    let mut preview = GcReport::default();
    collect_owned(
        &paths,
        &Vec::new().into(),
        &mut Sweep::new(true),
        &mut preview,
    )
    .unwrap();
    assert_eq!(preview.sidecar_files_removed, 2);
    assert!(orphan.exists() && terminal.exists() && running.exists());
    let mut actual = GcReport::default();
    collect_owned(
        &paths,
        &Vec::new().into(),
        &mut Sweep::new(false),
        &mut actual,
    )
    .unwrap();
    assert_eq!(preview, actual);
    assert!(!orphan.exists() && !terminal.exists() && running.exists());
}

#[test]
fn owned_grace_uses_ended_at_but_a_live_owner_always_wins() {
    let now = SystemTime::now();
    let old = now - Duration::from_secs(8 * 86_400);
    let mut agent = crate::testkit::agent_state("claude", "ended", jiff::Timestamp::now());
    agent.ended_at = Some(jiff::Timestamp::try_from(old).unwrap());
    assert!(owned_expired(Some(&agent), old, now));
    assert!(!owned_expired(Some(&agent), now, now));
    agent.ended_at =
        Some(jiff::Timestamp::try_from(now - Duration::from_secs(6 * 86_400)).unwrap());
    assert!(!owned_expired(Some(&agent), old, now));
    agent.ended_at = Some(jiff::Timestamp::try_from(old).unwrap());
    agent.runtime_owner = Some(crate::store::runtime::current_process_owner(
        crate::pane::RuntimeOwnerKind::Agent,
        "ended",
    ));
    assert!(!owned_expired(Some(&agent), old, now));
    assert!(owned_expired(None, old, now));
    assert!(!owned_expired(None, now, now));
    agent.runtime_owner = None;
    agent.ended_at = None;
    assert!(!owned_expired(Some(&agent), old, now));
}

#[test]
fn finished_workflow_homes_expire_by_age_whatever_wrote_them_and_live_homes_stay() {
    use crate::store::workflow::RunRecord;
    let temp = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::under(
        crate::WorkspaceId::from_project_root(temp.path()),
        temp.path(),
    )
    .unwrap();
    paths.ensure_dirs().unwrap();
    let old = SystemTime::now() - Duration::from_secs(8 * 86_400);
    let set_old = |path: &Path| fs::File::open(path).unwrap().set_modified(old).unwrap();
    // Each home: its record (none for a recordless home), whether it is aged, and whether it
    // sits in the finished directory.
    let mut cases: Vec<(Option<Vec<u8>>, bool, bool)> = Vec::new();
    for (state, report) in [
        ("done", "sent"),
        ("failed", "none"),
        ("canceled", "none"),
        ("active", "none"),
        ("parked", "none"),
        ("failed", "owed"),
    ] {
        let mut record: RunRecord = serde_json::from_value(serde_json::json!({
            "version":crate::store::workflow::VERSION, "id":crate::ids::WorkflowRunId::new(), "workflow":"review",
            "spec_dir":"/definitions/review", "spec_digest":"abc", "prompt":null,
            "scope":{"kind":"worktree","path":"/project"}, "project_root":"/project",
            "created_at":"2026-01-01T00:00:00Z", "state":state, "park":null, "node":"pr",
            "entered_at":"2026-01-01T00:00:00Z", "input":null, "key":null, "recheck_at":null,
            "wait":null, "visits":{}, "visit_grant":0, "deadline":null,
            "deadline_fired":false, "next_seq":1, "attempt":null, "prev_attempt":null,
            "terminal_at":null
        }))
        .unwrap();
        record.task = Some("nightly".to_owned());
        record.loop_report = serde_json::from_value(serde_json::json!(report)).unwrap();
        let bytes = serde_json::to_vec(&record).unwrap();
        cases.push((Some(bytes.clone()), true, false));
        cases.push((Some(bytes), true, true));
    }
    let older = format!(
        r#"{{"version":{},"state":"active"}}"#,
        crate::store::workflow::VERSION - 1
    );
    for bytes in [
        &b"corrupt"[..],
        older.as_bytes(),
        br#"{"version":999,"state":"active"}"#,
    ] {
        cases.push((Some(bytes.to_vec()), true, false));
        cases.push((Some(bytes.to_vec()), true, true));
        cases.push((Some(bytes.to_vec()), false, true));
    }
    cases.push((None, true, false));
    cases.push((None, false, false));
    cases.push((None, true, true));
    let homes: Vec<_> = cases
        .iter()
        .map(|(record, aged, finished)| {
            let id = crate::ids::WorkflowRunId::new();
            let home = if *finished {
                paths.finished_workflow(&id)
            } else {
                paths.workflow(&id)
            };
            fs::create_dir_all(&home.work).unwrap();
            fs::write(&home.spec, "spec").unwrap();
            if let Some(record) = record {
                fs::write(&home.record, record).unwrap();
            }
            if *aged {
                set_old(if record.is_some() {
                    &home.record
                } else {
                    &home.home
                });
            }
            home
        })
        .collect();
    let mut preview = GcReport::default();
    let collected = collect_owned(
        &paths,
        &Vec::new().into(),
        &mut Sweep::new(true),
        &mut preview,
    );
    assert!(
        collected.is_ok(),
        "unreadable workflow homes must not block GC: {collected:?}"
    );
    assert!(homes.iter().all(|home| home.home.exists()));
    let mut actual = GcReport::default();
    collect_owned(
        &paths,
        &Vec::new().into(),
        &mut Sweep::new(false),
        &mut actual,
    )
    .unwrap();
    assert_eq!(actual, preview);
    let gone: Vec<_> = cases
        .iter()
        .zip(&homes)
        .map(|((record, aged, finished), home)| {
            (record.is_some(), *aged, *finished, !home.home.exists())
        })
        .collect();
    assert_eq!(
        gone,
        cases
            .iter()
            .map(|(record, aged, finished)| {
                let expected = *aged && (*finished || record.is_none());
                (record.is_some(), *aged, *finished, expected)
            })
            .collect::<Vec<_>>(),
        "aged finished homes go whatever their record holds; in the live directory only an \
         aged recordless home goes"
    );
}

#[test]
fn owned_sweep_keeps_a_relaunch_with_a_fresh_direct_child() {
    let temp = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::under(
        crate::WorkspaceId::from_project_root(temp.path()),
        temp.path(),
    )
    .unwrap();
    paths.ensure_dirs().unwrap();
    let unit = paths.agents_dir.join("resumed");
    fs::create_dir_all(unit.join("scratch")).unwrap();
    let old = SystemTime::now() - Duration::from_secs(8 * 86_400);
    fs::File::open(&unit).unwrap().set_modified(old).unwrap();
    let mut agent = crate::testkit::agent_state("claude", "ended", jiff::Timestamp::now());
    agent.name = Some("resumed".to_owned());
    agent.ended_at = Some(jiff::Timestamp::try_from(old).unwrap());
    collect_owned(
        &paths,
        &vec![agent].into(),
        &mut Sweep::new(false),
        &mut GcReport::default(),
    )
    .unwrap();
    assert!(unit.join("scratch").exists());
}

#[test]
fn audit_expires_then_trims_oldest_with_dry_run_parity() {
    let temp = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::under(
        crate::WorkspaceId::from_project_root(temp.path()),
        temp.path(),
    )
    .unwrap();
    paths.ensure_dirs().unwrap();
    fs::create_dir_all(paths.audit_path("")).unwrap();
    let now = SystemTime::now();
    let expired = paths.audit_path("expired.jsonl");
    let oldest = paths.audit_path("a-oldest.jsonl");
    let newest = paths.audit_path("newest.jsonl");
    for (path, days, bytes) in [
        (&expired, 31, 1),
        (&oldest, 29, crate::disk::retention::AUDIT_MAX_BYTES),
        (&newest, 29, 1),
    ] {
        let file = fs::File::create(path).unwrap();
        file.set_len(bytes).unwrap();
        file.set_modified(now - Duration::from_secs(days * 86_400))
            .unwrap();
    }
    let mut preview = GcReport::default();
    collect_audit(&paths, &mut Sweep::new(true), &mut preview).unwrap();
    assert_eq!(preview.sidecar_files_removed, 2);
    assert!(expired.exists() && oldest.exists() && newest.exists());
    let mut actual = GcReport::default();
    collect_audit(&paths, &mut Sweep::new(false), &mut actual).unwrap();
    assert_eq!(preview, actual);
    assert!(!expired.exists() && !oldest.exists() && newest.exists());
}

#[test]
fn default_sweep_preserves_transcript_and_messages_outside_the_audit_cap() {
    let temp = tempfile::tempdir().unwrap();
    let paths = crate::StatePaths::under(
        crate::WorkspaceId::from_project_root(temp.path()),
        temp.path(),
    )
    .unwrap();
    paths.ensure_dirs().unwrap();
    let expired = paths.audit_path("expired.jsonl");
    let oldest = paths.audit_path("a-oldest.jsonl");
    let newest = paths.audit_path("newest.jsonl");
    let transcript = paths.transcript_dir.join("old.jsonl");
    let messages = paths.message_history_dir.join("old.jsonl");
    let history_size = 40 * 1024 * 1024;
    for dir in [&paths.transcript_dir, &paths.message_history_dir] {
        fs::create_dir_all(dir.join("empty")).unwrap();
    }
    let now = SystemTime::now();
    for (path, days, bytes) in [
        (&expired, 31, 1),
        (&oldest, 29, retention::AUDIT_MAX_BYTES),
        (&newest, 29, 1),
        (&transcript, 60, history_size),
        (&messages, 60, history_size),
    ] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let file = fs::File::create(path).unwrap();
        file.set_len(bytes).unwrap();
        file.set_modified(now - Duration::from_secs(days * 86_400))
            .unwrap();
    }
    let sweep = |dry_run| {
        collect_state(
            &paths,
            GcPolicy {
                older_than: retention::DEFAULT_OLDER_THAN,
                tmp_grace: retention::DEFAULT_TMP_GRACE,
                transcript_retention: HistoryRetention::Forever,
                team_notes_retention: HistoryRetention::Forever,
                automation_notes_retention: HistoryRetention::Forever,
                team_notes_open: |_| false,
            },
            dry_run,
            &|_| Ok(false),
            &|_| temp_owner,
        )
        .unwrap()
        .0
    };
    let preview = sweep(true);
    assert!(transcript.exists() && messages.exists() && expired.exists() && oldest.exists());
    let actual = sweep(false);
    assert!(
        transcript.exists() && messages.exists(),
        "history has its own retention"
    );
    assert!(
        paths.transcript_dir.join("empty").exists()
            && paths.message_history_dir.join("empty").exists()
    );
    assert!(!expired.exists() && !oldest.exists() && newest.exists());
    for (class, removed, retained) in [("audit", 2, 1), ("transcript", 0, 2 * history_size)] {
        let row = actual
            .iter()
            .find(|row| row.class == class)
            .expect("class row");
        assert_eq!(preview.iter().find(|row| row.class == class), Some(row));
        assert_eq!(row.files_removed, removed);
        if class == "transcript" {
            assert_eq!(row.bytes_removed, 0);
        }
        assert_eq!(
            serde_json::to_value(row).unwrap()["bytes_retained"],
            retained
        );
    }
}
