use super::*;
use std::collections::HashMap;

fn tmux_pane(command: &str) -> crate::feed::PaneRef {
    let mut pane = pane("%1", Some(command), Some("/repo"));
    pane.pane_id = crate::ids::PaneId::from_parts(crate::ids::MuxName::Tmux, "%1");
    pane
}

#[test]
fn rotate_from_cache_repairs_raced_nulls_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    let cache_path = dir.path().join("snapshot.json");
    let prior = frame(vec![pane("terminal_1", Some("claude"), Some("/repo"))]);
    atomic::write_temp_then_rename_cache(&cache_path, &prior).unwrap();
    let mut fresh = frame(vec![pane("terminal_1", None, None)]);

    rotate_from_cache(&mut fresh, &cache_path, "s");

    assert_eq!(first(&fresh).current.command.as_deref(), Some("claude"));
    assert_eq!(first(&fresh).current.cwd.as_deref(), Some("/repo"));
}

#[test]
fn rotate_from_cache_is_noop_without_prior() {
    let dir = tempfile::tempdir().unwrap();
    let cache_path = dir.path().join("snapshot.json");
    let mut fresh = frame(vec![pane("terminal_1", None, None)]);

    rotate_from_cache(&mut fresh, &cache_path, "s");

    assert_eq!(first(&fresh).current.command, None);
    assert_eq!(first(&fresh).current.cwd, None);
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
fn spawn_handoff_rotates_without_carrying_process_start() {
    let old_start: jiff::Timestamp = "2026-06-05T12:00:00Z".parse().unwrap();
    let mut prior = frame(vec![pane("terminal_1", Some("codex"), Some("/repo"))]);
    first_mut(&mut prior).current.spawn_command = Some("rimz agents exec codex".to_owned());
    first_mut(&mut prior).current.started_at = Some(old_start);
    let mut fresh = frame(vec![pane("terminal_1", Some("zsh"), Some("/repo"))]);
    first_mut(&mut fresh).current.spawn_command = Some("zsh".to_owned());

    fresh.rotate_against_prior(&prior);

    assert_eq!(first(&fresh).current.command.as_deref(), Some("zsh"));
    assert_eq!(first(&fresh).current.started_at, None);
    assert_eq!(
        first(&fresh)
            .previous
            .as_ref()
            .and_then(|previous| previous.command.as_deref()),
        Some("codex")
    );
}

#[test]
fn foreground_handoff_with_stable_spawn_keeps_process_start() {
    let old_start: jiff::Timestamp = "2026-06-05T12:00:00Z".parse().unwrap();
    let mut prior = frame(vec![pane("terminal_1", Some("codex"), Some("/repo"))]);
    first_mut(&mut prior).current.spawn_command = Some("rimz agents exec codex".to_owned());
    first_mut(&mut prior).current.started_at = Some(old_start);
    let mut fresh = frame(vec![pane(
        "terminal_1",
        Some("/usr/bin/codex"),
        Some("/repo"),
    )]);
    first_mut(&mut fresh).current.spawn_command = Some("rimz agents exec codex".to_owned());

    fresh.rotate_against_prior(&prior);

    assert_eq!(
        first(&fresh).current.command.as_deref(),
        Some("/usr/bin/codex")
    );
    assert_eq!(first(&fresh).current.started_at, Some(old_start));
    assert!(first(&fresh).previous.is_none());
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
fn stamp_pane_resumed_session_ids_derives_from_the_bound_root_pid() {
    let mut frame = frame(vec![pane("terminal_30", Some("codex"), Some("/repo"))]);
    first_mut(&mut frame).current.pid = Some(200);

    stamp_pane_resumed_session_ids(&mut frame, &|pid| {
        assert_eq!(pid, 200);
        Some("sess-resumed".into())
    });

    assert_eq!(
        first(&frame)
            .current
            .resumed_session_id
            .as_ref()
            .map(|id| id.as_str()),
        Some("sess-resumed")
    );
}

#[test]
fn stamp_pane_process_starts_assigns_the_unique_unaccounted_cwd_start() {
    let accounted: jiff::Timestamp = "2026-06-05T13:49:53Z".parse().unwrap();
    let remaining: jiff::Timestamp = "2026-06-05T14:22:43Z".parse().unwrap();
    let mut frame = frame(vec![
        pane("terminal_4", Some("codex"), Some("/repo")),
        pane("terminal_58", Some("codex"), Some("/repo")),
    ]);
    let unstamped = natively_unstamped(&frame);
    frame.tabs[0].panes[0].current.pid = Some(200);
    stamp_pane_process_starts(
        &mut frame,
        &unstamped,
        &|_, pid| (pid == 200).then_some(accounted),
        &|_, _| vec![accounted, remaining],
    );

    let starts = frame
        .pane_states()
        .map(|pane| pane.current.started_at)
        .collect::<Vec<_>>();
    assert_eq!(starts, vec![Some(accounted), Some(remaining)]);
}

#[test]
fn stamp_pane_process_starts_replaces_a_duplicate_carried_floor_with_the_unique_remainder() {
    let accounted: jiff::Timestamp = "2026-06-05T13:49:53Z".parse().unwrap();
    let remaining: jiff::Timestamp = "2026-06-05T14:22:43Z".parse().unwrap();
    let mut frame = frame(vec![
        pane("terminal_4", Some("codex"), Some("/repo")),
        pane("terminal_58", Some("codex"), Some("/repo")),
    ]);
    let unstamped = natively_unstamped(&frame);
    {
        let mut panes = frame.pane_states_mut();
        panes.next().unwrap().current.pid = Some(200);
        panes.next().unwrap().current.started_at = Some(accounted);
    }

    stamp_pane_process_starts(
        &mut frame,
        &unstamped,
        &|_, pid| (pid == 200).then_some(accounted),
        &|_, _| vec![accounted, remaining],
    );

    let starts = frame
        .pane_states()
        .map(|pane| pane.current.started_at)
        .collect::<Vec<_>>();
    assert_eq!(starts, vec![Some(accounted), Some(remaining)]);
}

#[test]
fn stamp_pane_process_starts_clears_ambiguous_duplicate_carried_floors() {
    let floor: jiff::Timestamp = "2026-06-05T13:49:53Z".parse().unwrap();
    let later: jiff::Timestamp = "2026-06-05T14:22:43Z".parse().unwrap();
    let mut frame = frame(vec![
        pane("terminal_4", Some("codex"), Some("/repo")),
        pane("terminal_58", Some("codex"), Some("/repo")),
    ]);
    let unstamped = natively_unstamped(&frame);
    for pane in frame.pane_states_mut() {
        pane.current.started_at = Some(floor);
    }

    stamp_pane_process_starts(&mut frame, &unstamped, &|_, _| None, &|_, _| {
        vec![floor, later]
    });

    assert!(
        frame
            .pane_states()
            .all(|pane| pane.current.started_at.is_none()),
        "duplicated carried cwd floors are cleared instead of republished"
    );
}

#[test]
fn stamp_pane_process_starts_abstains_for_ambiguous_cwd_remainders() {
    let first: jiff::Timestamp = "2026-06-05T13:49:53Z".parse().unwrap();
    let second: jiff::Timestamp = "2026-06-05T14:22:43Z".parse().unwrap();
    let mut frame = frame(vec![
        pane("terminal_4", Some("codex"), Some("/repo")),
        pane("terminal_58", Some("codex"), Some("/repo")),
    ]);
    let unstamped = natively_unstamped(&frame);
    stamp_pane_process_starts(&mut frame, &unstamped, &|_, _| None, &|_, _| {
        vec![first, second]
    });

    assert!(
        frame
            .pane_states()
            .all(|pane| pane.current.started_at.is_none()),
        "ambiguous cwd starts are never duplicated across panes"
    );
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
fn stale_active_command_clears_when_process_tree_has_no_match() {
    let mut frame = frame(vec![pane(
        "terminal_1",
        Some("cargo build --release"),
        Some("/repo"),
    )]);
    first_mut(&mut frame).current.pid = Some(100);
    first_mut(&mut frame).children = vec![200];
    first_mut(&mut frame).metrics = PaneMetrics {
        process_state: Some(crate::ProcessState::Stuck),
        rss_kb: Some(1024),
        cpu_pct: Some(250),
        io_bps: Some(4096),
    };
    let cmdlines = HashMap::from([(100, "zsh".to_owned())]);
    let comms = HashMap::from([(100, "zsh".to_owned())]);
    let children = HashMap::<u32, Vec<u32>>::new();

    drop_finished_active_commands(
        &mut frame,
        &|pid| cmdlines.get(&pid).cloned(),
        &|pid| comms.get(&pid).cloned(),
        &|pid| children.get(&pid).cloned().unwrap_or_default(),
    );

    assert_eq!(
        first(&frame).current.command,
        None,
        "a stale active foreground with only an idle shell behind it drops"
    );
    assert_eq!(first(&frame).metrics, PaneMetrics::default());
    assert!(
        first(&frame).children.is_empty(),
        "stale process metadata is cleared with the command"
    );
}

#[test]
fn zellij_active_command_stays_when_a_descendant_matches_exactly() {
    let mut frame = frame(vec![pane(
        "terminal_1",
        Some("cargo build --release"),
        Some("/repo"),
    )]);
    first_mut(&mut frame).current.pid = Some(100);
    let cmdlines = HashMap::from([
        (100, "zsh".to_owned()),
        (200, "bash -lc cargo build --release".to_owned()),
        (300, "cargo build --release".to_owned()),
    ]);
    let comms = HashMap::from([
        (100, "zsh".to_owned()),
        (200, "bash".to_owned()),
        (300, "cargo".to_owned()),
    ]);
    let children = HashMap::from([(100, vec![200]), (200, vec![300])]);

    drop_finished_active_commands(
        &mut frame,
        &|pid| cmdlines.get(&pid).cloned(),
        &|pid| comms.get(&pid).cloned(),
        &|pid| children.get(&pid).cloned().unwrap_or_default(),
    );

    assert_eq!(
        first(&frame).current.command.as_deref(),
        Some("cargo build --release")
    );
}

#[test]
fn zellij_stale_full_command_ignores_same_program_with_different_argv() {
    let mut frame = frame(vec![pane(
        "terminal_1",
        Some("cargo build --release"),
        Some("/repo"),
    )]);
    first_mut(&mut frame).current.pid = Some(100);
    let cmdlines = HashMap::from([(100, "zsh".to_owned()), (200, "cargo test".to_owned())]);
    let comms = HashMap::from([(100, "zsh".to_owned()), (200, "cargo".to_owned())]);
    let children = HashMap::from([(100, vec![200])]);

    drop_finished_active_commands(
        &mut frame,
        &|pid| cmdlines.get(&pid).cloned(),
        &|pid| comms.get(&pid).cloned(),
        &|pid| children.get(&pid).cloned().unwrap_or_default(),
    );

    assert_eq!(
        first(&frame).current.command,
        None,
        "Zellij retains full argv commands; a same-program descendant with \
         different argv must not keep a stale command alive"
    );
}

#[test]
fn tmux_short_command_matches_child_program_label_when_cmdline_has_args() {
    let mut frame = frame(vec![tmux_pane("cargo")]);
    first_mut(&mut frame).current.pid = Some(100);
    let cmdlines = HashMap::from([
        (100, "zsh".to_owned()),
        (200, "/usr/bin/cargo build --release".to_owned()),
    ]);
    let comms = HashMap::from([(100, "zsh".to_owned()), (200, "cargo".to_owned())]);
    let children = HashMap::from([(100, vec![200])]);

    drop_finished_active_commands(
        &mut frame,
        &|pid| cmdlines.get(&pid).cloned(),
        &|pid| comms.get(&pid).cloned(),
        &|pid| children.get(&pid).cloned().unwrap_or_default(),
    );

    assert_eq!(first(&frame).current.command.as_deref(), Some("cargo"));
}

#[test]
fn tmux_long_program_prefers_cmdline_label_over_truncated_comm() {
    let mut frame = frame(vec![tmux_pane("mutable-unicorn-server")]);
    first_mut(&mut frame).current.pid = Some(100);
    let cmdlines = HashMap::from([
        (100, "zsh".to_owned()),
        (200, "/opt/bin/mutable-unicorn-server --serve".to_owned()),
    ]);
    let comms = HashMap::from([(100, "zsh".to_owned()), (200, "mutable-unicor".to_owned())]);
    let children = HashMap::from([(100, vec![200])]);

    drop_finished_active_commands(
        &mut frame,
        &|pid| cmdlines.get(&pid).cloned(),
        &|pid| comms.get(&pid).cloned(),
        &|pid| children.get(&pid).cloned().unwrap_or_default(),
    );

    assert_eq!(
        first(&frame).current.command.as_deref(),
        Some("mutable-unicorn-server")
    );
}

#[test]
fn tmux_mismatched_cmdline_without_comm_counts_as_absent() {
    let mut frame = frame(vec![tmux_pane("cargo")]);
    first_mut(&mut frame).current.pid = Some(100);
    let cmdlines = HashMap::from([(100, "zsh".to_owned()), (200, "make check".to_owned())]);
    let children = HashMap::from([(100, vec![200])]);

    drop_finished_active_commands(
        &mut frame,
        &|pid| cmdlines.get(&pid).cloned(),
        &|_| None,
        &|pid| children.get(&pid).cloned().unwrap_or_default(),
    );

    assert_eq!(
        first(&frame).current.command,
        None,
        "a readable tmux cmdline mismatch is process evidence even when comm is unavailable"
    );
}

#[test]
fn command_pane_root_matches_without_children() {
    let mut frame = frame(vec![pane(
        "terminal_1",
        Some("cargo build --release"),
        Some("/repo"),
    )]);
    first_mut(&mut frame).current.pid = Some(100);
    let cmdlines = HashMap::from([(100, "cargo build --release".to_owned())]);
    let comms = HashMap::from([(100, "cargo".to_owned())]);
    let children = HashMap::<u32, Vec<u32>>::new();

    drop_finished_active_commands(
        &mut frame,
        &|pid| cmdlines.get(&pid).cloned(),
        &|pid| comms.get(&pid).cloned(),
        &|pid| children.get(&pid).cloned().unwrap_or_default(),
    );

    assert_eq!(
        first(&frame).current.command.as_deref(),
        Some("cargo build --release")
    );
}

#[test]
fn active_command_stays_when_process_evidence_is_unavailable() {
    let mut frame = frame(vec![pane(
        "terminal_1",
        Some("cargo build --release"),
        Some("/repo"),
    )]);
    first_mut(&mut frame).current.pid = Some(100);
    let children = HashMap::<u32, Vec<u32>>::new();

    drop_finished_active_commands(&mut frame, &|_| None, &|_| None, &|pid| {
        children.get(&pid).cloned().unwrap_or_default()
    });

    assert_eq!(
        first(&frame).current.command.as_deref(),
        Some("cargo build --release"),
        "missing /proc evidence is an abstention, not proof of exit"
    );
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
        Some(crate::feed::ElevatedAgent {
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
