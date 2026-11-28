use serde_json::json;

use super::*;

fn home() -> (
    tempfile::TempDir,
    crate::disk::paths::StatePaths,
    WorkflowRunPaths,
) {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::disk::paths::StatePaths::for_project_root_under(
        &dir.path().join("project"),
        dir.path(),
    )
    .unwrap();
    std::fs::create_dir_all(state.workspace_lock.parent().unwrap()).unwrap();
    let paths = state.workflow_run(&WorkflowRunId::new());
    (dir, state, paths)
}

fn record(id: WorkflowRunId) -> Record {
    Record {
        version: VERSION,
        id,
        origin: Origin::Script("return 1".to_owned()),
        checkout: PathBuf::from("/checkout"),
        args: None,
        engine: 1,
        bounds: Bounds {
            launches: 50,
            steps: 10_000,
            parallel: 8,
            expires: None,
        },
        give_up: None,
        skip_capacity: false,
        launches: 0,
        files: 1,
        log: LogCursor {
            generation: 1,
            length: 0,
            lines: 0,
        },
        journal: 0,
        covered: None,
        open: BTreeMap::new(),
        owed: Vec::new(),
        ended: None,
        created_at: Timestamp::UNIX_EPOCH,
    }
}

fn line(number: u64, result: Value) -> AnswerLine {
    AnswerLine {
        number,
        block: "now".to_owned(),
        digest: "d".to_owned(),
        phase: None,
        summary: String::new(),
        at: Timestamp::UNIX_EPOCH,
        settled: Settled::Result(result),
    }
}

/// A published home holding `record`, and the lock its writes take.
fn created(
    state: &crate::disk::paths::StatePaths,
    paths: &WorkflowRunPaths,
) -> (Record, WorkspaceLock) {
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    let staged = paths.home.with_extension("staged");
    std::fs::create_dir_all(&staged).unwrap();
    let id = WorkflowRunId::parse(paths.home.file_name().unwrap().to_str().unwrap()).unwrap();
    let first = record(id);
    create(&staged, paths, &first, &guard).unwrap();
    (first, guard)
}

#[test]
fn a_line_past_the_recorded_length_is_cut_before_the_next_append() {
    let (_dir, state, paths) = home();
    let (first, guard) = created(&state, &paths);
    let one = commit(
        &paths,
        &first,
        first.clone(),
        &[line(1, json!(1))],
        &[],
        &guard,
    )
    .unwrap();
    // A writer that died after its append and before its record.
    crate::disk::atomic::append_record_bytes(&paths.answers(1), b"{\"torn\":").unwrap();
    let two = commit(&paths, &one, one.clone(), &[line(2, json!(2))], &[], &guard).unwrap();
    let numbers: Vec<u64> = answers(&paths, &two)
        .unwrap()
        .iter()
        .map(|line| line.number)
        .collect();
    assert_eq!(numbers, [1, 2]);
    assert_eq!(two.log.lines, 2);
    assert_eq!(
        std::fs::metadata(paths.answers(1)).unwrap().len(),
        two.log.length
    );
}

#[test]
fn a_file_shorter_than_its_record_is_refused() {
    let (_dir, state, paths) = home();
    let (first, guard) = created(&state, &paths);
    let one = commit(
        &paths,
        &first,
        first.clone(),
        &[line(1, json!(1))],
        &[],
        &guard,
    )
    .unwrap();
    crate::disk::atomic::truncate_file(&paths.answers(1), 3).unwrap();
    let refused = commit(&paths, &one, one.clone(), &[line(2, json!(2))], &[], &guard);
    assert!(
        matches!(refused, Err(WorkflowRunStoreErr::Short { recorded, found: 3, .. }) if recorded == one.log.length),
        "{refused:?}"
    );
    assert!(matches!(
        answers(&paths, &one),
        Err(WorkflowRunStoreErr::Short { found: 3, .. })
    ));
}

