use super::*;
use std::collections::HashMap;

fn tmux_pane(command: &str) -> crate::pane::PaneRef {
    let mut pane = pane("%1", Some(command), Some("/repo"));
    pane.pane_id = crate::ids::PaneId::from_parts(crate::ids::MuxName::Tmux, "%1");
    pane
}

#[test]
fn rotate_from_prior_repairs_plain_shell_empty_command_before_row_gate() {
    let mut prior = frame(vec![pane("terminal_5", Some("zsh"), Some("/repo"))]);
    first_mut(&mut prior).current.pid = Some(3324242);
    let mut fresh_pane = pane("terminal_5", None, Some("/repo"));
    fresh_pane.pane_pid = Some(3324242);
    let mut fresh = frame(vec![fresh_pane]);

    rotate_from_prior(&mut fresh, Some(&prior));

    assert_eq!(first(&fresh).current.command.as_deref(), Some("zsh"));
    assert!(
        live_row_ids(&fresh).contains(&"zellij:terminal_5".to_owned()),
        "the repaired command admits the process row"
    );
}

#[test]
fn backfill_pane_cwds_repairs_missing_or_empty_cwd_from_proc() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_path_buf();
    let expected = cwd.to_string_lossy().into_owned();

    for (name, pane_ref) in [
        ("missing cwd", pane("terminal_1", Some("zsh"), None)),
        ("empty cwd", pane("terminal_1", Some("zsh"), Some(""))),
        ("commandless pane", pane("terminal_1", None, None)),
    ] {
        let seen = std::cell::Cell::new(None);
        let mut frame = frame(vec![pane_ref]);
        first_mut(&mut frame).current.pid = Some(100);

        backfill_pane_cwds(&mut frame, &|pid| {
            seen.set(Some(pid));
            Some(cwd.clone())
        });

        assert_eq!(seen.get(), Some(100), "{name}");
        assert_eq!(
            first(&frame).current.cwd.as_deref(),
            Some(expected.as_str()),
            "{name}"
        );
    }
}

#[test]
fn backfill_pane_cwds_skips_reported_pidless_missing_and_deleted_cwds() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().to_path_buf();
    let deleted = dir.path().join("gone");
    std::fs::create_dir(&deleted).unwrap();
    std::fs::remove_dir(&deleted).unwrap();

    let cases = [
        (
            "mux reported cwd",
            pane("terminal_1", Some("zsh"), Some("/repo/main")),
            Some(100),
            Some(live.clone()),
            Some("/repo/main"),
            false,
        ),
        (
            "pidless pane",
            pane("terminal_1", Some("zsh"), None),
            None,
            Some(live),
            None,
            false,
        ),
        (
            "no proc cwd",
            pane("terminal_1", Some("zsh"), None),
            Some(100),
            None,
            None,
            true,
        ),
        (
            "deleted proc cwd",
            pane("terminal_1", Some("zsh"), None),
            Some(100),
            Some(deleted),
            None,
            true,
        ),
    ];

    for (name, pane_ref, pid, proc_cwd, expected, expect_read) in cases {
        let seen = std::cell::Cell::new(None);
        let mut frame = frame(vec![pane_ref]);
        first_mut(&mut frame).current.pid = pid;

        backfill_pane_cwds(&mut frame, &|pid| {
            seen.set(Some(pid));
            proc_cwd.clone()
        });

        assert_eq!(seen.get().is_some(), expect_read, "{name}");
        assert_eq!(first(&frame).current.cwd.as_deref(), expected, "{name}");
    }
}

