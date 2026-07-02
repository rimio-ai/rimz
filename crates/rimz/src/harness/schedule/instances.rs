//! Rimz-owned loop task instances.
//!
//! Durable recurring definitions live in `loop.toml`. Machine-generated
//! one-shots, self-wakes, and poll-until instances live here as state, using
//! the same task entry shape without turning runtime churn into user config
//! edits.

use std::path::{Path, PathBuf};

use crate::config::{TaskEntry, Tasks};
use crate::ledger::atomic::{Result, write_temp_then_rename_cache};
use crate::ledger::paths::state_home;

const NAME: &str = "loop-instances.json";

pub fn path(state_root: &Path) -> PathBuf {
    state_root.join("rimz").join(NAME)
}

pub fn load() -> Tasks {
    load_from(&state_home())
}

pub fn insert(name: &str, entry: &TaskEntry) -> Result<()> {
    insert_into(&state_home(), name, entry)
}

pub fn remove(name: &str) -> Result<bool> {
    remove_from(&state_home(), name)
}

pub fn rename(old: &str, new: &str) -> Result<bool> {
    rename_from(&state_home(), old, new)
}

fn load_from(state_root: &Path) -> Tasks {
    let Ok(bytes) = std::fs::read(path(state_root)) else {
        return Tasks::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

fn insert_into(state_root: &Path, name: &str, entry: &TaskEntry) -> Result<()> {
    let mut tasks = load_from(state_root);
    tasks.0.insert(name.to_owned(), entry.clone());
    write_temp_then_rename_cache(&path(state_root), &tasks)
}

fn remove_from(state_root: &Path, name: &str) -> Result<bool> {
    let mut tasks = load_from(state_root);
    let removed = tasks.0.remove(name).is_some();
    if removed {
        write_temp_then_rename_cache(&path(state_root), &tasks)?;
    }
    Ok(removed)
}

fn rename_from(state_root: &Path, old: &str, new: &str) -> Result<bool> {
    let mut tasks = load_from(state_root);
    let Some(entry) = tasks.0.remove(old) else {
        return Ok(false);
    };
    tasks.0.insert(new.to_owned(), entry);
    write_temp_then_rename_cache(&path(state_root), &tasks)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task() -> TaskEntry {
        TaskEntry {
            spec: Some("claude".to_owned()),
            prompt: Some("wake".to_owned()),
            root: PathBuf::from("/repo"),
            at: Some("07:00".to_owned()),
            once: true,
            ..TaskEntry::default()
        }
    }

    #[test]
    fn missing_file_loads_empty() {
        let dir = tempfile::tempdir().expect("tempdir");

        assert!(load_from(dir.path()).0.is_empty());
    }

    #[test]
    fn insert_and_remove_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = task();

        insert_into(dir.path(), "wake", &entry).expect("insert");
        assert_eq!(
            load_from(dir.path()).0.get("wake").map(|entry| entry.once),
            Some(true)
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
}
