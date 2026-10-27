use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::disk::usage::dir_size;
use crate::ids::{WorkspaceDirName, WorkspaceId};
use crate::workspace::record;

use super::classes::collect_empty_agent_dirs;
use super::collect::Sweep;
use super::{GcErr, GcReport, Result, read_dir_if_exists};

/// Why a workspace directory was reaped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PruneReason {
    /// `workspace.json` records a project root that no longer exists on disk.
    ProjectRootGone,
    /// No usable `workspace.json` and no durable history — an abandoned
    /// `rimz start` scaffold.
    AbandonedScaffold,
    /// The room was written under an incompatible layout.
    IncompatibleLayout,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailedWorkspace {
    pub dir_name: String,
    pub error: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspacePruneReport {
    pub removed: Vec<RemovedWorkspace>,
    pub failed: Vec<FailedWorkspace>,
    pub kept: usize,
    /// Dirs with an unreadable record that still hold history, kept for the
    /// operator to inspect rather than silently deleted: `(directory name, error)`.
    pub retained_unreadable: Vec<(String, String)>,
    /// Every orphan tree visited, including one where nothing was removed yet.
    pub empty_dirs: Vec<OrphanEmptyTree>,
}

/// An `empty/<workspace-dir>/` tree whose room store is gone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrphanEmptyTree {
    pub dir_name: String,
    /// Files inside the agent directories removed, or that would be removed.
    pub files_removed: usize,
    pub bytes: u64,
    /// Agent directories kept because their mtime is within the grace.
    pub agent_dirs_kept: usize,
}

impl WorkspacePruneReport {
    pub fn bytes_removed(&self) -> u64 {
        self.removed
            .iter()
            .map(|removed| removed.bytes)
            .chain(self.empty_dirs.iter().map(|tree| tree.bytes))
            .fold(0_u64, u64::saturating_add)
    }
}

/// A workspace is removed when it is *provably dead*:
/// 1. its record uses an incompatible layout, or
/// 2. its `workspace.json` reads and the recorded `project_root` no longer
///    exists, or
/// 3. it has no usable record (missing or unparseable `workspace.json`) **and**
///    no durable history — an abandoned start scaffold.
///
/// A dir with an unreadable record that still holds history is kept and
/// reported, never deleted.
#[must_use = "maintenance report; surface it to the caller"]
pub(crate) fn prune_dead_workspaces_under(
    workspaces_root: &Path,
    runtime_workspaces_root: &Path,
    empty_root: &Path,
    dry_run: bool,
) -> Result<WorkspacePruneReport> {
    let entries = read_dir_if_exists(workspaces_root)?;

    let mut report = WorkspacePruneReport::default();
    for entry in entries.into_iter().flatten() {
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
                    let state = remove_state_dir(&path);
                    let runtime = remove_dir_all_if_exists(&runtime_dir);
                    if let Err(err) = state.and(runtime) {
                        report.failed.push(FailedWorkspace {
                            dir_name: name.to_string_lossy().into_owned(),
                            error: err.to_string(),
                        });
                        continue;
                    }
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
    let pruned = report
        .removed
        .iter()
        .map(|row| row.dir_name.as_str())
        .collect();
    report.empty_dirs = collect_orphan_empty_trees(workspaces_root, empty_root, &pruned, dry_run)
        .unwrap_or_else(|err| {
            tracing::warn!(error = %err, "machine gc skipped the empty-dir root");
            Vec::new()
        });
    Ok(report)
}

fn collect_orphan_empty_trees(
    workspaces_root: &Path,
    empty_root: &Path,
    pruned: &BTreeSet<&str>,
    dry_run: bool,
) -> Result<Vec<OrphanEmptyTree>> {
    match fs::symlink_metadata(empty_root) {
        Ok(metadata) if !metadata.is_dir() => return Ok(Vec::new()),
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(GcErr::Io {
                path: empty_root.to_path_buf(),
                source,
            });
        }
    }
    let mut entries = read_dir_if_exists(empty_root)?
        .into_iter()
        .flatten()
        .collect::<io::Result<Vec<_>>>()
        .map_err(|source| GcErr::ReadDir {
            path: empty_root.to_path_buf(),
            source,
        })?;
    entries.sort_by_key(|entry| entry.file_name());
    let mut trees = Vec::new();
    let mut sweep = Sweep::new(dry_run);
    for entry in entries {
        let name = entry.file_name();
        let Some(name) = name.to_str().and_then(WorkspaceDirName::parse) else {
            continue;
        };
        match collect_orphan_empty_tree(workspaces_root, &entry.path(), &name, pruned, &mut sweep) {
            Ok(Some(tree)) => trees.push(tree),
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(workspace = %name, error = %err, "machine gc skipped an inaccessible empty-dir tree");
            }
        }
    }
    Ok(trees)
}