#[test]
fn backfill_wrapper_spawn_commands_matrix() {
    let wrapper = "/home/me/.cargo/bin/rimz agents exec codex --worktree-path /repo/wt --";
    for (name, pid, existing_spawn, proc_cmdline, expect_read, expected_spawn) in [
        (
            "recovers tmux agent wrapper",
            Some(4242),
            None,
            Some(wrapper),
            true,
            Some(wrapper),
        ),
        (
            "ignores real foregrounds",
            Some(4242),
            None,
            Some("zsh"),
            true,
            None,
        ),
        (
            "never overwrites existing spawn command",
            Some(4242),
            Some("zellij-born"),
            Some(wrapper),
            false,
            Some("zellij-born"),
        ),
        (
            "abstains without pid",
            None,
            None,
            Some(wrapper),
            false,
            None,
        ),
    ] {
        let seen = std::cell::Cell::new(None);
        let mut frame = frame(vec![tmux_pane("rimz")]);
        first_mut(&mut frame).current.pid = pid;
        first_mut(&mut frame).current.cwd = None;
        first_mut(&mut frame).current.spawn_command = existing_spawn.map(str::to_owned);

        backfill_wrapper_spawn_commands(&mut frame, &|pid| {
            seen.set(Some(pid));
            proc_cmdline.map(str::to_owned)
        });

        assert_eq!(seen.get().is_some(), expect_read, "{name}");
        let pane = frame.to_pane_refs().into_iter().next().expect("pane ref");
        assert_eq!(pane.spawn_command.as_deref(), expected_spawn, "{name}");
        if name == "recovers tmux agent wrapper" {
            assert_eq!(
                crate::ledger::snapshot::pane_agent_kind(&pane),
                Some("codex")
            );
            assert_eq!(
                crate::ledger::snapshot::pane_worktree_path(&pane),
                Some("/repo/wt")
            );
        }
    }
}

#[test]
fn backfilled_wrapper_spawn_command_drives_process_start_stamp() {
    let wrapper = "/home/me/.cargo/bin/rimz agents exec claude --worktree-path /repo/wt --";
    let start: jiff::Timestamp = "2026-06-05T13:54:33Z".parse().unwrap();
    let mut frame = frame(vec![tmux_pane("rimz")]);
    let unstamped = natively_unstamped(&frame);
    first_mut(&mut frame).current.pid = Some(4242);

    backfill_wrapper_spawn_commands(&mut frame, &|pid| {
        assert_eq!(pid, 4242);
        Some(wrapper.to_owned())
    });
    stamp_pane_process_starts(
        &mut frame,
        &unstamped,
        &|kind, pid| {
            assert_eq!(kind, "claude");
            assert_eq!(pid, 4242);
            Some(start)
        },
        &|_, _| -> Vec<jiff::Timestamp> {
            panic!("root-pid derivation owns spawn-command-classified panes")
        },
    );

    assert_eq!(
        first(&frame).current.spawn_command.as_deref(),
        Some(wrapper)
    );
    assert_eq!(first(&frame).current.started_at, Some(start));
}

#[test]
fn stamp_pane_process_starts_derives_from_command_or_spawn_command() {
    let start: jiff::Timestamp = "2026-06-05T13:54:33Z".parse().unwrap();

    // A Zellij codex pane arrives with no native process start and no pid
    // binding yet; the warmup cwd scan derives one so the published frame
    // carries it and the cwd-fallback guard fires on the consumer in-process
    // fold, not just the produce fork.
    let mut cwd_frame = frame(vec![pane("terminal_30", Some("codex"), Some("/repo"))]);
    let unstamped = natively_unstamped(&cwd_frame);
    stamp_pane_process_starts(&mut cwd_frame, &unstamped, &|_, _| None, &|kind, cwd| {
        assert_eq!(kind, "codex");
        assert_eq!(cwd, "/repo");
        vec![start]
    });
    assert_eq!(first(&cwd_frame).current.started_at, Some(start));

    let mut pane = pane("terminal_30", None, Some("/repo"));
    pane.pane_pid = Some(777);
    pane.spawn_command = Some("rimz agents exec codex --worktree-path /repo".to_owned());
    let mut spawn_frame = frame(vec![pane]);
    let unstamped = natively_unstamped(&spawn_frame);

    stamp_pane_process_starts(
        &mut spawn_frame,
        &unstamped,
        &|kind, pid| {
            assert_eq!(kind, "codex");
            assert_eq!(pid, 777);
            Some(start)
        },
        &|_, _| -> Vec<jiff::Timestamp> {
            panic!("root-pid derivation owns spawn_command-classified panes")
        },
    );

    assert_eq!(first(&spawn_frame).current.started_at, Some(start));
}

