use super::*;
use crate::sidebar::enrich::WorkspaceSnapshot;
use crate::sidebar_pane::app::fixtures::{serve_config, snapshot, workspace};

#[test]
fn a_cold_plane_has_no_seed() {
    let plane = DataPlane::detached();
    let config = serve_config(&workspace());
    let subscription = plane.subscribe(&config, std::sync::mpsc::sync_channel(64).0);
    assert!(subscription.seed.is_none());
}

#[test]
fn a_warm_plane_returns_its_fold_before_later_delivery() {
    let plane = DataPlane::detached();
    let config = serve_config(&workspace());
    let mut remembered = snapshot(&config.workspace_id);
    remembered.display_name = "seed".into();
    plane.remember(WorkspaceFold {
        workspace: WorkspaceSnapshot(remembered.clone()),
        frame: None,
    });
    let subscription = plane.subscribe(&config, std::sync::mpsc::sync_channel(64).0);
    assert_eq!(
        subscription
            .seed
            .as_ref()
            .unwrap()
            .workspace
            .snapshot()
            .display_name,
        "seed"
    );
    assert!(subscription.results.take().snapshot.is_none());
    assert!(Arc::ptr_eq(
        &subscription
            .seed
            .as_ref()
            .unwrap()
            .workspace
            .snapshot()
            .agents,
        &remembered.agents,
    ));

    let mut newer = remembered;
    newer.display_name = "correction".into();
    plane.publish_snapshot(&config.instance_id, newer);
    let update = subscription.results.take().snapshot.unwrap();
    let FetchUpdate::Snapshot { snapshot, .. } = update else {
        panic!("expected a later snapshot")
    };
    assert_eq!(snapshot.display_name, "correction");
    assert_eq!(
        subscription.seed.unwrap().workspace.snapshot().display_name,
        "seed"
    );
}
