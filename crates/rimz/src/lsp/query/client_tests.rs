use super::*;
use std::sync::{Condvar, Mutex};

#[test]
fn pool_overlaps_work_with_a_four_request_bound_and_preserves_order() {
    let state = Mutex::new((0, 0, false));
    let ready = Condvar::new();
    let result = bounded_map(&(0..12).collect::<Vec<_>>(), |index| {
        let mut state = state.lock().unwrap();
        state.0 += 1;
        state.1 = state.1.max(state.0);
        if state.0 == QUERY_WORKERS {
            state.2 = true;
            ready.notify_all();
        }
        let (mut state, timeout) = ready
            .wait_timeout_while(state, Duration::from_secs(2), |state| !state.2)
            .unwrap();
        let overlapped = !timeout.timed_out();
        state.0 -= 1;
        if overlapped {
            Ok(index * 2)
        } else {
            Err("workers did not overlap")
        }
    });
    assert_eq!(result.unwrap(), (0..12).map(|n| n * 2).collect::<Vec<_>>());
    assert_eq!(state.lock().unwrap().1, 4);
}

#[test]
fn pool_returns_the_first_input_error_with_its_type_intact() {
    let finished = Mutex::new(false);
    let ready = Condvar::new();
    let result = bounded_map(&[0, 1, 2], |index| -> std::result::Result<(), QueryErr> {
        Err(if *index == 0 {
            let (_guard, timeout) = ready
                .wait_timeout_while(
                    finished.lock().unwrap(),
                    Duration::from_secs(2),
                    |finished| !*finished,
                )
                .unwrap();
            assert!(!timeout.timed_out(), "later error must finish first");
            QueryErr::Indexing {
                server: "rust".into(),
                seconds: 30,
            }
        } else {
            *finished.lock().unwrap() = true;
            ready.notify_all();
            QueryErr::Unavailable {
                root: "/checkout".into(),
                reason: UnavailableReason::Crashed,
            }
        })
    });
    assert!(matches!(
        result,
        Err(QueryErr::Indexing { seconds: 30, .. })
    ));
}

#[test]
fn pool_claims_no_new_items_after_an_error() {
    let started = Mutex::new(0);
    let all_started = Condvar::new();
    let result = bounded_map(&(0..12).collect::<Vec<_>>(), |index| {
        let mut started = started.lock().unwrap();
        *started += 1;
        all_started.notify_all();
        let (_started, timeout) = all_started
            .wait_timeout_while(started, Duration::from_secs(2), |started| {
                *started < QUERY_WORKERS
            })
            .unwrap();
        assert!(!timeout.timed_out(), "workers must overlap");
        Err::<(), _>(*index)
    });
    assert_eq!(result, Err(0));
    assert_eq!(*started.lock().unwrap(), QUERY_WORKERS);
}
