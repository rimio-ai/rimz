use super::*;
use crate::diag::focus_trace::log_path;
use crate::sidebar_pane::app::fixtures::{serve_config, workspace};
use crate::sidebar_pane::app::plane::DataPlane;
use std::cell::Cell;

#[test]
fn lease_superseded_while_waiting_for_anchor_lock_never_dispatches() {
    let rig = TraceRig::new();
    let clicked = Instant::now();
    let anchor = rig.anchor();
    let runtime = crate::RuntimePaths::under(workspace(), rig.dir.path()).unwrap();
    crate::mux::focus_anchor::store(&runtime, &anchor).unwrap();
    let first = rig.peers.arm(anchor.pane_id.clone(), clicked);
    let guard = crate::disk::lock::WorkspaceLock::acquire(&runtime.focus_anchor_lock()).unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let waiting = scope.spawn(|| {
            first.dispatch_with(
                anchor.clone(),
                clicked,
                || {
                    started_tx.send(()).unwrap();
                    // A mismatched backend rejects before spawning if the fence fails to stop it.
                    let backend = crate::mux::backend_for(match anchor.pane_id.mux() {
                        crate::MuxName::Tmux => crate::MuxName::Zellij,
                        crate::MuxName::Zellij => crate::MuxName::Tmux,
                    });
                    crate::mux::focus_anchor::dispatch_action_if_current(
                        backend.as_ref(),
                        &runtime,
                        &anchor.session_name,
                        &anchor.pane_id,
                        anchor.nonce,
                        Default::default(),
                        || first.is_current(),
                    )
                },
                || clicked,
                |_, _| Err(RecvTimeoutError::Disconnected),
            )
        });
        started_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let _newest = rig.peers.arm(anchor.pane_id.clone(), clicked);
        drop(guard);
        let result = waiting.join().unwrap();
        assert_eq!(
            crate::mux::focus_anchor::load(&runtime),
            Some(anchor.clone()),
            "an obsolete lease must not dispatch or clear its still-matching anchor"
        );
        assert!(
            matches!(result, Some(Ok(false))),
            "a lease superseded during the lock wait must be refused: {result:?}"
        );
    });
}

