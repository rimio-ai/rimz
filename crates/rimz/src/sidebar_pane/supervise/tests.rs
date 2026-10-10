use super::*;

/// A link whose host says each of `controls` and then closes.
fn link_to_a_host_saying(controls: Vec<Control>) -> HostLink {
    use crate::sidebar_pane::attach::{self, ControlLine, Reply};
    use std::os::fd::AsFd;

    let (pane, host) = std::os::unix::net::UnixStream::pair().unwrap();
    let config = crate::sidebar_pane::app::fixtures::serve_config(
        &crate::sidebar_pane::app::fixtures::workspace(),
    );
    thread::spawn(move || {
        let _hello = attach::recv_hello(&host).unwrap().unwrap();
        attach::write_line(&host, &Reply::Accept { build: None }).unwrap();
        for control in controls {
            attach::write_line(&host, &ControlLine { control }).unwrap();
        }
    });
    let hello = host_link::hello_for(&config, None);
    let output = std::fs::File::create("/dev/null").unwrap();
    HostLink::open(pane, &hello, output.as_fd(), Duration::from_secs(10)).unwrap()
}

fn watch(mut link: HostLink) -> RoundExit {
    let workspace = crate::sidebar_pane::app::fixtures::workspace();
    watch_host(
        &mut link,
        HostMonitor {
            record_watch: &mut RecordWatch::new(&workspace),
            exec_state: &mut PendingExec::default(),
            supervisor_build: None,
            started: Instant::now(),
            watchdog: &mut None,
            stopped: &AtomicBool::new(false),
        },
    )
}

#[test]
fn a_hosts_self_close_requires_confirmation() {
    assert_eq!(
        watch(link_to_a_host_saying(vec![Control::SelfClose])),
        RoundExit::ConfirmSelfClose
    );
}

#[test]
fn a_hosts_reload_ends_the_round() {
    assert_eq!(
        watch(link_to_a_host_saying(vec![Control::Reload])),
        RoundExit::Reload
    );
}

#[test]
fn a_host_that_ends_without_a_word_is_lost_and_attached_to_again() {
    assert_eq!(
        watch(link_to_a_host_saying(Vec::new())),
        RoundExit::HostLost
    );
}

#[test]
fn respawn_backoff_doubles_caps_and_resets_after_a_stable_run() {
    assert_eq!(
        respawn_backoff(Duration::from_secs(1), Duration::from_secs(2)),
        (Duration::from_secs(1), Duration::from_secs(2))
    );
    assert_eq!(
        respawn_backoff(Duration::from_secs(60), Duration::from_secs(2)),
        (Duration::from_secs(60), Duration::from_secs(60))
    );
    assert_eq!(
        respawn_backoff(Duration::from_secs(32), RESPAWN_STABLE_RUN),
        (Duration::from_secs(1), Duration::from_secs(2))
    );
}

#[test]
fn pane_watchdog_requires_three_fresh_absences() {
    let mut watchdog = PaneWatchdog {
        pane: crate::ids::PaneId::from_parts(crate::ids::MuxName::Tmux, "%1"),
        mux: crate::ids::MuxName::Tmux,
        session_name: "rimz-test".to_owned(),
        workspace_id: crate::ids::WorkspaceId::from_project_root(Path::new("/repo")),
        next_probe: Instant::now(),
        strikes: 0,
        last_observed_at_ms: None,
    };

    assert!(!watchdog.observe(PaneProbe::Absent(1)));
    assert!(!watchdog.observe(PaneProbe::Absent(1)));
    assert_eq!(watchdog.strikes, 1, "a cached absence counts once");
    assert!(!watchdog.observe(PaneProbe::Unknown));
    assert_eq!(watchdog.strikes, 1);
    assert!(!watchdog.observe(PaneProbe::Present(2)));
    assert_eq!(watchdog.strikes, 0);
    assert!(!watchdog.observe(PaneProbe::Absent(3)));
    assert!(!watchdog.observe(PaneProbe::Absent(4)));
    assert!(watchdog.observe(PaneProbe::Absent(5)));
}

