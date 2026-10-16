use super::*;
use crate::diag::focus_trace::log_path;
use crate::sidebar_pane::app::fixtures::{serve_config, workspace};
use crate::sidebar_pane::app::plane::DataPlane;
use std::cell::Cell;

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
        for painted in [false, true] {
            let clock = Cell::new(start);
            assert_eq!(
                wait_for_paint(
                    start + PAINT_WAIT,
                    || clock.get(),
                    |_| {
                        clock.set(start + elapsed);
                        if painted {
                            Ok(report(PushVerdict::Painted))
                        } else {
                            Err(RecvTimeoutError::Disconnected)
                        }
                    },
                    &mut Vec::new()
                ),
                PushOutcome::TimedOut,
                "elapsed={elapsed:?}, painted={painted}"
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
            &source.session_name,
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
            session_name: self.source.session_name.clone(),
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
        std::fs::read_to_string(log_path(self.dir.path()))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
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
        assert_eq!(
            rig.peers.dispatch(anchor.clone(), || {
                assert!(rig.records().is_empty(), "recording must follow the jump");
                17
            }),
            17
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
        assert_eq!(row["session_name"], rig.source.session_name);
        let event = &row["event"];
        assert_eq!(event["kind"], "push_handshake");
        assert_eq!(event["nonce"], anchor.nonce.to_string());
        assert_eq!(event["target"], "tmux:%3");
        assert_eq!(event["outcome"], "released");
        assert_eq!(event["budget_ms"], 100);
        assert!(event["dispatched_ms"].as_u64().unwrap() >= event["decided_ms"].as_u64().unwrap());
        let mut expected = if with_peers {
            vec![
                serde_json::json!({"instance_id": full.instance_id, "delivery": "inbox_full"}),
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
    let destination_thread = std::thread::spawn(move || {
        let Wakeup::FocusPush(mut push) = rx.recv().unwrap() else {
            panic!("expected push")
        };
        push.peer.received_ms = Some(push.elapsed_ms());
        push.finish(PushVerdict::Painted);
    });
    rig.peers
        .dispatch(rig.anchor(), || assert!(rig.records().is_empty()));
    destination_thread.join().unwrap();
    let rows = rig.records();
    assert_eq!(rows.len(), 1, "successful handshakes are recorded too");
    assert_eq!(rows[0]["event"]["outcome"], "painted");
    assert_eq!(rows[0]["event"]["peers"][0]["verdict"], "painted");
    assert_eq!(rows[0]["event"]["peers"][0]["delivery"], "sent");
}

#[test]
fn dispatch_records_the_fitted_push_budget() {
    let rig = TraceRig::new();
    let peers = rig.peers.clone().with_fit(Some(Arc::new(|anchor| {
        Some(HashMap::from([(
            anchor.pane_id.clone(),
            crate::mux::FittedPane {
                rows: 70,
                cols: None,
            },
        )]))
    })));
    peers.dispatch(rig.anchor(), || assert!(rig.records().is_empty()));
    assert_eq!(rig.records()[0]["event"]["budget_ms"], 300);
}

#[test]
fn dispatch_collects_reports_sent_after_the_budget_but_before_the_horizon() {
    let rig = TraceRig::new();
    let destination = serve_config(&workspace());
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let _subscription = rig.plane.subscribe(&destination, tx);
    let (go_tx, go_rx) = channel();
    let destination_thread = std::thread::spawn(move || {
        let Wakeup::FocusPush(mut push) = rx.recv().unwrap() else {
            panic!("expected push")
        };
        push.peer.received_ms = Some(push.elapsed_ms());
        go_rx.recv().unwrap();
        push.finish(PushVerdict::Expired);
    });
    rig.peers.dispatch(rig.anchor(), || {
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
    assert!(event["decided_ms"].as_u64().unwrap() >= 100);
    assert!(event["peers"][0]["finished_ms"].as_u64().unwrap() >= 100);
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
        let destination_thread = std::thread::spawn(move || {
            let Wakeup::FocusPush(mut push) = rx.recv().unwrap() else {
                panic!("expected push")
            };
            push.peer.received_ms = Some(push.elapsed_ms());
            go_rx.recv().unwrap();
            if !within_horizon {
                std::thread::sleep(REPORT_HORIZON);
            }
            push.finish(PushVerdict::Expired);
            done_tx.send(()).unwrap();
        });
        rig.peers.dispatch(rig.anchor(), || {
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
fn stale_push_carries_rows_and_waits_from_fit_return_until_first_ack_or_cap() {
    use crate::mux::focus_anchor::{FocusIntentState, FocusOrigin};
    use crate::sidebar_pane::app::fixtures::{serve_config, workspace};
    use crate::sidebar_pane::app::plane::DataPlane;
    use std::sync::Mutex;

    for painted in [true, false] {
        let plane = DataPlane::detached();
        let destination = serve_config(&workspace());
        let (tx, rx) = std::sync::mpsc::sync_channel(8);
        let _subscription = plane.subscribe(&destination, tx);
        let start = Instant::now();
        let returned = start + Duration::from_secs(2);
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
            session_name: destination.session_name,
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
        let (sent, until, ack, _) = peers.push(&anchor, || *clock.lock().unwrap());
        assert_eq!(sent, returned, "send timing starts only after the fit");
        assert_eq!(
            until,
            returned + Duration::from_millis(300),
            "fit time must not spend the stale paint budget"
        );
        let Wakeup::FocusPush(push) = rx.try_recv().unwrap() else {
            panic!("destination must receive the focus push")
        };
        assert_eq!(push.expected_panes.as_deref(), Some(&rows));
        assert_eq!(push.until, until);
        let mut push = Some(push);
        let outcome = wait_for_paint(
            until,
            || *clock.lock().unwrap(),
            |timeout| {
                assert_eq!(timeout, Duration::from_millis(300));
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
            if painted {
                PushOutcome::Painted
            } else {
                PushOutcome::TimedOut
            }
        );
    }
}
