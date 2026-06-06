use super::*;
use crate::ledger::atomic;
use crate::sidebar::produce::test_support::pane;
use crate::sidebar::snapshot::SNAPSHOT_CACHE_TTL;

#[test]
fn pane_fields_carry_forward_by_pane_id() {
    // A degraded read drops command and cwd; the last good read of the same
    // pane id backfills them so the row keeps its agent label and worktree
    // group instead of flashing a bare `process` under `external`.
    let mut fresh = vec![pane("terminal_1", None, None)];
    let prev = vec![pane("terminal_1", Some("claude"), Some("/repo"))];
    carry_forward_pane_fields(&mut fresh, &prev);
    assert_eq!(fresh[0].command.as_deref(), Some("claude"));
    assert_eq!(fresh[0].cwd.as_deref(), Some("/repo"));
}

#[test]
fn carry_forward_does_not_cross_pane_id() {
    // A different (e.g. reused) pane id reports its own fresh fields and is
    // never backfilled from a stranger's last-good read.
    let mut fresh = vec![pane("terminal_2", None, None)];
    let prev = vec![pane("terminal_1", Some("claude"), Some("/repo"))];
    carry_forward_pane_fields(&mut fresh, &prev);
    assert_eq!(fresh[0].command, None);
    assert_eq!(fresh[0].cwd, None);
}

#[test]
fn fresh_pane_field_wins_when_present() {
    // A genuine handoff (claude → zsh) is a real fresh value, not a dropped
    // field, so it is never overwritten by the prior tenant's command.
    let mut fresh = vec![pane("terminal_1", Some("zsh"), Some("/now"))];
    let prev = vec![pane("terminal_1", Some("claude"), Some("/repo"))];
    carry_forward_pane_fields(&mut fresh, &prev);
    assert_eq!(fresh[0].command.as_deref(), Some("zsh"));
    assert_eq!(fresh[0].cwd.as_deref(), Some("/now"));
}

#[test]
fn command_handoff_does_not_carry_forward_process_start() {
    // A foreground handoff (codex → zsh) means the old in-pane agent process is
    // gone. Carrying its start into the shell pane would let stale daemon-backed
    // session state keep binding to the pane.
    let old_start: jiff::Timestamp = "2026-06-05T12:00:00Z".parse().unwrap();
    let mut fresh = vec![pane("terminal_1", Some("zsh"), Some("/repo"))];
    let mut prev = vec![pane("terminal_1", Some("codex"), Some("/repo"))];
    prev[0].pane_process_start = Some(old_start);

    carry_forward_pane_fields(&mut fresh, &prev);

    assert_eq!(fresh[0].command.as_deref(), Some("zsh"));
    assert_eq!(fresh[0].pane_process_start, None);
}

#[test]
fn carry_forward_from_cache_backfills_from_disk() {
    // The shared repair both produce arms run: a raced read's dropped
    // fields backfill from the on-disk pane cache, so the wedged-producer
    // fallback path renders no anonymous row either.
    let dir = tempfile::tempdir().unwrap();
    let cache_path = dir.path().join("snapshot.json");
    let prior = SnapshotCache {
        produced_at_ms: 1,
        session_name: "s".to_owned(),
        panes: vec![pane("terminal_1", Some("claude"), Some("/repo"))],
    };
    atomic::write_temp_then_rename_cache(&cache_path, &prior).unwrap();
    let mut panes = vec![pane("terminal_1", None, None)];
    carry_forward_from_cache(&mut panes, &cache_path, "s");
    assert_eq!(panes[0].command.as_deref(), Some("claude"));
    assert_eq!(panes[0].cwd.as_deref(), Some("/repo"));
}

#[test]
fn carry_forward_from_cache_is_noop_without_prior() {
    // No cache on disk (the first tick after session birth): the read
    // passes through untouched rather than erroring.
    let dir = tempfile::tempdir().unwrap();
    let cache_path = dir.path().join("snapshot.json");
    let mut panes = vec![pane("terminal_1", None, None)];
    carry_forward_from_cache(&mut panes, &cache_path, "s");
    assert_eq!(panes[0].command, None);
    assert_eq!(panes[0].cwd, None);
}

#[test]
fn stamp_pane_process_starts_stamps_a_codex_pane_lacking_a_native_start() {
    // A Zellij codex pane arrives with no native process start and no pid
    // binding yet; the warmup cwd scan derives one so the published frame
    // carries it and the cwd-fallback guard fires on the consumer in-process
    // fold, not just the produce fork.
    let start: jiff::Timestamp = "2026-06-05T13:54:33Z".parse().unwrap();
    let mut panes = vec![pane("terminal_30", Some("codex"), Some("/repo"))];
    let unstamped = natively_unstamped(&panes);
    stamp_pane_process_starts(&mut panes, &unstamped, &|_, _| None, &|kind, cwd| {
        assert_eq!(kind, "codex");
        assert_eq!(cwd, "/repo");
        Some(start)
    });
    assert_eq!(panes[0].pane_process_start, Some(start));
}