#[test]
fn pre_push_stops_keep_the_click_sequence_and_stage() {
    let rig = TraceRig::new();
    let clicked = Instant::now();
    let anchor = rig.anchor();
    let first = rig.peers.arm(anchor.pane_id.clone(), clicked);
    let last = rig.peers.arm(anchor.pane_id.clone(), clicked);
    assert_eq!(first.request(|| panic!("superseded request")), None);
    assert_eq!(last.request(|| 17), Some(17));
    super::super::focus::finish_focus_request(
        &anchor.pane_id,
        None,
        Some(Err(
            crate::mux::focus_anchor::FocusActionError::ClientSample(
                crate::mux::MuxErr::SessionNotFound {
                    session: rig.source.room.session_name.clone(),
                },
            ),
        )),
        Some(&last),
        |_| panic!("failed request cannot dispatch"),
    );
    assert_eq!(
        first.dispatch_with(
            anchor.clone(),
            clicked,
            || (),
            || clicked,
            |_, _| { panic!("superseded push cannot wait") }
        ),
        None
    );
    let rows = rig.all_records();
    let progress: Vec<_> = rows
        .iter()
        .filter(|row| row["event"]["kind"] == "jump_progress")
        .collect();
    assert_eq!(
        progress.len(),
        8,
        "every armed click and pre-push stop must remain observable"
    );
    let stages: Vec<_> = progress
        .iter()
        .map(|row| {
            (
                row["event"]["sequence"].as_u64().unwrap(),
                row["event"]["stage"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        stages,
        [
            (1, "armed"),
            (2, "armed"),
            (1, "request_waiting"),
            (1, "request_superseded"),
            (2, "request_waiting"),
            (2, "request_started"),
            (2, "request_failed"),
            (1, "push_superseded")
        ]
    );
    for row in progress {
        assert_eq!(row["instance_id"], rig.source.instance_id.as_str());
        assert_eq!(row["event"]["target"], anchor.pane_id.to_string());
        assert!(row["event"]["elapsed_ms"].is_u64());
        let event: FocusTraceEvent = serde_json::from_value(row["event"].clone()).unwrap();
        assert_eq!(serde_json::to_value(event).unwrap(), row["event"]);
    }
}

#[test]
fn request_threads_in_reverse_order_cannot_overwrite_the_latest_click() {
    let rig = TraceRig::new();
    let clicked = Instant::now();
    let a = rig.anchor().pane_id;
    let first = rig.peers.arm(a.clone(), clicked);
    let middle = rig.peers.arm(PaneId::from_parts(a.mux(), "%7"), clicked);
    let last = rig.peers.arm(a.clone(), clicked);
    let stored = std::sync::Mutex::new(Vec::new());
    let runtime = crate::RuntimePaths::under(workspace(), rig.dir.path()).unwrap();
    let request = |label| {
        let nonce = crate::mux::focus_anchor::request_action_with_test_sample(
            &runtime,
            &rig.source.room.session_name,
            crate::mux::focus_anchor::FocusActionRequest {
                pane_id: a.clone(),
                origin: crate::mux::focus_anchor::FocusOrigin::User,
                repair_generation: None,
                expected_pre_action: None,
                placement: None,
                order: None,
            },
            || Ok(Vec::new()),
        )
        .unwrap();
        stored.lock().unwrap().push(label);
        nonce
    };
    let nonce = std::thread::scope(|scope| {
        // Both detached threads exist, but the newest reaches request_action first.
        let (go, ready) = std::sync::mpsc::channel();
        let request = &request;
        let first = first.clone();
        let old = scope.spawn(move || {
            ready.recv().unwrap();
            first.request(|| request("first"))
        });
        let newest = scope.spawn(|| last.request(|| request("last")));
        let nonce = newest
            .join()
            .unwrap()
            .expect("latest click stores its anchor");
        go.send(()).unwrap();
        assert_eq!(
            old.join().unwrap(),
            None,
            "old click must never store its anchor"
        );
        nonce
    });
    assert_eq!(middle.request(|| request("middle")), None);
    assert_eq!(*stored.lock().unwrap(), ["last"]);
    let anchor = crate::mux::focus_anchor::load(&runtime).unwrap();
    assert_eq!(
        anchor.nonce, nonce,
        "stored nonce order must follow click order"
    );
    let destination = serve_config(&workspace());
    let (tx, _rx) = std::sync::mpsc::sync_channel(1);
    tx.send(Wakeup::Snapshot).unwrap();
    let _subscription = rig.plane.subscribe(&destination, tx);
    let target = rig
        .plane
        .focus_peers(destination.instance_id.clone(), DiagSink::disabled());
    let dispatches = Cell::new(0);
    let landings = Cell::new(0);
    for peers in [&first, &middle, &last] {
        let result = peers.dispatch_with(
            anchor.clone(),
            clicked,
            || {
                assert!(last.is_current(), "lease lasts through dispatch");
                dispatches.set(dispatches.get() + 1);
            },
            || clicked,
            |ack, _| {
                if let Some(push) = target.take_push() {
                    assert_eq!(push.anchor.pane_id, a, "B must never reach a landing");
                    landings.set(landings.get() + 1);
                    push.finish_painted(PushVerdict::Painted, clicked);
                }
                ack.try_recv().map_err(|_| RecvTimeoutError::Disconnected)
            },
        );
        assert_eq!(result.is_some(), std::ptr::eq(peers, &last));
    }
    assert_eq!((dispatches.get(), landings.get()), (1, 1));
    assert!(!last.is_current(), "dispatch releases before late reports");

    for during_fit in [false, true] {
        let rig = TraceRig::new();
        let anchor = rig.anchor();
        let destination = serve_config(&workspace());
        let (tx, _rx) = std::sync::mpsc::sync_channel(1);
        let _subscription = rig.plane.subscribe(&destination, tx);
        let newer = Arc::new(std::sync::Mutex::new(None));
        let fit_newer = newer.clone();
        let owner = rig.peers.clone();
        let target = anchor.pane_id.clone();
        let peers = rig
            .peers
            .arm(target.clone(), clicked)
            .with_fit(during_fit.then(|| {
                Arc::new(move |_: &FocusAnchor| {
                    *fit_newer.lock().unwrap() = Some(owner.arm(target.clone(), clicked));
                    None
                }) as FitView
            }));
        let mut received = false;
        assert_eq!(
            peers.dispatch_with(
                anchor.clone(),
                clicked,
                || panic!("superseded jump must not dispatch"),
                || clicked,
                |ack, _| {
                    if !std::mem::replace(&mut received, true) {
                        *newer.lock().unwrap() =
                            Some(rig.peers.arm(anchor.pane_id.clone(), clicked));
                    }
                    ack.try_recv().map_err(|_| RecvTimeoutError::Disconnected)
                }
            ),
            None
        );
        let rows = rig.records();
        if during_fit {
            assert!(
                rows.is_empty(),
                "supersession during fit must stop before push"
            );
        } else {
            assert_eq!(rows[0]["event"]["outcome"], "superseded");
            assert!(rows[0]["event"].get("dispatched_ms").is_none());
        }
    }
}

#[test]
fn stale_release_and_unwinding_cannot_leave_or_clear_the_wrong_lease() {
    let rig = TraceRig::new();
    let now = Instant::now();
    let first = rig.peers.arm(rig.anchor().pane_id, now);
    assert!(first.is_current());
    let newest = rig.peers.arm(rig.anchor().pane_id, now);
    first.release();
    assert!(
        newest.is_current(),
        "stale release must not clear the newest click"
    );
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _guard = newest;
        panic!("injected dispatch panic");
    }));
    assert!(
        rig.peers.accepts(None, now),
        "unwinding must release the nonce fence"
    );

    let lease = rig.peers.arm(rig.anchor().pane_id, Instant::now());
    let destination = serve_config(&workspace());
    let (tx, _rx) = std::sync::mpsc::sync_channel(1);
    tx.send(Wakeup::Snapshot).unwrap();
    let _subscription = rig.plane.subscribe(&destination, tx);
    let target = rig
        .plane
        .focus_peers(destination.instance_id, DiagSink::disabled());
    let (_, _, _, ack, _) = lease
        .push(&rig.anchor(), lease.clicked().unwrap(), Instant::now)
        .unwrap();
    target
        .take_push()
        .expect("a full ordinary inbox cannot lose the priority slot")
        .finish(PushVerdict::NotDestination);
    let report = ack.recv_timeout(PAINT_WAIT).expect("priority push receipt");
    assert_eq!(report.verdict, Some(PushVerdict::NotDestination));
    assert!(
        report.finished_ms.unwrap() < duration_millis(PAINT_WAIT),
        "push receipt must not wait for the lease deadline"
    );
}

