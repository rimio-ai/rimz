//! Workspace metadata stored beside the store.
//!
//! `workspace.json` lets maintenance commands reason about known stores
//! after the project root has moved or disappeared. The store event log
//! remains the correctness source; this record is an index for
//! operator workflows such as `rimz gc` — and, in [`WorkspaceRecord::pins`],
//! the room's account pins for future launches. Each allocated identity carries
//! the account stamp its exec wrapper resolves instead.
//! There is no version upgrade path: an incompatible layout is refused.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::disk::atomic::{self, write_temp_then_rename};
use crate::disk::paths::StatePaths;
use crate::ids::{RoomLogins, WorkspaceId};
use crate::workspace::{ResolvedWorkspace, RootClass};

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceRecordErr {
    #[error(transparent)]
    Layout(#[from] crate::disk::paths::PathErr),
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("json parse error on {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

pub type Result<T> = std::result::Result<T, WorkspaceRecordErr>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "WorkspaceRecordRead")]
pub struct WorkspaceRecord {
    #[serde(default = "crate::disk::paths::legacy_layout")]
    pub layout: u32,
    pub workspace_id: WorkspaceId,
    pub project_root: PathBuf,
    /// Active worktree cwd for room-local helper panes. Older records fall
    /// back to [`Self::project_root`] and self-heal on the next owner re-record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_root: Option<PathBuf>,
    /// The mux session this room was last born under.
    pub session_name: String,
    /// Which ladder tier the root is. Records predating the field decode as
    /// [`RootClass::Repo`] — today's behavior — and self-heal on the next
    /// start/attach re-record.
    #[serde(default = "default_root_class")]
    pub root_class: RootClass,
    /// Room-owning RimZ binary used for session-local helpers such as the
    /// Zellij presence plugin. Generic re-records preserve it; owner flows
    /// (`start`, cwd-based `attach`, `reload`) set it explicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rimz_bin: Option<PathBuf>,
    /// Digest of [`Self::rimz_bin`]. The pair is the verified executable target
    /// for long-lived room processes; legacy records omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rimz_build: Option<String>,
    /// Provider accounts pinned for future launches; absent kinds inherit live.
    /// Empty pins are omitted. Legacy `logins`, when `pins` is absent, contributes
    /// only named entries; explicit `pins` wins, including a `default` pin.
    /// Writers emit only `pins`, without changing the workspace layout.
    /// Generic re-records preserve pins; `rimz reset` clears them.
    #[serde(skip_serializing_if = "RoomLogins::is_empty")]
    pub pins: RoomLogins,
    pub updated_at: Timestamp,
}

#[derive(Deserialize)]
struct WorkspaceRecordRead {
    #[serde(default = "crate::disk::paths::legacy_layout")]
    layout: u32,
    workspace_id: WorkspaceId,
    project_root: PathBuf,
    #[serde(default)]
    worktree_root: Option<PathBuf>,
    session_name: String,
    #[serde(default = "default_root_class")]
    root_class: RootClass,
    #[serde(default)]
    rimz_bin: Option<PathBuf>,
    #[serde(default)]
    rimz_build: Option<String>,
    #[serde(default, deserialize_with = "read_pins")]
    pins: Option<RoomLogins>,
    #[serde(default)]
    logins: Option<serde_json::Value>,
    updated_at: Timestamp,
}

impl TryFrom<WorkspaceRecordRead> for WorkspaceRecord {
    type Error = serde_json::Error;

    fn try_from(record: WorkspaceRecordRead) -> std::result::Result<Self, Self::Error> {
        let pins = match record.pins {
            Some(pins) => pins,
            None => record
                .logins
                .map(serde_json::from_value::<RoomLogins>)
                .transpose()?
                .unwrap_or_default()
                .into_iter()
                .filter(|(_, name)| !name.is_default())
                .collect(),
        };
        Ok(Self {
            layout: record.layout,
            workspace_id: record.workspace_id,
            project_root: record.project_root,
            worktree_root: record.worktree_root,
            session_name: record.session_name,
            root_class: record.root_class,
            rimz_bin: record.rimz_bin,
            rimz_build: record.rimz_build,
            pins,
            updated_at: record.updated_at,
        })
    }
}

fn read_pins<'de, D: serde::Deserializer<'de>>(
    reader: D,
) -> std::result::Result<Option<RoomLogins>, D::Error> {
    RoomLogins::deserialize(reader).map(Some)
}

fn default_root_class() -> RootClass {
    RootClass::Repo
}

impl WorkspaceRecord {
    pub(crate) fn from_resolved(workspace: &ResolvedWorkspace) -> Self {
        Self {
            layout: crate::disk::paths::WORKSPACE_LAYOUT,
            workspace_id: workspace.workspace_id.clone(),
            project_root: workspace.project_root.clone(),
            worktree_root: Some(workspace.worktree_root.clone()),
            session_name: workspace.session_name.clone(),
            root_class: workspace.root_class,
            rimz_bin: None,
            rimz_build: None,
            pins: RoomLogins::new(),
            updated_at: Timestamp::now(),
        }
    }
}

#[must_use = "durability barrier; check the result"]
pub fn write(paths: &StatePaths, record: &WorkspaceRecord) -> Result<()> {
    write_path(&paths.workspace_record, record)?;
    Ok(())
}

#[must_use = "durability barrier; check the result"]
pub(super) fn write_path(path: &Path, record: &WorkspaceRecord) -> Result<()> {
    let mut record = record.clone();
    record.layout = crate::disk::paths::WORKSPACE_LAYOUT;
    write_temp_then_rename(path, &record)?;
    Ok(())
}

/// The record, or `None` where the room has never written one. A record that
/// exists but cannot be parsed is an error: the room's launch-account
/// selection lives here, and silently treating a corrupt file as absent would
/// reset it.
pub fn read_optional(path: &Path) -> Result<Option<WorkspaceRecord>> {
    match read(path) {
        Ok(record) => Ok(Some(record)),
        Err(WorkspaceRecordErr::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

pub fn read(path: &Path) -> Result<WorkspaceRecord> {
    let bytes = fs::read(path).map_err(|source| WorkspaceRecordErr::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let record: WorkspaceRecord =
        serde_json::from_slice(&bytes).map_err(|source| WorkspaceRecordErr::Json {
            path: path.to_path_buf(),
            source,
        })?;
    crate::disk::paths::require_layout(path, record.layout)?;
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::WorkspaceResolver;
    use tempfile::tempdir;

    #[test]
    fn legacy_accounts_migrate_to_named_pins_and_never_write_logins() {
        let base = serde_json::json!({
            "workspace_id": "ws_0123456789abcdef01234567",
            "project_root": "/repo", "session_name": "rimz-repo",
            "updated_at": "2024-01-01T00:00:00Z",
            "logins": {"claude": "default", "codex": "work"}
        });
        let record: WorkspaceRecord = serde_json::from_value(base.clone()).unwrap();
        let written = serde_json::to_value(record).unwrap();
        assert_eq!(written["pins"], serde_json::json!({"codex": "work"}));
        assert!(written.get("logins").is_none());

        let mut explicit = base.clone();
        explicit["pins"] = serde_json::json!({"claude": "default"});
        let record: WorkspaceRecord = serde_json::from_value(explicit).unwrap();
        assert_eq!(
            serde_json::to_value(record).unwrap()["pins"],
            serde_json::json!({"claude": "default"})
        );

        for pins in [serde_json::json!({}), serde_json::Value::Null] {
            let mut empty = base.clone();
            empty["logins"] = pins;
            let record: WorkspaceRecord = serde_json::from_value(empty).unwrap();
            let written = serde_json::to_value(record).unwrap();
            assert!(written.get("pins").is_none());
            assert!(written.get("logins").is_none());
        }
        let mut explicit_empty = base;
        explicit_empty["pins"] = serde_json::json!({});
        let record: WorkspaceRecord = serde_json::from_value(explicit_empty).unwrap();
        assert!(serde_json::to_value(record).unwrap().get("pins").is_none());
    }

    #[test]
    fn record_round_trips() {
        let dir = tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let workspace = WorkspaceResolver::resolve(&project, None).unwrap();
        let paths = StatePaths::under(workspace.workspace_id.clone(), dir.path()).unwrap();
        let mut record = WorkspaceRecord::from_resolved(&workspace);
        record.layout = 1;
        record.rimz_bin = Some(dir.path().join("builds/build/rimz"));
        record.rimz_build = Some("build".to_owned());
        record.pins = RoomLogins::from([(
            crate::ids::AgentKind::new_unchecked("claude"),
            "work".parse().unwrap(),
        )]);

        write(&paths, &record).unwrap();
        let loaded = read(&paths.workspace_record).unwrap();

        assert_eq!(loaded.workspace_id, workspace.workspace_id);
        assert_eq!(loaded.layout, crate::disk::paths::WORKSPACE_LAYOUT);
        assert_eq!(loaded.project_root, workspace.project_root);
        assert_eq!(
            loaded.worktree_root.as_ref(),
            Some(&workspace.worktree_root)
        );
        assert_eq!(loaded.session_name, workspace.session_name);
        assert_eq!(loaded.rimz_bin, record.rimz_bin);
        assert_eq!(loaded.rimz_build, record.rimz_build);
        assert_eq!(loaded.pins, record.pins);
    }

    #[test]
    fn legacy_record_without_rimz_bin_parses() {
        let record: WorkspaceRecord = serde_json::from_str(
            r#"{
                "workspace_id": "ws_0123456789abcdef01234567",
                "project_root": "/repo",
                "session_name": "rimz-repo",
                "root_class": "repo",
                "updated_at": "2024-01-01T00:00:00Z"
            }"#,
        )
        .expect("legacy record parses");

        assert_eq!(record.rimz_bin, None);
        assert_eq!(record.layout, 1);
        assert_eq!(record.rimz_build, None);
        assert_eq!(record.worktree_root, None);
        assert!(record.pins.is_empty());
    }

    #[test]
    fn read_optional_separates_an_absent_record_from_a_corrupt_one() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("workspace.json");
        assert!(read_optional(&missing).unwrap().is_none());

        std::fs::write(&missing, b"{ not json").unwrap();
        assert!(matches!(
            read_optional(&missing),
            Err(WorkspaceRecordErr::Json { .. })
        ));
    }
}