#[test]
fn stamp_hosted_agent_processes_ignores_foreground_command() {
    let start: jiff::Timestamp = "2026-06-05T13:54:33Z".parse().unwrap();
    let mut frame = frame(vec![pane("terminal_30", Some("git status"), Some("/repo"))]);
    first_mut(&mut frame).current.pid = Some(777);

    stamp_hosted_agent_processes(&mut frame, &|kind, pid| {
        assert_eq!(pid, 777);
        (kind == "codex").then_some(crate::remote_control::InPaneAgentProcess {
            started_at: start,
            cwd: None,
        })
    });

    assert_eq!(
        first(&frame)
            .current
            .hosted_agent_kind
            .as_ref()
            .map(|kind| kind.as_str()),
        Some("codex")
    );
    assert_eq!(
        first(&frame).current.hosted_agent_process_start,
        Some(start)
    );
}

#[test]
fn hosted_agent_process_fills_empty_cwd_for_wrapped_shell_pane() {
    let start: jiff::Timestamp = "2026-06-30T11:18:03Z".parse().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let expected = cwd.path().to_string_lossy().into_owned();
    let mut frame = frame(vec![pane("terminal_176", Some("chezmoi cd"), None)]);
    first_mut(&mut frame).current.pid = Some(3153567);

    stamp_hosted_agent_processes(&mut frame, &|kind, pid| {
        assert_eq!(pid, 3153567);
        (kind == "codex").then_some(crate::remote_control::InPaneAgentProcess {
            started_at: start,
            cwd: Some(cwd.path().to_path_buf()),
        })
    });

    let pane = first(&frame);
    assert_eq!(
        pane.current
            .hosted_agent_kind
            .as_ref()
            .map(|kind| kind.as_str()),
        Some("codex")
    );
    assert_eq!(pane.current.hosted_agent_process_start, Some(start));
    assert_eq!(pane.current.cwd.as_deref(), Some(expected.as_str()));
}

#[test]
fn hosted_agent_stamp_carry_anchor_does_not_advance_and_expires() {
    let start: jiff::Timestamp = "2026-06-30T11:18:03Z".parse().unwrap();
    let mut prior = frame(vec![pane("terminal_30", Some("git status"), Some("/repo"))]);
    first_mut(&mut prior).current.hosted_agent_kind =
        Some(crate::ids::AgentKind::new_unchecked("codex"));
    first_mut(&mut prior).current.hosted_agent_process_start = Some(start);
    first_mut(&mut prior).hosted_carry_since_ms = Some(10);
    let ttl_ms = crate::sidebar::timing::PANE_CARRY_TTL.as_millis() as u64;

    let mut still_fresh = frame(vec![pane("terminal_30", Some("git status"), Some("/repo"))]);
    carry_hosted_agent_stamps(&mut still_fresh, Some(&prior), 10 + ttl_ms, &|_, _| {
        panic!("pidless scan miss must not probe absence")
    });
    assert_eq!(first(&still_fresh).hosted_carry_since_ms, Some(10));
    assert_eq!(
        first(&still_fresh)
            .current
            .hosted_agent_kind
            .as_ref()
            .map(|kind| kind.as_str()),
        Some("codex")
    );

    let mut expired = frame(vec![pane("terminal_30", Some("git status"), Some("/repo"))]);
    carry_hosted_agent_stamps(&mut expired, Some(&prior), 11 + ttl_ms, &|_, _| {
        panic!("expired carry must not probe absence")
    });
    assert!(first(&expired).current.hosted_agent_kind.is_none());
    assert!(first(&expired).current.hosted_agent_process_start.is_none());
    assert_eq!(first(&expired).hosted_carry_since_ms, None);
}