#[test]
fn cross_tab_push_reaches_the_other_attachment_before_dispatch() {
    const SETTLE: Duration = Duration::from_secs(10);
    for (outcome, stale) in ["painted", "released", "capped", "superseded"]
        .into_iter()
        .flat_map(|outcome| [false, true].map(|stale| (outcome, stale)))
    {
        let plane = DataPlane::detached();
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let runtime = crate::RuntimePaths::under(workspace(), dir.path()).unwrap();
        let source = serve_config(&workspace());
        let destination = serve_config(&workspace());
        let (source_tx, source_rx) = std::sync::mpsc::sync_channel(64);
        let (destination_tx, destination_rx) = std::sync::mpsc::sync_channel(64);
        let _source = plane.subscribe(&source, source_tx);
        let _destination = plane.subscribe(&destination, destination_tx);
        let peers = plane
            .focus_peers(source.instance_id, crate::diag::DiagSink::disabled())
            .with_fit(Some(Arc::new(move |anchor| {
                stale.then(|| {
                    std::collections::HashMap::from([(
                        anchor.pane_id.clone(),
                        crate::mux::FittedPane {
                            rows: 70,
                            cols: None,
                        },
                    )])
                })
            })));
        let anchor = crate::mux::focus_anchor::FocusAnchor {
            nonce: crate::ids::FocusNonce::new(),
            session_name: source.room.session_name,
            pane_id: crate::ids::PaneId::from_parts(crate::MuxName::Zellij, "terminal_2"),
            origin: crate::mux::focus_anchor::FocusOrigin::User,
            repair_generation: None,
            issued_at_ms: crate::utils::time::unix_now_ms(),
            applied_at_ms: None,
            state: crate::mux::focus_anchor::FocusIntentState::Requested,
            pre_action: Vec::new(),
            placement: None,
            order: None,
        };
        let expected = anchor.clone();
        crate::mux::focus_anchor::store(&runtime, &anchor).unwrap();
        let nonce = anchor.nonce;
        let target = anchor.pane_id.clone();
        let session = anchor.session_name.clone();
        let (painted_tx, painted_rx) = std::sync::mpsc::channel();
        let (dispatched_tx, dispatched_rx) = std::sync::mpsc::channel();
        let peer_runtime = runtime.clone();
        let started = Instant::now();
        let peers = peers.arm(anchor.pane_id.clone(), started);
        let destination_peers =
            plane.focus_peers(destination.instance_id, crate::diag::DiagSink::disabled());
        let destination = std::thread::spawn(move || {
            assert_eq!(destination_rx.recv_timeout(SETTLE).unwrap(), Wakeup::Tick);
            let push = destination_peers
                .take_push()
                .expect("destination must receive the focus push");
            assert_eq!(push.anchor, expected);
            assert_eq!(
                push.expected_panes.as_deref(),
                stale
                    .then(|| std::collections::HashMap::from([(
                        expected.pane_id.clone(),
                        crate::mux::FittedPane {
                            rows: 70,
                            cols: None
                        }
                    )]))
                    .as_ref()
            );
            if outcome == "superseded" {
                let mut newer = expected;
                newer.nonce = crate::ids::FocusNonce::new();
                crate::mux::focus_anchor::store(&peer_runtime, &newer).unwrap();
            }
            painted_tx.send(()).unwrap();
            match outcome {
                "painted" | "superseded" => {
                    push.finish(crate::diag::focus_trace::PushVerdict::Painted)
                }
                "capped" => {
                    dispatched_rx.recv_timeout(SETTLE).unwrap();
                    drop(push);
                }
                _ => drop(push),
            }
        });
        peers.dispatch(anchor, started, || {
            assert!(
                painted_rx.try_recv().is_ok(),
                "dispatch must follow destination paint, release, or cap"
            );
            assert_eq!(
                source_rx.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Empty),
                "the source must not receive its own push"
            );
            if outcome == "capped" {
                assert!(started.elapsed() >= Duration::from_millis(500));
                dispatched_tx.send(()).unwrap();
            }
            if outcome == "superseded" {
                let backend = crate::mux::backend_for(target.mux());
                assert!(
                    !crate::mux::focus_anchor::dispatch_action(
                        backend.as_ref(),
                        &runtime,
                        &session,
                        &target,
                        nonce,
                        Default::default()
                    )
                    .unwrap(),
                    "the post-wait nonce check must still prevent dispatch"
                );
            }
        });
        destination.join().unwrap();
    }
}

