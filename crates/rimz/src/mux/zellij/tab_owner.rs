//! Session-scoped tab-name ownership in the room runtime cache.

use std::collections::BTreeMap;
use std::fs;

use serde::{Deserialize, Serialize};

use crate::disk::paths::RuntimePaths;
use crate::disk::{atomic, lock::WorkspaceLock};
use crate::mux::Result;
use crate::mux::tab_name::TabOwnerRecord;

#[derive(Deserialize, Serialize)]
struct TabOwners {
    version: u32,
    session_name: String,
    tabs: BTreeMap<u64, TabOwnerRecord>,
}

pub(super) fn read(runtime: &RuntimePaths, session: &str) -> BTreeMap<u64, TabOwnerRecord> {
    let path = runtime.lane_path("tab-owners.json");
    let load = || -> Option<TabOwners> {
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
            Err(err) => {
                tracing::debug!(path = %path.display(), error = %err, "tab owners unreadable");
                return None;
            }
        };
        match serde_json::from_slice(&bytes) {
            Ok(file) => Some(file),
            Err(err) => {
                tracing::debug!(path = %path.display(), error = %err, "tab owners invalid");
                None
            }
        }
    };
    load()
        .filter(|file| file.version == 1 && file.session_name == session)
        .map(|file| file.tabs)
        .unwrap_or_default()
}

pub(super) fn update(
    runtime: &RuntimePaths,
    session: &str,
    edit: impl FnOnce(&mut BTreeMap<u64, TabOwnerRecord>) -> Result<bool>,
) -> Result<()> {
    let _guard = WorkspaceLock::acquire_with_timeout(
        &runtime.lock_path("tab-owners.lock"),
        crate::mux::TAB_RENAME_TIMEOUT,
    )
    .map_err(super::output_error)?;
    let mut tabs = read(runtime, session);
    if !edit(&mut tabs)? {
        return Ok(());
    }
    atomic::write_temp_then_rename_cache(
        &runtime.lane_path("tab-owners.json"),
        &TabOwners {
            version: 1,
            session_name: session.to_owned(),
            tabs,
        },
    )
    .map_err(super::output_error)
}

pub(super) fn clear(runtime: &RuntimePaths) -> Result<()> {
    let _guard = WorkspaceLock::acquire_with_timeout(
        &runtime.lock_path("tab-owners.lock"),
        crate::mux::TAB_RENAME_TIMEOUT,
    )
    .map_err(super::output_error)?;
    match fs::remove_file(runtime.lane_path("tab-owners.json")) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{MuxName, PaneId, WorkspaceId};
    use std::collections::BTreeSet;

    fn runtime(dir: &std::path::Path) -> RuntimePaths {
        RuntimePaths::under(WorkspaceId::from_project_root(dir), dir).unwrap()
    }

    fn owner(base: &str) -> TabOwnerRecord {
        TabOwnerRecord {
            base: base.to_owned(),
            founders: vec![PaneId::from_parts(MuxName::Zellij, "terminal_12")],
        }
    }

    #[test]
    fn round_trip_and_base_update_preserve_founders() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(dir.path());
        update(&runtime, "room", |tabs| {
            tabs.insert(7, owner("debugger"));
            Ok(true)
        })
        .unwrap();
        assert_eq!(
            read(&runtime, "room"),
            BTreeMap::from([(7, owner("debugger"))])
        );
        update(&runtime, "room", |tabs| {
            tabs.get_mut(&7).unwrap().base = "brainstormer".to_owned();
            Ok(true)
        })
        .unwrap();
        assert_eq!(
            read(&runtime, "room"),
            BTreeMap::from([(7, owner("brainstormer"))])
        );
        update(&runtime, "room", |tabs| {
            tabs.remove(&7);
            Ok(true)
        })
        .unwrap();
        assert!(read(&runtime, "room").is_empty());
    }

    #[test]
    fn session_and_version_mismatch_read_empty() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(dir.path());
        runtime.ensure_dirs().unwrap();
        let path = runtime.lane_path("tab-owners.json");
        let file = serde_json::json!({"version": 1, "session_name": "room", "tabs": {"7": owner("debugger")}});
        std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
        assert_eq!(read(&runtime, "room").len(), 1);
        assert!(read(&runtime, "other").is_empty());
        let mut file = file;
        file["version"] = 2.into();
        std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
        assert!(read(&runtime, "room").is_empty());
    }

    #[test]
    fn clear_removes_a_previous_session_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(dir.path());
        runtime.ensure_dirs().unwrap();
        let path = runtime.lane_path("tab-owners.json");
        std::fs::write(&path, b"{}").unwrap();
        clear(&runtime).unwrap();
        assert!(!path.exists());
        clear(&runtime).unwrap();
    }

    #[test]
    fn concurrent_writers_keep_each_others_live_records() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(dir.path());
        std::thread::scope(|scope| {
            for id in 0..16 {
                let runtime = &runtime;
                scope.spawn(move || {
                    update(runtime, "room", |tabs| {
                        tabs.insert(id, owner("agent"));
                        Ok(true)
                    })
                    .unwrap();
                });
            }
        });
        assert_eq!(read(&runtime, "room").len(), 16);
        update(&runtime, "room", |tabs| {
            tabs.insert(16, owner("unresolved"));
            Ok(true)
        })
        .unwrap();
        assert_eq!(read(&runtime, "room").len(), 17);
    }

    #[test]
    fn a_delayed_writer_preserves_a_tab_born_after_its_first_listing() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(dir.path());
        let live_tabs = std::sync::Mutex::new(BTreeSet::from([7]));
        update(&runtime, "room", |tabs| {
            tabs.insert(7, owner("debugger"));
            Ok(true)
        })
        .unwrap();
        let first_listing = live_tabs.lock().unwrap().clone();
        let delayed_rebuild = || {
            update(&runtime, "room", |tabs| {
                let current_listing = live_tabs.lock().unwrap();
                tabs.retain(|id, _| current_listing.contains(id));
                tabs.get_mut(&7).unwrap().base = "survivor".to_owned();
                Ok(true)
            })
        };
        live_tabs.lock().unwrap().insert(8);
        update(&runtime, "room", |tabs| {
            tabs.insert(8, owner("peer"));
            Ok(true)
        })
        .unwrap();
        assert!(!first_listing.contains(&8));
        delayed_rebuild().unwrap();
        assert_eq!(
            read(&runtime, "room"),
            BTreeMap::from([(7, owner("survivor")), (8, owner("peer"))])
        );
    }
}
