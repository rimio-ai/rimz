//! One room-runtime read-mark record shared by panes and hostless CLI writers.
//!
//! Locked max-updates preserve clear timestamps across pane exits. Produced
//! room frames prune departed rows; absent records import legacy receipts.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::disk::lock::WorkspaceLock;
use crate::disk::parse_cache::ParseCache;
use crate::disk::{atomic, paths::RuntimePaths};
use crate::ids::SidebarInstanceId;

thread_local! {
    static READ_MARKS_CACHE: ParseCache<ReadMarks> = const { ParseCache::new() };
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct ReadMarks {
    #[serde(default)]
    marks: BTreeMap<String, i64>,
}

impl ReadMarks {
    pub(crate) fn load(runtime: &RuntimePaths) -> Arc<Self> {
        READ_MARKS_CACHE.with(|cache| {
            cache
                .read_stamped_json(&runtime.read_marks_path())
                .unwrap_or_else(|| Arc::new(load_legacy(runtime)))
        })
    }

    pub(crate) fn cleared_at_ms(&self, row_id: &str) -> Option<i64> {
        self.marks.get(row_id).copied()
    }

    #[cfg(test)]
    pub(crate) fn from_entries(entries: impl IntoIterator<Item = (String, i64)>) -> Self {
        Self {
            marks: entries.into_iter().collect(),
        }
    }
}

fn load_legacy(runtime: &RuntimePaths) -> ReadMarks {
    let path = runtime.read_marks_path();
    if !matches!(fs::metadata(&path), Err(err) if err.kind() == std::io::ErrorKind::NotFound) {
        return ReadMarks::default();
    }
    let mut merged = ReadMarks::default();
    let Ok(entries) = fs::read_dir(&runtime.read_marks_dir) else {
        return merged;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let legacy = name == "manual.json"
            || name
                .strip_prefix("sidebar.")
                .and_then(|id| id.strip_suffix(".json"))
                .is_some_and(|id| SidebarInstanceId::parse(id).is_ok());
        if !legacy {
            continue;
        }
        let Some(file) = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ReadMarks>(&bytes).ok())
        else {
            continue;
        };
        for (row_id, cleared_at_ms) in file.marks {
            merged
                .marks
                .entry(row_id)
                .and_modify(|seen| *seen = (*seen).max(cleared_at_ms))
                .or_insert(cleared_at_ms);
        }
    }
    merged
}

fn read_marks_lock(runtime: &RuntimePaths) -> atomic::Result<WorkspaceLock> {
    let path = runtime.read_marks_lock_path();
    WorkspaceLock::acquire_with_timeout(&path, Duration::from_millis(250)).map_err(|err| {
        atomic::AtomicErr::Io {
            path,
            source: std::io::Error::other(err),
        }
    })
}

pub fn write_read_marks(
    runtime: &RuntimePaths,
    row_ids: impl IntoIterator<Item = String>,
    cleared_at_ms: i64,
) -> atomic::Result<bool> {
    let current = ReadMarks::load(runtime);
    let row_ids: Vec<String> = row_ids
        .into_iter()
        .filter(|id| current.cleared_at_ms(id) < Some(cleared_at_ms))
        .collect();
    if row_ids.is_empty() {
        return Ok(false);
    }
    let _lock = read_marks_lock(runtime)?;
    let mut file = (*ReadMarks::load(runtime)).clone();
    for row_id in row_ids {
        let mark = file.marks.entry(row_id).or_insert(cleared_at_ms);
        *mark = (*mark).max(cleared_at_ms);
    }
    atomic::write_temp_then_rename_cache(&runtime.read_marks_path(), &file)?;
    Ok(true)
}