fn report(verdict: PushVerdict) -> PushPeer {
    let mut peer = PushPeer::new(SidebarInstanceId::new());
    peer.verdict = Some(verdict);
    peer
}

#[test]
fn paint_wait_returns_on_first_painted_without_waiting_for_other_peers() {
    let now = Instant::now();
    let mut reports = Vec::new();
    let outcome = wait_for_paint(
        now + PAINT_WAIT,
        || now,
        |timeout| {
            assert_eq!(timeout, PAINT_WAIT);
            Ok(report(PushVerdict::Painted))
        },
        &mut reports,
    );
    assert_eq!(outcome, PushOutcome::Painted);
    assert_eq!(reports.len(), 1);
}

#[test]
fn paint_wait_returns_on_a_displaced_landing() {
    let now = Instant::now();
    let mut reports = Vec::new();
    let mut first = true;
    let outcome = wait_for_paint(
        now + PAINT_WAIT,
        || now,
        |_| {
            if std::mem::take(&mut first) {
                Ok(report(PushVerdict::Displaced))
            } else {
                Err(RecvTimeoutError::Disconnected)
            }
        },
        &mut reports,
    );
    assert_eq!(outcome, PushOutcome::Painted);
    assert_eq!(reports.len(), 1);
}

#[test]
fn paint_wait_keeps_listening_through_every_non_painted_verdict() {
    let now = Instant::now();
    let mut verdicts = [
        PushVerdict::NotDestination,
        PushVerdict::Exiting,
        PushVerdict::Expired,
        PushVerdict::Superseded,
        PushVerdict::Painted,
    ]
    .into_iter();
    let mut reports = Vec::new();
    assert_eq!(
        wait_for_paint(
            now + PAINT_WAIT,
            || now,
            |_| Ok(report(verdicts.next().unwrap())),
            &mut reports
        ),
        PushOutcome::Painted
    );
    assert_eq!(reports.len(), 5, "only a painted verdict ends the wait");
}

#[test]
fn paint_wait_returns_when_all_peers_release() {
    let now = Instant::now();
    let receives = Cell::new(0);
    assert_eq!(
        wait_for_paint(
            now + PAINT_WAIT,
            || now,
            |_| {
                receives.set(receives.get() + 1);
                Err(RecvTimeoutError::Disconnected)
            },
            &mut Vec::new()
        ),
        PushOutcome::Released
    );
    assert_eq!(receives.get(), 1);
}

#[test]
fn paint_wait_is_capped_even_when_a_peer_retains_its_ack() {
    let start = Instant::now();
    let clock = Cell::new(start);
    assert_eq!(
        wait_for_paint(
            start + PAINT_WAIT,
            || clock.get(),
            |timeout| {
                assert_eq!(timeout, PAINT_WAIT);
                clock.set(start + timeout);
                Err(RecvTimeoutError::Timeout)
            },
            &mut Vec::new()
        ),
        PushOutcome::TimedOut
    );
    assert_eq!(
        wait_for_paint(
            start + PAINT_WAIT,
            || clock.get(),
            |_| panic!("budget is spent"),
            &mut Vec::new()
        ),
        PushOutcome::TimedOut
    );
}

