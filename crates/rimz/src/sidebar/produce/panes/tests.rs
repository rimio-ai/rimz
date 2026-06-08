use super::*;
use crate::ledger::atomic;
use crate::sidebar::produce::test_support::pane;
use crate::sidebar::snapshot::SNAPSHOT_CACHE_TTL;

fn frame(panes: Vec<crate::feed::PaneRef>) -> crate::sidebar::frame::PaneFrame {
    crate::sidebar::frame::assemble_frame(panes, 1, "s")
}

fn first(frame: &crate::sidebar::frame::PaneFrame) -> &crate::sidebar::frame::PaneState {
    &frame.tabs[0].panes[0]
}

fn first_mut(
    frame: &mut crate::sidebar::frame::PaneFrame,
) -> &mut crate::sidebar::frame::PaneState {
    &mut frame.tabs[0].panes[0]
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
fn backfill_pane_cwds_repairs_a_raced_empty_cwd_from_proc() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_path_buf();
    let expected = cwd.to_string_lossy().into_owned();
    let seen = std::cell::Cell::new(None);
    let mut frame = frame(vec![pane("terminal_1", Some("zsh"), None)]);
    first_mut(&mut frame).current.pid = Some(100);

    backfill_pane_cwds(&mut frame, &|pid| {
        seen.set(Some(pid));
        Some(cwd.clone())
    });

    assert_eq!(seen.get(), Some(100));
    assert_eq!(
        first(&frame).current.cwd.as_deref(),
        Some(expected.as_str())
    );
}

#[test]
fn backfill_pane_cwds_repairs_an_empty_string_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_path_buf();
    let expected = cwd.to_string_lossy().into_owned();
    let mut frame = frame(vec![pane("terminal_1", Some("zsh"), Some(""))]);
    first_mut(&mut frame).current.pid = Some(100);

    backfill_pane_cwds(&mut frame, &|pid| {
        assert_eq!(pid, 100);
        Some(cwd.clone())
    });

    assert_eq!(
        first(&frame).current.cwd.as_deref(),
        Some(expected.as_str())
    );
}

#[test]
fn backfill_pane_cwds_never_overrides_a_mux_reported_cwd() {
    let mut frame = frame(vec![pane("terminal_1", Some("zsh"), Some("/repo/main"))]);
    first_mut(&mut frame).current.pid = Some(100);

    backfill_pane_cwds(&mut frame, &|_| {
        panic!("must not read /proc when the mux reported cwd")
    });

    assert_eq!(first(&frame).current.cwd.as_deref(), Some("/repo/main"));
}

#[test]
fn backfill_pane_cwds_leaves_a_pidless_pane_untouched() {
    let mut frame = frame(vec![pane("terminal_1", Some("zsh"), None)]);

    backfill_pane_cwds(&mut frame, &|_| {
        panic!("must not read /proc without a pane pid")
    });

    assert_eq!(first(&frame).current.cwd, None);
}

#[test]
fn backfill_pane_cwds_repairs_a_command_less_pane() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_path_buf();
    let expected = cwd.to_string_lossy().into_owned();
    let mut frame = frame(vec![pane("terminal_1", None, None)]);
    first_mut(&mut frame).current.pid = Some(100);

    backfill_pane_cwds(&mut frame, &|pid| {
        assert_eq!(pid, 100);
        Some(cwd.clone())
    });

    assert_eq!(first(&frame).current.command, None);
    assert_eq!(
        first(&frame).current.cwd.as_deref(),
        Some(expected.as_str())
    );
}

#[test]
fn backfill_pane_cwds_skips_a_pane_with_no_proc_cwd() {
    let mut frame = frame(vec![pane("terminal_1", Some("zsh"), None)]);
    first_mut(&mut frame).current.pid = Some(100);

    backfill_pane_cwds(&mut frame, &|pid| {
        assert_eq!(pid, 100);
        None
    });

    assert_eq!(first(&frame).current.cwd, None);
}