#[test]
fn hosted_agent_stamp_carry_matrix() {
    let start: jiff::Timestamp = "2026-06-30T11:18:03Z".parse().unwrap();
    for (
        name,
        fresh_command,
        fresh_pid,
        absence_probe,
        expected_kind,
        expected_start,
        expected_carry_since,
    ) in [
        (
            "carries across transient pidless scan miss",
            "git status",
            None,
            None,
            Some("codex"),
            Some(start),
            Some(10),
        ),
        (
            "carries when absence is indeterminate",
            "git status",
            Some(200),
            Some(false),
            Some("codex"),
            Some(start),
            Some(10),
        ),
        (
            "drops on authoritative absence",
            "git status",
            Some(200),
            Some(true),
            None,
            None,
            None,
        ),
        (
            "respects foreground agent kind",
            "claude",
            None,
            None,
            None,
            None,
            None,
        ),
    ] {
        let mut prior = frame(vec![pane("terminal_30", Some("git status"), Some("/repo"))]);
        first_mut(&mut prior).current.started_at = Some(start);
        first_mut(&mut prior).current.hosted_agent_kind =
            Some(crate::ids::AgentKind::new_unchecked("codex"));
        first_mut(&mut prior).current.hosted_agent_process_start = Some(start);
        let mut fresh = frame(vec![pane(
            "terminal_30",
            Some(fresh_command),
            Some("/repo"),
        )]);
        first_mut(&mut fresh).current.pid = fresh_pid;
        let probed = std::cell::Cell::new(false);

        carry_hosted_agent_stamps(&mut fresh, Some(&prior), 10, &|kind, pid| {
            probed.set(true);
            assert_eq!((kind, pid), ("codex", 200), "{name}");
            absence_probe.unwrap_or_else(|| panic!("unexpected absence probe for {name}"))
        });

        let pane = first(&fresh);
        assert_eq!(
            pane.current
                .hosted_agent_kind
                .as_ref()
                .map(|kind| kind.as_str()),
            expected_kind,
            "{name}"
        );
        assert_eq!(
            pane.current.hosted_agent_process_start, expected_start,
            "{name}"
        );
        assert_eq!(pane.hosted_carry_since_ms, expected_carry_since, "{name}");
        assert_eq!(probed.get(), absence_probe.is_some(), "{name}");
    }
}

#[test]
fn carried_hosted_stamp_survives_tmux_scan_miss_until_ttl() {
    let agent_start: jiff::Timestamp = "2026-06-30T11:18:03Z".parse().unwrap();
    let mut prior = frame(vec![tmux_pane("git")]);
    first_mut(&mut prior).current.pid = Some(100);
    first_mut(&mut prior).current.started_at = Some(agent_start);
    first_mut(&mut prior).current.hosted_agent_kind =
        Some(crate::ids::AgentKind::new_unchecked("codex"));
    first_mut(&mut prior).current.hosted_agent_process_start = Some(agent_start);
    let mut fresh = frame(vec![tmux_pane("git")]);
    first_mut(&mut fresh).current.pid = Some(100);
    fresh.rotate_against_prior(&prior);

    stamp_hosted_agent_processes(&mut fresh, &|_, _| None);
    carry_hosted_agent_stamps(&mut fresh, Some(&prior), 10, &|kind, pid| {
        assert_eq!((kind, pid), ("codex", 100));
        false
    });
    drop_reused_pid_bindings(
        &mut fresh,
        &|kind, pid| {
            assert_eq!(kind, "codex");
            assert_eq!(pid, 100);
            None
        },
        &|pid| -> Option<jiff::Timestamp> {
            panic!("carried hosted stamp must not fall back to root start {pid}")
        },
    );

    let pane = first(&fresh);
    assert_eq!(pane.current.pid, Some(100));
    assert_eq!(
        pane.current
            .hosted_agent_kind
            .as_ref()
            .map(|kind| kind.as_str()),
        Some("codex")
    );
    assert_eq!(pane.current.hosted_agent_process_start, Some(agent_start));
    assert_eq!(pane.hosted_carry_since_ms, Some(10));
}

#[test]
fn carried_hosted_stamp_drops_on_positive_child_start_mismatch() {
    let expected: jiff::Timestamp = "2026-06-30T11:18:03Z".parse().unwrap();
    let actual: jiff::Timestamp = "2026-06-30T11:30:03Z".parse().unwrap();
    let mut frame = frame(vec![tmux_pane("git")]);
    first_mut(&mut frame).current.pid = Some(100);
    first_mut(&mut frame).current.started_at = Some(expected);
    first_mut(&mut frame).current.hosted_agent_kind =
        Some(crate::ids::AgentKind::new_unchecked("codex"));
    first_mut(&mut frame).current.hosted_agent_process_start = Some(expected);
    first_mut(&mut frame).hosted_carry_since_ms = Some(10);

    drop_reused_pid_bindings(
        &mut frame,
        &|kind, pid| {
            assert_eq!(kind, "codex");
            assert_eq!(pid, 100);
            Some(actual)
        },
        &|pid| -> Option<jiff::Timestamp> {
            panic!("hosted agent child owns the live identity: {pid}")
        },
    );

    let pane = first(&frame);
    assert_eq!(pane.current.pid, None);
    assert!(pane.current.hosted_agent_kind.is_none());
    assert!(pane.current.hosted_agent_process_start.is_none());
    assert_eq!(pane.hosted_carry_since_ms, None);
}