#[test]
fn paint_wait_classifies_release_and_paint_at_or_after_budget_as_timed_out() {
    let start = Instant::now();
    for elapsed in [PAINT_WAIT, PAINT_WAIT + Duration::from_millis(1)] {
        for verdict in [
            None,
            Some(PushVerdict::Painted),
            Some(PushVerdict::Displaced),
        ] {
            let clock = Cell::new(start);
            assert_eq!(
                wait_for_paint(
                    start + PAINT_WAIT,
                    || clock.get(),
                    |_| {
                        clock.set(start + elapsed);
                        if let Some(verdict) = verdict {
                            Ok(report(verdict))
                        } else {
                            Err(RecvTimeoutError::Disconnected)
                        }
                    },
                    &mut Vec::new()
                ),
                PushOutcome::TimedOut,
                "elapsed={elapsed:?}, verdict={verdict:?}"
            );
        }
    }
}

struct TraceRig {
    dir: tempfile::TempDir,
    plane: DataPlane,
    source: super::super::ServeConfig,
    peers: FocusPeers,
}

impl TraceRig {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let source = serve_config(&workspace());
        let diag = DiagSink::under(
            dir.path().to_owned(),
            workspace(),
            &source.room.session_name,
            Some(source.instance_id.clone()),
        );
        let plane = DataPlane::detached();
        let peers = plane.focus_peers(source.instance_id.clone(), diag);
        Self {
            dir,
            plane,
            source,
            peers,
        }
    }

    fn anchor(&self) -> FocusAnchor {
        FocusAnchor {
            nonce: crate::ids::FocusNonce::new(),
            session_name: self.source.room.session_name.clone(),
            pane_id: crate::PaneId::from_parts(crate::MuxName::Tmux, "%3"),
            origin: crate::mux::focus_anchor::FocusOrigin::User,
            repair_generation: None,
            issued_at_ms: crate::utils::time::unix_now_ms(),
            applied_at_ms: None,
            state: crate::mux::focus_anchor::FocusIntentState::Requested,
            pre_action: Vec::new(),
            placement: None,
            order: None,
        }
    }

    fn records(&self) -> Vec<serde_json::Value> {
        self.all_records()
            .into_iter()
            .filter(|row| row["event"]["kind"] == "push_handshake")
            .collect()
    }

    fn all_records(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(log_path(self.dir.path()))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

#[test]
fn dispatch_spends_one_click_budget_on_request_work_and_fit() {
    for (lead_ms, fit_ms) in [(42, None), (42, Some(70)), (42, Some(458)), (42, Some(700))] {
        let rig = TraceRig::new();
        let clicked = Instant::now();
        let clock = Arc::new(std::sync::Mutex::new(
            clicked + Duration::from_millis(lead_ms),
        ));
        let fit_clock = clock.clone();
        let anchor = rig.anchor();
        let peers = rig
            .peers
            .arm(anchor.pane_id.clone(), clicked)
            .with_fit(fit_ms.map(|fit_ms| {
                Arc::new(move |_: &FocusAnchor| {
                    *fit_clock.lock().unwrap() += Duration::from_millis(fit_ms);
                    Some(HashMap::new())
                }) as FitView
            }));
        peers.dispatch_with(
            anchor,
            clicked,
            || (),
            || *clock.lock().unwrap(),
            |ack, _| ack.try_recv().map_err(|_| RecvTimeoutError::Disconnected),
        );
        let records = rig.records();
        let event = &records[0]["event"];
        let spent_ms = lead_ms + fit_ms.unwrap_or(0);
        assert_eq!(event["budget_ms"], 500_u64.saturating_sub(spent_ms));
        assert_eq!(event["lead_ms"], spent_ms);
        assert_eq!(event["fit_ms"].as_u64(), fit_ms);
        assert_eq!(
            event["outcome"],
            if spent_ms >= 500 {
                "timed_out"
            } else {
                "released"
            }
        );
    }
}

#[test]
fn dispatch_records_click_lead_and_the_landing_paint_instead_of_receipt_time() {
    for offset in [-17_i64, 23] {
        for verdict in [PushVerdict::Painted, PushVerdict::Displaced] {
            let rig = TraceRig::new();
            let destination = serve_config(&workspace());
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            let _subscription = rig.plane.subscribe(&destination, tx);
            let sent = Instant::now();
            let clicked = sent - Duration::from_millis(42);
            let painted = if offset < 0 {
                sent - Duration::from_millis(offset.unsigned_abs())
            } else {
                sent + Duration::from_millis(offset as u64)
            };
            let mut received = false;
            let anchor = rig.anchor();
            rig.peers
                .arm(anchor.pane_id.clone(), clicked)
                .dispatch_with(
                    anchor,
                    clicked,
                    || assert!(rig.records().is_empty()),
                    || sent,
                    |ack, _| {
                        if std::mem::replace(&mut received, true) {
                            return Err(RecvTimeoutError::Disconnected);
                        }
                        assert_eq!(rx.try_recv().unwrap(), Wakeup::Tick);
                        let mut push = rig.plane.take_focus_push(&destination.instance_id).unwrap();
                        push.peer.received_ms = Some(30);
                        push.finish_painted(verdict, painted);
                        ack.try_recv().map_err(|_| RecvTimeoutError::Disconnected)
                    },
                );
            let rows = rig.records();
            let event = &rows[0]["event"];
            assert_eq!(event["lead_ms"], 42);
            assert_eq!(event["peers"][0]["painted_ms"], offset);
            assert_eq!(event["peers"][0]["received_ms"], 30);
        }
    }
}

#[test]
fn dispatch_records_empty_and_failed_deliveries_only_after_the_jump() {
    for with_peers in [false, true] {
        let rig = TraceRig::new();
        let full = serve_config(&workspace());
        let closed = serve_config(&workspace());
        let (full_tx, _full_rx) = std::sync::mpsc::sync_channel(0);
        let (closed_tx, closed_rx) = std::sync::mpsc::sync_channel(1);
        drop(closed_rx);
        let _subscriptions = with_peers.then(|| {
            [
                rig.plane.subscribe(&full, full_tx),
                rig.plane.subscribe(&closed, closed_tx),
            ]
        });
        let anchor = rig.anchor();
        let clicked = Instant::now();
        let target = rig
            .plane
            .focus_peers(full.instance_id.clone(), DiagSink::disabled());
        assert_eq!(
            rig.peers
                .arm(anchor.pane_id.clone(), clicked)
                .dispatch_with(
                    anchor.clone(),
                    clicked,
                    || {
                        assert!(rig.records().is_empty(), "recording must follow the jump");
                        17
                    },
                    Instant::now,
                    |ack, timeout| {
                        drop(target.take_push());
                        ack.recv_timeout(timeout)
                    }
                ),
            Some(17)
        );
        let rows = rig.records();
        assert_eq!(
            rows.len(),
            1,
            "one record per handshake, even without peers"
        );
        let row = &rows[0];
        assert_eq!(row["v"], "rimz.focus_trace.v1");
        assert_eq!(row["instance_id"], rig.source.instance_id.as_str());
        assert_eq!(
            row["workspace_id"],
            serde_json::to_value(workspace()).unwrap()
        );
        assert_eq!(row["session_name"], rig.source.room.session_name);
        let event = &row["event"];
        assert_eq!(event["kind"], "push_handshake");
        assert_eq!(event["nonce"], anchor.nonce.to_string());
        assert_eq!(event["target"], "tmux:%3");
        assert_eq!(event["outcome"], "released");
        let remaining_ms = 500_u64.saturating_sub(event["lead_ms"].as_u64().unwrap());
        assert!(
            (remaining_ms.saturating_sub(1)..=remaining_ms)
                .contains(&event["budget_ms"].as_u64().unwrap())
        );
        assert!(event.get("fit_ms").is_none(), "no fit ran");
        assert!(event["dispatched_ms"].as_u64().unwrap() >= event["decided_ms"].as_u64().unwrap());
        let mut expected = if with_peers {
            vec![
                serde_json::json!({"instance_id": full.instance_id, "delivery": "sent"}),
                serde_json::json!({"instance_id": closed.instance_id, "delivery": "inbox_closed"}),
            ]
        } else {
            Vec::new()
        };
        expected.sort_by(|a, b| a["instance_id"].as_str().cmp(&b["instance_id"].as_str()));
        assert_eq!(event["peers"], serde_json::json!(expected));
    }
}

#[test]
fn dispatch_records_a_successful_paint_after_the_jump() {
    let rig = TraceRig::new();
    let destination = serve_config(&workspace());
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let _subscription = rig.plane.subscribe(&destination, tx);
    let target = rig
        .plane
        .focus_peers(destination.instance_id.clone(), DiagSink::disabled());
    let destination_thread = std::thread::spawn(move || {
        assert_eq!(rx.recv().unwrap(), Wakeup::Tick);
        let mut push = target.take_push().unwrap();
        push.peer.received_ms = Some(push.elapsed_ms());
        push.finish(PushVerdict::Painted);
    });
    let anchor = rig.anchor();
    let clicked = Instant::now();
    rig.peers
        .arm(anchor.pane_id.clone(), clicked)
        .dispatch(anchor, clicked, || assert!(rig.records().is_empty()));
    destination_thread.join().unwrap();
    let rows = rig.records();
    assert_eq!(rows.len(), 1, "successful handshakes are recorded too");
    assert_eq!(rows[0]["event"]["outcome"], "painted");
    assert_eq!(rows[0]["event"]["peers"][0]["verdict"], "painted");
    assert_eq!(rows[0]["event"]["peers"][0]["delivery"], "sent");
}

#[test]
fn dispatch_records_a_displaced_paint_after_the_jump() {
    let rig = TraceRig::new();
    let destination = serve_config(&workspace());
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let _subscription = rig.plane.subscribe(&destination, tx);
    let target = rig
        .plane
        .focus_peers(destination.instance_id.clone(), DiagSink::disabled());
    let destination_thread = std::thread::spawn(move || {
        assert_eq!(rx.recv().unwrap(), Wakeup::Tick);
        let push = target.take_push().unwrap();
        push.finish(PushVerdict::Displaced);
    });
    let anchor = rig.anchor();
    let clicked = Instant::now();
    rig.peers
        .arm(anchor.pane_id.clone(), clicked)
        .dispatch(anchor, clicked, || assert!(rig.records().is_empty()));
    destination_thread.join().unwrap();
    let rows = rig.records();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["event"]["outcome"], "painted");
    assert_eq!(rows[0]["event"]["peers"][0]["verdict"], "displaced");
}

