//! The in-process produce cost at fleet scale.
//!
//! The elder renderer runs `rimz::sidebar::produce::produce_workspace_snapshot` on its
//! fetch worker once per data tick (docs/internals/performance.md, the 2026-06
//! warm-producer pass). The contract: a warm steady-state produce — every
//! fork-bearing input pre-published fresh, the rollup folding O(new bytes)
//! through the worker's cursor — forks no subprocess even with a fleet-scale
//! store and pane set. These tests pin that count; the wall-clock side lives in
//! the `fleet::produce_warm` bench (`cargo xtask perf`), because a timing
//! budget checked while nextest runs the whole suite in parallel fails on load,
//! not on regressions.

use rimz::sidebar::consumer::RollupCursor;
use rimz::store::event_log;
use rimz::testkit::fleet::{SESSION_NAME, registered_lifecycle, seed_fleet_store, synthetic_panes};
use rimz::testkit::spawn_count;

use crate::common::Harness;

const FLEET: usize = 40;
const HISTORY_EVENTS: usize = 2_000;
const ROUNDS: u32 = 20;

#[test]
fn warm_fleet_refresh_forks_zero_subprocesses() {
    use rimz::sidebar::produce::{HostInputs, refresh_producer_caches};
    use rimz::sidebar::refresh::ProducerRefreshState;
    use rimz::utils::time::unix_now_ms;

    const CHILD_ROOT: &str = "RIMZ_TEST_REFRESH_BUDGET_ROOT";
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        let home = std::path::PathBuf::from(std::env::var_os("RIMZ_HOME").unwrap());
        let state =
            rimz::StatePaths::for_project_root_under(std::path::Path::new(&root), &home).unwrap();
        let mut runtime = rimz::RuntimePaths::for_state_under(
            &state,
            &std::path::PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap()),
        );
        runtime.persistent_shared_root = home.join("cache/providers");
        let frame =
            serde_json::from_slice(&std::fs::read(runtime.pane_frame_path()).unwrap()).unwrap();
        let inputs = HostInputs::from_frame(frame);
        let mut cursor = RollupCursor::new();
        let mut refresh_state = ProducerRefreshState::default();
        let store = rimz::Store::open(state.clone(), runtime.clone()).unwrap();
        let snapshot = store.snapshot().unwrap();
        assert_eq!(snapshot.agents.len(), FLEET);
        for kind in ["codex", "claude"] {
            assert_eq!(
                snapshot
                    .agents
                    .iter()
                    .filter(|agent| agent.kind.as_str() == kind)
                    .count(),
                FLEET / 2
            );
        }
        let log = std::path::PathBuf::from(std::env::var_os("RIMZ_TEST_GIT_LOG").unwrap());
        std::fs::write(&log, "").unwrap();
        refresh_producer_caches(
            &mut cursor,
            &state,
            &runtime,
            Some(&inputs),
            &mut refresh_state,
        )
        .unwrap();
        let cold_git = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            cold_git.matches("remote\tget-url\torigin").count(),
            1,
            "cold PR assembly lane:\n{cold_git}"
        );
        assert!(
            !cold_git.contains("rev-parse\t--abbrev-ref"),
            "PR branch lane must read HEAD in process:\n{cold_git}"
        );
        std::fs::write(&log, "").unwrap();
        let before = spawn_count();
        refresh_producer_caches(
            &mut cursor,
            &state,
            &runtime,
            Some(&inputs),
            &mut refresh_state,
        )
        .unwrap();
        let warm_git = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            spawn_count() - before,
            0,
            "warm refresh lane (session/provider helpers and Git/forge): expected no spawns; Git lane trace:\n{warm_git}"
        );
        return;
    }

    let Some(fixture) = super::sidebar_diff_stats::Fixture::with_linked_worktrees(3) else {
        return;
    };
    fixture.publish_pane_frame();
    let runtime = fixture.env.runtime_paths();
    let frame: rimz::sidebar::frame::PaneFrame =
        serde_json::from_slice(&std::fs::read(runtime.pane_frame_path()).unwrap()).unwrap();
    let roots: Vec<_> = frame
        .to_pane_refs()
        .into_iter()
        .map(|pane| pane.cwd.unwrap())
        .collect();
    let mut panes = synthetic_panes(FLEET);
    for (index, pane) in panes.iter_mut().enumerate() {
        pane.cwd = Some(roots[index % roots.len()].clone());
    }
    let store = fixture.env.store();
    for index in 0..FLEET {
        let mut event = registered_lifecycle(&fixture.env.workspace_id, index);
        if index % 2 == 0 {
            event.source = "codex".to_owned();
        }
        event_log::append(&store.paths().events_log, &event).unwrap();
    }
    std::fs::write(
        runtime.pane_frame_path(),
        serde_json::to_vec(&rimz::sidebar::frame::assemble_frame(
            panes,
            unix_now_ms(),
            SESSION_NAME.to_owned(),
        ))
        .unwrap(),
    )
    .unwrap();
    rimz::agents::spending::write_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
        &rimz::agents::spending::ProviderSpendingCache {
            refreshed_at_ms: unix_now_ms(),
            day_cutoff_secs: rimz::agents::spending::local_day_start_secs(
                unix_now_ms() / 1000,
                None,
            )
            .unwrap(),
            timezone: "UTC".into(),
            chart: rimz::agents::spending::ChartSpend {
                today: (unix_now_ms() / 86_400_000) as i64,
                ..Default::default()
            },
            ..Default::default()
        },
    );
    crate::common::write_path_shim(&fixture.env.project_root.join("bin"), "gh", "exit 1");
    let mut child = fixture.env.rimz_at(&std::env::current_exe().unwrap());
    let traced = fixture.snapshot_command();
    child.envs(
        traced
            .get_envs()
            .filter_map(|(key, value)| value.map(|value| (key, value))),
    );
    let output = child
        .args([
            "--exact",
            "performance::produce_budget::warm_fleet_refresh_forks_zero_subprocesses",
            "--nocapture",
        ])
        .env(CHILD_ROOT, &fixture.env.project_root)
        .env("RIMZ_OAUTH_USAGE_OFFLINE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn scale_room_preserves_agent_pane_and_fence_dimensions() {
    for (agents, panes, ended_sessions) in [(51, 24, 4_600), (200, 100, 5_000)] {
        let h = Harness::new();
        let paths = h.store.paths();
        let frame = rimz::testkit::fleet::seed_scale_room(
            paths,
            &h.runtime_paths,
            rimz::testkit::fleet::ScaleRoom {
                agents,
                panes,
                ended_sessions,
                orphan_contexts: 0,
            },
        )
        .expect("scale room");
        let mut cursor = RollupCursor::new();
        let mut producer =
            rimz::sidebar::produce::SnapshotProducer::seeded(&h.runtime_paths, SESSION_NAME);
        let snapshot = rimz::sidebar::produce::produce_snapshot(
            &mut cursor,
            paths,
            &h.runtime_paths,
            &rimz::testkit::fleet::produce_options(),
            &mut producer,
        )
        .expect("produce scale room");
        assert_eq!(frame.to_pane_refs().len(), panes);
        assert_eq!(snapshot.agents.len(), agents);
        assert_eq!(snapshot.agent_panes.len(), panes);
        assert_eq!(snapshot.worktree_groups.len(), panes);
        assert!(snapshot.fenced_sessions.len() >= ended_sessions);
        assert_eq!(
            snapshot
                .worktree_groups
                .iter()
                .map(|group| group.rows.len())
                .sum::<usize>(),
            panes
        );
    }
}