#[test]
fn stamp_pane_process_starts_never_touches_a_native_start() {
    // A pane the backend stamped natively (tmux) is outside the set captured
    // from the fresh read, so its start is authoritative — neither deriver is
    // ever consulted, even when a pid binding exists.
    let native: jiff::Timestamp = "2026-06-05T12:00:00Z".parse().unwrap();
    let mut pane = pane("terminal_30", Some("codex"), Some("/repo"));
    pane.pane_process_start = Some(native);
    pane.pane_pid = Some(100);
    let mut frame = frame(vec![pane]);
    let unstamped = natively_unstamped(&frame);
    stamp_pane_process_starts(
        &mut frame,
        &unstamped,
        &|_, _| panic!("must not derive over a native start"),
        &|_, _| panic!("must not scan over a native start"),
    );
    assert_eq!(first(&frame).current.started_at, Some(native));
}

#[test]
fn stamp_pane_process_starts_rederives_from_the_bound_root_pid() {
    // A re-tenanted pane — the agent exited and was re-run in place — carries
    // the prior tenant's stamp forward; the agent CLI behind the bound root
    // is the live process, so its start overwrites the carried one and the
    // bind guard refuses the old tenant's session again.
    let carried: jiff::Timestamp = "2026-06-05T12:00:00Z".parse().unwrap();
    let rebound: jiff::Timestamp = "2026-06-05T14:35:00Z".parse().unwrap();
    let mut frame = frame(vec![pane("terminal_30", Some("codex"), Some("/repo"))]);
    let unstamped = natively_unstamped(&frame);
    first_mut(&mut frame).current.started_at = Some(carried);
    first_mut(&mut frame).current.pid = Some(200);
    stamp_pane_process_starts(
        &mut frame,
        &unstamped,
        &|kind, pid| {
            assert_eq!(kind, "codex");
            assert_eq!(pid, 200);
            Some(rebound)
        },
        &|_, _| panic!("the root rung resolved; the cwd scan must not run"),
    );
    assert_eq!(first(&frame).current.started_at, Some(rebound));
}

#[test]
fn stamp_pane_process_starts_resolves_cwd_scan_remainders() {
    let accounted: jiff::Timestamp = "2026-06-05T13:49:53Z".parse().unwrap();
    let remaining: jiff::Timestamp = "2026-06-05T14:22:43Z".parse().unwrap();
    for (name, first_pid, carried, scan, expected) in [
        (
            "assigns unique unaccounted start",
            Some(200),
            [None, None],
            vec![accounted, remaining],
            vec![Some(accounted), Some(remaining)],
        ),
        (
            "replaces duplicate carried floor with unique remainder",
            Some(200),
            [None, Some(accounted)],
            vec![accounted, remaining],
            vec![Some(accounted), Some(remaining)],
        ),
        (
            "clears ambiguous duplicate carried floors",
            None,
            [Some(accounted), Some(accounted)],
            vec![accounted, remaining],
            vec![None, None],
        ),
        (
            "abstains for ambiguous cwd remainders",
            None,
            [None, None],
            vec![accounted, remaining],
            vec![None, None],
        ),
    ] {
        let mut frame = frame(vec![
            pane("terminal_4", Some("codex"), Some("/repo")),
            pane("terminal_58", Some("codex"), Some("/repo")),
        ]);
        let unstamped = natively_unstamped(&frame);
        for (index, pane) in frame.pane_states_mut().enumerate() {
            if index == 0 {
                pane.current.pid = first_pid;
            }
            pane.current.started_at = carried[index];
        }

        stamp_pane_process_starts(
            &mut frame,
            &unstamped,
            &|_, pid| (pid == 200).then_some(accounted),
            &|_, _| scan.clone(),
        );

        let starts = frame
            .pane_states()
            .map(|pane| pane.current.started_at)
            .collect::<Vec<_>>();
        assert_eq!(starts, expected, "{name}");
    }
}

