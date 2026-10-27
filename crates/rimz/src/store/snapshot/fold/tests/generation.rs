use super::*;
use crate::disk::paths::RuntimePaths;
use crate::store::Store;

fn fixture() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let id = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(id.clone(), dir.path()).unwrap();
    let runtime = RuntimePaths::under(id, dir.path()).unwrap();
    let store = Store::open(paths, runtime).unwrap();
    (dir, store)
}

fn record_path(paths: &StatePaths) -> PathBuf {
    crate::disk::paths::Class::Records
        .path_under(&paths.root)
        .join("log-generation.json")
}

fn append(store: &Store) {
    store
        .append_event(&lifecycle_at(
            &store.paths().workspace_id,
            "claude",
            "SessionStart",
            "a",
            lifecycle::LifecycleSignal::Registered,
        ))
        .unwrap();
}

#[test]
fn generation_record_is_bounded_defaulted_and_read_only() {
    let (_dir, store) = fixture();
    let paths = store.paths();
    let record = record_path(paths);
    fs::create_dir_all(record.parent().unwrap()).unwrap();
    assert_eq!(lifecycle_log_generation(paths), 0);
    assert!(!record.exists());
    let mut results = Vec::new();
    for bytes in [
        b"".to_vec(),
        b"{".to_vec(),
        br#"{"generation":7}garbage"#.to_vec(),
        br#"{"generation":-1}"#.to_vec(),
        br#"{"generation":7,"future":true}"#.to_vec(),
        format!("{{\"generation\":7}}{}", " ".repeat(4096)).into_bytes(),
    ] {
        fs::write(&record, &bytes).unwrap();
        let before = FileStamp::from_metadata(&fs::metadata(&record).unwrap());
        results.push(lifecycle_log_generation(paths));
        assert_eq!(fs::read(&record).unwrap(), bytes);
        assert_eq!(
            FileStamp::from_metadata(&fs::metadata(&record).unwrap()),
            before
        );
    }
    assert_eq!(results, [0, 0, 0, 0, 7, 0]);
}

#[test]
fn rotation_persists_generation_without_the_checkpoint() {
    let (_dir, store) = fixture();
    append(&store);
    store.rotate_event_log(1, None).unwrap();
    let paths = store.paths();
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(record_path(paths)).unwrap_or_default())
            .unwrap_or_default();
    assert_eq!(record, serde_json::json!({"generation": 1}));
    fs::remove_dir_all(paths.snapshots_dir().unwrap()).unwrap();
    assert_eq!(lifecycle_log_generation(paths), 1);
    assert!(!paths.snapshots_dir().unwrap().exists());
    assert_eq!(catch_up_rollup(paths).unwrap().0.extent.generation, 1);
}

#[test]
fn repair_keeps_generation_and_hard_reset_adds_one_only_when_rotated() {
    let (_dir, store) = fixture();
    append(&store);
    store.rotate_event_log(1, None).unwrap();
    append(&store);
    use std::io::Write;
    fs::OpenOptions::new()
        .append(true)
        .open(&store.paths().events_log)
        .unwrap()
        .write_all(b"torn tail")
        .unwrap();
    assert!(store.repair_event_log().unwrap().truncated());
    let repaired = lifecycle_log_generation(store.paths());
    store.reset_records(true).unwrap();
    let reset = lifecycle_log_generation(store.paths());
    store.reset_records(true).unwrap();
    assert_eq!(
        [repaired, reset, lifecycle_log_generation(store.paths())],
        [1, 2, 2]
    );
}
