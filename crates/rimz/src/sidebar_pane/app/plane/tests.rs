use super::*;
use crate::sidebar::enrich::WorkspaceSnapshot;
use crate::sidebar_pane::app::fetch::FetchMode;
use crate::sidebar_pane::app::fixtures::{serve_config, snapshot, workspace};

#[test]
fn fold_request_maps_each_event_kind() {
    use crate::agents::LifecycleSignal;

    let pane = crate::ids::PaneId::from_parts(MuxName::Zellij, "terminal_1");
    for (event, expected) in [
        (
            SidebarEvent::StoreDelta {
                event_method: None,
                agent_signal: None,
            },
            Some(FetchMode::Normal),
        ),
        (SidebarEvent::PanesChanged, Some(FetchMode::FreshPanes)),
        (
            SidebarEvent::StoreDelta {
                event_method: Some(crate::store::event::AGENT_LIFECYCLE_METHOD.to_owned()),
                agent_signal: Some(LifecycleSignal::Registered.tag().to_owned()),
            },
            Some(FetchMode::FreshPanes),
        ),
        (
            SidebarEvent::StoreDelta {
                event_method: Some(crate::store::event::AGENT_LIFECYCLE_METHOD.to_owned()),
                agent_signal: Some(LifecycleSignal::Ended.tag().to_owned()),
            },
            Some(FetchMode::FreshPanes),
        ),
        (
            SidebarEvent::PaneOpened {
                pane_id: pane.clone(),
                command: Some("sh".into()),
            },
            Some(FetchMode::FreshPanes),
        ),
        (
            SidebarEvent::PaneOpened {
                pane_id: pane.clone(),
                command: None,
            },
            Some(FetchMode::FreshPanes),
        ),
        (
            SidebarEvent::PaneClosed {
                pane_id: pane.clone(),
            },
            Some(FetchMode::FreshPanes),
        ),
        (
            SidebarEvent::CommandChanged {
                pane_id: pane.clone(),
                command: "vim".into(),
            },
            Some(FetchMode::FreshPanes),
        ),
        (
            SidebarEvent::FocusChanged {
                focused: vec![pane.clone()],
                unfocused: Vec::new(),
            },
            None,
        ),
        (SidebarEvent::WidthTargetChanged, None),
        (SidebarEvent::BodyFilterChanged, None),
        (
            SidebarEvent::Notify {
                title: "test".into(),
                body: "test".into(),
                panes: vec![pane.clone()],
                recheck_unread: true,
                notification_kind: None,
            },
            None,
        ),
        (
            SidebarEvent::FocusStranded {
                pane_id: pane.clone(),
                generation: 1,
                clients: Vec::new(),
            },
            None,
        ),
        (
            SidebarEvent::FocusIntent {
                pane_id: pane,
                nonce: crate::ids::FocusNonce::new(),
            },
            None,
        ),
        (SidebarEvent::Reload, Some(FetchMode::Normal)),
    ] {
        assert_eq!(
            fold_request(&event).map(FetchRequest::mode),
            expected,
            "{event:?}"
        );
    }
}

#[test]
fn dispatch_requests_one_fold_and_wakes_every_subscriber() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(workspace(), dir.path()).unwrap();
    let (plane, requests, _) = DataPlane::idle();
    let mut config = serve_config(&workspace());
    let (full_tx, full_rx) = std::sync::mpsc::sync_channel(1);
    full_tx.send(Wakeup::Tick).unwrap();
    let _full = plane.subscribe(&config, full_tx);
    config.instance_id = SidebarInstanceId::new();
    let (wake, received) = std::sync::mpsc::sync_channel(1);
    let _other = plane.subscribe(&config, wake);
    let envelope = SidebarEventEnvelope::new(
        workspace(),
        None,
        42,
        SidebarEvent::StoreDelta {
            event_method: None,
            agent_signal: None,
        },
    );

    plane.room_events(&runtime).dispatch(envelope.clone());

    assert_eq!(requests.try_recv().unwrap().mode(), FetchMode::Normal);
    assert!(requests.try_recv().is_err(), "one request for both panes");
    let Wakeup::Event(actual) = received.try_recv().unwrap() else {
        panic!("expected the room event");
    };
    assert_eq!(actual, envelope);
    assert!(matches!(full_rx.try_recv().unwrap(), Wakeup::Tick));
}

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
        std::sync::mpsc::channel().0,
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
    let mut subscription = plane.subscribe(&config, std::sync::mpsc::sync_channel(64).0);
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
    assert!(
        lock(&plane.views)
            .latest
            .since(subscription.revision)
            .is_empty()
    );
    assert!(!subscription.fetch.completed());
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
    let update = lock(&plane.views)
        .latest
        .since(subscription.revision)
        .pop()
        .unwrap();
    let update = super::super::views::Cursor::default().replay(
        update,
        |fold| super::super::loop_state::LoopState::project_fold(fold, None, None),
        false,
    );
    assert!(subscription.fetch.completed());
    let FetchUpdate::Snapshot { snapshot, .. } = update else {
        panic!("expected a later snapshot")
    };
    assert_eq!(snapshot.display_name, "correction");
    assert_eq!(
        subscription.seed.unwrap().workspace.snapshot().display_name,
        "seed"
    );
}