#[test]
fn dispatch_records_the_fitted_push_budget() {
    let rig = TraceRig::new();
    let anchor = rig.anchor();
    let clicked = Instant::now();
    let peers = rig
        .peers
        .arm(anchor.pane_id.clone(), clicked)
        .with_fit(Some(Arc::new(|anchor| {
            std::thread::sleep(Duration::from_millis(2));
            Some(HashMap::from([(
                anchor.pane_id.clone(),
                crate::mux::FittedPane {
                    rows: 70,
                    cols: None,
                },
            )]))
        })));
    peers.dispatch(anchor, clicked, || assert!(rig.records().is_empty()));
    let records = rig.records();
    let event = &records[0]["event"];
    let remaining_ms = 500_u64.saturating_sub(event["lead_ms"].as_u64().unwrap());
    assert!(
        (remaining_ms.saturating_sub(1)..=remaining_ms)
            .contains(&event["budget_ms"].as_u64().unwrap())
    );
    assert!(
        event["fit_ms"].as_u64().is_some_and(|fit_ms| fit_ms >= 2),
        "the fitted push must record the pre-send fit duration"
    );
}

#[test]
fn dispatch_collects_reports_sent_after_the_budget_but_before_the_horizon() {
    let rig = TraceRig::new();
    let destination = serve_config(&workspace());
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let _subscription = rig.plane.subscribe(&destination, tx);
    let (go_tx, go_rx) = channel();
    let target = rig
        .plane
        .focus_peers(destination.instance_id.clone(), DiagSink::disabled());
    let destination_thread = std::thread::spawn(move || {
        assert_eq!(rx.recv().unwrap(), Wakeup::Tick);
        let mut push = target.take_push().unwrap();
        push.peer.received_ms = Some(push.elapsed_ms());
        go_rx.recv().unwrap();
        push.finish(PushVerdict::Expired);
    });
    let anchor = rig.anchor();
    let clicked = Instant::now();
    rig.peers
        .arm(anchor.pane_id.clone(), clicked)
        .dispatch(anchor, clicked, || {
            assert!(rig.records().is_empty());
            go_tx.send(()).unwrap();
        });
    destination_thread.join().unwrap();
    let rows = rig.records();
    assert_eq!(rows.len(), 1, "a source must write the collected report");
    let event = &rows[0]["event"];
    assert_eq!(event["outcome"], "timed_out");
    assert_eq!(
        event["peers"][0]["instance_id"],
        destination.instance_id.as_str()
    );
    assert_eq!(event["peers"][0]["verdict"], "expired");
    let budget_ms = event["budget_ms"].as_u64().unwrap();
    assert!(event["decided_ms"].as_u64().unwrap() >= budget_ms);
    assert!(event["peers"][0]["finished_ms"].as_u64().unwrap() >= budget_ms);
}