#[test]
fn stamp_pane_process_starts_keeps_the_carried_stamp_when_the_pid_is_gone() {
    // The binding's process is gone (a fresh-window re-tenancy, an exited
    // pane): the carried stamp bridges the gap rather than rescanning — a cwd
    // scan on an exited pane would erase the stamp and let a stale session
    // bind again.
    let carried: jiff::Timestamp = "2026-06-05T12:00:00Z".parse().unwrap();
    let mut frame = frame(vec![pane("terminal_30", Some("codex"), Some("/repo"))]);
    let unstamped = natively_unstamped(&frame);
    first_mut(&mut frame).current.started_at = Some(carried);
    first_mut(&mut frame).current.pid = Some(100);
    stamp_pane_process_starts(&mut frame, &unstamped, &|_, _| None, &|_, _| {
        panic!("carried stamp present; the cwd scan must not run")
    });
    assert_eq!(first(&frame).current.started_at, Some(carried));
}

#[test]
fn stamp_pane_process_starts_skips_non_agent_and_cwdless_panes() {
    // The derivers are consulted only for an agent pane: a shell pane stays
    // unstamped even with a pid binding, and a cwd-less agent pane skips the
    // scan (the guard then falls back to most-recently-active, the documented
    // other-user case).
    let start: jiff::Timestamp = "2026-06-05T13:54:33Z".parse().unwrap();
    let mut frame = frame(vec![
        pane("terminal_1", Some("zsh"), Some("/repo")),
        pane("terminal_2", Some("codex"), None),
        pane("terminal_3", Some("codex"), Some("")),
    ]);
    frame.pane_states_mut().next().unwrap().current.pid = Some(100);
    let unstamped = natively_unstamped(&frame);
    stamp_pane_process_starts(&mut frame, &unstamped, &|_, _| Some(start), &|_, _| {
        vec![start]
    });
    assert!(
        frame
            .pane_states()
            .all(|pane| pane.current.started_at.is_none())
    );
}

#[test]
fn drop_reused_pid_bindings_keeps_hosted_agent_child_identity() {
    let expected: jiff::Timestamp = "2026-06-05T12:00:00Z".parse().unwrap();
    let actual: jiff::Timestamp = "2026-06-05T12:00:02Z".parse().unwrap();
    let mut frame = frame(vec![pane("terminal_1", Some("git status"), Some("/repo"))]);
    first_mut(&mut frame).current.pid = Some(100);
    first_mut(&mut frame).current.hosted_agent_kind =
        Some(crate::ids::AgentKind::new_unchecked("codex"));
    first_mut(&mut frame).current.hosted_agent_process_start = Some(expected);
    first_mut(&mut frame).metrics = PaneMetrics {
        process_state: None,
        rss_kb: Some(1024),
        cpu_pct: Some(250),
        io_bps: Some(4096),
    };

    drop_reused_pid_bindings(
        &mut frame,
        &|kind, pid| {
            assert_eq!(kind, "codex");
            assert_eq!(pid, 100);
            Some(actual)
        },
        &|pid| -> Option<jiff::Timestamp> {
            panic!("hosted agent child owns the live identity: {pid}")
        },
    );

    assert_eq!(first(&frame).current.pid, Some(100));
    assert_eq!(
        first(&frame)
            .current
            .hosted_agent_kind
            .as_ref()
            .map(|kind| kind.as_str()),
        Some("codex")
    );
    assert_eq!(first(&frame).metrics.rss_kb, Some(1024));
}

