use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use super::collect::Sweep;
use super::{GcErr, GcReport, Result, read_dir_if_exists};
use crate::disk::paths::Class;
use crate::disk::retention;

pub(super) fn collect_state(
    paths: &crate::StatePaths,
    tmp_grace: Duration,
    dry_run: bool,
    watcher_is_live: &impl Fn(&str) -> std::io::Result<bool>,
    temp_owner: &impl Fn(
        &crate::agents::AgentState,
        &crate::store::snapshot::AgentRollup,
    ) -> Option<String>,
) -> Result<(Vec<super::ClassReport>, usize)> {
    let agents = {
        let _guard = crate::disk::lock::WorkspaceLock::acquire(&paths.workspace_lock)?;
        match crate::store::snapshot::catch_up_rollup(paths) {
            Ok((_, agents, _)) => Some(agents),
            Err(err) => {
                tracing::warn!(workspace = %paths.dir_name, error = %err, "owned gc skipped with unreadable rollup");
                None
            }
        }
    };
    let mut classes = Vec::new();
    let mut waits_removed = 0;
    for class in Class::STATE {
        let mut report = GcReport::default();
        let mut sweep = Sweep::new(dry_run);
        let result = match class {
            Class::Log => super::collect::collect_ttl(
                &paths.events_archive_dir,
                retention::DEFAULT_RETENTION,
                &mut sweep,
                &mut report,
            ),
            Class::Audit => collect_audit(&paths.audit_path(""), &mut sweep, &mut report),
            Class::Locks => {
                super::collect::collect_locks(&paths.lock_path(""), &mut sweep, &mut report)
            }
            Class::Owned => match &agents {
                Some(agents) => collect_owned(paths, agents, &mut sweep, &mut report),
                None => Ok(()),
            },
            Class::Tmp => match &agents {
                Some(agents) => collect_tmp(
                    paths,
                    agents,
                    tmp_grace,
                    temp_owner,
                    &mut sweep,
                    &mut report,
                ),
                None => Ok(()),
            },
            Class::Out => {
                let result = collect_out(paths, watcher_is_live, &mut sweep, &mut report);
                waits_removed = report.wait_outputs_removed;
                result
            }
            Class::Records => sweep.remove_file_if_exists(
                &paths.retired_channels_record(),
                |report| report.sidecar_files_removed += 1,
                &mut report,
            ),
            _ => Ok(()),
        };
        if let Err(err) = result {
            tracing::warn!(workspace = %paths.dir_name, class = class.dir_name(), error = %err, "class gc stopped at unreadable state");
        }
        classes.push(super::ClassReport {
            class: class.dir_name().to_owned(),
            files_removed: report.sidecar_files_removed,
            bytes_removed: report.bytes_removed,
            locks_would_check: report.locks_would_check,
        });
    }
    let mut report = GcReport::default();
    if let Some(agents) = &agents
        && let Err(err) = collect_empty(paths, agents, &mut Sweep::new(dry_run), &mut report)
    {
        tracing::warn!(workspace = %paths.dir_name, error = %err, "empty-dir gc stopped at unreadable state");
    }
    classes.push(super::ClassReport {
        class: "empty".to_owned(),
        files_removed: report.sidecar_files_removed,
        bytes_removed: report.bytes_removed,
        locks_would_check: report.locks_would_check,
    });
    Ok((classes, waits_removed))
}

fn collect_audit(dir: &Path, sweep: &mut Sweep, report: &mut GcReport) -> Result<()> {
    let mut files = sweep.files_under(dir)?;
    files.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    let mut bytes: u64 = files.iter().map(|(_, _, bytes)| bytes).sum();
    let now = SystemTime::now();
    for (path, modified, size) in files {
        if !expired(modified, now, retention::AUDIT_RETENTION)
            && bytes <= retention::AUDIT_MAX_BYTES
        {
            break;
        }
        sweep.remove_file_if_exists(&path, |report| report.sidecar_files_removed += 1, report)?;
        bytes = bytes.saturating_sub(size);
    }
    sweep.remove_empty_dirs(dir, None, report)
}

fn expired(modified: SystemTime, now: SystemTime, grace: Duration) -> bool {
    now.duration_since(modified).is_ok_and(|age| age > grace)
}