pub(crate) fn prune_read_marks(runtime: &RuntimePaths, live: &BTreeSet<String>) {
    if ReadMarks::load(runtime)
        .marks
        .keys()
        .all(|row_id| live.contains(row_id))
    {
        return;
    }
    let prune = || -> atomic::Result<()> {
        let _lock = read_marks_lock(runtime)?;
        let mut file = (*ReadMarks::load(runtime)).clone();
        file.marks.retain(|row_id, _| live.contains(row_id));
        atomic::write_temp_then_rename_cache(&runtime.read_marks_path(), &file)
    };
    if let Err(err) = prune() {
        debug!(error = %err, "sidebar read-mark prune failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::WorkspaceId;
    use tempfile::TempDir;

    fn runtime() -> (TempDir, RuntimePaths) {
        let dir = TempDir::new().expect("tempdir");
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).expect("runtime");
        (dir, runtime)
    }

    #[test]
    fn writes_max_update_and_never_lower_a_mark() {
        let (_dir, runtime) = runtime();
        assert!(write_read_marks(&runtime, ["row-a".to_owned()], 2_000).unwrap());
        assert!(!write_read_marks(&runtime, ["row-a".to_owned()], 1_000).unwrap());
        assert!(write_read_marks(&runtime, ["row-b".to_owned()], 1_000).unwrap());
        let marks = ReadMarks::load(&runtime);
        assert_eq!(marks.cleared_at_ms("row-a"), Some(2_000));
        assert_eq!(marks.cleared_at_ms("row-b"), Some(1_000));
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let writers: Vec<_> = (0..8)
            .map(|n| {
                let runtime = runtime.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    write_read_marks(&runtime, [format!("row-{n}")], n).unwrap();
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        let marks = ReadMarks::load(&runtime);
        for n in 0..8 {
            assert_eq!(marks.cleared_at_ms(&format!("row-{n}")), Some(n));
        }
    }

    #[test]
    fn load_reuses_the_parse_and_observes_atomic_writes() {
        let (_dir, runtime) = runtime();
        write_read_marks(&runtime, ["row-a".to_owned()], 1_000).unwrap();
        assert_eq!(
            ReadMarks::load(&runtime).cleared_at_ms("row-a"),
            Some(1_000)
        );
        let cached = READ_MARKS_CACHE
            .with(|cache| cache.read_stamped_json(&runtime.read_marks_path()))
            .unwrap();
        assert_eq!(
            ReadMarks::load(&runtime).cleared_at_ms("row-a"),
            Some(1_000)
        );
        let reused = READ_MARKS_CACHE
            .with(|cache| cache.read_stamped_json(&runtime.read_marks_path()))
            .unwrap();
        assert!(Arc::ptr_eq(&cached, &reused));
        write_read_marks(&runtime, ["row-a".to_owned()], 2_000).unwrap();
        assert_eq!(
            ReadMarks::load(&runtime).cleared_at_ms("row-a"),
            Some(2_000)
        );
        let replaced = READ_MARKS_CACHE
            .with(|cache| cache.read_stamped_json(&runtime.read_marks_path()))
            .unwrap();
        assert!(!Arc::ptr_eq(&cached, &replaced));
    }

    #[test]
    fn absent_record_imports_legacy_maxima_and_first_write_materializes_them() {
        let (_dir, runtime) = runtime();
        runtime.ensure_dirs().unwrap();
        for (name, bytes) in [
            ("manual.json", r#"{"marks":{"row-a":2000}}"#),
            (
                "sidebar.sb_00000000000000000000000000000001.json",
                r#"{"marks":{"row-a":1000,"row-b":1000}}"#,
            ),
            (
                "sidebar.sb_00000000000000000000000000000002.json",
                r#"{"marks":{"row-b":3000}}"#,
            ),
            ("sidebar.invalid.json", r#"{"marks":{"row-a":9999}}"#),
            ("generation.json", r#"{"marks":{"row-a":9999}}"#),
            ("notes.txt", r#"{"marks":{"row-a":9999}}"#),
        ] {
            fs::write(runtime.read_marks_dir.join(name), bytes).unwrap();
        }
        let marks = ReadMarks::load(&runtime);
        assert_eq!(marks.cleared_at_ms("row-a"), Some(2_000));
        assert_eq!(marks.cleared_at_ms("row-b"), Some(3_000));
        assert!(!runtime.read_marks_path().exists());
        assert!(!write_read_marks(&runtime, ["row-a".to_owned()], 1_000).unwrap());
        assert!(!runtime.read_marks_path().exists());
        assert!(write_read_marks(&runtime, ["row-c".to_owned()], 1_000).unwrap());
        assert!(runtime.read_marks_path().exists());
        fs::write(
            runtime.read_marks_dir.join("manual.json"),
            r#"{"marks":{"row-a":9999}}"#,
        )
        .unwrap();
        assert_eq!(
            ReadMarks::load(&runtime).cleared_at_ms("row-a"),
            Some(2_000)
        );
    }

    #[test]
    fn prune_drops_departed_rows_and_keeps_live_marks() {
        let (_dir, runtime) = runtime();
        write_read_marks(&runtime, ["row-a".to_owned(), "row-b".to_owned()], 1_000).unwrap();
        prune_read_marks(&runtime, &BTreeSet::from(["row-b".to_owned()]));
        let marks = ReadMarks::load(&runtime);
        assert_eq!(marks.cleared_at_ms("row-a"), None);
        assert_eq!(marks.cleared_at_ms("row-b"), Some(1_000));
    }

    #[test]
    fn missing_or_garbage_read_marks_read_empty() {
        let (_dir, runtime) = runtime();
        assert_eq!(*ReadMarks::load(&runtime), ReadMarks::default());
        runtime.ensure_dirs().unwrap();
        fs::write(runtime.read_marks_path(), b"{ not json").unwrap();
        assert_eq!(*ReadMarks::load(&runtime), ReadMarks::default());
    }
}
