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
            Class::Owned => match &agents {
                Some(agents) => collect_owned(paths, agents, &mut sweep, &mut report),
                None => Ok(()),
            },
            Class::Tmp => {
                let result = collect_tmp(paths, watcher_is_live, &mut sweep, &mut report);
                waits_removed = report.wait_outputs_removed;
                result
            }
            _ => Ok(()),
        };
        if let Err(err) = result {
            tracing::warn!(workspace = %paths.dir_name, class = class.dir_name(), error = %err, "class gc stopped at unreadable state");
        }
        classes.push(super::ClassReport {
            class: class.dir_name().to_owned(),
            files_removed: report.sidecar_files_removed,
            bytes_removed: report.bytes_removed,
        });
    }
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
    let Some(agent) = agent else {
        return expired(modified, now, retention::OWNED_GRACE);
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

fn collect_owned(
    paths: &crate::StatePaths,
    agents: &[crate::agents::AgentState],
    sweep: &mut Sweep,
    report: &mut GcReport,
) -> Result<()> {
    let now = SystemTime::now();
    for entry in read_dir_if_exists(&paths.agents_dir)?.into_iter().flatten() {
        let entry = entry.map_err(|source| GcErr::ReadDir {
            path: paths.agents_dir.clone(),
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
        let modified = metadata.modified().map_err(|source| GcErr::Io {
            path: path.clone(),
            source,
        })?;
        if owned_expired(agent, modified, now) {
            let files = sweep.files_under(&path)?.len();
            sweep.remove_tree(&path, files, report)?;
        }
    }
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
    Ok(())
}

fn collect_tmp(
    paths: &crate::StatePaths,
    watcher_is_live: &impl Fn(&str) -> std::io::Result<bool>,
    sweep: &mut Sweep,
    report: &mut GcReport,
) -> Result<()> {
    let runs = {
        let _guard = crate::disk::lock::WorkspaceLock::acquire(&paths.workspace_lock)?;
        crate::store::run::list(&paths.runs_dir)?
    };
    for dir in [&paths.waits_dir, &paths.subagents_dir] {
        for entry in read_dir_if_exists(dir)?.into_iter().flatten() {
            let entry = entry.map_err(|source| GcErr::ReadDir {
                path: dir.to_owned(),
                source,
            })?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|source| GcErr::Io {
                path: path.clone(),
                source,
            })?;
            if !file_type.is_file()
                || path
                    .extension()
                    .is_none_or(|extension| extension != "output")
                || !super::collect::is_older_than(&path, retention::OWNED_GRACE)?
            {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|name| name.to_str()) else {
                continue;
            };
            let is_wait = dir == &paths.waits_dir;
            let live = if is_wait {
                watcher_is_live(name).map_err(|source| GcErr::Io {
                    path: path.clone(),
                    source,
                })?
            } else {
                runs.iter()
                    .any(|run| !run.status.is_terminal() && run.agent_name.as_deref() == Some(name))
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
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, SystemTime};

    #[test]
    fn output_sweep_does_not_hold_the_store_lock() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::StatePaths::under(
            crate::WorkspaceId::from_project_root(temp.path()),
            temp.path(),
        )
        .unwrap();
        paths.ensure_dirs().unwrap();
        fs::create_dir_all(&paths.waits_dir).unwrap();
        let output = paths.waits_dir.join("old.output");
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
    }

    #[test]
    fn tmp_outputs_keep_recent_and_live_owners_and_preview_expiry() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::StatePaths::under(
            crate::WorkspaceId::from_project_root(temp.path()),
            temp.path(),
        )
        .unwrap();
        let old = SystemTime::now() - Duration::from_secs(8 * 86_400);
        for dir in [&paths.waits_dir, &paths.subagents_dir] {
            fs::create_dir_all(dir).unwrap();
            for name in [
                "old.output",
                "recent.output",
                "live.output",
                "unrelated.txt",
            ] {
                let file = fs::File::create(dir.join(name)).unwrap();
                if name != "recent.output" {
                    file.set_modified(old).unwrap();
                }
            }
        }
        let mut run = crate::store::run::RunRecord::new(
            paths.workspace_id.clone(),
            crate::ids::AgentKind::new_unchecked("claude"),
            crate::agents::PermissionMode::Auto,
            String::new(),
            temp.path().to_owned(),
        );
        run.agent_name = Some("live".to_owned());
        crate::store::run::write(&paths.runs_dir, &run).unwrap();
        let watcher = |name: &str| Ok(name == "live");
        let mut preview = GcReport::default();
        collect_tmp(&paths, &watcher, &mut Sweep::new(true), &mut preview).unwrap();
        assert_eq!(preview.sidecar_files_removed, 2);
        assert!(paths.waits_dir.join("old.output").exists());
        let mut actual = GcReport::default();
        collect_tmp(&paths, &watcher, &mut Sweep::new(false), &mut actual).unwrap();
        assert_eq!(preview, actual);
        for dir in [&paths.waits_dir, &paths.subagents_dir] {
            assert!(!dir.join("old.output").exists());
            for name in ["recent.output", "live.output", "unrelated.txt"] {
                assert!(dir.join(name).exists());
            }
        }
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
        collect_owned(&paths, &[], &mut Sweep::new(true), &mut preview).unwrap();
        assert_eq!(preview.sidecar_files_removed, 2);
        assert!(orphan.exists() && terminal.exists() && running.exists());
        let mut actual = GcReport::default();
        collect_owned(&paths, &[], &mut Sweep::new(false), &mut actual).unwrap();
        assert_eq!(preview, actual);
        assert!(!orphan.exists() && !terminal.exists() && running.exists());
    }

    #[test]
    fn owned_grace_uses_ended_at_but_a_live_owner_always_wins() {
        let now = SystemTime::now();
        let old = now - Duration::from_secs(8 * 86_400);
        let mut agent = crate::testkit::agent_state("claude", "ended", jiff::Timestamp::now());
        agent.ended_at = Some(jiff::Timestamp::try_from(old).unwrap());
        assert!(owned_expired(Some(&agent), now, now));
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