fn owned_expired(
    agent: Option<&crate::agents::AgentState>,
    modified: SystemTime,
    now: SystemTime,
) -> bool {
    if !expired(modified, now, retention::OWNED_GRACE) {
        return false;
    }
    let Some(agent) = agent else {
        return true;
    };
    if agent
        .runtime_owner
        .as_ref()
        .is_some_and(crate::store::runtime::owner_is_live)
    {
        return false;
    }
    agent
        .ended_at
        .is_some_and(|ended| expired(ended.into(), now, retention::OWNED_GRACE))
}

/// Remove each per-handle directory under `dir` by [`owned_expired`] against
/// the newest row holding that handle.
fn collect_units(
    dir: &Path,
    agents: &crate::store::snapshot::AgentRollup,
    sweep: &mut Sweep,
    report: &mut GcReport,
) -> Result<()> {
    let now = SystemTime::now();
    for entry in read_dir_if_exists(dir)?.into_iter().flatten() {
        let entry = entry.map_err(|source| GcErr::ReadDir {
            path: dir.to_owned(),
            source,
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|source| GcErr::Io {
            path: path.clone(),
            source,
        })?;
        if !metadata.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let agent = agents
            .iter()
            .filter(|agent| agent.name.as_deref().is_some_and(|handle| name == handle))
            .max_by_key(|agent| agent.last_seen);
        let mut modified = metadata.modified().map_err(|source| GcErr::Io {
            path: path.clone(),
            source,
        })?;
        for child in read_dir_if_exists(&path)?.into_iter().flatten() {
            let child = child.map_err(|source| GcErr::ReadDir {
                path: path.clone(),
                source,
            })?;
            let child_path = child.path();
            let child_modified = fs::symlink_metadata(&child_path)
                .and_then(|metadata| metadata.modified())
                .map_err(|source| GcErr::Io {
                    path: child_path,
                    source,
                })?;
            modified = modified.max(child_modified);
        }
        if owned_expired(agent, modified, now) {
            let files = sweep.files_under(&path)?.len();
            sweep.remove_tree(&path, files, report)?;
        }
    }
    Ok(())
}

fn collect_owned(
    paths: &crate::StatePaths,
    agents: &crate::store::snapshot::AgentRollup,
    sweep: &mut Sweep,
    report: &mut GcReport,
) -> Result<()> {
    collect_units(&paths.agents_dir, agents, sweep, report)?;
    let _guard = crate::disk::lock::WorkspaceLock::acquire(&paths.workspace_lock)?;
    for run in crate::store::run::list(&paths.runs_dir)? {
        let path = crate::store::run::run_path(&paths.runs_dir, &run.run_id);
        if run.status.is_terminal() && super::collect::is_older_than(&path, retention::OWNED_GRACE)?
        {
            sweep.remove_file_if_exists(
                &path,
                |report| report.sidecar_files_removed += 1,
                report,
            )?;
        }
    }
    for run in crate::store::workflow::list(paths)? {
        let home = paths.workflow(&run.id);
        if run.state.is_terminal()
            && super::collect::is_older_than(&home.record, retention::OWNED_GRACE)?
        {
            let files = sweep.files_under(&home.home)?.len();
            sweep.remove_tree(&home.home, files, report)?;
        }
    }
    // A run home without its record is a crash during `initialize`, which runs
    // under the workspace lock held here, so no creation is in flight; every
    // reader skips it, so it goes once the home itself is past the grace.
    for home in paths.workflow_runs()? {
        let recordless = !home.record.try_exists().map_err(|source| GcErr::Io {
            path: home.record.clone(),
            source,
        })?;
        if recordless && super::collect::is_older_than(&home.home, retention::OWNED_GRACE)? {
            let files = sweep.files_under(&home.home)?.len();
            sweep.remove_tree(&home.home, files, report)?;
        }
    }
    Ok(())
}

/// Temp units use their own grace; a shared unit keeps every owner's hold.
fn collect_tmp(
    paths: &crate::StatePaths,
    agents: &crate::store::snapshot::AgentRollup,
    tmp_grace: Duration,
    temp_owner: &impl Fn(
        &crate::agents::AgentState,
        &crate::store::snapshot::AgentRollup,
    ) -> Option<String>,
    sweep: &mut Sweep,
    report: &mut GcReport,
) -> Result<()> {
    let now = SystemTime::now();
    for unit in super::tmp_inventory(paths, agents, temp_owner)? {
        if !unit.past_grace(now, tmp_grace) {
            continue;
        }
        let result = sweep
            .files_under(&unit.path)
            .and_then(|files| sweep.remove_tree(&unit.path, files.len(), report));
        if let Err(error) = result {
            tracing::warn!(path = %unit.path.display(), %error, "tmp gc skipped inaccessible unit");
        }
    }
    Ok(())
}

fn collect_empty(
    paths: &crate::StatePaths,
    agents: &crate::store::snapshot::AgentRollup,
    sweep: &mut Sweep,
    report: &mut GcReport,
) -> Result<()> {
    let dir = paths.empty_room_dir();
    for parent in [paths.empty_dir_root(), dir.clone()] {
        match fs::symlink_metadata(&parent) {
            Ok(metadata) if !metadata.is_dir() => return Ok(()),
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(GcErr::Io {
                    path: parent,
                    source,
                });
            }
        }
    }
    let resolved_dir = dir.canonicalize().map_err(|source| GcErr::Io {
        path: dir.clone(),
        source,
    })?;
    collect_empty_agent_dirs(
        &dir,
        |path| {
            agents.iter().any(|agent| {
                agent
                    .worktree_path
                    .as_deref()
                    .map(Path::new)
                    .is_some_and(|recorded| {
                        recorded.file_name() == path.file_name()
                            && recorded
                                .parent()
                                .and_then(|parent| parent.canonicalize().ok())
                                .as_deref()
                                == Some(resolved_dir.as_path())
                    })
            })
        },
        sweep,
        report,
    )
    .map(|_| ())
}

