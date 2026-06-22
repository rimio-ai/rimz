//! The enrichment cadence gates, end to end (docs/internals/health/performance.md,
//! the 2026-06 enrichment cadence pass): each producer enrichment runs on its
//! own TTL'd clock, gated by a persisted stamp, so a produce inside every
//! window does stamp reads only — no git forks, no transcript IO. Reuses the
//! diff-stats [`Fixture`]: a real on-disk worktree pane, the `git-trace` shim
//! on PATH counting true cross-process forks.
//!
//! The `/proc` metrics gate has no witness here by design: the
//! `RIMZ_TEST_PANE_LIST` seam deliberately bypasses the shared pane cache and
//! its produce arm, where `enrich_pane_metrics` lives. Its skip/due behaviour
//! is pinned by the unit gates in `sidebar::produce::metrics::tests`
//! (`metric*`).

#![allow(clippy::print_stderr)] // self-skip notices, like the sibling fixture

use rimz::sidebar::cache::unix_now_ms;
use rimz::sidebar::consumer::RollupCursor;

use super::sidebar_diff_stats::Fixture;

/// `git worktree list` runs once per `WORKTREE_ROOTS_TTL`, except that a
/// session boundary — a produce carrying the `--min-pane-cache-ms` freshness
/// floor — refuses the cached enumeration even inside the TTL, so a brand-new
/// checkout groups correctly on its first agent's first snapshot. The fork
/// under witness is the repo room's enumeration, so the room root must be a
/// repo: a bare root records as a directory room, whose enumeration is one
/// `read_dir` and forks no git (pinned by
/// `directory_room_enumeration_forks_no_git`).
#[test]
fn worktree_roots_reenumerate_on_session_boundary_only() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    if !fixture.init_repo_room() {
        eprintln!("git init failed; skipping enumeration-cadence test");
        return;
    }
    // The enumeration runs only for a recorded workspace (the snapshot's
    // `project_root` feeds it), and the record carries the root class.
    fixture.env.record(&fixture.env.project_root);

    let cold = fixture.run_snapshot();
    assert!(
        cold.status.success(),
        "cold snapshot failed:\n{}",
        String::from_utf8_lossy(&cold.stderr),
    );
    assert_eq!(
        fixture.git_forks("worktree\tlist"),
        1,
        "the cold produce enumerates once:\n{}",
        fixture.git_log_contents(),
    );

    let warm = fixture.run_snapshot();
    assert!(warm.status.success());
    assert_eq!(
        fixture.git_forks("worktree\tlist"),
        1,
        "within WORKTREE_ROOTS_TTL the enumeration serves from cache:\n{}",
        fixture.git_log_contents(),
    );

    // The session boundary: the renderer demands caches at least this young.
    let mut boundary = fixture.snapshot_command();
    boundary
        .arg("--min-pane-cache-ms")
        .arg(unix_now_ms().to_string());
    let output = boundary.output().expect("spawn boundary snapshot");
    assert!(
        output.status.success(),
        "boundary snapshot failed:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        fixture.git_forks("worktree\tlist"),
        2,
        "the freshness floor refuses the cached enumeration and re-runs it:\n{}",
        fixture.git_log_contents(),
    );
}

/// The fixture seam's isolation contract: a produce driven by
/// `RIMZ_TEST_PANE_LIST` short-circuits before the shared pane cache, so a
/// deterministic test can neither poison nor read it. The short-circuit lives
/// at the library entry (`rimz::sidebar::produce::produce_snapshot` resolves
/// the fixture itself), so the CLI delegate and the in-process fetch worker
/// honor it identically.
#[test]
fn fixture_produce_never_touches_the_shared_pane_cache() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let cold = fixture.run_snapshot();
    assert!(
        cold.status.success(),
        "fixture snapshot failed:\n{}",
        String::from_utf8_lossy(&cold.stderr),
    );
    assert!(
        !fixture
            .env
            .runtime_paths()
            .root
            .join("snapshot.json")
            .exists(),
        "a fixture-driven produce must not write the shared pane cache"
    );
}