#[test]
fn dispatch_keeps_timely_buffered_reports_after_a_slow_jump() {
    for within_horizon in [true, false] {
        let rig = TraceRig::new();
        let destination = serve_config(&workspace());
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let _subscription = rig.plane.subscribe(&destination, tx);
        let (go_tx, go_rx) = channel();
        let (done_tx, done_rx) = channel();
        let target = rig
            .plane
            .focus_peers(destination.instance_id.clone(), DiagSink::disabled());
        let destination_thread = std::thread::spawn(move || {
            assert_eq!(rx.recv().unwrap(), Wakeup::Tick);
            let mut push = target.take_push().unwrap();
            push.peer.received_ms = Some(push.elapsed_ms());
            go_rx.recv().unwrap();
            if !within_horizon {
                std::thread::sleep(REPORT_HORIZON);
            }
            push.finish(PushVerdict::Expired);
            done_tx.send(()).unwrap();
        });
        let anchor = rig.anchor();
        let clicked = Instant::now();
        rig.peers
            .arm(anchor.pane_id.clone(), clicked)
            .dispatch(anchor, clicked, || {
                go_tx.send(()).unwrap();
                done_rx.recv().unwrap();
                if within_horizon {
                    std::thread::sleep(REPORT_HORIZON);
                }
                assert!(
                    rig.records().is_empty(),
                    "no logging while the mux dispatch is running"
                );
            });
        destination_thread.join().unwrap();
        let rows = rig.records();
        assert_eq!(rows.len(), 1);
        let peer = &rows[0]["event"]["peers"][0];
        assert_eq!(
            peer["verdict"].as_str(),
            within_horizon.then_some("expired"),
            "drain reports buffered within the horizon, even if mux dispatch ends later"
        );
        assert!(rows[0]["event"]["dispatched_ms"].as_u64().unwrap() >= 1000);
    }
}

