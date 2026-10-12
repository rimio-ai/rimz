//! Detached-helper writes, outside the sidebar producer's read-only graph.

use super::*;
use crate::harness::assist_log::{Assist, AssistRecord};
use crate::proc::cgroup::{self, CgroupErr, ReclaimReport};
use crate::store::hibernate::Entry;

pub fn run_request(
    store: &Store,
    snapshot: &crate::store::snapshot::SidebarSnapshot,
    request: &HibernateHelperRequest,
    config: &MachineConfig,
    seen: &dyn Fn(&AgentState) -> bool,
) -> Result<(), HibernateErr> {
    if !cfg!(target_os = "linux") || !config.agents.hibernate.enabled {
        return Ok(());
    }
    let policy = Policy::new(config)?;
    let Some(agent) =
        crate::store::snapshot::find_agent(&snapshot.agents, &request.kind, &request.agent_id)
    else {
        return Ok(());
    };
    if !snapshot.agent_panes.iter().any(|pane| {
        pane.kind == request.kind
            && pane.agent_id.as_ref() == Some(&request.agent_id)
            && pane.pane_id == request.pane_id
            && pane.pane_pid == Some(request.root_pid)
    }) {
        return Ok(());
    }
    let scope = ScopeView::probe(
        &agent.kind,
        request.root_pid,
        seen(agent),
        snapshot.focused_pane.as_ref() == Some(&request.pane_id),
    );
    let verdict = decide(store, agent, &policy, jiff::Timestamp::now(), &scope)?;
    let idle_secs = match verdict {
        Verdict::Hibernate { idle_secs } => idle_secs,
        Verdict::Hold(hold @ (Hold::NoScope | Hold::ForeignProcess { .. })) => {
            report_hold(store.runtime_paths(), request, &format!("{hold:?}"), None);
            return Ok(());
        }
        Verdict::Hold(_) => return Ok(()),
    };
    // The Hibernate verdict proves that scope resolution and membership succeeded.
    let path = scope
        .scope
        .as_ref()
        .expect("Hibernate verdict requires a scope");
    let entry = Entry {
        kind: request.kind.clone(),
        agent_id: request.agent_id.clone(),
        pane_id: request.pane_id.clone(),
        scope: path.as_path().to_owned(),
        provider_pid: scope.provider_pid,
        frozen_at: jiff::Timestamp::now(),
        reclaimed_bytes: 0,
    };
    let result = freeze_and_record_with(store.paths(), entry, path, || cgroup::reclaim(path));
    match result {
        Ok(report) => {
            let record_path =
                fire_record_path(store.runtime_paths(), &request.kind, &request.agent_id);
            let mut pacing = read_fire_record(&record_path);
            pacing.hold = None;
            if let Err(error) =
                crate::disk::atomic::write_temp_then_rename_cache(&record_path, &pacing)
            {
                tracing::debug!(%error, "failed to clear hibernation hold pacing");
            }
            crate::harness::assist_log::append(&AssistRecord {
                at: jiff::Timestamp::now(),
                assist: Assist::Hibernate {
                    kind: request.kind.clone(),
                    agent_id: request.agent_id.clone(),
                    label: request.label.clone(),
                    idle_secs,
                    frozen: true,
                    reclaimed_bytes: Some(report.before.saturating_sub(report.after)),
                    hold: None,
                    error: None,
                },
            });
            Ok(())
        }
        Err(error) => {
            let reason = if matches!(error, HibernateErr::Cgroup(CgroupErr::FreezeTimeout)) {
                "FreezeTimeout".into()
            } else {
                error.to_string()
            };
            report_hold(
                store.runtime_paths(),
                request,
                &reason,
                Some(error.to_string()),
            );
            Err(error)
        }
    }
}

fn report_hold(
    runtime: &crate::RuntimePaths,
    request: &HibernateHelperRequest,
    reason: &str,
    error: Option<String>,
) {
    if !hold_changed(runtime, request, reason) {
        return;
    }
    crate::harness::assist_log::append(&AssistRecord {
        at: jiff::Timestamp::now(),
        assist: Assist::Hibernate {
            kind: request.kind.clone(),
            agent_id: request.agent_id.clone(),
            label: request.label.clone(),
            idle_secs: request.idle_secs,
            frozen: false,
            reclaimed_bytes: None,
            hold: Some(reason.into()),
            error,
        },
    });
}

fn freeze_and_record_with(
    paths: &crate::StatePaths,
    mut entry: Entry,
    scope: &ScopePath,
    reclaim: impl FnOnce() -> Result<ReclaimReport, CgroupErr>,
) -> Result<ReclaimReport, HibernateErr> {
    cgroup::freeze(scope)?;
    let result = (|| {
        let report = reclaim()?;
        entry.reclaimed_bytes = report.before.saturating_sub(report.after);
        entry.frozen_at = jiff::Timestamp::now();
        crate::store::hibernate::record(paths, entry)?;
        Ok(report)
    })();
    if result.is_err()
        && let Err(error) = cgroup::thaw(scope)
    {
        tracing::warn!(%error, "failed to thaw after unsuccessful hibernation");
    }
    result
}

