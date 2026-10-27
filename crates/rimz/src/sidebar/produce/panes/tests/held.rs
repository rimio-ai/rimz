use super::*;
use std::time::Instant;

fn assert_held_start_census(starts: &[i64], carried_due: Option<i64>, expected: Option<i64>) {
    let mut a = pane("terminal_1", Some("claude"), Some("/repo"));
    a.pane_pid = Some(1);
    let mut b = pane("terminal_2", Some("claude"), Some("/repo"));
    b.pane_pid = Some(2);
    let mut prior = frame(vec![a.clone(), b.clone()]);
    first_mut(&mut prior).current.started_at = Some(jiff::Timestamp::from_second(10).unwrap());
    prior
        .pane_states_mut()
        .find(|pane| pane.current.pid == Some(2))
        .unwrap()
        .current
        .started_at = carried_due.map(|second| jiff::Timestamp::from_second(second).unwrap());
    let skipped = HashSet::from([first(&prior).pane_id.clone()]);
    let mut fresh = frame(vec![a, b]);
    let readers = RepairReaders {
        cmdline: &|_| Some("claude".into()),
        comm: &|_| None,
        children: &|_| Vec::new(),
        hosted_process: &|_| None,
        cwd: &|_| None,
        resumed_session: &|_| None,
        root_start: &|kind, pid| {
            assert_eq!((kind, pid), ("claude", 2), "only the due root is probed");
            None
        },
        cwd_starts: &|kind, cwd| {
            assert_eq!((kind, cwd), ("claude", "/repo"));
            starts
                .iter()
                .map(|second| jiff::Timestamp::from_second(*second).unwrap())
                .collect()
        },
        hosted_absent: &|_, _| false,
        process_start: &|_| None,
        elevated_agent: &|_| None,
        backfill_pids: &|_, _| {},
    };
    repair_pane_frame(
        &mut fresh,
        Some(&prior),
        "s",
        None,
        &skipped,
        &skipped,
        &readers,
    );
    assert_eq!(first(&fresh).current, first(&prior).current);
    assert_eq!(
        fresh
            .pane_states()
            .find(|pane| pane.current.pid == Some(2))
            .unwrap()
            .current
            .started_at,
        expected.map(|second| jiff::Timestamp::from_second(second).unwrap()),
        "the held pane's start must remain accounted for"
    );
}

#[test]
fn held_start_is_not_assigned_to_a_due_sibling() {
    assert_held_start_census(&[10], None, None);
}

#[test]
fn only_the_unaccounted_start_is_assigned_to_a_due_sibling() {
    assert_held_start_census(&[10, 20], None, Some(20));
}

#[test]
fn a_due_carried_duplicate_is_cleared_beside_a_held_start() {
    assert_held_start_census(&[10], Some(10), None);
}

