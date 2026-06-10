//! Runtime/audit split for `rimz feed` CLI views.

use std::fs;
use std::io::{BufRead, BufReader};
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::common::{Env, tmux_pane};
use serde_json::Value;

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

#[test]
fn blocking_feed_ask_reports_closed_when_item_file_disappears() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }

    let child = env
        .rimz()
        .args([
            "feed",
            "ask",
            "--title",
            "approve deploy?",
            "--options",
            "yes,no",
            "--timeout",
            "1s",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn blocking feed ask");
    let request_id = env
        .poll_pending_request_id(Instant::now() + Duration::from_secs(5))
        .expect("blocking ask should reach pending state");

    remove_feed_files(&env, &request_id);

    let output = child.wait_with_output().expect("wait feed ask");
    assert!(
        !output.status.success(),
        "missing feed item should close the ask without a decision"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!(
            "request {request_id} closed before a decision was delivered"
        )),
        "feed ask should map missing item files to the closed-request message, stderr:\n{stderr}"
    );
}

#[test]
fn blocking_feed_ask_in_a_pane_renders_a_frame_admitted_card() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }

    // The asking script runs inside a mux pane, so the per-pane env survives
    // into the CLI child and stamps the ask's pane.
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
        .env("TMUX_PANE", "%5")
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

    // The frame admits the stamped pane → the ask renders as its card.
    let parsed = env.snapshot_json_with_panes(&[tmux_pane("%5", "deploy.sh", &env.project_root)]);
    let rows: Vec<&Value> = parsed["worktree_groups"]
        .as_array()
        .expect("groups")
        .iter()
        .flat_map(|group| group["rows"].as_array().expect("rows"))
        .collect();
    assert_eq!(rows.len(), 1, "one frame-admitted ask card: {rows:?}");
    assert_eq!(rows[0]["status"], "waiting");
    assert_eq!(rows[0]["request_id"], request_id.as_str());
    assert_eq!(rows[0]["pane"]["pane_id"], "tmux:%5");

    // The stamped pane absent from the frame → metadata, not a card.
    let parsed = env.snapshot_json_with_panes(&[tmux_pane("%9", "zsh", &env.project_root)]);
    let groups = parsed["worktree_groups"].as_array().expect("groups");
    assert!(
        groups
            .iter()
            .flat_map(|group| group["rows"].as_array().expect("rows"))
            .all(|row| row["request_id"] != request_id.as_str()),
        "an ask whose pane left the frame renders no card: {groups:?}"
    );
    let needs_attention = parsed["needs_attention"].as_array().expect("needs");
    assert!(
        needs_attention
            .iter()
            .any(|item| item["request_id"] == request_id.as_str()),
        "the pending ask survives as rollup metadata"
    );

    let _ = child.kill();
    let _ = child.wait();
}

fn remove_feed_files(env: &Env, request_id: &str) {
    let paths = env.state_path_for(&env.project_root);
    let _ = fs::remove_file(paths.feed_dir.join(format!("{request_id}.json")));
    let _ = fs::remove_file(
        paths
            .feed_dir
            .join("terminal")
            .join(format!("{request_id}.json")),
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
