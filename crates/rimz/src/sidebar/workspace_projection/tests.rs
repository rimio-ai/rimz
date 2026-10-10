use jiff::Timestamp;

use super::*;
use crate::StatePaths;
use crate::agents::AgentStatus;
use crate::ids::WorkspaceId;
use crate::sidebar::enrich::{FoldOpts, enrich_workspace};
use crate::store::snapshot::SidebarSnapshot;

fn fixture() -> (
    tempfile::TempDir,
    RuntimePaths,
    StatePaths,
    WorkspaceSnapshot,
    PaneFrame,
) {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let state = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
    state.ensure_dirs().unwrap();
    let extent = LogExtent {
        generation: 2,
        offset: 0,
    };
    let mut snapshot = SidebarSnapshot::build(workspace_id, Vec::new(), Timestamp::now());
    snapshot.reflects_log = Some(extent);
    crate::disk::atomic::write_temp_then_rename_cache(&state.latest_snapshot, &snapshot).unwrap();
    let mut frame = crate::sidebar::frame::assemble_frame(Vec::new(), 10, "rimz-test");
    frame.topology_stamp_ms = Some(11);
    frame.metrics_stamp_ms = Some(12);
    (dir, runtime, state, WorkspaceSnapshot(snapshot), frame)
}

#[test]
fn publisher_suppresses_equal_bytes_and_republishes_changed_verdicts() {
    let (_dir, runtime, _state, mut workspace, frame) = fixture();
    let mut publisher = WorkspaceProjectionPublisher::default();

    assert_eq!(
        publisher
            .publish(&runtime, "rimz-test", &workspace, &frame)
            .unwrap(),
        WorkspaceProjectionPublish::Published
    );
    assert_eq!(
        publisher
            .publish(&runtime, "rimz-test", &workspace, &frame)
            .unwrap(),
        WorkspaceProjectionPublish::Unchanged
    );

    // A producer-time window crossing changes an already-derived field while
    // rollup/frame/config source identity remains fixed.
    workspace.0.truth_degraded = Some(crate::store::snapshot::TruthNotice {
        carried: 1,
        since_ms: 10,
        pane_ids: Vec::new(),
    });
    assert_eq!(
        publisher
            .publish(&runtime, "rimz-test", &workspace, &frame)
            .unwrap(),
        WorkspaceProjectionPublish::Published
    );
    let published = read_workspace_projection(&runtime).expect("published projection");
    assert_eq!(
        published.source,
        WorkspaceProjectionSource::from_fold(&workspace, &frame).unwrap()
    );
    assert_eq!(
        published
            .projection
            .0
            .truth_degraded
            .as_ref()
            .unwrap()
            .carried,
        1
    );

    let mut restarted = WorkspaceProjectionPublisher::default();
    assert_eq!(
        restarted
            .publish(&runtime, "rimz-test", &workspace, &frame)
            .unwrap(),
        WorkspaceProjectionPublish::Unchanged,
        "a replacement producer compares the existing serialized bytes"
    );
}

#[test]
fn failed_publication_retries_the_same_content() {
    let (_dir, runtime, _state, workspace, frame) = fixture();
    std::fs::remove_dir_all(&runtime.root).unwrap();
    std::fs::write(&runtime.root, b"blocks projection directory").unwrap();
    let mut publisher = WorkspaceProjectionPublisher::default();
    assert!(
        publisher
            .publish(&runtime, "rimz-test", &workspace, &frame)
            .is_err()
    );

    std::fs::remove_file(&runtime.root).unwrap();
    runtime.ensure_dirs().unwrap();
    assert_eq!(
        publisher
            .publish(&runtime, "rimz-test", &workspace, &frame)
            .unwrap(),
        WorkspaceProjectionPublish::Published,
        "a failed write does not poison content suppression",
    );
}

#[test]
fn quiet_time_transition_republishes_for_attachment_seeds() {
    let (_dir, runtime, state, _workspace, _frame) = fixture();
    let last_activity = Timestamp::from_second(1_750_000_000).unwrap();
    let pane = crate::sidebar::test_support::pane(
        "terminal_agent",
        "claude",
        &runtime.root.to_string_lossy(),
    );
    let mut frame = crate::sidebar::frame::assemble_frame(vec![pane.clone()], 10, "rimz-test");
    frame.topology_stamp_ms = Some(11);
    frame.metrics_stamp_ms = Some(12);
    crate::disk::atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &frame).unwrap();

    let fold_at = |now: Timestamp| {
        let mut agent = crate::testkit::agent_state("claude", "quiet-agent", last_activity);
        agent.status = AgentStatus::Running;
        agent.pane = Some(pane.clone());
        agent.worktree_path = Some(runtime.root.to_string_lossy().into_owned());
        let mut snapshot =
            SidebarSnapshot::build_with_agents(runtime.workspace_id.clone(), vec![agent], now);
        snapshot.reflects_log = Some(LogExtent {
            generation: 2,
            offset: 0,
        });
        enrich_workspace(
            snapshot,
            Some(&frame),
            &state,
            &runtime,
            None,
            FoldOpts {
                producing: false,
                fresh_roots: None,
                config: Some(Arc::new(crate::config::MachineConfig::default())),
                lanes: None,
                agent_projection: Default::default(),
            },
            &crate::diag::DiagSink::disabled(),
        )
    };
    let status = |snapshot: &SidebarSnapshot| {
        snapshot
            .worktree_groups
            .iter()
            .flat_map(|group| &group.rows)
            .find(|row| row.id == "quiet-agent")
            .and_then(crate::store::snapshot::SidebarRow::status)
    };

    let before = fold_at(
        last_activity
            + jiff::SignedDuration::from_secs(i64::from(
                crate::config::AttentionConfig::default()
                    .stalled_after_secs
                    .get()
                    - 1,
            )),
    );
    let after = fold_at(
        last_activity
            + jiff::SignedDuration::from_secs(i64::from(
                crate::config::AttentionConfig::default()
                    .stalled_after_secs
                    .get(),
            )),
    );
    assert_eq!(status(before.snapshot()), Some(AgentStatus::Running));
    assert_eq!(status(after.snapshot()), Some(AgentStatus::Failed));
    assert_eq!(
        WorkspaceProjectionSource::from_fold(&before, &frame),
        WorkspaceProjectionSource::from_fold(&after, &frame),
    );

    let mut publisher = WorkspaceProjectionPublisher::default();
    assert_eq!(
        publisher
            .publish(&runtime, "rimz-test", &before, &frame)
            .unwrap(),
        WorkspaceProjectionPublish::Published
    );
    let first = read_workspace_projection(&runtime).unwrap();
    assert_eq!(
        status(first.projection.snapshot()),
        Some(AgentStatus::Running)
    );

    assert_eq!(
        publisher
            .publish(&runtime, "rimz-test", &after, &frame)
            .unwrap(),
        WorkspaceProjectionPublish::Published,
        "the skipped projection clock still changes its serialized verdict",
    );
    let second = read_workspace_projection(&runtime).unwrap();
    assert_eq!(
        status(second.projection.snapshot()),
        Some(AgentStatus::Failed)
    );
}
