//! Runtime/audit split for `rimz feed` CLI views.

use std::io::{BufRead, BufReader};
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::common::Env;

#[test]
fn feed_list_default_hides_dead_owner_pending_record_but_audit_shows_it() {
    let env = Env::new();
    let request_id = env.feed_ask_no_block("approve deploy?", &["yes", "no"]);

    let runtime = env.feed_list_json();
    assert!(
        runtime.as_array().expect("runtime feed array").is_empty(),
        "short-lived --no-block owner should be expelled from runtime list: {runtime:?}"
    );

    let audit = env.feed_list_audit_json();
    let items = audit.as_array().expect("audit feed array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["request_id"], request_id);
    assert_eq!(items[0]["status"], "pending");
}

#[test]
fn blocking_feed_ask_is_runtime_only_while_waiter_lives_and_gc_abandons_it() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }

    let mut child = env
        .rimz()
        .args([
            "feed",
            "ask",
            "--title",
            "approve deploy?",
            "--options",
            "yes,no",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn blocking feed ask");
    let stdout = child.stdout.take().expect("feed ask stdout");
    let mut reader = BufReader::new(stdout);
    let mut request_id = String::new();
    reader
        .read_line(&mut request_id)
        .expect("read request id line");
    let request_id = request_id.trim().to_owned();
    assert!(!request_id.is_empty(), "feed ask printed no request id");

    let seen_live = poll_until(Duration::from_secs(5), || {
        let runtime = env.feed_list_json();
        runtime
            .as_array()
            .expect("runtime feed array")
            .iter()
            .any(|item| item["request_id"] == request_id)
    });
    assert!(
        seen_live,
        "blocking ask should be runtime-visible while it waits"
    );

    let _ = child.kill();
    let _ = child.wait();

    let expelled = poll_until(Duration::from_secs(5), || {
        let runtime = env.feed_list_json();
        runtime
            .as_array()
            .expect("runtime feed array")
            .iter()
            .all(|item| item["request_id"] != request_id)
    });
    assert!(expelled, "dead waiter should be expelled from runtime list");

    let gc = env.rimz().arg("gc").output().expect("run gc");
    assert!(
        gc.status.success(),
        "gc failed: {}",
        String::from_utf8_lossy(&gc.stderr)
    );

    let item = env.feed_show_json(&request_id);
    assert_eq!(item["status"], "abandoned");
    assert_eq!(
        item["resolution"]["reason"], "owner_process_exited",
        "gc records the abandonment reason"
    );
}

fn poll_until(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if predicate() {
            return true;
        }
        // The predicate spawns `rimz feed list`; back off above the spawn cost.
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}
