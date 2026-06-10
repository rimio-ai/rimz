//! Runtime read receipts for sidebar unread rows.
//!
//! Each renderer owns one disposable receipt file. A focus clear writes the row
//! id and the clear time to that renderer's file; every renderer folds all files
//! and treats the max clear time per row as the workspace-wide read mark.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::ids::SidebarInstanceId;
use crate::ledger::{RuntimePaths, atomic};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ReadMarks {
    marks: HashMap<String, i64>,
}

impl ReadMarks {
    pub(crate) fn empty() -> Self {
        Self::default()
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

#[derive(Clone, Debug)]
pub(crate) struct ReadMarkStore {
    runtime: RuntimePaths,
    instance_id: SidebarInstanceId,
    own: BTreeMap<String, i64>,
}

impl ReadMarkStore {
    pub(crate) fn new(runtime: RuntimePaths, instance_id: SidebarInstanceId) -> Self {
        let own = read_file(&runtime.sidebar_read_marks_path(&instance_id))
            .map(|file| file.marks)
            .unwrap_or_default();
        Self {
            runtime,
            instance_id,
            own,
        }
    }

    pub(crate) fn load_merged(&self) -> ReadMarks {
        let entries = match fs::read_dir(&self.runtime.read_marks_dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return ReadMarks::empty(),
            Err(err) => {
                debug!(
                    path = %self.runtime.read_marks_dir.display(),
                    error = %err,
                    "sidebar read-mark dir unreadable",
                );
                return ReadMarks::empty();
            }
        };

        let mut marks: HashMap<String, i64> = HashMap::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !is_read_mark_file(&path) {
                continue;
            }
            let file = match read_file(&path) {
                Some(file) => file,
                None => continue,
            };
            for (row_id, cleared_at_ms) in file.marks {
                marks
                    .entry(row_id)
                    .and_modify(|seen| *seen = (*seen).max(cleared_at_ms))
                    .or_insert(cleared_at_ms);
            }
        }
        ReadMarks { marks }
    }

    pub(crate) fn observe_fold(
        &mut self,
        cleared: Vec<String>,
        cleared_at_ms: i64,
        live: &HashSet<String>,
    ) {
        let mut changed = false;
        for row_id in cleared {
            if !live.contains(&row_id) {
                continue;
            }
            match self.own.get(&row_id) {
                Some(existing) if *existing >= cleared_at_ms => {}
                _ => {
                    self.own.insert(row_id, cleared_at_ms);
                    changed = true;
                }
            }
        }

        let before = self.own.len();
        self.own.retain(|row_id, _| live.contains(row_id));
        changed |= self.own.len() != before;

        if !changed {
            return;
        }

        let path = self.runtime.sidebar_read_marks_path(&self.instance_id);
        let file = ReadMarksFile {
            marks: self.own.clone(),
        };
        if let Err(err) = atomic::write_temp_then_rename_cache(&path, &file) {
            debug!(
                path = %path.display(),
                error = %err,
                "sidebar read-mark write failed",
            );
        }
    }
}

pub(crate) fn read_mark_file_instance_id(path: &Path) -> Option<SidebarInstanceId> {
    let name = path.file_name()?.to_str()?;
    let id = name.strip_prefix("sidebar.")?.strip_suffix(".json")?;
    SidebarInstanceId::parse(id).ok()
}

pub(crate) fn is_read_mark_file(path: &Path) -> bool {
    read_mark_file_instance_id(path).is_some()
}

fn read_file(path: &Path) -> Option<ReadMarksFile> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            debug!(path = %path.display(), error = %err, "sidebar read-mark unreadable");
            return None;
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(file) => Some(file),
        Err(err) => {
            debug!(path = %path.display(), error = %err, "sidebar read-mark invalid");
            None
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct ReadMarksFile {
    #[serde(default)]
    marks: BTreeMap<String, i64>,
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

    fn instance(hex_tail: &str) -> SidebarInstanceId {
        SidebarInstanceId::parse(&format!("sb_{hex_tail:0>32}")).expect("instance id")
    }

    fn live(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    #[test]
    fn missing_dir_reads_empty() {
        let (_dir, runtime) = runtime();
        let store = ReadMarkStore::new(runtime, instance("01"));

        assert_eq!(store.load_merged(), ReadMarks::empty());
    }

    #[test]
    fn writes_and_reads_one_instances_marks() {
        let (_dir, runtime) = runtime();
        let mut store = ReadMarkStore::new(runtime.clone(), instance("01"));
        store.observe_fold(vec!["row-a".to_owned()], 1_000, &live(&["row-a"]));

        let merged = store.load_merged();
        assert_eq!(merged.cleared_at_ms("row-a"), Some(1_000));
        let text = fs::read_to_string(runtime.sidebar_read_marks_path(&instance("01")))
            .expect("read mark file");
        assert!(text.contains("\"marks\""));
        assert!(text.contains("\"row-a\""));
    }

    #[test]
    fn merge_takes_the_max_receipt_per_row() {
        let (_dir, runtime) = runtime();
        let mut first = ReadMarkStore::new(runtime.clone(), instance("01"));
        let mut second = ReadMarkStore::new(runtime.clone(), instance("02"));
        first.observe_fold(vec!["row-a".to_owned()], 1_000, &live(&["row-a"]));
        second.observe_fold(
            vec!["row-a".to_owned(), "row-b".to_owned()],
            2_000,
            &live(&["row-a", "row-b"]),
        );

        let merged = first.load_merged();
        assert_eq!(merged.cleared_at_ms("row-a"), Some(2_000));
        assert_eq!(merged.cleared_at_ms("row-b"), Some(2_000));
    }

    #[test]
    fn garbage_files_are_skipped() {
        let (_dir, runtime) = runtime();
        runtime.ensure_dirs().expect("runtime dirs");
        fs::write(
            runtime.sidebar_read_marks_path(&instance("02")),
            b"{ not json",
        )
        .expect("garbage");
        fs::write(runtime.read_marks_dir.join("notes.txt"), b"not a mark").expect("other file");
        let store = ReadMarkStore::new(runtime, instance("01"));

        assert_eq!(store.load_merged(), ReadMarks::empty());
    }

    #[test]
    fn departed_rows_are_pruned_from_the_own_file() {
        let (_dir, runtime) = runtime();
        let id = instance("01");
        let mut store = ReadMarkStore::new(runtime.clone(), id.clone());
        store.observe_fold(
            vec!["row-a".to_owned(), "row-b".to_owned()],
            1_000,
            &live(&["row-a", "row-b"]),
        );
        store.observe_fold(Vec::new(), 2_000, &live(&["row-b"]));

        let file: ReadMarksFile = serde_json::from_slice(
            &fs::read(runtime.sidebar_read_marks_path(&id)).expect("read file"),
        )
        .expect("json");
        assert_eq!(
            file.marks.keys().cloned().collect::<Vec<_>>(),
            vec!["row-b"]
        );
    }

    #[test]
    fn reexec_keeps_prior_own_marks_on_the_next_write() {
        let (_dir, runtime) = runtime();
        let id = instance("01");
        let mut before = ReadMarkStore::new(runtime.clone(), id.clone());
        before.observe_fold(vec!["row-a".to_owned()], 1_000, &live(&["row-a"]));

        let mut after = ReadMarkStore::new(runtime.clone(), id.clone());
        after.observe_fold(vec!["row-b".to_owned()], 2_000, &live(&["row-a", "row-b"]));

        let file: ReadMarksFile = serde_json::from_slice(
            &fs::read(runtime.sidebar_read_marks_path(&id)).expect("read file"),
        )
        .expect("json");
        assert_eq!(file.marks.get("row-a"), Some(&1_000));
        assert_eq!(file.marks.get("row-b"), Some(&2_000));
    }
}