#[test]
fn cache_refresher_publishes_diff_stats_project_matches_refresh() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let _ledger = fixture.env.ledger();
    let session = fixture.publish_pane_frame();
    let state = fixture.env.state_path_for(&fixture.env.project_root);
    let runtime = fixture.env.runtime_paths();
    let accounts = rimz::sidebar::cache::AccountsCache {
        refreshed_at_ms: unix_now_ms(),
        accounts: Default::default(),
        ok: true,
    };
    std::fs::write(
        runtime.shared_accounts_path(),
        serde_json::to_vec(&accounts).expect("serialize accounts"),
    )
    .expect("seed accounts cache");
    rimz::agents::spending::write_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
        unix_now_ms(),
        &rimz::agents::spending::Spending::default(),
    );

    let mut cursor = RollupCursor::new();
    rimz::sidebar::produce::refresh_producer_caches(&mut cursor, &state, &runtime, &session, None)
        .expect("refresh producer caches");

    let provider_path = runtime.shared_provider_spending_path();
    let accounts_path = runtime.shared_accounts_path();
    let diff_stats_path = runtime.root.join("diff-stats.json");
    let diff_stats = rimz::sidebar::cache::read_diff_stats_cache(&diff_stats_path);
    assert!(
        !diff_stats.entries.is_empty(),
        "refresher publishes diff stats for the live worktree"
    );

    let provider_bytes = std::fs::read(&provider_path).expect("provider cache");
    let accounts_bytes = std::fs::read(&accounts_path).expect("accounts cache");
    let diff_stats_bytes = std::fs::read(&diff_stats_path).expect("diff stats cache");
    rimz::sidebar::produce::refresh_producer_caches(&mut cursor, &state, &runtime, &session, None)
        .expect("second refresh");
    assert_eq!(
        std::fs::read(&provider_path).expect("provider cache"),
        provider_bytes,
        "provider spending TTL serves the second refresh without re-publishing"
    );
    assert_eq!(
        std::fs::read(&accounts_path).expect("accounts cache"),
        accounts_bytes,
        "accounts TTL serves the second refresh without re-publishing"
    );
    assert_eq!(
        std::fs::read(&diff_stats_path).expect("diff stats cache"),
        diff_stats_bytes,
        "diff-stats TTL serves the second refresh without re-publishing"
    );

    let project_opts = rimz::sidebar::produce::ProduceOptions {
        mux: rimz::MuxName::Zellij,
        session_name: session.clone(),
        exclude: None,
        min_pane_cache_ms: None,
        diag: None,
        heavy_lanes: rimz::sidebar::produce::HeavyLaneMode::Project,
    };
    let refresh_opts = rimz::sidebar::produce::ProduceOptions {
        heavy_lanes: rimz::sidebar::produce::HeavyLaneMode::Refresh,
        ..project_opts.clone()
    };
    let project = rimz::sidebar::produce::produce_snapshot(
        &mut RollupCursor::new(),
        &state,
        &runtime,
        &project_opts,
    )
    .expect("project produce");
    let refresh = rimz::sidebar::produce::produce_snapshot(
        &mut RollupCursor::new(),
        &state,
        &runtime,
        &refresh_opts,
    )
    .expect("refresh produce");

    assert_eq!(project.value_tally, refresh.value_tally);
    assert_eq!(project.workspace_value_tally, refresh.workspace_value_tally);
    assert_eq!(project.providers, refresh.providers);
    assert_eq!(project.worktree_groups.len(), refresh.worktree_groups.len());
    for (project_group, refresh_group) in project
        .worktree_groups
        .iter()
        .zip(refresh.worktree_groups.iter())
    {
        assert_eq!(project_group.key, refresh_group.key);
        assert_eq!(project_group.diff_added, refresh_group.diff_added);
        assert_eq!(project_group.diff_removed, refresh_group.diff_removed);
        assert_eq!(project_group.commits_ahead, refresh_group.commits_ahead);
        assert_eq!(project_group.commits_behind, refresh_group.commits_behind);
        assert_eq!(project_group.trunk, refresh_group.trunk);
        assert_eq!(project_group.clean, refresh_group.clean);
        assert_eq!(project_group.landed, refresh_group.landed);
    }
}

/// A directory room's group-root enumeration is one `read_dir`: the cold
/// produce discovers the depth-1 child repo and pays its diff-stats chain,
/// but never forks `git worktree list` — the fixture's bare room root records
/// as a directory room, and the child repo at `<root>/worktree` is its one
/// depth-1 child.
#[test]
fn directory_room_enumeration_forks_no_git() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    fixture.env.record(&fixture.env.project_root);

    let cold = fixture.run_snapshot();
    assert!(
        cold.status.success(),
        "cold snapshot failed:\n{}",
        String::from_utf8_lossy(&cold.stderr),
    );
    assert_eq!(
        fixture.git_forks("worktree\tlist"),
        0,
        "a directory room enumerates by read_dir, never `git worktree list`:\n{}",
        fixture.git_log_contents(),
    );
    assert!(
        fixture.git_forks("rev-parse") >= 1,
        "the child-repo pod still pays its diff-stats probes:\n{}",
        cadence_debug(&fixture, &cold.stdout),
    );
}