/// `None` when the entry is not a directory or its room store still exists.
fn collect_orphan_empty_tree(
    workspaces_root: &Path,
    path: &Path,
    name: &WorkspaceDirName,
    pruned: &BTreeSet<&str>,
    sweep: &mut Sweep,
) -> Result<Option<OrphanEmptyTree>> {
    let metadata = fs::symlink_metadata(path).map_err(|source| GcErr::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_dir() {
        return Ok(None);
    }
    let store_absent = matches!(
        fs::symlink_metadata(workspaces_root.join(name.as_str())),
        Err(err) if err.kind() == io::ErrorKind::NotFound
    );
    if !pruned.contains(name.as_str()) && !store_absent {
        return Ok(None);
    }
    let mut report = GcReport::default();
    sweep.remember_dir_size(path);
    let agent_dirs_kept = collect_empty_agent_dirs(path, |_| false, sweep, &mut report)?;
    sweep.remove_dir_if_empty(path, &mut report)?;
    Ok(Some(OrphanEmptyTree {
        dir_name: name.to_string(),
        files_removed: report.sidecar_files_removed,
        bytes: report.bytes_removed,
        agent_dirs_kept,
    }))
}

enum Verdict {
    Keep,
    Retain(String),
    Remove(PruneReason, Option<PathBuf>, Option<WorkspaceId>),
}

fn classify_workspace(path: &Path) -> Verdict {
    if let Err(crate::disk::paths::PathErr::Layout { .. }) =
        crate::disk::paths::check_workspace_layout(path)
    {
        return Verdict::Remove(PruneReason::IncompatibleLayout, None, None);
    }
    match record::read(&path.join("workspace.json")) {
        Ok(record) if record.project_root.exists() => Verdict::Keep,
        Ok(record) => Verdict::Remove(
            PruneReason::ProjectRootGone,
            Some(record.project_root),
            Some(record.workspace_id),
        ),
        Err(record::WorkspaceRecordErr::Layout(_)) => {
            Verdict::Remove(PruneReason::IncompatibleLayout, None, None)
        }
        Err(err) if workspace_has_history(path) => Verdict::Retain(err.to_string()),
        Err(_) => Verdict::Remove(PruneReason::AbandonedScaffold, None, None),
    }
}

/// Whether a workspace dir holds durable history worth preserving.
fn workspace_has_history(path: &Path) -> bool {
    // `owned/runs` is not history here: a run record exists only after its
    // launch recorded the workspace, so a dir holding one classifies as
    // recorded. Guarded by
    // `loop_schedule::loop_stop_cancels_a_spawn_run_in_an_unroomed_project`.
    let history = crate::StatePaths::history_paths(path);
    history.events_log.exists() || dir_has_entries(&history.events_archive_dir)
}

fn dir_has_entries(path: &Path) -> bool {
    fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_some())
}

