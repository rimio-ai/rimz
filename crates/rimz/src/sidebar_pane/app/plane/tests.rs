use super::*;
use crate::sidebar::enrich::WorkspaceSnapshot;
use crate::sidebar_pane::app::fixtures::{serve_config, snapshot, workspace};

#[test]
fn room_event_recording_stamps_one_overlay_and_attributes_focus_without_lanes() {
    use crate::sidebar::event_store::EventStore;
    use crate::wakeup::events::{SidebarEvent, SidebarEventEnvelope};

    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(workspace(), dir.path()).unwrap();
    let views = Arc::new(Mutex::new(super::super::views::Views::default()));
    let events = Arc::new(Mutex::new(EventStore::default()));
    let ingress = RoomEvents::new(
        &runtime,
        views.clone(),
        events.clone(),
        Subscribers::default(),
    );
    let pane = crate::ids::PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let envelope = SidebarEventEnvelope::new(
        workspace(),
        Some("rimz-test".into()),
        42,
        SidebarEvent::FocusChanged {
            focused: vec![pane.clone()],
            unfocused: Vec::new(),
        },
    );
    ingress.record(&envelope, 100);
    let mut expected = EventStore::default();
    expected.append(envelope.event.clone(), 42, 100);
    assert_eq!(
        *lock(&events),
        expected,
        "record the ingress receive stamp once"
    );
    assert_eq!(lock(&views).attributed_focus_event(&pane), Some(42));

    ingress.record(
        &SidebarEventEnvelope::new(workspace(), None, 43, SidebarEvent::PanesChanged),
        101,
    );
    assert_eq!(*lock(&events), expected, "nudges record nothing");
    assert_eq!(lock(&views).attributed_focus_event(&pane), Some(42));
}

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