/// The principle-5 regression net: an idle room's produce performs no
/// enrichment IO. The worktree group here holds only a process row (bash), so
/// it is cold by definition; its diff-stats stamp is backdated to
/// hot-stale-but-idle-fresh, which the pre-tiering flat 5s TTL would have
/// re-forked — zero new git forks is the activity tiering working end to end.
/// The spending stamp proves its gate the same way: a byte-equal published
/// cache across the produce.
#[test]
fn idle_room_produce_runs_no_enrichment_io() {
    let Some(fixture) = Fixture::new() else {
        return;
    };

    let cold = fixture.run_snapshot();
    assert!(
        cold.status.success(),
        "cold snapshot failed:\n{}",
        String::from_utf8_lossy(&cold.stderr),
    );

    let runtime = fixture.env.runtime_paths();
    let runtime_root = runtime.root.clone();
    let now_ms = unix_now_ms();

    // Backdate the per-worktree git stamps into the tier gap: stale under
    // DIFF_STATS_TTL (5s), fresh under DIFF_STATS_IDLE_TTL (60s).
    let diff_stats_path = runtime_root.join("diff-stats.json");
    let mut diff_stats = rimz::sidebar::cache::read_diff_stats_cache(&diff_stats_path);
    assert!(
        !diff_stats.entries.is_empty(),
        "the cold produce cached the worktree's git facts:\n{}",
        cadence_debug(&fixture, &cold.stdout),
    );
    for entry in diff_stats.entries.values_mut() {
        entry.refreshed_at_ms = now_ms - 10_000;
    }
    std::fs::write(
        &diff_stats_path,
        serde_json::to_vec(&diff_stats).expect("serialize diff stats"),
    )
    .expect("backdate diff stats");

    // The cold produce stamped the (empty-fleet) spending publish; hold it.
    let spending_path = runtime.shared_provider_spending_path();
    let spending_bytes = std::fs::read(&spending_path)
        .expect("the cold produce publishes a stamped provider-spending cache");

    let forks_before = fixture.git_log_len();

    let idle = fixture.run_snapshot();
    assert!(
        idle.status.success(),
        "idle snapshot failed:\n{}",
        String::from_utf8_lossy(&idle.stderr),
    );

    assert_eq!(
        fixture.git_log_len(),
        forks_before,
        "an idle room's produce forks zero git — the idle tier holds where the \
         flat 5s TTL would have re-forked:\n{}",
        fixture.git_log_contents(),
    );
    assert_eq!(
        std::fs::read(&spending_path).expect("provider-spending cache"),
        spending_bytes,
        "the spending gate serves the published walk without re-stamping"
    );
}

fn cadence_debug(fixture: &Fixture, stdout: &[u8]) -> String {
    let worktree = fixture.env.project_root.join("worktree");
    let worktree_git = worktree.join(".git");
    let groups = serde_json::from_slice::<serde_json::Value>(stdout)
        .ok()
        .and_then(|snapshot| serde_json::to_string_pretty(&snapshot["worktree_groups"]).ok())
        .unwrap_or_else(|| String::from("<snapshot json unavailable>"));
    let entries = std::fs::read_dir(&fixture.env.project_root)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| {
                    let path = entry.path();
                    let git = path.join(".git");
                    format!(
                        "{} dir={} git_exists={} git_dir={}",
                        path.display(),
                        path.is_dir(),
                        git.exists(),
                        git.is_dir()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|err| format!("<read_dir failed: {err}>"));
    let diff_stats =
        std::fs::read_to_string(fixture.env.runtime_paths().root.join("diff-stats.json"))
            .unwrap_or_else(|err| format!("<diff-stats unavailable: {err}>"));
    format!(
        "project_root: {}\nworktree: {} exists={} dir={}\nworktree_git: {} exists={} dir={}\nproject entries:\n{}\nworktree_groups:\n{}\ndiff-stats.json:\n{}\ngit trace:\n{}",
        fixture.env.project_root.display(),
        worktree.display(),
        worktree.exists(),
        worktree.is_dir(),
        worktree_git.display(),
        worktree_git.exists(),
        worktree_git.is_dir(),
        entries,
        groups,
        diff_stats,
        fixture.git_log_contents()
    )
}