#[test]
fn stamp_pane_process_starts_never_touches_a_native_start() {
    // A pane the backend stamped natively (tmux) is outside the set captured
    // from the fresh read, so its start is authoritative — neither deriver is
    // ever consulted, even when a pid binding exists.
    let native: jiff::Timestamp = "2026-06-05T12:00:00Z".parse().unwrap();
    let mut panes = vec![pane("terminal_30", Some("codex"), Some("/repo"))];
    panes[0].pane_process_start = Some(native);
    panes[0].pane_pid = Some(100);
    let unstamped = natively_unstamped(&panes);
    stamp_pane_process_starts(
        &mut panes,
        &unstamped,
        &|_, _| panic!("must not derive over a native start"),
        &|_, _| panic!("must not scan over a native start"),
    );
    assert_eq!(panes[0].pane_process_start, Some(native));
}

#[test]
fn stamp_pane_process_starts_rederives_from_the_bound_root_pid() {
    // A re-tenanted pane — the agent exited and was re-run in place — carries
    // the prior tenant's stamp forward; the agent CLI behind the bound root
    // is the live process, so its start overwrites the carried one and the
    // bind guard refuses the old tenant's session again.
    let carried: jiff::Timestamp = "2026-06-05T12:00:00Z".parse().unwrap();
    let rebound: jiff::Timestamp = "2026-06-05T14:35:00Z".parse().unwrap();
    let mut panes = vec![pane("terminal_30", Some("codex"), Some("/repo"))];
    let unstamped = natively_unstamped(&panes);
    panes[0].pane_process_start = Some(carried);
    panes[0].pane_pid = Some(200);
    stamp_pane_process_starts(
        &mut panes,
        &unstamped,
        &|kind, pid| {
            assert_eq!(kind, "codex");
            assert_eq!(pid, 200);
            Some(rebound)
        },
        &|_, _| panic!("the root rung resolved; the cwd scan must not run"),
    );
    assert_eq!(panes[0].pane_process_start, Some(rebound));
}

#[test]
fn stamp_pane_process_starts_keeps_the_carried_stamp_when_the_pid_is_gone() {
    // The binding's process is gone (a fresh-window re-tenancy, an exited
    // pane): the carried stamp bridges the gap rather than rescanning — a cwd
    // scan on an exited pane would erase the stamp and let a stale session
    // bind again.
    let carried: jiff::Timestamp = "2026-06-05T12:00:00Z".parse().unwrap();
    let mut panes = vec![pane("terminal_30", Some("codex"), Some("/repo"))];
    let unstamped = natively_unstamped(&panes);
    panes[0].pane_process_start = Some(carried);
    panes[0].pane_pid = Some(100);
    stamp_pane_process_starts(&mut panes, &unstamped, &|_, _| None, &|_, _| {
        panic!("carried stamp present; the cwd scan must not run")
    });
    assert_eq!(panes[0].pane_process_start, Some(carried));
}

#[test]
fn stamp_pane_process_starts_skips_non_agent_and_cwdless_panes() {
    // The derivers are consulted only for an agent pane: a shell pane stays
    // unstamped even with a pid binding, and a cwd-less agent pane skips the
    // scan (the guard then falls back to most-recently-active, the documented
    // other-user case).
    let start: jiff::Timestamp = "2026-06-05T13:54:33Z".parse().unwrap();
    let mut panes = vec![
        pane("terminal_1", Some("zsh"), Some("/repo")),
        pane("terminal_2", Some("codex"), None),
        pane("terminal_3", Some("codex"), Some("")),
    ];
    panes[0].pane_pid = Some(100);
    let unstamped = natively_unstamped(&panes);
    stamp_pane_process_starts(&mut panes, &unstamped, &|_, _| Some(start), &|_, _| {
        Some(start)
    });
    assert!(panes[0].pane_process_start.is_none());
    assert!(panes[1].pane_process_start.is_none());
    assert!(panes[2].pane_process_start.is_none());
}

fn write_snapshot_cache(path: &Path, session: &str, produced_at_ms: u64) {
    let cache = SnapshotCache {
        produced_at_ms,
        session_name: session.to_owned(),
        panes: Vec::new(),
    };
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