#[test]
fn warm_fleet_produce_forks_zero_subprocesses() {
    let h = Harness::new();
    let paths = h.store.paths();
    seed_fleet_store(paths, FLEET, HISTORY_EVENTS).expect("seed event");
    let panes = synthetic_panes(FLEET);
    let opts = rimz::sidebar::produce::ProduceOptions {
        mux: rimz::MuxName::Zellij,
        session_name: SESSION_NAME.to_owned(),
        exclude: None,
        min_pane_cache_ms: None,
        hibernated_panes: Default::default(),
        diag: rimz::diag::DiagSink::disabled(),
    };
    let mut cursor = RollupCursor::new();

    // The cold produce pays the one-time history fold; uncounted, like the
    // first frame after attach.
    h.publish_fresh_produce_inputs(SESSION_NAME, panes.clone());
    let mut producer =
        rimz::sidebar::produce::SnapshotProducer::seeded(&h.runtime_paths, SESSION_NAME);
    rimz::sidebar::produce::produce_snapshot(
        &mut cursor,
        paths,
        &h.runtime_paths,
        &opts,
        &mut producer,
    )
    .expect("cold produce");

    // Steady state: one delta per tick, every stamp young — the elder's
    // common case. Inputs re-publish before each produce.
    let spawns_before = spawn_count();
    for round in 0..ROUNDS {
        let event = registered_lifecycle(&paths.workspace_id, round as usize % FLEET);
        event_log::append(&paths.events_log, &event).expect("append delta");
        h.publish_fresh_produce_inputs(SESSION_NAME, panes.clone());
        let snapshot = rimz::sidebar::produce::produce_snapshot(
            &mut cursor,
            paths,
            &h.runtime_paths,
            &opts,
            &mut producer,
        )
        .expect("warm produce");
        assert_eq!(snapshot.agents.len(), FLEET);
    }
    assert_eq!(
        spawn_count() - spawns_before,
        0,
        "a warm produce with every fork-bearing input pre-published forks no \
         subprocesses"
    );
}

#[test]
fn project_produce_over_stale_heavy_caches_forks_zero_subprocesses() {
    let h = Harness::new();
    let paths = h.store.paths();
    seed_fleet_store(paths, FLEET, HISTORY_EVENTS).expect("seed event");
    h.publish_fresh_produce_inputs(SESSION_NAME, synthetic_panes(FLEET));
    let _ = std::fs::remove_file(h.runtime_paths.shared_provider_spending_path());
    let _ = std::fs::remove_file(h.runtime_paths.shared_accounts_path());
    let _ = std::fs::remove_file(h.runtime_paths.diff_stats_path());

    let opts = rimz::sidebar::produce::ProduceOptions {
        mux: rimz::MuxName::Zellij,
        session_name: SESSION_NAME.to_owned(),
        exclude: None,
        min_pane_cache_ms: None,
        hibernated_panes: Default::default(),
        diag: rimz::diag::DiagSink::disabled(),
    };
    let mut cursor = RollupCursor::new();

    let mut producer =
        rimz::sidebar::produce::SnapshotProducer::seeded(&h.runtime_paths, SESSION_NAME);
    let spawns_before = spawn_count();
    let snapshot = rimz::sidebar::produce::produce_snapshot(
        &mut cursor,
        paths,
        &h.runtime_paths,
        &opts,
        &mut producer,
    )
    .expect("project produce");
    assert_eq!(snapshot.agents.len(), FLEET);
    assert_eq!(
        spawn_count() - spawns_before,
        0,
        "produce projects missing/stale heavy caches and never refreshes them inline"
    );
}
