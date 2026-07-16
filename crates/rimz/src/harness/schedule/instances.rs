//! RimZ-owned loop task instances and merged loop task reads.
//!
//! Durable recurring definitions live in `loop.toml`. Machine-generated
//! one-shots, self-wakes, and poll-until instances live here as state, using
//! the same task entry shape without turning runtime churn into user config
//! edits. Readers merge both backings here; durable config wins when both
//! stores contain a name.

use std::path::{Path, PathBuf};

use super::overlay_store::{OverlayStore, Result};
use crate::config::{TaskEntry, Tasks};
use crate::store::paths::state_home;
use anyhow::Context;

const STORE: OverlayStore = OverlayStore::new("loop-instances.json", "loop-instances.lock");

pub(super) fn path(state_root: &Path) -> PathBuf {
    STORE.path(state_root)
}

pub(super) fn load() -> Tasks {
    load_from(&state_home())
}

pub(super) fn insert(name: &str, entry: &TaskEntry) -> Result<()> {
    insert_into(&state_home(), name, entry)
}

pub(super) fn remove(name: &str) -> Result<bool> {
    remove_from(&state_home(), name)
}

pub(super) fn rename(old: &str, new: &str) -> Result<bool> {
    rename_from(&state_home(), old, new)
}

pub(super) fn load_from(state_root: &Path) -> Tasks {
    Tasks(STORE.load(state_root))
}

pub(super) fn load_strict_from(state_root: &Path) -> anyhow::Result<Tasks> {
    let path = path(state_root);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("reading {}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Tasks::default()),
        Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
    }
}

fn insert_into(state_root: &Path, name: &str, entry: &TaskEntry) -> Result<()> {
    STORE.mutate(state_root, |tasks| {
        tasks.insert(name.to_owned(), entry.clone());
        ((), true)
    })
}

fn remove_from(state_root: &Path, name: &str) -> Result<bool> {
    STORE.remove::<TaskEntry>(state_root, name)
}

fn rename_from(state_root: &Path, old: &str, new: &str) -> Result<bool> {
    STORE.rename::<TaskEntry>(state_root, old, new)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task() -> TaskEntry {
        TaskEntry {
            agent: Some("claude".to_owned()),
            prompt: Some("wake".to_owned()),
            root: PathBuf::from("/repo"),
            at: Some("07:00".to_owned()),
            ..TaskEntry::default()
        }
    }

    #[test]
    fn missing_or_corrupt_file_loads_empty() {
        let dir = tempfile::tempdir().expect("tempdir");

        assert!(load_from(dir.path()).0.is_empty());
        std::fs::create_dir_all(dir.path().join("rimz")).expect("state dir");
        std::fs::write(path(dir.path()), b"not json").expect("corrupt state");
        assert!(load_from(dir.path()).0.is_empty());
    }

    #[test]
    fn insert_and_remove_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = task();

        insert_into(dir.path(), "wake", &entry).expect("insert");
        let encoded = std::fs::read_to_string(path(dir.path())).expect("serialized instances");
        let value: serde_json::Value = serde_json::from_str(&encoded).expect("instances json");
        assert_eq!(value["wake"]["agent"], "claude");
        assert_eq!(value["wake"]["prompt"], "wake");
        assert_eq!(value["wake"]["root"], "/repo");
        assert_eq!(value["wake"]["at"], "07:00");
        assert_eq!(
            load_from(dir.path())
                .0
                .get("wake")
                .map(|entry| entry.prompt.as_deref()),
            Some(Some("wake"))
        );

        assert!(remove_from(dir.path(), "wake").expect("remove"));
        assert!(load_from(dir.path()).0.is_empty());
        assert!(!remove_from(dir.path(), "wake").expect("remove absent"));
    }

    #[test]
    fn rename_moves_existing_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = task();

        insert_into(dir.path(), "wake", &entry).expect("insert");

        assert!(rename_from(dir.path(), "wake", "nudge").expect("rename"));
        let tasks = load_from(dir.path());
        assert!(!tasks.0.contains_key("wake"));
        assert_eq!(
            tasks.0.get("nudge").map(|entry| entry.prompt.as_deref()),
            Some(Some("wake"))
        );
        assert!(!rename_from(dir.path(), "wake", "later").expect("rename absent"));
    }

    #[test]
    fn concurrent_inserts_preserve_both_instances() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let writers = ["first", "second"].map(|name| {
            let root = root.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                insert_into(&root, name, &task()).expect("insert instance");
            })
        });
        barrier.wait();
        for writer in writers {
            writer.join().expect("writer thread");
        }

        let tasks = load_from(&root);
        assert!(tasks.0.contains_key("first"));
        assert!(tasks.0.contains_key("second"));
    }
}