fn hold_changed(
    runtime: &crate::RuntimePaths,
    request: &HibernateHelperRequest,
    reason: &str,
) -> bool {
    let path = fire_record_path(runtime, &request.kind, &request.agent_id);
    let mut record = read_fire_record(&path);
    if record.hold.as_deref() == Some(reason) {
        return false;
    }
    record.hold = Some(reason.into());
    if let Err(error) = crate::disk::atomic::write_temp_then_rename_cache(&path, &record) {
        tracing::debug!(%error, "failed to record hibernation hold pacing");
    }
    true
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn fixture() -> (
        tempfile::TempDir,
        crate::StatePaths,
        crate::RuntimePaths,
        ScopePath,
        Entry,
        HibernateHelperRequest,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let id = WorkspaceId::from_project_root(dir.path());
        let paths = crate::StatePaths::under(id.clone(), &dir.path().join("state")).unwrap();
        let runtime = crate::RuntimePaths::under(id.clone(), &dir.path().join("runtime")).unwrap();
        let uid = nix::unistd::Uid::current().as_raw();
        let scope = ScopePath::from_cgroup(
            dir.path(),
            &format!(
                "0::/user.slice/user-{uid}.slice/user@{uid}.service/app.slice/{}test.scope",
                scope_prefix(&id)
            ),
        )
        .unwrap();
        std::fs::create_dir_all(scope.as_path()).unwrap();
        std::fs::write(scope.as_path().join("cgroup.freeze"), "0").unwrap();
        std::fs::write(scope.as_path().join("cgroup.events"), "frozen 1\n").unwrap();
        let request = HibernateHelperRequest {
            workspace_id: id,
            kind: AgentKind::new_unchecked("claude"),
            agent_id: "session".into(),
            pane_id: "tmux:%3".parse().unwrap(),
            label: "@coder".into(),
            root_pid: 10,
            idle_secs: 600,
        };
        let entry = Entry {
            kind: request.kind.clone(),
            agent_id: request.agent_id.clone(),
            pane_id: request.pane_id.clone(),
            scope: scope.as_path().into(),
            provider_pid: 10,
            frozen_at: jiff::Timestamp::UNIX_EPOCH,
            reclaimed_bytes: 0,
        };
        (dir, paths, runtime, scope, entry, request)
    }

    #[test]
    fn record_is_published_only_after_freeze_and_successful_reclaim() {
        let (_dir, paths, _runtime, scope, entry, _request) = fixture();
        let report = freeze_and_record_with(&paths, entry, &scope, || {
            assert_eq!(
                std::fs::read_to_string(scope.as_path().join("cgroup.freeze")).unwrap(),
                "1"
            );
            assert!(crate::store::hibernate::read(&paths).is_empty());
            Ok(ReclaimReport {
                before: 400,
                after: 100,
            })
        })
        .unwrap();
        let records = crate::store::hibernate::read(&paths);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].reclaimed_bytes, 300);
        assert!(records[0].frozen_at > jiff::Timestamp::UNIX_EPOCH);
        assert_eq!(
            report,
            ReclaimReport {
                before: 400,
                after: 100
            }
        );
    }

    #[test]
    fn reclaim_and_publication_failures_thaw_without_a_record() {
        for publication in [false, true] {
            let (_dir, paths, _runtime, scope, entry, _request) = fixture();
            if publication {
                std::fs::create_dir_all(&paths.hibernation).unwrap();
            }
            let result = freeze_and_record_with(&paths, entry, &scope, || {
                if publication {
                    Ok(ReclaimReport {
                        before: 400,
                        after: 100,
                    })
                } else {
                    Err(std::io::Error::other("reclaim failed").into())
                }
            });
            assert!(result.is_err());
            assert_eq!(
                std::fs::read_to_string(scope.as_path().join("cgroup.freeze")).unwrap(),
                "0"
            );
            assert!(crate::store::hibernate::read(&paths).is_empty());
        }
    }

    #[test]
    fn assists_for_holds_are_paced_by_changes_in_reason() {
        let (_dir, _paths, runtime, _scope, _entry, request) = fixture();
        assert!(hold_changed(&runtime, &request, "NoScope"));
        assert!(!hold_changed(&runtime, &request, "NoScope"));
        assert!(hold_changed(&runtime, &request, "ForeignProcess"));
        assert!(!hold_changed(&runtime, &request, "ForeignProcess"));
        assert!(hold_changed(&runtime, &request, "NoScope"));
    }
}
