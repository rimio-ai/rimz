use super::*;
use serde_json::json;
use std::path::Path;

fn log(dir: &Path) -> Log {
    Log {
        path: dir.join("usage.jsonl"),
        lock: dir.join("usage.lock"),
    }
}

fn record(root: Option<&Path>, kind: &str, outcome: &str, count: u64) -> Record {
    Record {
        at_ms: 10,
        root: root.map(Path::to_owned),
        server: "rust".into(),
        kind: kind.into(),
        outcome: outcome.into(),
        count,
    }
}

#[test]
fn reads_counted_rows_and_skips_corruption_and_unknown_outcomes() {
    let dir = tempfile::tempdir().unwrap();
    let log = log(dir.path());
    let mut rows = String::from("not json\n{\"outcome\":\"results\"}\n");
    for (outcome, count) in [
        ("results", 4),
        ("empty", 2),
        ("indexing", 3),
        ("memory", 2),
        ("error", 1),
        ("future", 99),
    ] {
        rows.push_str(
            &serde_json::to_string(&record(Some(dir.path()), "def", outcome, count)).unwrap(),
        );
        rows.push('\n');
    }
    rows.push_str(
        &json!({"at_ms":1,"root":null,"server":"rust","kind":"show","outcome":"empty"}).to_string(),
    );
    rows.push_str("\n{\"torn\":");
    let mut bytes = b"\xff\n".to_vec();
    bytes.extend_from_slice(rows.as_bytes());
    std::fs::write(&log.path, bytes).unwrap();
    let snapshot = log.read();
    assert_eq!(
        snapshot.total,
        Counts {
            answered: 7,
            failed: 6
        }
    );
    assert_eq!(
        snapshot.counts[&(dir.path().to_owned(), "rust".into())],
        Counts {
            answered: 6,
            failed: 6
        }
    );
}

#[test]
fn compaction_keeps_live_counts_folds_removed_roots_and_preserves_unknown_strings() {
    let dir = tempfile::tempdir().unwrap();
    let log = log(dir.path());
    let removed = dir.path().join("gone");
    let mut rows = Vec::new();
    for row in [
        record(Some(dir.path()), "def", "results", 3),
        Record {
            at_ms: 20,
            ..record(Some(dir.path()), "def", "results", 2)
        },
        record(Some(dir.path()), "check", "error", 4),
        record(Some(&removed), "hover", "empty", 5),
        record(None, "hover", "empty", 6),
        record(Some(&removed), "new-kind", "new-outcome", 7),
    ] {
        serde_json::to_writer(&mut rows, &row).unwrap();
        rows.push(b'\n');
    }
    std::fs::write(&log.path, rows).unwrap();
    let before = log.read();
    log.compact().unwrap();
    let after = log.read();
    assert_eq!(
        after.total,
        Counts {
            answered: 16,
            failed: 4
        }
    );
    assert_eq!(after.total, before.total);
    assert_eq!(
        after.counts[&(dir.path().to_owned(), "rust".into())],
        before.counts[&(dir.path().to_owned(), "rust".into())]
    );
    assert!(!after.counts.contains_key(&(removed, "rust".into())));
    let rows: Vec<Record> = std::fs::read_to_string(&log.path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 4);
    assert!(
        rows.iter()
            .any(|row| row.root.is_none() && row.kind == "hover" && row.count == 11)
    );
    assert!(rows.iter().any(|row| row.root.is_none()
        && row.kind == "new-kind"
        && row.outcome == "new-outcome"
        && row.count == 7));
    assert!(
        rows.iter()
            .any(|row| row.root.as_deref() == Some(dir.path())
                && row.kind == "def"
                && row.at_ms == 20
                && row.count == 5)
    );
}

#[test]
fn concurrent_appends_and_compactions_count_every_query_once() {
    let dir = tempfile::tempdir().unwrap();
    let log = log(dir.path());
    let start = std::sync::Barrier::new(5);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let (log, start) = (&log, &start);
            let root = dir.path();
            scope.spawn(move || {
                start.wait();
                for _ in 0..100 {
                    log.append(&record(Some(root), "refs", "results", 1))
                        .unwrap();
                }
            });
        }
        start.wait();
        for _ in 0..100 {
            log.compact().unwrap();
            std::thread::yield_now();
        }
    });
    log.compact().unwrap();
    assert_eq!(
        log.read().total,
        Counts {
            answered: 400,
            failed: 0
        }
    );
}

#[test]
fn append_compacts_at_the_cap_and_skips_a_busy_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let log = log(dir.path());
    std::fs::write(&log.path, vec![b'\n'; COMPACT_BYTES as usize]).unwrap();
    let hold = crate::disk::lock::SharedAppendLock::acquire(&log.lock).unwrap();
    log.append(&record(Some(dir.path()), "symbols", "empty", 1))
        .unwrap();
    assert_eq!(log.read().total.answered, 1);
    assert!(std::fs::metadata(&log.path).unwrap().len() > COMPACT_BYTES);
    drop(hold);
    log.append(&record(Some(dir.path()), "symbols", "empty", 1))
        .unwrap();
    assert!(std::fs::metadata(&log.path).unwrap().len() < COMPACT_BYTES);
    assert_eq!(log.read().total.answered, 2);
}