#[test]
fn pane_watchdog_requires_authoritative_mux_truth() {
    let watchdog = PaneWatchdog {
        pane: crate::ids::PaneId::from_parts(crate::ids::MuxName::Zellij, "terminal_9"),
        mux: crate::ids::MuxName::Zellij,
        session_name: "rimz-test".to_owned(),
        workspace_id: crate::ids::WorkspaceId::from_project_root(Path::new("/repo")),
        next_probe: Instant::now(),
        strikes: 0,
        last_observed_at_ms: None,
    };

    let options = watchdog.probe_options();
    assert_eq!(
        options.consistency,
        crate::mux::PaneReadConsistency::RequireAuthoritative
    );
    assert_eq!(options.command_timeout, Some(PANE_PROBE_TIMEOUT));
}

#[test]
fn pane_watchdog_presence_ladder_escalates_only_on_suspicion() {
    let pane = crate::ids::PaneId::from_parts(crate::ids::MuxName::Zellij, "terminal_9");
    let roster = crate::mux::CachedPaneRoster {
        pane_ids: vec![pane.clone()],
        observed_at_ms: 42,
    };
    let escalations = std::cell::Cell::new(0);
    let escalate = || {
        escalations.set(escalations.get() + 1);
        PaneProbe::Absent(43)
    };

    assert_eq!(
        ladder_probe(&pane, Some(&roster), escalate),
        PaneProbe::Present(42),
    );
    assert_eq!(escalations.get(), 0);

    let missing = crate::mux::CachedPaneRoster {
        pane_ids: Vec::new(),
        observed_at_ms: 44,
    };
    assert_eq!(
        ladder_probe(&pane, Some(&missing), escalate),
        PaneProbe::Absent(43),
    );
    assert_eq!(ladder_probe(&pane, None, escalate), PaneProbe::Absent(43));
    assert_eq!(escalations.get(), 2);
}

#[test]
fn self_close_verdict_requires_authoritative_view_emptiness() {
    let own_id = crate::ids::PaneId::from_parts(crate::ids::MuxName::Tmux, "%1");
    let sibling_id = crate::ids::PaneId::from_parts(crate::ids::MuxName::Tmux, "%2");
    let pane = |pane_id: crate::ids::PaneId, view_id: Option<&str>, is_floating: bool| {
        crate::pane::PaneRef {
            pane_id,
            session_name: "rimz-test".to_owned(),
            view_id: view_id.map(str::to_owned),
            is_floating,
            ..crate::pane::PaneRef::from_id(crate::ids::PaneId::from_parts(
                crate::ids::MuxName::Tmux,
                "%unused",
            ))
        }
    };

    assert_eq!(self_close_verdict(&[], &own_id), SelfCloseVerdict::PaneGone);
    assert!(matches!(
        self_close_verdict(&[pane(own_id.clone(), None, false)], &own_id),
        SelfCloseVerdict::Keep { .. }
    ));
    assert_eq!(
        self_close_verdict(&[pane(own_id.clone(), Some("@1"), false)], &own_id),
        SelfCloseVerdict::Empty {
            floating_siblings: 0
        }
    );
    assert!(matches!(
        self_close_verdict(
            &[
                pane(own_id.clone(), Some("@1"), false),
                pane(sibling_id.clone(), Some("@1"), false),
            ],
            &own_id,
        ),
        SelfCloseVerdict::Keep { siblings: 1, .. }
    ));
    assert_eq!(
        self_close_verdict(
            &[
                pane(own_id.clone(), Some("@1"), false),
                pane(sibling_id, Some("@1"), true),
            ],
            &own_id,
        ),
        SelfCloseVerdict::Empty {
            floating_siblings: 1
        }
    );
}

