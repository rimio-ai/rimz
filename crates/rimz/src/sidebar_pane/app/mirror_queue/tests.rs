use super::*;
use crate::sidebar_pane::app::fixtures;

fn pane(index: usize) -> PaneId {
    PaneId::from_parts(crate::MuxName::Zellij, format!("terminal_{index}"))
}

fn record(current: usize) -> Arc<ViewsRecord> {
    let snapshot = fixtures::agent_snapshot(&fixtures::workspace());
    let panes = (0..8)
        .map(|index| fixtures::pane(&format!("terminal_{index}"), &format!("tab_{index}"), false))
        .collect();
    let mut fold = super::super::fetch::WorkspaceFold {
        workspace: crate::sidebar::enrich::WorkspaceSnapshot(snapshot),
        sampled_at: Instant::now(),
        frame: Some(Arc::new(crate::sidebar::frame::assemble_frame(
            panes,
            7,
            "rimz-test",
        ))),
    };
    for (index, tab) in Arc::make_mut(fold.frame.as_mut().unwrap())
        .tabs
        .iter_mut()
        .enumerate()
    {
        tab.position = (index < 7).then_some(index as u32);
    }
    let mut record = ViewsRecord::default();
    let id = super::super::views::Views::default()
        .presentation(&pane(current), Instant::now(), false)
        .0;
    record.mirror_target = Some((id, 1));
    record.fold = Some(Arc::new(fold));
    record.mirror_pane = Some(pane(current));
    Arc::new(record)
}

#[test]
fn ranks_neighbours_then_previous_then_outward_then_unknown() {
    let queue = Arc::new(MirrorQueue::default());
    let now = Instant::now();
    let subscribers = (0..8)
        .filter(|index| *index != 3)
        .map(pane)
        .collect::<Vec<_>>();
    queue.observe(record(6), subscribers.clone());
    queue.observe(Arc::new(ViewsRecord::default()), subscribers.clone());
    queue.observe(record(6), subscribers.clone());
    let mut missing = record(6);
    Arc::get_mut(&mut missing).unwrap().mirror_pane = Some(pane(99));
    queue.observe(missing, subscribers.clone());
    queue.observe(record(3), subscribers.clone());
    for pane in &subscribers {
        assert!(queue.take(pane, now).is_none());
    }
    let mut order = Vec::new();
    loop {
        let (offered, _) = queue.step(now, subscribers.clone());
        if offered.is_empty() {
            break;
        }
        for pane in offered {
            order.push(pane.clone());
            drop(queue.take(&pane, now).unwrap());
        }
    }
    assert_eq!(order, [2, 4, 6, 1, 5, 0, 7].map(pane));
}

#[test]
fn ranks_neighbours_and_outward_distance_across_native_position_gaps() {
    for (positions, expected) in [
        (
            [None, Some(1), None, Some(3), Some(4), None, Some(9), None],
            vec![1, 4, 6],
        ),
        (
            [
                Some(0),
                Some(100),
                Some(200),
                Some(300),
                Some(301),
                Some(302),
                Some(303),
                None,
            ],
            vec![2, 4, 6, 1, 5, 0],
        ),
    ] {
        let queue = Arc::new(MirrorQueue::default());
        let now = Instant::now();
        let mut current = record(3);
        let fold = Arc::make_mut(Arc::get_mut(&mut current).unwrap().fold.as_mut().unwrap());
        let tabs = &mut Arc::make_mut(fold.frame.as_mut().unwrap()).tabs;
        for (tab, position) in tabs.iter_mut().zip(positions) {
            tab.position = position;
        }
        tabs.retain(|tab| tab.position.is_some());
        let subscribers = positions
            .iter()
            .enumerate()
            .filter(|(index, position)| *index != 3 && position.is_some())
            .map(|(index, _)| pane(index))
            .collect::<Vec<_>>();
        queue.observe(record(6), subscribers.clone());
        queue.observe(current, subscribers.clone());
        let mut order = Vec::new();
        loop {
            let (offered, _) = queue.step(now, subscribers.clone());
            if offered.is_empty() {
                break;
            }
            for pane in offered {
                order.push(pane.clone());
                drop(queue.take(&pane, now).unwrap());
            }
        }
        assert_eq!(order, expected.into_iter().map(pane).collect::<Vec<_>>());
    }
}

#[test]
fn ten_peers_share_four_slots_and_drop_releases_the_fifth() {
    let queue = Arc::new(MirrorQueue::default());
    let now = Instant::now();
    let subscribers = (10..20).map(pane).collect::<Vec<_>>();
    queue.observe(record(3), subscribers.clone());
    for pane in &subscribers {
        queue.take(pane, now);
    }
    let (first, _) = queue.step(now, subscribers.clone());
    assert_eq!(first.len(), MIRROR_POOL);
    let grant = queue.take(&first[0], now).unwrap();
    assert!(queue.step(now, subscribers.clone()).0.is_empty());
    {
        let state = queue.lock();
        assert_eq!(state.offers.len() + state.grants.len(), MIRROR_POOL);
    }
    drop(grant);
    assert_eq!(queue.step(now, subscribers).0.len(), 1);
}

#[test]
fn input_revokes_offers_but_not_grants_and_reranks_after_rest() {
    let queue = Arc::new(MirrorQueue::default());
    let now = Instant::now();
    let subscribers = (0..8).map(pane).collect::<Vec<_>>();
    queue.observe(record(3), subscribers.clone());
    for pane in &subscribers {
        queue.take(pane, now);
    }
    let offered = queue.step(now, subscribers.clone()).0;
    assert_eq!(offered.len(), 4);
    let grant = queue.take(&offered[0], now).unwrap();
    queue.hold(now);
    assert!(queue.take(&offered[1], now).is_none());
    assert!(
        queue
            .step(
                now + MIRROR_QUIET - Duration::from_nanos(1),
                subscribers.clone()
            )
            .0
            .is_empty()
    );
    assert_eq!(queue.lock().grants.len(), 1);
    drop(grant);
    queue.observe(record(5), subscribers.clone());
    let offered = queue.step(now + MIRROR_QUIET, subscribers).0;
    assert_eq!(&offered[..2], &[pane(4), pane(6)]);
}

#[test]
fn unpainted_drop_waits_for_a_new_due_pass_and_stale_offers_expire() {
    let queue = Arc::new(MirrorQueue::default());
    let now = Instant::now();
    let subscribers = vec![pane(1)];
    queue.observe(record(3), subscribers.clone());
    queue.take(&pane(1), now);
    assert_eq!(queue.step(now, subscribers.clone()).0, subscribers);
    drop(queue.take(&pane(1), now).unwrap());
    assert!(queue.step(now, subscribers.clone()).0.is_empty());
    assert!(queue.take(&pane(1), now).is_none());
    assert_eq!(queue.step(now, subscribers.clone()).0, subscribers);
    assert!(queue.take(&pane(1), now + OFFER_LEASE).is_none());
    queue.step(now + OFFER_LEASE, Vec::new());
    assert!(queue.lock().pending.is_empty());
    assert!(!queue.flag(&pane(1)).load(Ordering::Acquire));
}
