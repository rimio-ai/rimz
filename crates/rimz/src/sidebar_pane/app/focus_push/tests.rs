use super::*;
use std::cell::Cell;

#[test]
fn paint_wait_returns_on_first_painted_without_waiting_for_other_peers() {
    let now = Instant::now();
    let receives = Cell::new(0);
    let timed_out = wait_for_paint(
        now + PAINT_WAIT,
        || now,
        |timeout| {
            assert_eq!(timeout, PAINT_WAIT);
            receives.set(receives.get() + 1);
            Ok(())
        },
    );
    assert_eq!(receives.get(), 1, "must receive the destination's paint");
    assert!(!timed_out);
}

#[test]
fn paint_wait_returns_when_all_peers_release() {
    let now = Instant::now();
    let receives = Cell::new(0);
    let timed_out = wait_for_paint(
        now + PAINT_WAIT,
        || now,
        |_| {
            receives.set(receives.get() + 1);
            Err(RecvTimeoutError::Disconnected)
        },
    );
    assert_eq!(receives.get(), 1, "must observe all peers releasing");
    assert!(!timed_out);
}

#[test]
fn paint_wait_is_capped_even_when_a_peer_retains_its_ack() {
    let start = Instant::now();
    let clock = Cell::new(start);
    let timed_out = wait_for_paint(
        start + PAINT_WAIT,
        || clock.get(),
        |timeout| {
            assert_eq!(timeout, PAINT_WAIT);
            clock.set(clock.get() + timeout);
            Err(RecvTimeoutError::Timeout)
        },
    );
    assert!(timed_out, "an unresponsive destination must hit the cap");
    assert_eq!(clock.get(), start + PAINT_WAIT);
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
            .focus_peers(SidebarInstanceId::new())
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
        let (until, ack) = peers.push(&anchor, || *clock.lock().unwrap());
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
        let timed_out = wait_for_paint(
            until,
            || *clock.lock().unwrap(),
            |timeout| {
                assert_eq!(timeout, Duration::from_millis(300));
                if painted {
                    push.ack.clone().painted();
                    ack.try_recv().unwrap();
                    Ok(())
                } else {
                    Err(RecvTimeoutError::Timeout)
                }
            },
        );
        assert_eq!(timed_out, !painted);
    }
}