#[test]
fn drop_reused_pid_bindings_matrix() {
    let expected: jiff::Timestamp = "2026-06-05T12:00:00Z".parse().unwrap();
    let within_tolerance: jiff::Timestamp = "2026-06-05T12:00:02Z".parse().unwrap();
    let beyond_tolerance: jiff::Timestamp = "2026-06-05T12:00:10Z".parse().unwrap();
    for (
        name,
        pid,
        started_at,
        root_start,
        process_start,
        expected_pid,
        expected_start,
        expect_metadata_clear,
    ) in [
        (
            "clears stale process identity",
            Some(100),
            Some(expected),
            Some(None),
            Some(Some(beyond_tolerance)),
            None,
            None,
            true,
        ),
        (
            "keeps matching process identity",
            Some(100),
            Some(expected),
            Some(Some(within_tolerance)),
            None,
            Some(100),
            Some(expected),
            false,
        ),
        (
            "clears missing process start",
            Some(100),
            Some(expected),
            Some(None),
            Some(None),
            None,
            None,
            true,
        ),
        (
            "pidless",
            None,
            Some("2026-06-05T12:00:00Z".parse().unwrap()),
            None,
            None,
            None,
            Some(expected),
            false,
        ),
        (
            "startless",
            Some(100),
            None,
            None,
            None,
            Some(100),
            None,
            false,
        ),
    ] {
        let mut frame = frame(vec![pane("terminal_1", Some("codex"), Some("/repo"))]);
        first_mut(&mut frame).current.pid = pid;
        first_mut(&mut frame).current.started_at = started_at;
        let previous = first(&frame).current.clone();
        first_mut(&mut frame).previous = Some(previous);
        first_mut(&mut frame).children = vec![200];
        first_mut(&mut frame).metrics = PaneMetrics {
            process_state: Some(crate::ProcessState::Stuck),
            rss_kb: Some(1024),
            cpu_pct: Some(250),
            io_bps: Some(4096),
        };

        drop_reused_pid_bindings(
            &mut frame,
            &|kind, pid| {
                assert_eq!(kind, "codex", "{name}");
                assert_eq!(pid, 100, "{name}");
                root_start.unwrap_or_else(|| panic!("must not read root start for {name}: {pid}"))
            },
            &|pid| {
                assert_eq!(pid, 100, "{name}");
                process_start
                    .unwrap_or_else(|| panic!("must not read process start for {name}: {pid}"))
            },
        );

        assert_eq!(first(&frame).current.pid, expected_pid, "{name}");
        assert_eq!(first(&frame).current.started_at, expected_start, "{name}");
        if expect_metadata_clear {
            assert_eq!(first(&frame).previous, None, "{name}");
            assert!(first(&frame).children.is_empty(), "{name}");
            assert_eq!(first(&frame).metrics, PaneMetrics::default(), "{name}");
        } else {
            assert!(first(&frame).previous.is_some(), "{name}");
            assert_eq!(first(&frame).children, vec![200], "{name}");
            assert_eq!(first(&frame).metrics.rss_kb, Some(1024), "{name}");
        }
    }
}

#[test]
fn active_command_liveness_matrix_matches_backend_contracts() {
    for (name, pane_ref, cmdlines, comms, children, expected, expect_metadata_clear) in [
        (
            "stale zellij command clears with process metadata",
            pane("terminal_1", Some("cargo build --release"), Some("/repo")),
            vec![(100, "zsh")],
            vec![(100, "zsh")],
            vec![],
            None,
            true,
        ),
        (
            "zellij descendant exact argv keeps command",
            pane("terminal_1", Some("cargo build --release"), Some("/repo")),
            vec![
                (100, "zsh"),
                (200, "bash -lc cargo build --release"),
                (300, "cargo build --release"),
            ],
            vec![(100, "zsh"), (200, "bash"), (300, "cargo")],
            vec![(100, vec![200]), (200, vec![300])],
            Some("cargo build --release"),
            false,
        ),
        (
            "zellij same program different argv is stale",
            pane("terminal_1", Some("cargo build --release"), Some("/repo")),
            vec![(100, "zsh"), (200, "cargo test")],
            vec![(100, "zsh"), (200, "cargo")],
            vec![(100, vec![200])],
            None,
            false,
        ),
        (
            "zellij root exact argv keeps command",
            pane("terminal_1", Some("cargo build --release"), Some("/repo")),
            vec![(100, "cargo build --release")],
            vec![(100, "cargo")],
            vec![],
            Some("cargo build --release"),
            false,
        ),
        (
            "missing proc evidence abstains",
            pane("terminal_1", Some("cargo build --release"), Some("/repo")),
            vec![],
            vec![],
            vec![],
            Some("cargo build --release"),
            false,
        ),
        (
            "tmux short command matches child program label",
            tmux_pane("cargo"),
            vec![(100, "zsh"), (200, "/usr/bin/cargo build --release")],
            vec![(100, "zsh"), (200, "cargo")],
            vec![(100, vec![200])],
            Some("cargo"),
            false,
        ),
        (
            "tmux long program reads cmdline over truncated comm",
            tmux_pane("mutable-unicorn-server"),
            vec![
                (100, "zsh"),
                (200, "/opt/bin/mutable-unicorn-server --serve"),
            ],
            vec![(100, "zsh"), (200, "mutable-unicor")],
            vec![(100, vec![200])],
            Some("mutable-unicorn-server"),
            false,
        ),
        (
            "tmux cmdline mismatch without comm is absent",
            tmux_pane("cargo"),
            vec![(100, "zsh"), (200, "make check")],
            vec![],
            vec![(100, vec![200])],
            None,
            false,
        ),
    ] {
        assert_active_command_case(
            name,
            pane_ref,
            cmdlines,
            comms,
            children,
            expected,
            expect_metadata_clear,
        );
    }
}

