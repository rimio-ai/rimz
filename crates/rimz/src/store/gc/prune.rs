use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::disk::usage::dir_size;
use crate::ids::WorkspaceId;
use crate::workspace::record;

use super::{GcErr, Result, read_dir_if_exists};

/// Why a workspace directory was reaped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PruneReason {
    /// `workspace.json` records a project root that no longer exists on disk.
    ProjectRootGone,
    /// No usable `workspace.json` and no durable history — an abandoned
    /// `rimz start` scaffold (empty `snapshots`/`runs`/`locks`).
    AbandonedScaffold,
}

/// A workspace store removed by [`super::prune_dead_workspaces`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemovedWorkspace {
    pub workspace_id: Option<WorkspaceId>,
    pub dir_name: String,
    pub reason: PruneReason,
    pub bytes: u64,
    /// The recorded project root, when the record was readable.
    pub project_root: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspacePruneReport {
    pub removed: Vec<RemovedWorkspace>,
    pub kept: usize,
    /// Dirs with an unreadable record that still hold history, kept for the
    /// operator to inspect rather than silently deleted: `(directory name, error)`.
    pub retained_unreadable: Vec<(String, String)>,
}

impl WorkspacePruneReport {
    pub fn bytes_removed(&self) -> u64 {
        self.removed
            .iter()
            .fold(0_u64, |total, removed| total.saturating_add(removed.bytes))
    }
}