#[test]
fn rejected_candidates_do_not_hold_the_next_tenant_to_the_prior_process() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = crate::RuntimePaths::under(
        crate::WorkspaceId::from_project_root(dir.path()),
        dir.path(),
    )
    .unwrap();
    runtime.ensure_dirs().unwrap();
    let mut old = pane("terminal_7", Some("zsh"), Some("/repo"));
    old.pane_pid = Some(u32::MAX - 1);
    let own = pane("terminal_8", Some("rimz sidebar serve"), Some("/repo"));
    let own_id = own.pane_id.clone();
    let prior = frame(vec![old, own]);
    let held_id = first(&prior).pane_id.clone();
    let mut producer = PaneProducer::seeded(&runtime, "s");
    producer.hibernated_panes = HashSet::from([held_id.clone()]);
    producer
        .native_tenants
        .insert(held_id.clone(), (u32::MAX - 1, None));
    producer.held_checks.insert(held_id.clone(), Instant::now());
    producer.prior = Some(Arc::new(prior));
    let diag = crate::diag::DiagSink::disabled();
    for include_own in [false, true] {
        let mut panes = vec![serde_json::json!({
            "id": 7, "tab_position": 0, "pane_command": "zsh",
            "pane_cwd": "/repo", "pane_pid": u32::MAX
        })];
        if include_own {
            panes.push(serde_json::json!({
                "id": 8, "tab_position": 0, "pane_command": "rimz sidebar serve",
                "pane_cwd": "/repo"
            }));
        }
        let topology = serde_json::from_value(serde_json::json!({
            "session_name": "s", "produced_at_ms": unix_now_ms().saturating_add(60_000),
            "focused_pane": 7, "clients": { "human_clients": 1, "viewed_panes": [7] },
            "panes": panes
        }))
        .unwrap();
        crate::mux::zellij::pane_topology::write_pane_topology_cache(&runtime, &topology).unwrap();
        let next = producer
            .tick_with(
                &runtime,
                MuxName::Zellij,
                "s",
                Some(u64::MAX),
                Some(&own_id),
                &diag,
                &mut |ctx, prior| {
                    let (mut candidate, _) = ctx.candidate(prior, true, None, false)?;
                    if !include_own {
                        assert!(matches!(
                            frame_publish_verdict(&candidate, Some(&own_id)),
                            PublishVerdict::Reject(FrameRejectReason::MissingOwnPane)
                        ));
                        candidate = ctx.candidate(prior, true, None, false)?.0;
                        assert!(matches!(
                            frame_publish_verdict(&candidate, Some(&own_id)),
                            PublishVerdict::Reject(FrameRejectReason::MissingOwnPane)
                        ));
                    }
                    accept(candidate, prior.cloned(), Some(&own_id), &diag)
                },
                &|_, _| None,
            )
            .unwrap();
        assert_eq!(
            next.pane_states()
                .find(|pane| pane.pane_id == held_id)
                .unwrap()
                .current
                .pid,
            Some(if include_own { u32::MAX } else { u32::MAX - 1 }),
            "a rejected candidate cannot authorize copying the previous tenant"
        );
    }
}