#[test]
fn backfill_pane_cwds_skips_a_proc_cwd_that_no_longer_exists() {
    let dir = tempfile::tempdir().unwrap();
    let deleted = dir.path().join("gone");
    std::fs::create_dir(&deleted).unwrap();
    std::fs::remove_dir(&deleted).unwrap();
    let mut frame = frame(vec![pane("terminal_1", Some("zsh"), None)]);
    first_mut(&mut frame).current.pid = Some(100);

    backfill_pane_cwds(&mut frame, &|pid| {
        assert_eq!(pid, 100);
        Some(deleted.clone())
    });

    assert_eq!(first(&frame).current.cwd, None);
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
fn stamp_pane_process_starts_stamps_a_codex_pane_lacking_a_native_start() {
    // A Zellij codex pane arrives with no native process start and no pid
    // binding yet; the warmup cwd scan derives one so the published frame
    // carries it and the cwd-fallback guard fires on the consumer in-process
    // fold, not just the produce fork.
    let start: jiff::Timestamp = "2026-06-05T13:54:33Z".parse().unwrap();
    let mut frame = frame(vec![pane("terminal_30", Some("codex"), Some("/repo"))]);
    let unstamped = natively_unstamped(&frame);
    stamp_pane_process_starts(&mut frame, &unstamped, &|_, _| None, &|kind, cwd| {
        assert_eq!(kind, "codex");
        assert_eq!(cwd, "/repo");
        vec![start]
    });
    assert_eq!(first(&frame).current.started_at, Some(start));
}

#[test]
fn stamp_pane_process_starts_classifies_from_spawn_command() {
    let start: jiff::Timestamp = "2026-06-05T13:54:33Z".parse().unwrap();
    let mut pane = pane("terminal_30", None, Some("/repo"));
    pane.pane_pid = Some(777);
    pane.spawn_command = Some("rimz agents exec codex --worktree-path /repo".to_owned());
    let mut frame = frame(vec![pane]);
    let unstamped = natively_unstamped(&frame);

    stamp_pane_process_starts(
        &mut frame,
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

    assert_eq!(first(&frame).current.started_at, Some(start));
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

fn write_snapshot_cache(path: &Path, session: &str, produced_at_ms: u64) {
    let cache = crate::sidebar::frame::assemble_frame(Vec::new(), produced_at_ms, session);
    atomic::write_temp_then_rename(path, &cache).expect("write snapshot cache");
}

#[test]
fn snapshot_cache_serves_a_fresh_same_session_entry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("snapshot.json");
    write_snapshot_cache(&path, "rimz-query-engine", unix_now_ms());
    assert!(fresh_snapshot_cache(&path, "rimz-query-engine", None, SNAPSHOT_CACHE_TTL).is_some());
}

#[test]
fn snapshot_cache_misses_a_different_session() {
    // One session's panes must never be served to a sidebar pinned to
    // another — the Zellij backend stamps PaneRef.session_name from the
    // requested session, so a cross-session hit would mislabel panes.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("snapshot.json");
    write_snapshot_cache(&path, "rimz-query-engine", unix_now_ms());
    assert!(fresh_snapshot_cache(&path, "rimz-other", None, SNAPSHOT_CACHE_TTL).is_none());
}

#[test]
fn snapshot_cache_misses_a_stale_entry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("snapshot.json");
    let stale = unix_now_ms().saturating_sub(SNAPSHOT_CACHE_TTL.as_millis() as u64 + 1);
    write_snapshot_cache(&path, "rimz-query-engine", stale);
    assert!(fresh_snapshot_cache(&path, "rimz-query-engine", None, SNAPSHOT_CACHE_TTL).is_none());
}

#[test]
fn snapshot_cache_misses_before_requested_pane_freshness_floor() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("snapshot.json");
    let produced_at_ms = unix_now_ms();
    write_snapshot_cache(&path, "rimz-query-engine", produced_at_ms);

    assert!(
        fresh_snapshot_cache(
            &path,
            "rimz-query-engine",
            Some(produced_at_ms),
            SNAPSHOT_CACHE_TTL
        )
        .is_some(),
        "a cache produced at the requested floor is usable"
    );
    assert!(
        fresh_snapshot_cache(
            &path,
            "rimz-query-engine",
            Some(produced_at_ms.saturating_add(1)),
            SNAPSHOT_CACHE_TTL,
        )
        .is_none(),
        "a pane-sensitive wakeup rejects the pre-signal pane cache"
    );
}

#[test]
fn snapshot_cache_misses_when_absent_or_unreadable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("snapshot.json");
    assert!(fresh_snapshot_cache(&path, "rimz-query-engine", None, SNAPSHOT_CACHE_TTL).is_none());
    std::fs::write(&path, b"{ not json").unwrap();
    assert!(fresh_snapshot_cache(&path, "rimz-query-engine", None, SNAPSHOT_CACHE_TTL).is_none());
}

#[test]
fn read_only_consumer_serves_a_stale_same_session_base() {
    // A `--no-produce` renderer holds the producer's last published base even
    // past the freshness TTL — it renders the last good frame rather than
    // forking its own `list-panes`. The fresh-only read (the producer's fast
    // path) misses the stale entry; the TTL-agnostic read still serves it.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("snapshot.json");
    let stale = unix_now_ms().saturating_sub(SNAPSHOT_CACHE_TTL.as_millis() as u64 + 1);
    write_snapshot_cache(&path, "rimz-query-engine", stale);
    assert!(
        fresh_snapshot_cache(&path, "rimz-query-engine", None, SNAPSHOT_CACHE_TTL).is_none(),
        "the producer's fresh-only fast path skips a stale entry"
    );
    assert!(
        read_snapshot_cache(&path, "rimz-query-engine").is_some(),
        "the consumer's read serves the stale entry as last-good"
    );
}

#[test]
fn metrics_only_refresh_preserves_the_pane_frame_timestamp() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = crate::RuntimePaths::under(
        crate::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/metrics-frame")),
        dir.path(),
    )
    .unwrap();
    runtime.ensure_dirs().unwrap();

    let mut pidded = pane("terminal_1", Some("zsh"), Some("/repo"));
    pidded.pane_pid = Some(std::process::id());
    let produced_at_ms = unix_now_ms();
    let frame = crate::sidebar::frame::assemble_frame(vec![pidded], produced_at_ms, "s");
    let cache_path = runtime.root.join("snapshot.json");
    atomic::write_temp_then_rename_cache(&cache_path, &frame).unwrap();

    let refreshed = refresh_cached_metrics(
        frame,
        &runtime,
        &cache_path,
        &runtime.root.join("snapshot.lock"),
        "s",
        None,
        SNAPSHOT_CACHE_TTL,
    );

    assert_eq!(refreshed.produced_at_ms, produced_at_ms);
    let published = read_snapshot_cache(&cache_path, "s").unwrap();
    assert_eq!(
        published.produced_at_ms, produced_at_ms,
        "a metrics-only publish must not masquerade as a fresh pane listing"
    );
    assert!(
        runtime.root.join("metrics-sample.json").exists(),
        "metrics refresh samples /proc and writes its own cache"
    );
}
