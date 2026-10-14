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
    mut refresh: impl FnMut() -> crate::store::snapshot::Result<crate::store::snapshot::SidebarSnapshot>,
) -> Result<(), HibernateErr> {
    if !cfg!(target_os = "linux")
        || !config.agents.hibernate.enabled()
        || !Admission::read(store.runtime_paths()).available
    {
        return Ok(());
    }
    let keep_warm = KeepWarmPolicy::load(config, snapshot.project_root.as_deref());
    let policy = Policy::new(config, &keep_warm)?;
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
        pane_focused(store.runtime_paths(), snapshot, &request.pane_id),
    );
    let verdict = decide(store, agent, &policy, jiff::Timestamp::now(), &scope)?;
    let frozen = matches!(verdict, Verdict::Hibernate { .. });
    let idle_secs = match verdict {
        Verdict::Deepen { idle_secs, .. } | Verdict::Hibernate { idle_secs } => idle_secs,
        Verdict::Hold(hold @ (Hold::NoScope | Hold::ForeignProcess { .. })) => {
            report_hold(store.runtime_paths(), request, &format!("{hold:?}"), None);
            return Ok(());
        }
        Verdict::Hold(_) => return Ok(()),
    };
    let result = if frozen {
        // The Hibernate verdict proves that scope resolution and membership succeeded.
        let path = scope
            .scope
            .as_ref()
            .expect("Hibernate verdict requires a scope");
        let provider_pid = scope.provider_pid;
        let entry = Entry {
            kind: request.kind.clone(),
            agent_id: request.agent_id.clone(),
            pane_id: request.pane_id.clone(),
            scope: path.as_path().to_owned(),
            provider_pid: scope.provider_pid,
            frozen_at: jiff::Timestamp::now(),
            anon_at_freeze: 0,
            step: 0,
            last_reclaimed_at: jiff::Timestamp::UNIX_EPOCH,
            reclaimed_bytes: 0,
        };
        freeze_and_record_with(
            store.paths(),
            entry,
            path,
            policy.schedule,
            || {
                let fresh = refresh().map_err(StoreErr::from)?;
                let Some(agent) = crate::store::snapshot::find_agent(
                    &fresh.agents,
                    &request.kind,
                    &request.agent_id,
                ) else {
                    return Ok(false);
                };
                let scope = ScopeView::probe(
                    &agent.kind,
                    request.root_pid,
                    pane_focused(store.runtime_paths(), &fresh, &request.pane_id),
                );
                if scope.scope.as_ref() != Some(path) || scope.provider_pid != provider_pid {
                    return Ok(false);
                }
                Ok(matches!(
                    decide(store, agent, &policy, jiff::Timestamp::now(), &scope)?,
                    Verdict::Hibernate { .. }
                ))
            },
            |bytes| cgroup::reclaim(path, bytes),
        )
    } else {
        let Some(entry) = crate::store::hibernate::read(store.paths())
            .into_iter()
            .find(|entry| entry.kind == request.kind && entry.agent_id == request.agent_id)
        else {
            return Ok(());
        };
        let path = super::wake::recorded_scope(Path::new("/sys/fs/cgroup"), &entry.scope)?;
        deepen_and_record_with(
            store.paths(),
            &entry,
            &path,
            policy.schedule,
            jiff::Timestamp::now(),
            |bytes| cgroup::reclaim(&path, bytes),
        )
    };
    match result {
        Ok(None) => Ok(()),
        Ok(Some(report)) => {
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
                    frozen,
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

fn pane_focused(
    runtime: &crate::RuntimePaths,
    snapshot: &crate::store::snapshot::SidebarSnapshot,
    pane: &PaneId,
) -> bool {
    snapshot.focused_pane.as_ref() == Some(pane)
        || crate::mux::focus_anchor::load(runtime).is_some_and(|anchor| {
            &anchor.pane_id == pane
                && crate::mux::focus_anchor::is_fresh(
                    anchor.applied_at_ms.unwrap_or(anchor.issued_at_ms),
                    crate::utils::time::unix_now_ms(),
                )
        })
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
    schedule: crate::config::Schedule,
    recheck: impl FnOnce() -> Result<bool, HibernateErr>,
    reclaim: impl FnOnce(u64) -> Result<ReclaimReport, CgroupErr>,
) -> Result<Option<ReclaimReport>, HibernateErr> {
    let _guard = crate::disk::lock::WorkspaceLock::acquire(&paths.lock_path("hibernate.lock"))
        .map_err(StoreErr::from)?;
    if !recheck()? {
        return Ok(None);
    }
    cgroup::freeze(scope)?;
    let result = (|| {
        entry.anon_at_freeze = cgroup::memory_stat(scope)?.anon;
        let report = reclaim(
            entry
                .anon_at_freeze
                .saturating_sub(schedule.target(entry.anon_at_freeze, 0)),
        )?;
        entry.reclaimed_bytes = report.before.saturating_sub(report.after);
        entry.frozen_at = jiff::Timestamp::now();
        entry.last_reclaimed_at = entry.frozen_at;
        crate::store::hibernate::record(paths, entry)?;
        Ok(Some(report))
    })();
    if result.is_err()
        && let Err(error) = cgroup::thaw(scope)
    {
        tracing::warn!(%error, "failed to thaw after unsuccessful hibernation");
    }
    result
}

fn deepen_and_record_with(
    paths: &crate::StatePaths,
    expected: &Entry,
    scope: &ScopePath,
    schedule: crate::config::Schedule,
    now: jiff::Timestamp,
    reclaim: impl FnOnce(u64) -> Result<ReclaimReport, CgroupErr>,
) -> Result<Option<ReclaimReport>, HibernateErr> {
    let _guard = crate::disk::lock::WorkspaceLock::acquire(&paths.lock_path("hibernate.lock"))
        .map_err(StoreErr::from)?;
    let Some(mut entry) = crate::store::hibernate::read(paths)
        .into_iter()
        .find(|entry| entry.kind == expected.kind && entry.agent_id == expected.agent_id)
    else {
        return Ok(None);
    };
    if entry.scope != expected.scope || entry.frozen_at != expected.frozen_at {
        return Ok(None);
    }
    if !super::wake::is_frozen(scope)? {
        crate::store::hibernate::withdraw(paths, &entry.kind, &entry.agent_id)?;
        return Ok(None);
    }
    let Some(step) = next_step(&entry, schedule, now) else {
        return Ok(None);
    };
    let anon = cgroup::memory_stat(scope)?.anon;
    let report = reclaim(anon.saturating_sub(schedule.target(entry.anon_at_freeze, step)))?;
    if !super::wake::is_frozen(scope)? {
        crate::store::hibernate::withdraw(paths, &entry.kind, &entry.agent_id)?;
        return Ok(None);
    }
    entry.step = step;
    entry.last_reclaimed_at = now;
    entry.reclaimed_bytes = entry
        .reclaimed_bytes
        .saturating_add(report.before.saturating_sub(report.after));
    crate::store::hibernate::record(paths, entry)?;
    Ok(Some(report))
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

    #[test]
    fn current_focus_intent_holds_before_the_published_frame_catches_up() {
        use crate::mux::focus_anchor::{FocusAnchor, FocusIntentState, FocusOrigin};
        let dir = tempfile::tempdir().unwrap();
        let id = WorkspaceId::from_project_root(dir.path());
        let runtime = crate::RuntimePaths::under(id.clone(), dir.path()).unwrap();
        let pane: PaneId = "tmux:%3".parse().unwrap();
        let now = crate::utils::time::unix_now_ms();
        let snapshot = crate::store::snapshot::SidebarSnapshot::build_with_agents(
            id,
            vec![],
            jiff::Timestamp::now(),
        );
        let anchor = FocusAnchor {
            nonce: crate::ids::FocusNonce::new(),
            session_name: "room".into(),
            pane_id: pane.clone(),
            origin: FocusOrigin::User,
            repair_generation: None,
            issued_at_ms: now,
            applied_at_ms: None,
            state: FocusIntentState::Requested,
            pre_action: vec![],
            offset: 0,
            order: None,
        };
        crate::mux::focus_anchor::store(&runtime, &anchor).unwrap();
        assert!(
            pane_focused(&runtime, &snapshot, &pane),
            "locked decision must see focus intent absent from the raw store snapshot"
        );
    }

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
        std::fs::write(
            scope.as_path().join("memory.stat"),
            "anon 600\nfile 20\nworkingset_refault_anon 0\nworkingset_refault_file 0\n",
        )
        .unwrap();
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
            anon_at_freeze: 0,
            step: 0,
            last_reclaimed_at: jiff::Timestamp::UNIX_EPOCH,
            reclaimed_bytes: 0,
        };
        (dir, paths, runtime, scope, entry, request)
    }

    #[test]
    fn wake_during_reclaim_waits_for_publication_and_thaws() {
        let (dir, paths, _runtime, scope, entry, _request) = fixture();
        std::thread::scope(|threads| {
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let pane = entry.pane_id.clone();
            freeze_and_record_with(
                &paths,
                entry,
                &scope,
                HibernateConfig::default().schedule().unwrap(),
                || Ok(true),
                |_| {
                    let paths = &paths;
                    let root = dir.path();
                    threads.spawn(move || {
                        started_tx.send(()).unwrap();
                        let result = super::super::wake::wake_pane_in(paths, &pane, root);
                        done_tx.send(result).unwrap();
                    });
                    started_rx.recv().unwrap();
                    // Let the wake reach the transaction while reclaim is still in flight.
                    assert!(
                        done_rx
                            .recv_timeout(std::time::Duration::from_millis(100))
                            .is_err()
                    );
                    Ok(ReclaimReport {
                        before: 400,
                        after: 100,
                    })
                },
            )
            .unwrap();
            assert!(matches!(
                done_rx.recv().unwrap().unwrap(),
                super::super::wake::Woken::Thawed { .. }
            ));
        });
        assert_eq!(
            std::fs::read_to_string(scope.as_path().join("cgroup.freeze")).unwrap(),
            "0"
        );
        assert!(crate::store::hibernate::read(&paths).is_empty());
    }

    #[test]
    fn record_is_published_only_after_freeze_and_successful_reclaim() {
        let (_dir, paths, _runtime, scope, entry, _request) = fixture();
        let report = freeze_and_record_with(
            &paths,
            entry,
            &scope,
            HibernateConfig::default().schedule().unwrap(),
            || Ok(true),
            |bytes| {
                assert_eq!(bytes, 300);
                assert_eq!(
                    std::fs::read_to_string(scope.as_path().join("cgroup.freeze")).unwrap(),
                    "1"
                );
                assert!(crate::store::hibernate::read(&paths).is_empty());
                Ok(ReclaimReport {
                    before: 400,
                    after: 100,
                })
            },
        )
        .unwrap()
        .unwrap();
        let records = crate::store::hibernate::read(&paths);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].reclaimed_bytes, 300);
        assert_eq!(records[0].anon_at_freeze, 600);
        assert_eq!(records[0].step, 0);
        assert_eq!(records[0].last_reclaimed_at, records[0].frozen_at);
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
            let result = freeze_and_record_with(
                &paths,
                entry,
                &scope,
                HibernateConfig::default().schedule().unwrap(),
                || Ok(true),
                |_| {
                    if publication {
                        Ok(ReclaimReport {
                            before: 400,
                            after: 100,
                        })
                    } else {
                        Err(std::io::Error::other("reclaim failed").into())
                    }
                },
            );
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

    #[test]
    fn deepen_advances_once_per_due_step_and_preserves_the_stretch() {
        let (_dir, paths, _runtime, scope, mut entry, _request) = fixture();
        entry.anon_at_freeze = 600;
        entry.reclaimed_bytes = 300;
        crate::store::hibernate::record(&paths, entry.clone()).unwrap();
        let schedule = HibernateConfig::default().schedule().unwrap();
        for (step, seconds, bytes) in [(1, 1800, 400), (2, 3600, 500), (3, 5400, 600)] {
            let now = jiff::Timestamp::from_second(seconds).unwrap();
            assert!(
                deepen_and_record_with(&paths, &entry, &scope, schedule, now, |requested| {
                    assert_eq!(requested, bytes);
                    Ok(ReclaimReport {
                        before: 100,
                        after: 50,
                    })
                })
                .unwrap()
                .is_some()
            );
            let recorded = crate::store::hibernate::read(&paths).remove(0);
            assert_eq!(recorded.step, step);
            assert_eq!(recorded.reclaimed_bytes, 300 + 50 * u64::from(step));
            assert_eq!(recorded.frozen_at, entry.frozen_at);
            assert_eq!(recorded.last_reclaimed_at, now);
            assert!(
                deepen_and_record_with(&paths, &entry, &scope, schedule, now, |_| panic!(
                    "duplicate step"
                ))
                .unwrap()
                .is_none()
            );
        }
    }

    #[test]
    fn equal_schedule_reclaims_all_at_freeze_and_zero_requests_advance() {
        let (_dir, paths, _runtime, scope, entry, _request) = fixture();
        let immediate = HibernateConfig {
            after: "30m".into(),
            deep_after: "30m".into(),
            ..Default::default()
        }
        .schedule()
        .unwrap();
        freeze_and_record_with(
            &paths,
            entry,
            &scope,
            immediate,
            || Ok(true),
            |bytes| {
                assert_eq!(bytes, 600);
                Ok(ReclaimReport {
                    before: 600,
                    after: 0,
                })
            },
        )
        .unwrap();
        let mut entry = crate::store::hibernate::read(&paths).remove(0);
        entry.frozen_at = jiff::Timestamp::UNIX_EPOCH;
        crate::store::hibernate::record(&paths, entry.clone()).unwrap();
        std::fs::write(
            scope.as_path().join("memory.stat"),
            "anon 0\nfile 20\nworkingset_refault_anon 0\nworkingset_refault_file 0\n",
        )
        .unwrap();
        assert!(
            deepen_and_record_with(
                &paths,
                &entry,
                &scope,
                HibernateConfig::default().schedule().unwrap(),
                jiff::Timestamp::from_second(1800).unwrap(),
                |bytes| {
                    assert_eq!(bytes, 0);
                    Ok(ReclaimReport {
                        before: 20,
                        after: 20,
                    })
                }
            )
            .unwrap()
            .is_some()
        );
        assert_eq!(crate::store::hibernate::read(&paths)[0].step, 1);
    }

    #[test]
    fn deepen_does_not_republish_after_a_wake_during_reclaim() {
        let (_dir, paths, _runtime, scope, entry, _request) = fixture();
        crate::store::hibernate::record(&paths, entry.clone()).unwrap();
        let result = deepen_and_record_with(
            &paths,
            &entry,
            &scope,
            HibernateConfig::default().schedule().unwrap(),
            jiff::Timestamp::from_second(5400).unwrap(),
            |_| {
                std::fs::write(scope.as_path().join("cgroup.events"), "frozen 0\n").unwrap();
                crate::store::hibernate::withdraw(&paths, &entry.kind, &entry.agent_id).unwrap();
                Ok(ReclaimReport {
                    before: 600,
                    after: 0,
                })
            },
        )
        .unwrap();
        assert!(
            result.is_none(),
            "a thawed scope must not complete a deepen"
        );
        assert!(crate::store::hibernate::read(&paths).is_empty());
    }

    #[test]
    fn deepen_withdraws_stale_entries_and_never_recreates_a_woken_entry() {
        for state in ["thawed", "missing", "withdrawn"] {
            let (_dir, paths, _runtime, scope, entry, _request) = fixture();
            crate::store::hibernate::record(&paths, entry.clone()).unwrap();
            match state {
                "thawed" => {
                    std::fs::write(scope.as_path().join("cgroup.events"), "frozen 0\n").unwrap()
                }
                "missing" => std::fs::remove_dir_all(scope.as_path()).unwrap(),
                _ => {
                    crate::store::hibernate::withdraw(&paths, &entry.kind, &entry.agent_id)
                        .unwrap();
                }
            }
            assert!(
                deepen_and_record_with(
                    &paths,
                    &entry,
                    &scope,
                    HibernateConfig::default().schedule().unwrap(),
                    jiff::Timestamp::from_second(1800).unwrap(),
                    |_| panic!("stale entry reclaimed")
                )
                .unwrap()
                .is_none()
            );
            assert!(crate::store::hibernate::read(&paths).is_empty(), "{state}");
        }
    }
}
