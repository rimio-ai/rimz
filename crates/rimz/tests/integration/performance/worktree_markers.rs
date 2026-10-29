//! Marker-read budgets over the host's fold and heavy-lane refresh, including
//! each-worktree condition discovery and scoped Git workers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;

use assert_cmd::assert::OutputAssertExt;
use rimz::config::{LoopConfig, TaskEntry, Tasks};
use rimz::sidebar::consumer::RollupCursor;
use rimz::sidebar::frame::PaneFrame;
use rimz::sidebar::produce::refresh_producer_caches;
use rimz::sidebar::refresh::ProducerRefreshState;
use rimz::store::snapshot::SidebarWorktreeKind;
use rimz::testkit::WORK_COUNTERS;

use crate::common::{Env, git};

#[test]
fn refresh_reads_each_marker_once_with_two_worktrees() {
    refresh_case(
        2,
        "performance::worktree_markers::refresh_reads_each_marker_once_with_two_worktrees",
    );
}

#[test]
fn refresh_reads_each_marker_once_with_five_worktrees() {
    refresh_case(
        5,
        "performance::worktree_markers::refresh_reads_each_marker_once_with_five_worktrees",
    );
}

fn refresh_case(count: usize, test: &str) {
    const CHILD_ROOT: &str = "RIMZ_TEST_MARKER_REFRESH_ROOT";
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        refresh_in_child(Path::new(&root), count);
        return;
    }
    if git::git_missing() {
        return;
    }
    let env = Env::new();
    git::init_repo(&env.project_root);
    env.record(&env.project_root);
    let config = rimz::config::WorktreeConfig::default();
    let mut panes = rimz::testkit::fleet::synthetic_panes(count);
    for (index, pane) in panes.iter_mut().enumerate() {
        let name = format!("marker-{index}");
        env.rimz()
            .args(["worktree", "new", &name])
            .assert()
            .success();
        pane.cwd = Some(
            rimz::worktree::worktree_path(&env.project_root, &config, &name)
                .unwrap()
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        );
    }
    let tasks = Tasks(BTreeMap::from([(
        "marker-demand".to_owned(),
        TaskEntry {
            root: env.project_root.clone(),
            agent: Some("codex".to_owned()),
            prompt: Some("inspect CI".to_owned()),
            stay: true,
            each_worktree: true,
            when: Some(vec!["ci=passed".to_owned()]),
            check: Some("true".into()),
            ..Default::default()
        },
    )]));
    std::fs::write(
        env.rimz_home().join("loop.toml"),
        toml::to_string(&LoopConfig {
            tasks,
            ..Default::default()
        })
        .unwrap(),
    )
    .unwrap();
    let runtime = env.runtime_paths();
    runtime.ensure_dirs().unwrap();
    let frame = rimz::sidebar::frame::assemble_frame(
        panes,
        rimz::utils::time::unix_now_ms(),
        rimz::testkit::fleet::SESSION_NAME.to_owned(),
    );
    std::fs::write(
        runtime.pane_frame_path(),
        serde_json::to_vec(&frame).unwrap(),
    )
    .unwrap();

    let bin = env.home_root.join("bin");
    std::fs::create_dir(&bin).unwrap();
    std::os::unix::fs::symlink(
        crate::common::cargo_bin("git-trace", env!("CARGO_BIN_EXE_git-trace")),
        bin.join("git"),
    )
    .unwrap();
    let output = env
        .rimz_at(&std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(CHILD_ROOT, &env.project_root)
        .env("RIMZ_TEST_GIT_LOG", env.home_root.join("git.log"))
        .env("RIMZ_TEST_REAL_GIT", which::which("git").unwrap())
        .env("PATH", crate::common::path_with_front(&bin))
        .env("RIMZ_OAUTH_USAGE_OFFLINE", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("test result: ok. 1 passed;"),
        "the child must run exactly this test:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn refresh_in_child(root: &Path, count: usize) {
    use rimz::harness::schedule::{arming, catalog::TaskCatalog};

    let home = PathBuf::from(std::env::var_os("RIMZ_HOME").unwrap());
    let state = rimz::StatePaths::for_project_root_under(root, &home).unwrap();
    let mut runtime = rimz::RuntimePaths::for_state_under(
        &state,
        Path::new(&std::env::var_os("XDG_RUNTIME_DIR").unwrap()),
    );
    runtime.persistent_shared_root = home.join("cache/providers");
    let frame: PaneFrame =
        serde_json::from_slice(&std::fs::read(runtime.pane_frame_path()).unwrap()).unwrap();
    let catalog = TaskCatalog::load_lenient(Some(root));
    let task = &catalog.visible()["marker-demand"];
    assert!(task.entry().each_worktree);
    assert!(task.action().is_ok(), "{:?}", task.action());
    assert_eq!(
        arming::ArmState::resolve(None, task.source(), jiff::Timestamp::now()),
        arming::ArmState::Live
    );
    let log = PathBuf::from(std::env::var_os("RIMZ_TEST_GIT_LOG").unwrap());
    let mut cursor = RollupCursor::new();
    let mut refresh = ProducerRefreshState::default();
    let mut reads = Vec::new();
    let before = WORK_COUNTERS.marker_reads.load(Relaxed);
    for pass in 0..3 {
        let workspace = super::workspace_for_refresh(&mut cursor, &state, &runtime, &frame);
        let snapshot = rimz::sidebar::enrich::project_local(workspace.clone(), Some(&frame), None);
        assert_eq!(snapshot.project_root.as_deref(), Some(root));
        assert_eq!(snapshot.worktree_groups.len(), count);
        let groups: Vec<_> = snapshot
            .worktree_groups
            .iter()
            .map(|group| (&group.label, group.kind))
            .collect();
        assert!(
            groups
                .iter()
                .all(|(_, kind)| *kind == SidebarWorktreeKind::Channel),
            "{groups:?}; home={:?}",
            snapshot.worktree_home
        );
        refresh_producer_caches(workspace, Some(&frame), &state, &runtime, &mut refresh).unwrap();
        reads.push(WORK_COUNTERS.marker_reads.load(Relaxed) - before);
        if pass == 0 {
            let trace = std::fs::read_to_string(&log).unwrap();
            assert_eq!(
                trace.matches("worktree\tlist\t--porcelain").count(),
                1,
                "live condition discovery must enumerate Git rows:\n{trace}"
            );
        }
        if pass == 1 {
            let checkout = PathBuf::from(frame.to_pane_refs()[0].cwd.as_ref().unwrap());
            let admin = git::git_stdout(&checkout, &["rev-parse", "--git-dir"]);
            let marker_path = Path::new(&admin).join("rimz-worktree.json");
            let old_file = std::fs::File::open(&marker_path).unwrap();
            let mut marker: rimz::worktree::WorktreeMarker =
                serde_json::from_reader(&old_file).unwrap();
            marker.from_pr = Some(1234);
            rimz::disk::atomic::write_temp_then_rename(&marker_path, &marker).unwrap();
        }
    }
    assert_eq!(
        reads,
        [count as u64, count as u64, count as u64 + 1],
        "cold, unchanged, one atomic replacement"
    );
}