#[test]
fn production_repair_reads_only_due_pane_trees_at_each_fleet_size() {
    let mut unheld_counts = Vec::new();
    for held_count in [2, 20] {
        let panes: Vec<_> = (100..=100 + held_count)
            .map(|pid| {
                let mut pane = pane(&format!("terminal_{pid}"), Some("sudo claude"), None);
                pane.pane_pid = Some(pid);
                pane.spawn_command = Some("claude".into());
                pane.title = Some("fresh listing".into());
                pane
            })
            .collect();
        let mut prior = frame(panes.clone());
        for pane in prior.pane_states_mut() {
            let start = jiff::Timestamp::from_second(pane.current.pid.unwrap().into()).unwrap();
            pane.current.started_at = Some(start);
            pane.current.hosted_agent_kind = Some(AgentKind::new_unchecked("claude"));
            pane.current.hosted_agent_process_start = Some(start);
            pane.previous = Some(pane.current.clone());
            pane.children = vec![900];
            pane.metrics.rss_kb = Some(22);
            pane.first_seen_at_ms = Some(7);
            pane.hosted_carry_since_ms = Some(9);
        }
        let unheld_pid = 100 + held_count;
        let held: HashSet<_> = prior
            .pane_states()
            .filter(|pane| pane.current.pid != Some(unheld_pid))
            .map(|pane| pane.pane_id.clone())
            .collect();
        let reads = std::cell::RefCell::new(Vec::new());
        let record = |pid: u32| reads.borrow_mut().push(pid);
        let cmdline = |pid| {
            record(pid);
            None
        };
        let comm = |pid| {
            record(pid);
            None
        };
        let children = |pid| {
            record(pid);
            Vec::new()
        };
        let hosted_process = |pid| {
            record(pid);
            None
        };
        let cwd = |pid| {
            record(pid);
            None
        };
        let resumed_session = |pid| {
            record(pid);
            None
        };
        let root_start = |_: &str, pid| {
            record(pid);
            Some(jiff::Timestamp::from_second(pid.into()).unwrap())
        };
        let hosted_absent = |_: &str, pid| {
            record(pid);
            false
        };
        let process_start = |pid| {
            assert_eq!(
                pid,
                std::process::id(),
                "only the backend availability probe"
            );
            Some(jiff::Timestamp::UNIX_EPOCH)
        };
        let elevated_agent = |pid| {
            record(pid);
            None
        };
        let readers = RepairReaders {
            cmdline: &cmdline,
            comm: &comm,
            children: &children,
            hosted_process: &hosted_process,
            cwd: &cwd,
            resumed_session: &resumed_session,
            root_start: &root_start,
            cwd_starts: &|_, _| panic!("root starts resolve every due pane"),
            hosted_absent: &hosted_absent,
            process_start: &process_start,
            elevated_agent: &elevated_agent,
            backfill_pids: &|_, _| {},
        };
        let now = Instant::now();
        let mut checks = HashMap::new();
        for seconds in [0, 30, 60] {
            let mut fresh = frame(panes.clone());
            let skipped = held_panes_for_check(
                &fresh,
                &held,
                &mut checks,
                now + Duration::from_secs(seconds),
            );
            reads.borrow_mut().clear();
            repair_pane_frame(
                &mut fresh,
                Some(&prior),
                "s",
                None,
                &held,
                &skipped,
                &readers,
            );
            let reads = reads.borrow();
            let unheld_reads = reads.iter().filter(|pid| **pid == unheld_pid).count();
            assert!(
                unheld_reads > 0,
                "the production path must check the unheld pane"
            );
            if seconds == 30 {
                assert_eq!(reads.len(), unheld_reads, "no held tree may be probed");
                unheld_counts.push(unheld_reads);
                for (fresh, old) in fresh.pane_states().zip(prior.pane_states()) {
                    if !held.contains(&fresh.pane_id) {
                        continue;
                    }
                    assert_eq!(fresh.current, old.current);
                    assert_eq!(fresh.previous, old.previous);
                    assert_eq!(fresh.children, old.children);
                    assert_eq!(fresh.metrics, old.metrics);
                    assert_eq!(fresh.first_seen_at_ms, old.first_seen_at_ms);
                    assert_eq!(fresh.hosted_carry_since_ms, old.hosted_carry_since_ms);
                    assert_eq!(fresh.title.as_deref(), Some("fresh listing"));
                }
            } else {
                for pid in 100..unheld_pid {
                    assert!(reads.contains(&pid), "held pid {pid} is due at {seconds}s");
                }
            }
        }
    }
    assert_eq!(
        unheld_counts[0], unheld_counts[1],
        "work depends only on the unheld pane"
    );
}

#[test]
fn due_held_panes_do_not_reopen_retained_topology() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = crate::RuntimePaths::under(
        crate::WorkspaceId::from_project_root(dir.path()),
        dir.path(),
    )
    .unwrap();
    runtime.ensure_dirs().unwrap();
    let panes = (1..=3)
        .map(|pid| {
            let mut pane = pane(&format!("terminal_{pid}"), Some("claude"), None);
            pane.pane_pid = Some(pid);
            pane.spawn_command = Some("claude".to_owned());
            pane
        })
        .collect();
    let mut retained = frame(panes);
    retained.produced_at_ms = unix_now_ms().saturating_add(60_000);
    let mut producer = PaneProducer::seeded(&runtime, "s");
    producer.hibernated_panes = retained
        .pane_states()
        .map(|pane| pane.pane_id.clone())
        .collect();
    producer.native_tenants = retained
        .pane_states()
        .map(|pane| {
            (
                pane.pane_id.clone(),
                (
                    pane.current.pid.unwrap(),
                    pane.current.spawn_command.clone(),
                ),
            )
        })
        .collect();
    producer.prior = Some(Arc::new(retained));
    let now = Instant::now();
    producer.held_checks = producer
        .hibernated_panes
        .iter()
        .map(|pane| (pane.clone(), now))
        .collect();
    let calls = std::cell::Cell::new(0);
    for pane in producer.hibernated_panes.clone() {
        producer
            .held_checks
            .insert(pane, now - crate::sidebar::timing::HIBERNATED_CHECK_TTL);
        producer
            .tick_with(
                &runtime,
                MuxName::Zellij,
                "s",
                None,
                None,
                &crate::diag::DiagSink::disabled(),
                &mut |_, prior| {
                    calls.set(calls.get() + 1);
                    Ok(Accepted::Fresh(prior.unwrap().clone()))
                },
                &|_, _| None,
            )
            .unwrap();
    }
    assert_eq!(
        calls.get(),
        0,
        "held deadlines must not cause a mux listing"
    );
}