fn assert_active_command_case(
    name: &str,
    pane_ref: crate::pane::PaneRef,
    cmdlines: Vec<(u32, &'static str)>,
    comms: Vec<(u32, &'static str)>,
    children: Vec<(u32, Vec<u32>)>,
    expected: Option<&str>,
    expect_metadata_clear: bool,
) {
    let mut frame = frame(vec![pane_ref]);
    first_mut(&mut frame).current.pid = Some(100);
    if expect_metadata_clear {
        first_mut(&mut frame).children = vec![200];
        first_mut(&mut frame).metrics = PaneMetrics {
            process_state: Some(crate::ProcessState::Stuck),
            rss_kb: Some(1024),
            cpu_pct: Some(250),
            io_bps: Some(4096),
        };
    }
    let cmdlines = cmdlines
        .into_iter()
        .map(|(pid, cmdline)| (pid, cmdline.to_owned()))
        .collect::<HashMap<_, _>>();
    let comms = comms
        .into_iter()
        .map(|(pid, comm)| (pid, comm.to_owned()))
        .collect::<HashMap<_, _>>();
    let children = children.into_iter().collect::<HashMap<_, _>>();

    drop_finished_active_commands(
        &mut frame,
        &|pid| cmdlines.get(&pid).cloned(),
        &|pid| comms.get(&pid).cloned(),
        &|pid| children.get(&pid).cloned().unwrap_or_default(),
    );

    assert_eq!(first(&frame).current.command.as_deref(), expected, "{name}");
    if expect_metadata_clear {
        assert_eq!(first(&frame).metrics, PaneMetrics::default(), "{name}");
        assert!(first(&frame).children.is_empty(), "{name}");
    }
}

#[test]
fn idle_and_agent_commands_are_not_process_gated() {
    for command in ["zsh", "codex"] {
        let mut frame = frame(vec![pane("terminal_1", Some(command), Some("/repo"))]);
        first_mut(&mut frame).current.pid = Some(100);

        drop_finished_active_commands(
            &mut frame,
            &|pid| -> Option<String> { panic!("cmdline must not be read for {command}: {pid}") },
            &|pid| -> Option<String> { panic!("comm must not be read for {command}: {pid}") },
            &|pid| -> Vec<u32> { panic!("children must not be read for {command}: {pid}") },
        );

        assert_eq!(first(&frame).current.command.as_deref(), Some(command));
    }
}

#[test]
fn annotate_elevated_agents_marks_only_wrapper_panes_with_a_pid() {
    let mut frame = frame(vec![
        pane("terminal_1", Some("sudo su"), Some("/repo")),
        pane("terminal_2", Some("zsh"), Some("/repo")),
        pane("terminal_3", Some("sudo su"), Some("/repo")),
    ]);
    let pane_ids: Vec<_> = frame
        .pane_states()
        .map(|pane| pane.pane_id.clone())
        .collect();
    for pane in frame.pane_states_mut() {
        match pane.pane_id.raw() {
            "terminal_1" => pane.current.pid = Some(100),
            "terminal_2" => pane.current.pid = Some(200),
            _ => {}
        }
    }

    annotate_elevated_agents(&mut frame, &|pid| {
        assert_eq!(pid, 100, "only the wrapper pane with a pid is scanned");
        Some(crate::pane::ElevatedAgent {
            kind: crate::ids::AgentKind::new_unchecked("claude"),
            uid: 0,
        })
    });

    let by_id = |id: &crate::ids::PaneId| {
        frame
            .pane_states()
            .find(|pane| pane.pane_id == *id)
            .expect("pane present")
    };
    assert_eq!(
        by_id(&pane_ids[0])
            .current
            .elevated_agent
            .as_ref()
            .map(|agent| (agent.kind.as_str(), agent.uid)),
        Some(("claude", 0))
    );
    assert_eq!(by_id(&pane_ids[1]).current.elevated_agent, None);
    assert_eq!(by_id(&pane_ids[2]).current.elevated_agent, None);
}
