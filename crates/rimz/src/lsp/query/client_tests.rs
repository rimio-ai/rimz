use super::*;
use std::sync::{Condvar, Mutex};

fn query_reply(mut response: Value) -> QueryErr {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    let root = tempfile::tempdir().unwrap();
    let entry: Entry = serde_json::from_value(json!({
        "root":root.path(), "server":"rust", "nonce":"test", "broker_pid":1,
        "broker_start_token":"test", "state":"ready", "started_at_ms":0,
        "estimate_bytes":0, "settings_hash":"", "request_count":0, "peak_rss_kb":0, "leases":[]
    }))
    .unwrap();
    let directory = registry::directory(root.path(), "rust").unwrap();
    std::fs::create_dir_all(&directory).unwrap();
    let listener = UnixListener::bind(directory.join("sock")).unwrap();
    response["nonce"] = json!("test");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["op"], "query");
        writeln!(stream, "{response}").unwrap();
    });
    let error = request(&entry, "workspace/symbol", json!({})).unwrap_err();
    server.join().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    error
}

#[test]
fn memory_refusal_preserves_the_brokers_figures() {
    let error = query_reply(json!({"refused":{
        "root":"/checkout", "server":"rust", "estimate_bytes":90_000,
        "available_bytes":5000, "swap_free_bytes":50_000, "reserve_bytes":1000, "holders":[]
    }}));
    assert_eq!(error.exit_code(), 4);
    assert!(error.to_string().contains("not started: memory short, needs 90 KB, 54 KB free (5 KB available + 50 KB swap, 1 KB reserved); use grep for this question and retry on the next"), "{error}");
}

#[test]
fn unparseable_refusals_are_still_not_ready() {
    for refused in [Value::Null, json!({"estimate_bytes":"bad"})] {
        let error = query_reply(json!({"refused":refused}));
        assert_eq!(error.exit_code(), 4);
        assert!(
            error.to_string().contains(
                "not started: memory short; use grep for this question and retry on the next"
            ),
            "{error}"
        );
    }
}

#[test]
fn watchdog_pressure_is_not_ready_but_other_stops_are_unavailable() {
    let error = query_reply(json!({"error":{"code":-32003,"message":"memory pressure"}}));
    assert_eq!(error.exit_code(), 4);
    assert!(
        error
            .to_string()
            .contains("stopped: memory pressure; use grep for this question and retry on the next"),
        "{error}"
    );
    for reason in [
        "idle",
        "evicted",
        "recycled",
        "crashed",
        "stopped by hand",
        "checkout removed",
        "released",
        "never leased",
        "team done",
    ] {
        let error = query_reply(json!({"error":{"code":-32003,"message":reason}}));
        assert_eq!(error.exit_code(), 3, "{reason}: {error}");
    }
}

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