#[test]
fn self_close_accepts_only_reproduced_pane_absence() {
    assert_eq!(
        reconfirm_pane_gone(|| Ok(SelfCloseVerdict::PaneGone), || {}),
        SelfCloseConfirmation::PaneGone
    );

    assert_eq!(
        reconfirm_pane_gone(
            || {
                Ok(SelfCloseVerdict::Empty {
                    floating_siblings: 0,
                })
            },
            || {},
        ),
        SelfCloseConfirmation::Keep {
            siblings: 0,
            reason: "authoritative absence not reproduced".to_owned(),
        }
    );

    assert_eq!(
        reconfirm_pane_gone(|| Err("mux timed out".to_owned()), || {}),
        SelfCloseConfirmation::Keep {
            siblings: 0,
            reason: "pane-gone reconfirmation probe failed: mux timed out".to_owned(),
        }
    );
}

#[test]
fn authoritative_probe_is_shared_across_consumers() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = crate::ids::WorkspaceId::from_project_root(dir.path());
    let runtime = crate::RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let pane = crate::ids::PaneId::from_parts(crate::ids::MuxName::Tmux, "%1");
    let watchdog = PaneWatchdog {
        pane: pane.clone(),
        mux: crate::ids::MuxName::Tmux,
        session_name: "rimz-test".to_owned(),
        workspace_id,
        next_probe: Instant::now(),
        strikes: 0,
        last_observed_at_ms: None,
    };
    let calls = std::cell::Cell::new(0);
    let observed_at_ms = crate::utils::time::unix_now_ms();
    let first = shared_authoritative_pane_probe(&watchdog, &runtime, || {
        calls.set(calls.get() + 1);
        Some(AuthoritativePaneProbe {
            mux: crate::ids::MuxName::Tmux,
            session_name: "rimz-test".to_owned(),
            observed_at_ms,
            pane_ids: vec![pane],
        })
    });
    let second = shared_authoritative_pane_probe(&watchdog, &runtime, || {
        calls.set(calls.get() + 1);
        None
    });

    assert_eq!(first, PaneProbe::Present(observed_at_ms));
    assert_eq!(second, first);
    assert_eq!(calls.get(), 1, "one producer feeds every consumer");
}

#[test]
fn authoritative_probe_rejects_malformed_and_mismatched_cache() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = crate::ids::WorkspaceId::from_project_root(dir.path());
    let runtime = crate::RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let watchdog = PaneWatchdog {
        pane: crate::ids::PaneId::from_parts(crate::ids::MuxName::Zellij, "terminal_1"),
        mux: crate::ids::MuxName::Zellij,
        session_name: "rimz-test".to_owned(),
        workspace_id,
        next_probe: Instant::now(),
        strikes: 0,
        last_observed_at_ms: None,
    };
    std::fs::write(runtime.authoritative_pane_probe_path(), b"not json").unwrap();
    assert!(
        read_authoritative_pane_probe(&runtime, &watchdog, crate::utils::time::unix_now_ms())
            .is_none()
    );

    crate::sidebar::cache::write_authoritative_pane_probe(
        &runtime,
        &AuthoritativePaneProbe {
            mux: crate::ids::MuxName::Tmux,
            session_name: "other".to_owned(),
            observed_at_ms: crate::utils::time::unix_now_ms(),
            pane_ids: Vec::new(),
        },
    )
    .unwrap();
    assert!(
        read_authoritative_pane_probe(&runtime, &watchdog, crate::utils::time::unix_now_ms())
            .is_none()
    );

    crate::sidebar::cache::write_authoritative_pane_probe(
        &runtime,
        &AuthoritativePaneProbe {
            mux: watchdog.mux,
            session_name: watchdog.session_name.clone(),
            observed_at_ms: 1,
            pane_ids: Vec::new(),
        },
    )
    .unwrap();
    assert!(read_authoritative_pane_probe(&runtime, &watchdog, 60_001).is_none());
}

