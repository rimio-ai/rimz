use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use super::collect::Sweep;
use super::{GcErr, GcReport, Result, read_dir_if_exists};
use crate::disk::paths::Class;
use crate::disk::retention;

pub(super) fn collect_state(
    paths: &crate::StatePaths,
    dry_run: bool,
    watcher_is_live: &impl Fn(&str) -> std::io::Result<bool>,
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
                Some(agents) => collect_tmp(paths, agents, &mut sweep, &mut report),
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
/// the newest row holding that handle; `keep` is never a candidate.
fn collect_units(
    dir: &Path,
    keep: Option<&Path>,
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
        if !metadata.is_dir() || keep == Some(path.as_path()) {
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
    collect_units(&paths.agents_dir, None, agents, sweep, report)?;
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
    Ok(())
}

/// Temp units by the owned rule; the handleless unit goes only with the room.
fn collect_tmp(
    paths: &crate::StatePaths,
    agents: &crate::store::snapshot::AgentRollup,
    sweep: &mut Sweep,
    report: &mut GcReport,
) -> Result<()> {
    let unnamed = paths.temp_unit_dir(None);
    collect_units(&paths.tmp_dir, Some(&unnamed), agents, sweep, report)
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
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, SystemTime};

    #[test]
    fn empty_dir_gc_does_not_follow_parent_symlinks() {
        for machine_level in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let paths = crate::StatePaths::under(
                crate::WorkspaceId::from_project_root(temp.path()),
                temp.path(),
            )
            .unwrap();
            let target = temp.path().join("elsewhere");
            let foreign = if machine_level {
                target.join(paths.dir_name.as_str()).join("saved")
            } else {
                target.join("saved")
            };
            fs::create_dir_all(&foreign).unwrap();
            fs::write(foreign.join("report"), "keep").unwrap();
            fs::File::open(&foreign)
                .unwrap()
                .set_modified(SystemTime::now() - Duration::from_secs(8 * 86_400))
                .unwrap();
            let link = if machine_level {
                paths.empty_dir_root()
            } else {
                paths.empty_room_dir()
            };
            fs::create_dir_all(link.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(&target, link).unwrap();
            collect_empty(
                &paths,
                &Vec::new().into(),
                &mut Sweep::new(false),
                &mut GcReport::default(),
            )
            .unwrap();
            assert!(
                foreign.join("report").exists(),
                "GC must not follow a symlinked empty-dir parent"
            );
        }
    }

    #[test]
    fn empty_dirs_wait_for_record_pruning_and_use_cwd_not_handle() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        fs::create_dir(&home).unwrap();
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&home, &link).unwrap();
        let paths =
            crate::StatePaths::under(crate::WorkspaceId::from_project_root(temp.path()), &link)
                .unwrap();
        let dir = home
            .join("empty")
            .join(paths.dir_name.as_str())
            .join("original-name");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("report"), "saved").unwrap();
        fs::File::open(&dir)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(8 * 86_400))
            .unwrap();
        let now = jiff::Timestamp::from_second(1_700_000_000).unwrap();
        let mut retained = crate::testkit::agent_state("claude", "ended", now);
        retained.name = Some("new-handle".into());
        retained.worktree_path = Some(dir.display().to_string());
        retained.ended_at = Some(now);
        collect_empty(
            &paths,
            &vec![retained].into(),
            &mut Sweep::new(false),
            &mut GcReport::default(),
        )
        .unwrap();
        assert!(dir.exists());
        let recent = dir.with_file_name("recent");
        fs::create_dir(&recent).unwrap();
        let mut preview = GcReport::default();
        collect_empty(
            &paths,
            &Vec::new().into(),
            &mut Sweep::new(true),
            &mut preview,
        )
        .unwrap();
        assert_eq!(preview.sidecar_files_removed, 1);
        assert!(dir.exists());
        let mut actual = GcReport::default();
        collect_empty(
            &paths,
            &Vec::new().into(),
            &mut Sweep::new(false),
            &mut actual,
        )
        .unwrap();
        assert_eq!(preview, actual);
        assert!(!dir.exists());
        assert!(recent.exists() && dir.parent().unwrap().exists());
    }

    #[test]
    fn output_sweep_does_not_hold_the_store_lock() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::StatePaths::under(
            crate::WorkspaceId::from_project_root(temp.path()),
            temp.path(),
        )
        .unwrap();
        paths.ensure_dirs().unwrap();
        let reader = paths.out_reader_dir(Some("armer"));
        fs::create_dir_all(&reader).unwrap();
        let pane_lock = paths.lock_path("pane-write/free.lock");
        let held_path = paths.lock_path("sidebar-launch.lock");
        let held = crate::disk::lock::WorkspaceLock::acquire(&held_path).unwrap();
        drop(crate::disk::lock::WorkspaceLock::acquire(&pane_lock).unwrap());
        let output = reader.join("wait-old.output");
        fs::File::create(&output)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(8 * 86_400))
            .unwrap();
        collect_state(&paths, false, &|_| {
            assert!(
                crate::disk::lock::WorkspaceLock::try_acquire(&paths.workspace_lock)
                    .unwrap()
                    .is_some(),
                "a hook writer can acquire the store lock during output collection"
            );
            Ok(false)
        })
        .unwrap();
        assert!(!output.exists());
        assert!(
            !paths.workspace_lock.exists(),
            "idle workspace lock has no exemption"
        );
        assert!(!pane_lock.exists(), "nested room locks are swept too");
        assert!(held_path.exists(), "busy state lock survives");
        drop(held);
    }

    #[test]
    fn out_files_keep_recent_live_and_recorded_owners_and_preview_expiry() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::StatePaths::under(
            crate::WorkspaceId::from_project_root(temp.path()),
            temp.path(),
        )
        .unwrap();
        let old = SystemTime::now() - Duration::from_secs(8 * 86_400);
        let reader = paths.out_reader_dir(Some("parent"));
        let emptied = paths.out_reader_dir(Some("gone"));
        fs::create_dir_all(&reader).unwrap();
        fs::create_dir_all(&emptied).unwrap();
        let file = |dir: &Path, name: &str, aged: bool| {
            let path = dir.join(name);
            let file = fs::File::create(&path).unwrap();
            if aged {
                file.set_modified(old).unwrap();
            }
            path
        };
        let kept = [
            file(&reader, "wait-recent.output", false),
            file(&reader, "wait-live.output", true),
            file(&reader, "fresh.output", false),
            file(&reader, "recorded.output", true),
            file(&reader, "recorded.2.output", true),
            file(&reader, "unrelated.txt", true),
        ];
        let removed = [
            file(&reader, "wait-old.output", true),
            file(&reader, "orphan.output", true),
            file(&emptied, "orphan.1234.output", true),
        ];
        let mut run = crate::store::run::RunRecord::new(
            paths.workspace_id.clone(),
            crate::ids::AgentKind::new_unchecked("claude"),
            crate::agents::PermissionMode::Auto,
            String::new(),
            temp.path().to_owned(),
        );
        run.agent_name = Some("recorded".to_owned());
        run.status = crate::store::run::RunStatus::Completed;
        crate::store::run::write(&paths.runs_dir, &run).unwrap();
        let watcher = |name: &str| Ok(name == "wait-live");
        let mut preview = GcReport::default();
        collect_out(&paths, &watcher, &mut Sweep::new(true), &mut preview).unwrap();
        assert_eq!(preview.sidecar_files_removed, 3);
        assert_eq!(preview.wait_outputs_removed, 1);
        assert_eq!(preview.dirs_removed, 1);
        assert!(removed.iter().all(|path| path.exists()));
        let mut actual = GcReport::default();
        collect_out(&paths, &watcher, &mut Sweep::new(false), &mut actual).unwrap();
        assert_eq!(preview, actual);
        assert!(kept.iter().all(|path| path.exists()));
        assert!(removed.iter().all(|path| !path.exists()));
        assert!(!emptied.exists(), "an emptied reader dir goes");
        assert!(reader.exists());
    }

    #[test]
    fn tmp_units_follow_the_owned_rule_and_keep_the_unnamed_unit() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::StatePaths::under(
            crate::WorkspaceId::from_project_root(temp.path()),
            temp.path(),
        )
        .unwrap();
        let old = SystemTime::now() - Duration::from_secs(8 * 86_400);
        let unit = |owner: Option<&str>| {
            let dir = paths.temp_unit_dir(owner);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("note"), b"temp").unwrap();
            fs::File::open(dir.join("note"))
                .unwrap()
                .set_modified(old)
                .unwrap();
            fs::File::open(&dir).unwrap().set_modified(old).unwrap();
            dir
        };
        let (ended, live, orphan, unnamed) = (
            unit(Some("ended")),
            unit(Some("live")),
            unit(Some("orphan")),
            unit(None),
        );
        let recent = paths.temp_unit_dir(Some("recent"));
        fs::create_dir_all(&recent).unwrap();
        let mut ended_row = crate::testkit::agent_state("claude", "ended", jiff::Timestamp::now());
        ended_row.name = Some("ended".to_owned());
        ended_row.ended_at = Some(jiff::Timestamp::try_from(old).unwrap());
        let mut live_row = ended_row.clone();
        live_row.name = Some("live".to_owned());
        live_row.runtime_owner = Some(crate::store::runtime::current_process_owner(
            crate::pane::RuntimeOwnerKind::Agent,
            "live",
        ));
        let mut report = GcReport::default();
        collect_tmp(
            &paths,
            &vec![ended_row, live_row].into(),
            &mut Sweep::new(false),
            &mut report,
        )
        .unwrap();
        assert!(!ended.exists() && !orphan.exists());
        assert!(live.exists(), "a live owner keeps its unit past the grace");
        assert!(unnamed.exists() && recent.exists());
    }

    #[test]
    fn owned_units_and_terminal_runs_expire_together() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::StatePaths::under(
            crate::WorkspaceId::from_project_root(temp.path()),
            temp.path(),
        )
        .unwrap();
        paths.ensure_dirs().unwrap();
        let old = SystemTime::now() - Duration::from_secs(8 * 86_400);
        let orphan = paths.agents_dir.join("orphan");
        fs::create_dir_all(orphan.join("scratch")).unwrap();
        fs::write(orphan.join("scratch/note"), b"keep together").unwrap();
        fs::File::open(orphan.join("scratch"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        fs::File::open(&orphan).unwrap().set_modified(old).unwrap();
        let mut run = crate::store::run::RunRecord::new(
            paths.workspace_id.clone(),
            crate::ids::AgentKind::new_unchecked("claude"),
            crate::agents::PermissionMode::Auto,
            String::new(),
            temp.path().to_owned(),
        );
        run.status = crate::store::run::RunStatus::Completed;
        crate::store::run::write(&paths.runs_dir, &run).unwrap();
        let terminal = paths.runs_dir.join(format!("{}.json", run.run_id));
        fs::File::open(&terminal)
            .unwrap()
            .set_modified(old)
            .unwrap();
        run.run_id = crate::ids::RunId::new();
        run.status = crate::store::run::RunStatus::Running;
        crate::store::run::write(&paths.runs_dir, &run).unwrap();
        let running = paths.runs_dir.join(format!("{}.json", run.run_id));
        fs::File::open(&running).unwrap().set_modified(old).unwrap();
        let mut preview = GcReport::default();
        collect_owned(
            &paths,
            &Vec::new().into(),
            &mut Sweep::new(true),
            &mut preview,
        )
        .unwrap();
        assert_eq!(preview.sidecar_files_removed, 2);
        assert!(orphan.exists() && terminal.exists() && running.exists());
        let mut actual = GcReport::default();
        collect_owned(
            &paths,
            &Vec::new().into(),
            &mut Sweep::new(false),
            &mut actual,
        )
        .unwrap();
        assert_eq!(preview, actual);
        assert!(!orphan.exists() && !terminal.exists() && running.exists());
    }

    #[test]
    fn owned_grace_uses_ended_at_but_a_live_owner_always_wins() {
        let now = SystemTime::now();
        let old = now - Duration::from_secs(8 * 86_400);
        let mut agent = crate::testkit::agent_state("claude", "ended", jiff::Timestamp::now());
        agent.ended_at = Some(jiff::Timestamp::try_from(old).unwrap());
        assert!(owned_expired(Some(&agent), old, now));
        assert!(!owned_expired(Some(&agent), now, now));
        agent.ended_at =
            Some(jiff::Timestamp::try_from(now - Duration::from_secs(6 * 86_400)).unwrap());
        assert!(!owned_expired(Some(&agent), old, now));
        agent.ended_at = Some(jiff::Timestamp::try_from(old).unwrap());
        agent.runtime_owner = Some(crate::store::runtime::current_process_owner(
            crate::pane::RuntimeOwnerKind::Agent,
            "ended",
        ));
        assert!(!owned_expired(Some(&agent), old, now));
        assert!(owned_expired(None, old, now));
        assert!(!owned_expired(None, now, now));
        agent.runtime_owner = None;
        agent.ended_at = None;
        assert!(!owned_expired(Some(&agent), old, now));
    }

    #[test]
    fn terminal_workflow_homes_expire_but_live_parked_and_recent_runs_stay() {
        use crate::store::workflow::{self, RunRecord};
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::StatePaths::under(
            crate::WorkspaceId::from_project_root(temp.path()),
            temp.path(),
        )
        .unwrap();
        paths.ensure_dirs().unwrap();
        let old = SystemTime::now() - Duration::from_secs(8 * 86_400);
        let mut homes = Vec::new();
        for (state, age) in [
            ("done", true),
            ("failed", true),
            ("canceled", true),
            ("active", true),
            ("parked", true),
            ("done", false),
        ] {
            let record: RunRecord = serde_json::from_value(serde_json::json!({
                "version":1, "id":crate::ids::WorkflowRunId::new(), "workflow":"review",
                "spec_dir":"/definitions/review", "spec_digest":"abc", "prompt":null,
                "scope":{"kind":"worktree","path":"/project"}, "project_root":"/project",
                "created_at":"2026-01-01T00:00:00Z", "state":state, "park":null, "node":"pr",
                "entered_at":"2026-01-01T00:00:00Z", "input":null, "key":null, "recheck_at":null,
                "wait":null, "visits":{}, "visit_grant":0, "deadline":null,
                "deadline_fired":false, "next_seq":1, "attempt":null, "prev_attempt":null,
                "terminal_at":null
            }))
            .unwrap();
            let guard = crate::disk::lock::WorkspaceLock::acquire(&paths.workspace_lock).unwrap();
            workflow::initialize(&paths, &record, b"spec", &guard).unwrap();
            let home = paths.workflow(&record.id);
            if age {
                fs::File::open(&home.record)
                    .unwrap()
                    .set_modified(old)
                    .unwrap();
            }
            homes.push(home);
        }
        for contents in [None, Some("corrupt")] {
            let home = paths.workflow(&crate::ids::WorkflowRunId::new());
            fs::create_dir_all(&home.home).unwrap();
            if let Some(contents) = contents {
                fs::write(&home.record, contents).unwrap();
            }
            homes.push(home);
        }
        let mut preview = GcReport::default();
        let collected = collect_owned(
            &paths,
            &Vec::new().into(),
            &mut Sweep::new(true),
            &mut preview,
        );
        assert!(
            collected.is_ok(),
            "unreadable workflow homes must not block GC: {collected:?}"
        );
        assert_eq!(
            preview.sidecar_files_removed, 6,
            "old terminal workflow homes must be collected together"
        );
        assert!(homes.iter().all(|p| p.home.exists()));
        let mut actual = GcReport::default();
        collect_owned(
            &paths,
            &Vec::new().into(),
            &mut Sweep::new(false),
            &mut actual,
        )
        .unwrap();
        assert_eq!(actual, preview);
        assert!(homes[..3].iter().all(|p| !p.home.exists()));
        assert!(homes[3..].iter().all(|p| p.home.exists()));
    }

    #[test]
    fn owned_sweep_keeps_a_relaunch_with_a_fresh_direct_child() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::StatePaths::under(
            crate::WorkspaceId::from_project_root(temp.path()),
            temp.path(),
        )
        .unwrap();
        paths.ensure_dirs().unwrap();
        let unit = paths.agents_dir.join("resumed");
        fs::create_dir_all(unit.join("scratch")).unwrap();
        let old = SystemTime::now() - Duration::from_secs(8 * 86_400);
        fs::File::open(&unit).unwrap().set_modified(old).unwrap();
        let mut agent = crate::testkit::agent_state("claude", "ended", jiff::Timestamp::now());
        agent.name = Some("resumed".to_owned());
        agent.ended_at = Some(jiff::Timestamp::try_from(old).unwrap());
        collect_owned(
            &paths,
            &vec![agent].into(),
            &mut Sweep::new(false),
            &mut GcReport::default(),
        )
        .unwrap();
        assert!(unit.join("scratch").exists());
    }

    #[test]
    fn audit_expires_then_trims_oldest_with_dry_run_parity() {
        let temp = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        let expired = temp.path().join("expired.jsonl");
        let oldest = temp.path().join("a-oldest.jsonl");
        let newest = temp.path().join("newest.jsonl");
        for (path, days, bytes) in [
            (&expired, 31, 1),
            (&oldest, 29, crate::disk::retention::AUDIT_MAX_BYTES),
            (&newest, 29, 1),
        ] {
            let file = fs::File::create(path).unwrap();
            file.set_len(bytes).unwrap();
            file.set_modified(now - Duration::from_secs(days * 86_400))
                .unwrap();
        }
        let mut preview = GcReport::default();
        collect_audit(temp.path(), &mut Sweep::new(true), &mut preview).unwrap();
        assert_eq!(preview.sidecar_files_removed, 2);
        assert!(expired.exists() && oldest.exists() && newest.exists());
        let mut actual = GcReport::default();
        collect_audit(temp.path(), &mut Sweep::new(false), &mut actual).unwrap();
        assert_eq!(preview, actual);
        assert!(!expired.exists() && !oldest.exists() && newest.exists());
    }
}