#[test]
fn stale_push_carries_rows_and_spends_fit_time_from_the_click_deadline() {
    use crate::mux::focus_anchor::{FocusIntentState, FocusOrigin};
    use crate::sidebar_pane::app::fixtures::{serve_config, workspace};
    use crate::sidebar_pane::app::plane::DataPlane;
    use std::sync::Mutex;

    for (painted, fit_time) in [
        (true, Duration::ZERO),
        (true, Duration::from_millis(200)),
        (false, Duration::from_millis(200)),
        (true, Duration::from_secs(2)),
        (false, Duration::from_secs(2)),
    ] {
        let plane = DataPlane::detached();
        let destination = serve_config(&workspace());
        let (tx, rx) = std::sync::mpsc::sync_channel(8);
        let _subscription = plane.subscribe(&destination, tx);
        let start = Instant::now();
        let returned = start + fit_time;
        let clock = Arc::new(Mutex::new(start));
        let fit_clock = clock.clone();
        let rows = HashMap::from([(
            PaneId::from_parts(crate::MuxName::Tmux, "%2"),
            crate::mux::FittedPane {
                rows: 70,
                cols: None,
            },
        )]);
        let fitted_rows = rows.clone();
        let peers = plane
            .focus_peers(SidebarInstanceId::new(), DiagSink::disabled())
            .with_fit(Some(Arc::new(move |_| {
                *fit_clock.lock().unwrap() = returned;
                Some(fitted_rows.clone())
            })));
        let anchor = FocusAnchor {
            nonce: crate::ids::FocusNonce::new(),
            session_name: destination.room.session_name,
            pane_id: PaneId::from_parts(crate::MuxName::Tmux, "%1"),
            origin: FocusOrigin::User,
            repair_generation: None,
            issued_at_ms: 1,
            applied_at_ms: None,
            state: FocusIntentState::Requested,
            pre_action: Vec::new(),
            placement: None,
            order: None,
        };
        let peers = peers.arm(anchor.pane_id.clone(), start);
        let (sent, until, fit_ms, ack, _) = peers
            .push(&anchor, start, || *clock.lock().unwrap())
            .unwrap();
        assert_eq!(
            fit_ms,
            (fit_time != Duration::ZERO).then_some(duration_millis(fit_time))
        );
        assert_eq!(sent, returned, "send timing starts only after the fit");
        assert_eq!(
            until,
            start + Duration::from_millis(500),
            "fit time spends the same click budget as request work and paint"
        );
        assert_eq!(rx.try_recv().unwrap(), Wakeup::Tick);
        let push = plane.take_focus_push(&destination.instance_id).unwrap();
        assert_eq!(push.expected_panes.as_deref(), Some(&rows));
        assert_eq!(push.until, until);
        let mut push = Some(push);
        let outcome = wait_for_paint(
            until,
            || *clock.lock().unwrap(),
            |timeout| {
                assert_eq!(timeout, Duration::from_millis(500).saturating_sub(fit_time));
                if painted {
                    push.take().unwrap().finish(PushVerdict::Painted);
                    Ok(ack.try_recv().unwrap())
                } else {
                    Err(RecvTimeoutError::Timeout)
                }
            },
            &mut Vec::new(),
        );
        assert_eq!(
            outcome,
            if painted && fit_time < Duration::from_millis(500) {
                PushOutcome::Painted
            } else {
                PushOutcome::TimedOut
            }
        );
    }
}