#[test]
fn authoritative_probe_cache_accepts_both_mux_identities() {
    for (mux, pane_raw) in [
        (crate::ids::MuxName::Zellij, "terminal_1"),
        (crate::ids::MuxName::Tmux, "%1"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = crate::ids::WorkspaceId::from_project_root(dir.path());
        let runtime = crate::RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let pane = crate::ids::PaneId::from_parts(mux, pane_raw);
        let watchdog = PaneWatchdog {
            pane: pane.clone(),
            mux,
            session_name: "rimz-test".to_owned(),
            workspace_id,
            next_probe: Instant::now(),
            strikes: 0,
            last_observed_at_ms: None,
        };
        crate::sidebar::cache::write_authoritative_pane_probe(
            &runtime,
            &AuthoritativePaneProbe {
                mux,
                session_name: "rimz-test".to_owned(),
                observed_at_ms: 10,
                pane_ids: vec![pane],
            },
        )
        .unwrap();

        let probe = read_authoritative_pane_probe(&runtime, &watchdog, 11).unwrap();
        assert_eq!(
            pane_probe_for(&probe, &watchdog.pane),
            PaneProbe::Present(10)
        );
    }
}

#[test]
fn host_spawn_prefers_the_durable_target_even_for_matching_bytes() {
    let durable = std::path::PathBuf::from("/state/rimz/builds/same/rimz");
    let ephemeral = std::path::PathBuf::from("/tmp/build/rimz (deleted)");
    assert_eq!(
        host_executable(
            crate::reload::WorkspaceReexecTarget::Verified(crate::reload::StagedBuild {
                path: durable.clone(),
                build: "same".to_owned(),
            }),
            ephemeral,
        ),
        durable,
    );
}

#[test]
fn record_change_requires_a_new_mtime_and_preserves_verification() {
    let prior = SystemTime::UNIX_EPOCH;
    let next = prior + Duration::from_secs(1);
    let target = crate::reload::StagedBuild {
        path: PathBuf::from("/state/builds/next/rimz"),
        build: "next".to_owned(),
    };
    assert_eq!(
        record_change(
            Some(prior),
            Some(prior),
            crate::reload::WorkspaceReexecTarget::Verified(target.clone()),
        ),
        None,
    );
    assert_eq!(
        record_change(
            Some(prior),
            Some(next),
            crate::reload::WorkspaceReexecTarget::Verified(target.clone()),
        ),
        Some(RecordChange::Verified(target)),
    );
    assert_eq!(
        record_change(
            Some(prior),
            Some(next),
            crate::reload::WorkspaceReexecTarget::Invalid,
        ),
        Some(RecordChange::Unavailable),
    );
}

#[test]
fn pending_exec_waits_for_the_painting_build_stability_window() {
    let target = crate::reload::StagedBuild {
        path: PathBuf::from("/state/builds/new/rimz"),
        build: "new".to_owned(),
    };
    let mut pending = PendingExec::default();
    pending.observe(
        &crate::reload::WorkspaceReexecTarget::Verified(target.clone()),
        Some("old"),
    );

    assert!(
        pending
            .promotable(Some("old"), RESPAWN_STABLE_RUN, RESPAWN_STABLE_RUN)
            .is_none(),
        "the old painting build cannot promote the new supervisor",
    );
    assert!(
        pending
            .promotable(
                Some("new"),
                RESPAWN_STABLE_RUN - Duration::from_millis(1),
                RESPAWN_STABLE_RUN,
            )
            .is_none(),
        "the new painting build must first serve stably",
    );
    assert_eq!(
        pending.promotable(Some("new"), RESPAWN_STABLE_RUN, RESPAWN_STABLE_RUN),
        Some(target),
    );
}

#[test]
fn pending_exec_resets_when_the_record_changes() {
    let target = |build: &str| {
        crate::reload::WorkspaceReexecTarget::Verified(crate::reload::StagedBuild {
            path: PathBuf::from(format!("/state/builds/{build}/rimz")),
            build: build.to_owned(),
        })
    };
    let mut pending = PendingExec::default();
    pending.observe(&target("first"), Some("old"));
    pending.reject("first");
    pending.observe(&target("first"), Some("old"));
    assert!(
        pending.target.is_none(),
        "a rejected build waits for a new record"
    );

    pending.observe(&target("second"), Some("old"));
    assert_eq!(
        pending.target.as_ref().map(|target| target.build.as_str()),
        Some("second")
    );
    assert_eq!(pending.rejected_build, None);
}