#[test]
fn held_repairs_keep_process_fields_and_fresh_listing_until_due_or_thawed() {
    let dir = tempfile::tempdir().unwrap();
    let panes = || {
        (1..=4)
            .map(|pid| {
                let mut pane = pane(&format!("terminal_{pid}"), Some("claude"), None);
                pane.pane_pid = Some(pid);
                pane.spawn_command = Some("claude".to_owned());
                pane.view_id = Some("tab_1".to_owned());
                pane.title = Some("fresh title".to_owned());
                pane.is_floating = true;
                pane
            })
            .collect()
    };
    let mut prior = frame(panes());
    for pane in prior.pane_states_mut() {
        pane.current.cwd = Some(dir.path().display().to_string());
        pane.current.started_at = Some(jiff::Timestamp::UNIX_EPOCH);
        pane.current.foreground_cmdline = Some("claude --resume old".to_owned());
        pane.previous = Some(pane.current.clone());
        pane.children = vec![100];
        pane.metrics.rss_kb = Some(22);
        pane.first_seen_at_ms = Some(7);
        pane.hosted_carry_since_ms = Some(9);
    }
    let held: HashSet<_> = prior
        .pane_states()
        .take(3)
        .map(|pane| pane.pane_id.clone())
        .collect();
    let now = Instant::now();
    let mut checks = HashMap::new();
    let mut counts = Vec::new();
    for seconds in [0, 30, 60, 61] {
        let requested = if seconds == 61 {
            HashSet::new()
        } else {
            held.clone()
        };
        let mut fresh = frame(panes());
        fresh.tabs[0].name = Some("fresh tab".to_owned());
        let skipped = held_panes_for_check(
            &fresh,
            &requested,
            &mut checks,
            now + Duration::from_secs(seconds),
        );
        let calls = std::cell::RefCell::new(Vec::new());
        with_due_panes(&mut fresh, Some(&prior), &skipped, |due| {
            backfill_pane_cwds(due, &|pid| {
                calls.borrow_mut().push(pid);
                Some(dir.path().to_owned())
            });
        });
        counts.push(calls.into_inner());
        if seconds == 30 {
            for (fresh, old) in fresh.pane_states().zip(prior.pane_states()).take(3) {
                assert_eq!(fresh.current, old.current);
                assert_eq!(fresh.previous, old.previous);
                assert_eq!(fresh.children, old.children);
                assert_eq!(fresh.metrics, old.metrics);
                assert_eq!(fresh.hosted_carry_since_ms, old.hosted_carry_since_ms);
                assert_eq!(fresh.first_seen_at_ms, old.first_seen_at_ms);
                assert_eq!(fresh.title.as_deref(), Some("fresh title"));
                assert!(fresh.is_floating);
            }
            assert_eq!(fresh.tabs[0].name.as_deref(), Some("fresh tab"));
        }
    }
    assert_eq!(
        counts,
        [
            vec![1, 2, 3, 4],
            vec![4],
            vec![1, 2, 3, 4],
            vec![1, 2, 3, 4]
        ]
    );
    assert!(checks.is_empty(), "thaw drops the stamps");
}