pub(super) fn collect_empty_agent_dirs(
    dir: &Path,
    retained: impl Fn(&Path) -> bool,
    sweep: &mut Sweep,
    report: &mut GcReport,
) -> Result<usize> {
    let mut kept = 0;
    for entry in read_dir_if_exists(dir)?.into_iter().flatten() {
        let entry = entry.map_err(|source| GcErr::ReadDir {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|source| GcErr::Io {
            path: path.clone(),
            source,
        })?;
        if !metadata.is_dir() || retained(&path) {
            continue;
        }
        let modified = metadata.modified().map_err(|source| GcErr::Io {
            path: path.clone(),
            source,
        })?;
        if expired(modified, SystemTime::now(), retention::OWNED_GRACE) {
            let files = sweep.files_under(&path)?.len();
            sweep.remove_tree(&path, files, report)?;
        } else {
            kept += 1;
        }
    }
    Ok(kept)
}

/// Result files past the grace: a wait's once its watcher is gone, a
/// response once no run record names its agent; then empty reader dirs.
fn collect_out(
    paths: &crate::StatePaths,
    watcher_is_live: &impl Fn(&str) -> std::io::Result<bool>,
    sweep: &mut Sweep,
    report: &mut GcReport,
) -> Result<()> {
    let runs = {
        let _guard = crate::disk::lock::WorkspaceLock::acquire(&paths.workspace_lock)?;
        crate::store::run::list(&paths.runs_dir)?
    };
    let now = SystemTime::now();
    for (path, modified, _) in sweep.files_under(&paths.out_dir)? {
        if path
            .extension()
            .is_none_or(|extension| extension != "output")
            || !expired(modified, now, retention::OWNED_GRACE)
        {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|name| name.to_str()) else {
            continue;
        };
        let is_wait = name.starts_with("wait-");
        let live = if is_wait {
            watcher_is_live(name).map_err(|source| GcErr::Io {
                path: path.clone(),
                source,
            })?
        } else {
            let agent = name.split_once('.').map_or(name, |(agent, _)| agent);
            runs.iter()
                .any(|run| run.agent_name.as_deref() == Some(agent))
        };
        if !live {
            sweep.remove_file_if_exists(
                &path,
                |report| {
                    report.sidecar_files_removed += 1;
                    report.wait_outputs_removed += usize::from(is_wait);
                },
                report,
            )?;
        }
    }
    sweep.remove_empty_dirs(&paths.out_dir, None, report)
}

#[cfg(test)]
#[path = "classes_tests.rs"]
mod tests;