#[test]
fn a_record_of_another_version_is_refused_before_it_decodes() {
    let (_dir, state, paths) = home();
    created(&state, &paths);
    std::fs::write(&paths.record, br#"{"version": 2, "shape": "unknown"}"#).unwrap();
    let error = load(&paths).unwrap_err();
    assert!(
        matches!(error, WorkflowRunStoreErr::Version { version: 2, .. }),
        "{error}"
    );
    assert!(error.to_string().ends_with(
        "has record version 2, written by another RimZ version; this RimZ reads only version 1"
    ));
}

#[test]
fn a_link_that_leaves_the_directory_refuses_the_copy() {
    let dir = tempfile::tempdir().unwrap();
    let origin = dir.path().join("flow");
    std::fs::create_dir_all(origin.join("checks")).unwrap();
    std::fs::write(origin.join("workflow.js"), "return 1").unwrap();
    std::fs::write(dir.path().join("secret"), "s").unwrap();
    std::os::unix::fs::symlink(dir.path().join("secret"), origin.join("checks/leak")).unwrap();
    let outcome = Sealed {
        name: "workflow.js".to_owned(),
        origin_dir: Some(origin.clone()),
        directory: true,
        refusal: None,
    };
    let at = dir.path().join("files/1/root");
    let error = seal(&at, &outcome, SealFrom::Dir(&origin)).unwrap_err();
    assert!(
        matches!(&error, WorkflowRunStoreErr::Link { link } if *link == origin.join("checks/leak")),
        "{error}"
    );
    assert!(!at.exists());
    assert_eq!(
        std::fs::read_dir(dir.path().join("files/1"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn a_link_inside_the_directory_is_copied_as_its_target() {
    let dir = tempfile::tempdir().unwrap();
    let origin = dir.path().join("flow");
    std::fs::create_dir_all(origin.join("checks")).unwrap();
    std::fs::write(origin.join("workflow.js"), "return 1").unwrap();
    std::fs::write(origin.join("checks/a.py"), "a").unwrap();
    std::os::unix::fs::symlink("a.py", origin.join("checks/b.py")).unwrap();
    let outcome = Sealed {
        name: "workflow.js".to_owned(),
        origin_dir: Some(origin.clone()),
        directory: true,
        refusal: None,
    };
    let at = dir.path().join("files/1/root");
    assert_eq!(
        seal(&at, &outcome, SealFrom::Dir(&origin)).unwrap(),
        outcome
    );
    assert_eq!(
        std::fs::read_to_string(copy_dir(&at).join("checks/b.py")).unwrap(),
        "a"
    );
    assert_eq!(sealed(&at).unwrap(), Some(outcome));
}

#[test]
fn a_large_result_round_trips_through_a_file() {
    let (_dir, state, paths) = home();
    let (first, guard) = created(&state, &paths);
    let large = json!({ "text": "x".repeat(LINE_RESULT_CAP) });
    let next = commit(
        &paths,
        &first,
        first.clone(),
        &[line(1, large.clone())],
        &[],
        &guard,
    )
    .unwrap();
    assert!(
        next.log.length < 1024,
        "the line holds the file's path, not the result"
    );
    assert_eq!(
        answers(&paths, &next).unwrap()[0].settled,
        Settled::Result(large)
    );
}

#[test]
fn a_generation_the_record_does_not_name_is_removed() {
    let (_dir, state, paths) = home();
    let (first, guard) = created(&state, &paths);
    let one = commit(
        &paths,
        &first,
        first.clone(),
        &[line(1, json!(1))],
        &[],
        &guard,
    )
    .unwrap();
    std::fs::create_dir_all(paths.files(1)).unwrap();
    std::fs::create_dir_all(paths.files(2).join("root")).unwrap();
    std::fs::write(paths.answers(2), "stale").unwrap();
    std::fs::create_dir_all(paths.results()).unwrap();
    let (kept, stale) = (
        paths.results().join("1.1.json"),
        paths.results().join("2.1.json"),
    );
    std::fs::write(&kept, "1").unwrap();
    std::fs::write(&stale, "1").unwrap();
    remove_stale(&paths, &one, &guard).unwrap();
    assert!(!paths.answers(2).exists());
    assert!(!paths.files(2).exists());
    assert!(!stale.exists());
    assert!(paths.answers(1).exists() && paths.files(1).exists() && kept.exists());
}

#[test]
fn the_journal_reads_only_what_its_record_covers() {
    let (_dir, state, paths) = home();
    let (first, guard) = created(&state, &paths);
    let entry = Entry {
        at: Timestamp::UNIX_EPOCH,
        event: Event::Log {
            after: 0,
            text: "hello".to_owned(),
        },
    };
    let next = commit(
        &paths,
        &first,
        first.clone(),
        &[],
        std::slice::from_ref(&entry),
        &guard,
    )
    .unwrap();
    crate::disk::atomic::append_record_bytes(&paths.journal, b"{\"at\":").unwrap();
    assert_eq!(journal(&paths, &next).unwrap(), [entry]);
}

#[test]
fn args_null_round_trips_apart_from_no_args() {
    let (_dir, state, paths) = home();
    let (first, guard) = created(&state, &paths);
    let null = Record {
        args: Some(Value::Null),
        ..first.clone()
    };
    commit(&paths, &first, null, &[], &[], &guard).unwrap();
    assert_eq!(load(&paths).unwrap().unwrap().args, Some(Value::Null));
    let none = load(&paths).unwrap().unwrap();
    commit(&paths, &none, first.clone(), &[], &[], &guard).unwrap();
    assert_eq!(load(&paths).unwrap().unwrap().args, None);
}