/// A workspace is removed when it is *provably dead*:
/// 1. its `workspace.json` reads and the recorded `project_root` no longer
///    exists, or
/// 2. it has no usable record (missing or unparseable `workspace.json`) **and**
///    no durable history — an abandoned start scaffold.
///
/// A dir with an unreadable record that still holds history is kept and
/// reported, never deleted.
#[must_use = "maintenance report; surface it to the caller"]
pub(crate) fn prune_dead_workspaces_under(
    workspaces_root: &Path,
    runtime_workspaces_root: &Path,
    dry_run: bool,
) -> Result<WorkspacePruneReport> {
    let Some(entries) = read_dir_if_exists(workspaces_root)? else {
        return Ok(WorkspacePruneReport::default());
    };

    let mut report = WorkspacePruneReport::default();
    for entry in entries {
        let entry = entry.map_err(|source| GcErr::ReadDir {
            path: workspaces_root.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let runtime_dir = runtime_workspaces_root.join(&name);

        match classify_workspace(&path) {
            Verdict::Keep => report.kept += 1,
            Verdict::Retain(err) => report
                .retained_unreadable
                .push((name.to_string_lossy().into_owned(), err)),
            Verdict::Remove(reason, project_root, workspace_id) => {
                let bytes = dir_size(&path).saturating_add(dir_size(&runtime_dir));
                if !dry_run {
                    remove_dir_all_if_exists(&path)?;
                    remove_dir_all_if_exists(&runtime_dir)?;
                }
                report.removed.push(RemovedWorkspace {
                    workspace_id,
                    dir_name: name.to_string_lossy().into_owned(),
                    reason,
                    bytes,
                    project_root,
                });
            }
        }
    }
    Ok(report)
}

enum Verdict {
    Keep,
    Retain(String),
    Remove(PruneReason, Option<PathBuf>, Option<WorkspaceId>),
}

fn classify_workspace(path: &Path) -> Verdict {
    match record::read(&path.join("workspace.json")) {
        Ok(record) if record.project_root.exists() => Verdict::Keep,
        Ok(record) => Verdict::Remove(
            PruneReason::ProjectRootGone,
            Some(record.project_root),
            Some(record.workspace_id),
        ),
        Err(err) if workspace_has_history(path) => Verdict::Retain(err.to_string()),
        Err(_) => Verdict::Remove(PruneReason::AbandonedScaffold, None, None),
    }
}

/// Whether a workspace dir holds durable history worth preserving.
fn workspace_has_history(path: &Path) -> bool {
    path.join("events.log.jsonl").exists()
        || path.join("snapshots").join("latest.json").exists()
        || dir_has_entries(&path.join("events.log.archive"))
}

fn dir_has_entries(path: &Path) -> bool {
    fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_some())
}

fn remove_dir_all_if_exists(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(GcErr::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn prune_reaps_dead_roots_and_scaffolds_but_keeps_history() {
        let temp = tempdir().unwrap();
        let workspaces = crate::disk::paths::workspaces_dir_under(temp.path());
        let runtime = temp.path().join("runtime/rimz/ws");
        fs::create_dir_all(&workspaces).unwrap();

        // 1. Alive: record points at a project root that still exists.
        let alive_root = temp.path().join("alive");
        fs::create_dir_all(&alive_root).unwrap();
        let alive_id = WorkspaceId::from_project_root(&alive_root);
        write_record(&workspaces.join("alive-abcd"), &alive_id, &alive_root);

        // 2. Dead root: recorded project root is gone; runtime dir present too.
        let gone_root = temp.path().join("gone");
        let gone_id = WorkspaceId::from_project_root(&gone_root);
        write_record(&workspaces.join("gone-abcd"), &gone_id, &gone_root);
        fs::create_dir_all(runtime.join("gone-abcd")).unwrap();

        // 3. Abandoned scaffold: empty snapshots/runs/locks, no record.
        let scaffold_dir = workspaces.join("unfinished");
        for sub in ["snapshots", "runs", "locks"] {
            fs::create_dir_all(scaffold_dir.join(sub)).unwrap();
        }

        // 4. Unreadable record but real history: retained, never deleted.
        let history_dir = workspaces.join("history");
        fs::create_dir_all(&history_dir).unwrap();
        fs::write(history_dir.join("workspace.json"), b"{ not json").unwrap();
        fs::write(history_dir.join("events.log.jsonl"), b"{}\n").unwrap();

        let report = prune_dead_workspaces_under(&workspaces, &runtime, false).unwrap();

        assert_eq!(report.kept, 1, "alive workspace kept");
        assert_eq!(report.removed.len(), 2, "dead root + scaffold removed");
        assert_eq!(report.retained_unreadable.len(), 1, "history dir retained");
        let reasons: Vec<_> = report.removed.iter().map(|r| r.reason).collect();
        assert!(reasons.contains(&PruneReason::ProjectRootGone));
        assert!(reasons.contains(&PruneReason::AbandonedScaffold));
        assert!(
            report
                .removed
                .iter()
                .find(|removed| removed.reason == PruneReason::ProjectRootGone)
                .is_some_and(|removed| removed.bytes > 0),
            "dead-root removal reports reclaimed bytes"
        );

        assert!(workspaces.join("alive-abcd").exists());
        assert!(!workspaces.join("gone-abcd").exists());
        assert!(!runtime.join("gone-abcd").exists(), "runtime dir reaped");
        assert_eq!(
            report
                .removed
                .iter()
                .find(|row| row.reason == PruneReason::ProjectRootGone)
                .unwrap()
                .workspace_id,
            Some(gone_id)
        );
        assert_eq!(
            report
                .removed
                .iter()
                .find(|row| row.reason == PruneReason::AbandonedScaffold)
                .unwrap()
                .dir_name,
            "unfinished"
        );
        assert_eq!(report.retained_unreadable[0].0, "history");
        assert!(!scaffold_dir.exists());
        assert!(history_dir.exists(), "history retained");
    }

    #[test]
    fn prune_dead_workspaces_dry_run_reports_without_removing() {
        let temp = tempdir().unwrap();
        let workspaces = crate::disk::paths::workspaces_dir_under(temp.path());
        let runtime = temp.path().join("runtime/rimz/ws");
        fs::create_dir_all(&workspaces).unwrap();

        let gone_root = temp.path().join("gone");
        let gone_id = WorkspaceId::from_project_root(&gone_root);
        let gone_dir = workspaces.join("gone-abcd");
        write_record(&gone_dir, &gone_id, &gone_root);
        fs::create_dir_all(runtime.join("gone-abcd")).unwrap();

        let report = prune_dead_workspaces_under(&workspaces, &runtime, true).unwrap();

        assert_eq!(report.removed.len(), 1);
        assert!(report.removed[0].bytes > 0);
        assert!(gone_dir.exists(), "dry-run keeps workspace dir");
        assert!(
            runtime.join("gone-abcd").exists(),
            "dry-run keeps runtime dir"
        );
    }

    fn write_record(dir: &Path, id: &WorkspaceId, project_root: &Path) {
        use crate::workspace::record::WorkspaceRecord;
        fs::create_dir_all(dir).unwrap();
        let record = WorkspaceRecord {
            workspace_id: id.clone(),
            project_root: project_root.to_path_buf(),
            worktree_root: None,
            session_name: "rimz-test".to_owned(),
            root_class: crate::workspace::RootClass::Repo,
            rimz_bin: None,
            rimz_build: None,
            logins: None,
            updated_at: jiff::Timestamp::now(),
        };
        fs::write(
            dir.join("workspace.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
    }
}
