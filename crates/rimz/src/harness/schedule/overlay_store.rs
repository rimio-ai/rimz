//! Shared locked persistence for machine-local keyed task overlays.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Serialize, de::DeserializeOwned};

use crate::store::atomic::{AtomicErr, write_temp_then_rename_cache};
use crate::store::lock::{LockErr, WorkspaceLock};

#[derive(Debug, thiserror::Error)]
pub(super) enum OverlayError {
    #[error(transparent)]
    Lock(#[from] LockErr),
    #[error(transparent)]
    Write(#[from] AtomicErr),
}

pub(super) type Result<T> = std::result::Result<T, OverlayError>;

pub(super) struct OverlayStore {
    data_name: &'static str,
    lock_name: &'static str,
}

impl OverlayStore {
    pub(super) const fn new(data_name: &'static str, lock_name: &'static str) -> Self {
        Self {
            data_name,
            lock_name,
        }
    }

    pub(super) fn path(&self, state_root: &Path) -> PathBuf {
        state_root.join("rimz").join(self.data_name)
    }

    fn lock_path(&self, state_root: &Path) -> PathBuf {
        state_root.join("rimz").join(self.lock_name)
    }

    pub(super) fn load<V>(&self, state_root: &Path) -> BTreeMap<String, V>
    where
        V: DeserializeOwned,
    {
        let Ok(bytes) = std::fs::read(self.path(state_root)) else {
            return BTreeMap::new();
        };
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    pub(super) fn mutate<V, T>(
        &self,
        state_root: &Path,
        edit: impl FnOnce(&mut BTreeMap<String, V>) -> (T, bool),
    ) -> Result<T>
    where
        V: DeserializeOwned + Serialize,
    {
        let _guard = WorkspaceLock::acquire(&self.lock_path(state_root))?;
        let mut entries = self.load(state_root);
        let (result, changed) = edit(&mut entries);
        if changed {
            write_temp_then_rename_cache(&self.path(state_root), &entries)?;
        }
        Ok(result)
    }

    pub(super) fn remove<V>(&self, state_root: &Path, name: &str) -> Result<bool>
    where
        V: DeserializeOwned + Serialize,
    {
        self.mutate(state_root, |entries: &mut BTreeMap<String, V>| {
            let removed = entries.remove(name).is_some();
            (removed, removed)
        })
    }

    pub(super) fn rename<V>(&self, state_root: &Path, old: &str, new: &str) -> Result<bool>
    where
        V: DeserializeOwned + Serialize,
    {
        self.mutate(state_root, |entries: &mut BTreeMap<String, V>| {
            let Some(value) = entries.remove(old) else {
                return (false, false);
            };
            entries.insert(new.to_owned(), value);
            (true, true)
        })
    }

    pub(super) fn prune_orphans_in_scopes<V>(
        &self,
        state_root: &Path,
        known: &BTreeSet<String>,
        scopes: &BTreeSet<String>,
    ) -> Result<usize>
    where
        V: DeserializeOwned + Serialize,
    {
        self.mutate(state_root, |entries: &mut BTreeMap<String, V>| {
            let before = entries.len();
            entries.retain(|name, _| {
                known.contains(name)
                    || (name.contains("::") && !scopes.iter().any(|scope| name.starts_with(scope)))
            });
            let removed = before - entries.len();
            (removed, removed > 0)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STORE: OverlayStore = OverlayStore::new("test-overlay.json", "test-overlay.lock");

    fn set(state_root: &Path, name: &str, value: u32) {
        STORE
            .mutate(state_root, |entries| {
                let changed = entries.get(name) != Some(&value);
                entries.insert(name.to_owned(), value);
                ((), changed)
            })
            .expect("set");
    }

    #[test]
    fn missing_or_corrupt_file_loads_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(STORE.load::<u32>(dir.path()).is_empty());

        std::fs::create_dir_all(dir.path().join("rimz")).expect("state dir");
        std::fs::write(STORE.path(dir.path()), b"not json").expect("corrupt state");
        assert!(STORE.load::<u32>(dir.path()).is_empty());
    }

    #[test]
    fn remove_rename_and_prune_share_keyed_edits() {
        let dir = tempfile::tempdir().expect("tempdir");
        set(dir.path(), "old", 1);
        set(dir.path(), "gone", 2);

        assert!(
            STORE
                .rename::<u32>(dir.path(), "old", "new")
                .expect("rename")
        );
        assert!(
            !STORE
                .rename::<u32>(dir.path(), "missing", "other")
                .expect("rename absent")
        );
        assert!(STORE.remove::<u32>(dir.path(), "gone").expect("remove"));
        assert!(
            !STORE
                .remove::<u32>(dir.path(), "gone")
                .expect("remove absent")
        );

        set(dir.path(), "orphan", 3);
        let removed = STORE
            .prune_orphans_in_scopes::<u32>(
                dir.path(),
                &BTreeSet::from(["new".to_owned()]),
                &BTreeSet::from([String::new()]),
            )
            .expect("prune");
        assert_eq!(removed, 1);
        assert_eq!(
            STORE.load(dir.path()),
            BTreeMap::from([("new".to_owned(), 1)])
        );
    }

    #[test]
    fn unchanged_mutation_neither_creates_nor_rewrites_data() {
        let dir = tempfile::tempdir().expect("tempdir");
        STORE
            .mutate::<u32, _>(dir.path(), |_| ((), false))
            .expect("missing no-op");
        assert!(!STORE.path(dir.path()).exists());

        std::fs::write(STORE.path(dir.path()), b"not json").expect("corrupt state");
        STORE
            .mutate::<u32, _>(dir.path(), |_| ((), false))
            .expect("existing no-op");
        assert_eq!(
            std::fs::read(STORE.path(dir.path())).expect("read unchanged data"),
            b"not json"
        );
    }

    #[test]
    fn scoped_prune_preserves_foreign_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        set(dir.path(), "machine::known", 1);
        set(dir.path(), "machine::gone", 2);
        set(dir.path(), "ws_foreign::keep", 3);
        set(dir.path(), "legacy-unscoped", 4);

        let removed = STORE
            .prune_orphans_in_scopes::<u32>(
                dir.path(),
                &BTreeSet::from(["machine::known".to_owned()]),
                &BTreeSet::from(["machine::".to_owned(), "ws_here::".to_owned()]),
            )
            .expect("scoped prune");

        assert_eq!(removed, 2);
        assert_eq!(
            STORE.load(dir.path()),
            BTreeMap::from([
                ("machine::known".to_owned(), 1),
                ("ws_foreign::keep".to_owned(), 3),
            ])
        );
    }
}