/// Removes a dead workspace's state dir with `workspace.json` last, so a
/// removal that fails part-way leaves the record that classifies the dir as
/// dead and the next sweep retries it rather than retaining its history.
fn remove_state_dir(path: &Path) -> Result<()> {
    let io_err = |source| GcErr::Io {
        path: path.to_path_buf(),
        source,
    };
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(io_err(source)),
    };
    let record = path.join("workspace.json");
    let mut result = Ok(());
    for entry in entries {
        let entry = entry.map_err(io_err)?;
        let child = entry.path();
        if child == record {
            continue;
        }
        let removed = if entry.file_type().map_err(io_err)?.is_dir() {
            remove_dir_all_if_exists(&child)
        } else {
            remove_file_if_exists(&child)
        };
        result = result.and(removed);
    }
    result?;
    remove_file_if_exists(&record)?;
    remove_dir_all_if_exists(path)
}

fn remove_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(GcErr::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
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
    use crate::StatePaths;
    use crate::ids::WorkspaceDirName;
    use tempfile::tempdir;

    #[test]
    fn flat_history_does_not_retain_an_unreadable_room() {
        let home = tempdir().unwrap();
        let room = home.path().join("legacy");
        for record in [None, Some("broken"), Some(r#"{"layout":2}"#)] {
            fs::create_dir_all(&room).unwrap();
            fs::write(room.join("events.log.jsonl"), b"history").unwrap();
            if let Some(record) = record {
                fs::write(room.join("workspace.json"), record).unwrap();
            }
            let report = prune_dead_workspaces_under(
                home.path(),
                &home.path().join("runtime"),
                &home.path().join("empty"),
                false,
            )
            .unwrap();
            assert_eq!(report.removed.len(), 1);
            assert_eq!(report.removed[0].reason, PruneReason::AbandonedScaffold);
            assert!(report.retained_unreadable.is_empty());
            assert!(!room.exists());
        }
    }

    #[test]
    fn legacy_room_is_pruned_with_its_runtime_tree() {
        let home = tempdir().unwrap();
        let runtime = tempdir().unwrap();
        let room = home.path().join("old-abcd");
        let runtime_room = runtime.path().join("old-abcd");
        fs::create_dir_all(&runtime_room).unwrap();
        fs::create_dir_all(&room).unwrap();
        fs::write(room.join("workspace.json"), br#"{"workspace_id":"old"}"#).unwrap();
        assert!(matches!(
            classify_workspace(&room),
            Verdict::Remove(PruneReason::IncompatibleLayout, _, _)
        ));
        let report = prune_dead_workspaces_under(
            home.path(),
            runtime.path(),
            &home.path().join("empty"),
            false,
        )
        .unwrap();
        assert_eq!(report.removed.len(), 1);
        assert_eq!(report.removed[0].reason, PruneReason::IncompatibleLayout);
        assert!(report.retained_unreadable.is_empty());
        assert!(!room.exists());
        assert!(!runtime_room.exists());
    }

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

        // 3. Abandoned scaffold: the classed dirs `ensure_dirs` creates plus
        // the lock dir, all empty, and no record.
        let scaffold = StatePaths::under_named(
            WorkspaceId::from_project_root(&temp.path().join("unfinished")),
            WorkspaceDirName::parse("unfinished-abcd").unwrap(),
            temp.path(),
        );
        scaffold.ensure_dirs().unwrap();
        fs::create_dir_all(scaffold.workspace_lock.parent().unwrap()).unwrap();
        let scaffold_dir = scaffold.root;

        // 4. Unreadable record but real history: retained, never deleted.
        let history_dir = workspaces.join("history");
        let history = StatePaths::history_paths(&history_dir);
        fs::create_dir_all(history.events_log.parent().unwrap()).unwrap();
        fs::write(history_dir.join("workspace.json"), b"{ not json").unwrap();
        fs::write(&history.events_log, b"{}\n").unwrap();

        let report =
            prune_dead_workspaces_under(&workspaces, &runtime, &temp.path().join("empty"), false)
                .unwrap();

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
            "unfinished-abcd"
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

        let report =
            prune_dead_workspaces_under(&workspaces, &runtime, &temp.path().join("empty"), true)
                .unwrap();

        assert_eq!(report.removed.len(), 1);
        assert!(report.removed[0].bytes > 0);
        assert!(gone_dir.exists(), "dry-run keeps workspace dir");
        assert!(
            runtime.join("gone-abcd").exists(),
            "dry-run keeps runtime dir"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_undeletable_workspace_spares_other_workspaces_and_attempts_both_removals() {
        use std::os::unix::fs::PermissionsExt;

        for locked_state in [true, false] {
            let temp = tempdir().unwrap();
            let workspaces = temp.path().join("ws");
            let runtime = temp.path().join("runtime");
            let alive_root = temp.path().join("alive");
            fs::create_dir(&alive_root).unwrap();
            write_record(
                &workspaces.join("alive-abcd"),
                &WorkspaceId::from_project_root(&alive_root),
                &alive_root,
            );
            let gone_root = temp.path().join("gone");
            for name in ["bad-abcd", "good-abcd"] {
                write_record(
                    &workspaces.join(name),
                    &WorkspaceId::from_project_root(&gone_root),
                    &gone_root,
                );
                fs::create_dir_all(runtime.join(name)).unwrap();
            }
            let bad = workspaces.join("bad-abcd");
            let bad_runtime = runtime.join("bad-abcd");
            let locked_root = if locked_state { &bad } else { &bad_runtime };
            let locked = locked_root.join("tmp/pkg");
            fs::create_dir_all(&locked).unwrap();
            fs::write(locked.join("file"), b"cached").unwrap();
            let events_log = crate::StatePaths::history_paths(&bad).events_log;
            fs::create_dir_all(events_log.parent().unwrap()).unwrap();
            fs::write(&events_log, b"").unwrap();
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();

            let result = prune_dead_workspaces_under(
                &workspaces,
                &runtime,
                &temp.path().join("empty"),
                false,
            );
            // Root bypasses the mode bits; restore any survivor before assertions or tempdir drop.
            let survived = locked.exists();
            if survived {
                fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
            }
            let report = result.expect("one failed removal must not abort the prune");
            let good = report
                .removed
                .iter()
                .find(|row| row.dir_name == "good-abcd")
                .unwrap();
            assert_eq!(good.reason, PruneReason::ProjectRootGone);
            assert!(!workspaces.join("good-abcd").exists());
            assert!(!runtime.join("good-abcd").exists());
            assert_eq!(report.kept, 1);
            assert!(workspaces.join("alive-abcd").exists());
            assert!(!if locked_state { &bad_runtime } else { &bad }.exists());
            if survived {
                assert_eq!(report.failed.len(), 1);
                assert_eq!(report.failed[0].dir_name, "bad-abcd");
                assert!(
                    report.failed[0]
                        .error
                        .contains(&locked_root.display().to_string())
                );
                assert_eq!(report.removed.len(), 1);
                assert_eq!(report.bytes_removed(), good.bytes);
            }
            if survived && locked_state {
                // The record outlives a partial removal, so the fixed dir is retried, not retained.
                let retry = prune_dead_workspaces_under(
                    &workspaces,
                    &runtime,
                    &temp.path().join("empty"),
                    false,
                )
                .unwrap();
                assert!(retry.failed.is_empty(), "{retry:?}");
                assert!(retry.retained_unreadable.is_empty(), "{retry:?}");
                assert!(retry.removed.iter().any(|row| row.dir_name == "bad-abcd"));
                assert!(!bad.exists() && !bad_runtime.exists());
            }
            if !survived {
                assert!(report.failed.is_empty());
                assert!(report.removed.iter().any(|row| row.dir_name == "bad-abcd"));
            }
        }
    }

    #[test]
    fn orphan_empty_trees_preview_and_apply_with_or_without_a_store() {
        for has_store in [false, true] {
            let home = tempdir().unwrap();
            let workspaces = home.path().join("ws");
            let empty = home.path().join("empty");
            if has_store {
                fs::create_dir_all(workspaces.join("gone-abcd")).unwrap();
            }
            let agent = empty.join("gone-abcd/otter");
            write_empty_agent(&agent, true);
            let run = |dry_run| {
                prune_dead_workspaces_under(
                    &workspaces,
                    &home.path().join("runtime"),
                    &empty,
                    dry_run,
                )
                .unwrap()
            };
            let preview = run(true);
            assert_eq!(preview.empty_dirs.len(), 1);
            assert_eq!(preview.empty_dirs[0].files_removed, 1);
            assert_eq!(preview.empty_dirs[0].agent_dirs_kept, 0);
            assert!(preview.empty_dirs[0].bytes > 0);
            assert!(agent.join("file").exists());
            let applied = run(false);
            assert_eq!(preview, applied);
            assert!(!agent.parent().unwrap().exists());
            assert_eq!(
                applied.bytes_removed(),
                applied.removed.iter().map(|row| row.bytes).sum::<u64>()
                    + applied.empty_dirs[0].bytes
            );
        }
    }

    #[test]
    fn orphan_empty_trees_keep_recent_agents_unknown_entries_and_existing_rooms() {
        let home = tempdir().unwrap();
        let workspaces = home.path().join("ws");
        let empty = home.path().join("empty");
        let root = home.path().join("project");
        fs::create_dir_all(&root).unwrap();
        write_record(
            &workspaces.join("alive-abcd"),
            &WorkspaceId::from_project_root(&root),
            &root,
        );
        let retained = workspaces.join("retained-abcd");
        let history = StatePaths::history_paths(&retained);
        fs::create_dir_all(history.events_log.parent().unwrap()).unwrap();
        fs::write(history.events_log, b"history").unwrap();
        for name in [
            "alive-abcd",
            "retained-abcd",
            "invalid.name",
            "orphan-abcd",
            "unknown-abcd",
        ] {
            write_empty_agent(&empty.join(name).join("old"), true);
        }
        let recent = empty.join("orphan-abcd/recent");
        write_empty_agent(&recent, false);
        let unknown = empty.join("unknown-abcd/notes");
        fs::write(&unknown, b"user file").unwrap();
        let run = |dry_run| {
            prune_dead_workspaces_under(&workspaces, &home.path().join("runtime"), &empty, dry_run)
                .unwrap()
        };
        let preview = run(true);
        assert_eq!(
            preview
                .empty_dirs
                .iter()
                .map(|row| row.dir_name.as_str())
                .collect::<Vec<_>>(),
            ["orphan-abcd", "unknown-abcd"]
        );
        assert_eq!(preview.empty_dirs[0].agent_dirs_kept, 1);
        let applied = run(false);
        assert_eq!(preview, applied);
        assert!(recent.join("file").exists());
        assert!(!empty.join("orphan-abcd/old").exists());
        assert!(unknown.exists());
        for name in ["alive-abcd", "retained-abcd", "invalid.name"] {
            assert!(empty.join(name).join("old/file").exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn orphan_empty_trees_do_not_follow_symlinks() {
        use std::os::unix::fs::symlink;
        for level in ["root", "room", "agent"] {
            let home = tempdir().unwrap();
            let empty = home.path().join("empty");
            let target = home.path().join("target");
            let target_agent = if level == "root" {
                target.join("room-abcd/old")
            } else {
                target.join("old")
            };
            write_empty_agent(&target_agent, true);
            let link = match level {
                "root" => empty.clone(),
                "room" => empty.join("linked-abcd"),
                _ => empty.join("linked-abcd/linked"),
            };
            fs::create_dir_all(link.parent().unwrap()).unwrap();
            symlink(&target, &link).unwrap();
            let run = |dry_run| {
                prune_dead_workspaces_under(
                    &home.path().join("ws"),
                    &home.path().join("runtime"),
                    &empty,
                    dry_run,
                )
                .unwrap()
            };
            if level == "root" {
                assert!(run(false).empty_dirs.is_empty());
                fs::remove_file(&link).unwrap();
                fs::create_dir(&empty).unwrap();
            }
            write_empty_agent(&empty.join("orphan-abcd/old"), true);
            let preview = run(true);
            assert!(preview.empty_dirs.iter().any(|row| row.files_removed == 1));
            assert_eq!(preview, run(false));
            assert!(target_agent.join("file").exists());
            if level != "root" {
                assert!(
                    fs::symlink_metadata(&link)
                        .unwrap()
                        .file_type()
                        .is_symlink()
                );
            }
        }
    }

    /// A tree `remove_dir_all` cannot delete (a read-only subdir, as a Go
    /// module cache leaves) costs only that tree, never the prune report.
    #[cfg(unix)]
    #[test]
    fn an_undeletable_orphan_tree_spares_the_workspace_report_and_other_trees() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempdir().unwrap();
        let workspaces = temp.path().join("ws");
        let empty = temp.path().join("empty");
        let gone_root = temp.path().join("gone-project");
        write_record(
            &workspaces.join("gone-abcd"),
            &WorkspaceId::from_project_root(&gone_root),
            &gone_root,
        );
        let locked = empty.join("bad-abcd/otter/pkg");
        fs::create_dir_all(locked.join("mod")).unwrap();
        fs::write(locked.join("mod/file"), b"cached").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
        write_empty_agent(&empty.join("bad-abcd/otter"), true);
        write_empty_agent(&empty.join("good-abcd/otter"), true);

        let result =
            prune_dead_workspaces_under(&workspaces, &temp.path().join("runtime"), &empty, false);
        // Root bypasses the mode bits and deletes the tree; only a survivor needs restoring.
        if locked.exists() {
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        }

        let report = result.unwrap();
        assert_eq!(report.removed.len(), 1);
        assert_eq!(report.removed[0].dir_name, "gone-abcd");
        assert!(
            report
                .empty_dirs
                .iter()
                .any(|tree| tree.dir_name == "good-abcd" && tree.files_removed == 1)
        );
        assert!(!empty.join("good-abcd").exists());
    }

    #[test]
    fn orphan_empty_tree_bytes_are_in_the_prune_total() {
        let report = WorkspacePruneReport {
            empty_dirs: vec![OrphanEmptyTree {
                dir_name: "gone-abcd".into(),
                files_removed: 1,
                bytes: 42,
                agent_dirs_kept: 0,
            }],
            ..WorkspacePruneReport::default()
        };
        assert_eq!(report.bytes_removed(), 42);
    }

    fn write_empty_agent(path: &Path, old: bool) {
        fs::create_dir_all(path).unwrap();
        fs::write(path.join("file"), b"agent output").unwrap();
        if old {
            fs::File::open(path)
                .unwrap()
                .set_modified(
                    std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 86_400),
                )
                .unwrap();
        }
    }

    fn write_record(dir: &Path, id: &WorkspaceId, project_root: &Path) {
        use crate::workspace::record::WorkspaceRecord;
        fs::create_dir_all(dir).unwrap();
        let record = WorkspaceRecord {
            layout: 2,
            workspace_id: id.clone(),
            project_root: project_root.to_path_buf(),
            worktree_root: None,
            session_name: "rimz-test".to_owned(),
            root_class: crate::workspace::RootClass::Repo,
            rimz_bin: None,
            rimz_build: None,
            pins: Default::default(),
            remote_control_off: Default::default(),
            agent_shell: None,
            skipped: Vec::new(),
            updated_at: jiff::Timestamp::now(),
        };
        fs::write(
            dir.join("workspace.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
    }
}
