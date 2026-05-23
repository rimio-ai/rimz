//! Synthetic hook driver. Exercises the per-request bridge socket and the
//! sidebar wakeup walk against real ledger writes — no subprocess.

mod common;

use std::time::Duration;

use rimz::bridge::{self, BridgeOutcome, ExpectedFrame, WakeupFrame};
use rimz::schema::heartbeat::SidebarHeartbeat;
use rimz::{FeedItem, FeedKind, MuxName, Resolution, ResolutionMethod, SidebarInstanceId, Surface};
use serde_json::json;

fn fresh_script_item(workspace_id: rimz::WorkspaceId) -> FeedItem {
    FeedItem::new(
        workspace_id,
        Surface::Script,
        FeedKind::Question,
        "Deploy staging?",
        "rimz",
        "cli",
    )
}

fn expected_from(item: &FeedItem) -> ExpectedFrame {
    ExpectedFrame {
        workspace_id: item.workspace_id.clone(),
        request_id: item.request_id.clone(),
        nonce: item.nonce.clone(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn script_surface_resolves_via_bridge_wakeup() {
    let h = common::Harness::new();
    if common::af_unix_bind_sandboxed(&h.runtime_paths.sock_dir) {
        tracing::warn!("skipping: AF_UNIX bind is forbidden in this sandbox");
        return;
    }
    let item = fresh_script_item(h.workspace_id.clone());
    let request_id = item.request_id.clone();
    let expected = expected_from(&item);

    let (sock, _path) = bridge::bind(&h.runtime_paths, &request_id).expect("bind");
    h.ledger.push_feed_item(&item, "rimz-test").expect("push");

    let ledger = h.ledger.clone();
    let req_for_task = request_id.clone();
    let resolver = tokio::task::spawn_blocking(move || {
        std::thread::sleep(Duration::from_millis(50));
        ledger.resolve_feed_item(
            &req_for_task,
            Resolution::new(json!({ "choice": "yes" }), ResolutionMethod::Cli),
            true,
            "rimz-test",
        )
    });

    let outcome = bridge::wait_for_resolution_owning(sock, expected, Some(Duration::from_secs(5)))
        .await
        .expect("wait_for_resolution_owning");
    resolver.await.expect("resolver join").expect("resolve");
    assert_eq!(outcome, BridgeOutcome::Resolved);

    let after = h.ledger.load_feed_item(&request_id).expect("reload");
    assert_eq!(after.status.to_string(), "resolved");
    let decision = after.resolution.expect("resolution").decision;
    assert_eq!(decision, json!({ "choice": "yes" }));
}

#[tokio::test(flavor = "current_thread")]
async fn cap_timeout_returns_neutral() {
    let h = common::Harness::new();
    if common::af_unix_bind_sandboxed(&h.runtime_paths.sock_dir) {
        tracing::warn!("skipping: AF_UNIX bind is forbidden in this sandbox");
        return;
    }
    let item = fresh_script_item(h.workspace_id.clone());
    let request_id = item.request_id.clone();
    let expected = expected_from(&item);

    let (sock, _path) = bridge::bind(&h.runtime_paths, &request_id).expect("bind");
    h.ledger.push_feed_item(&item, "rimz-test").expect("push");

    let outcome =
        bridge::wait_for_resolution_owning(sock, expected, Some(Duration::from_millis(100)))
            .await
            .expect("wait_for_resolution_owning");
    assert_eq!(outcome, BridgeOutcome::Neutral);

    let after = h.ledger.load_feed_item(&request_id).expect("reload");
    assert_eq!(
        after.status.to_string(),
        "pending",
        "the ledger entry is unchanged by a bridge timeout — caller decides next steps",
    );
}

#[tokio::test(flavor = "current_thread")]
async fn mismatched_nonce_is_dropped_real_resolve_wins() {
    use tokio::net::UnixDatagram;

    let h = common::Harness::new();
    if common::af_unix_bind_sandboxed(&h.runtime_paths.sock_dir) {
        tracing::warn!("skipping: AF_UNIX bind is forbidden in this sandbox");
        return;
    }
    let item = fresh_script_item(h.workspace_id.clone());
    let request_id = item.request_id.clone();
    let expected = expected_from(&item);

    let (sock, sock_path) = bridge::bind(&h.runtime_paths, &request_id).expect("bind");
    h.ledger.push_feed_item(&item, "rimz-test").expect("push");

    // Send a hand-crafted frame with the wrong nonce. The bridge must drop it.
    let bad_frame = WakeupFrame::FeedResolved {
        workspace_id: item.workspace_id.clone(),
        request_id: request_id.clone(),
        nonce: "this-nonce-is-wrong".to_owned(),
    };
    let bad_bytes = serde_json::to_vec(&bad_frame).expect("serialize bad frame");
    let sender = UnixDatagram::unbound().expect("sender");
    sender
        .send_to(&bad_bytes, &sock_path)
        .await
        .expect("send bad frame");

    let ledger = h.ledger.clone();
    let req_for_task = request_id.clone();
    let resolver = tokio::task::spawn_blocking(move || {
        std::thread::sleep(Duration::from_millis(50));
        ledger.resolve_feed_item(
            &req_for_task,
            Resolution::new(json!({ "choice": "yes" }), ResolutionMethod::Cli),
            true,
            "rimz-test",
        )
    });

    let outcome = bridge::wait_for_resolution_owning(sock, expected, Some(Duration::from_secs(5)))
        .await
        .expect("wait_for_resolution_owning");
    resolver.await.expect("resolver join").expect("resolve");
    assert_eq!(outcome, BridgeOutcome::Resolved);
}

#[test]
fn wake_sidebars_dispatches_to_fresh_heartbeats_and_skips_stale() {
    use std::os::unix::net::UnixDatagram;

    let h = common::Harness::new();
    if common::af_unix_bind_sandboxed(&h.runtime_paths.sock_dir) {
        tracing::warn!("skipping: AF_UNIX bind is forbidden in this sandbox");
        return;
    }

    let fresh_sock_path = h.runtime_paths.sock_dir.join("sidebar.fresh.sock");
    let fresh_recv = UnixDatagram::bind(&fresh_sock_path).expect("bind fresh");
    fresh_recv
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set read timeout");
    let fresh_hb = SidebarHeartbeat::new(
        h.workspace_id.clone(),
        SidebarInstanceId::new(),
        MuxName::Tmux,
        "rimz-test-fresh",
        fresh_sock_path,
    );
    std::fs::write(
        h.runtime_paths.heartbeat_dir.join("sidebar.fresh.json"),
        serde_json::to_vec(&fresh_hb).expect("serialize fresh hb"),
    )
    .expect("write fresh hb");

    let stale_sock_path = h.runtime_paths.sock_dir.join("sidebar.stale.sock");
    let stale_recv = UnixDatagram::bind(&stale_sock_path).expect("bind stale");
    stale_recv
        .set_read_timeout(Some(Duration::from_millis(200)))
        .expect("set read timeout stale");
    let mut stale_hb = SidebarHeartbeat::new(
        h.workspace_id.clone(),
        SidebarInstanceId::new(),
        MuxName::Tmux,
        "rimz-test-stale",
        stale_sock_path,
    );
    stale_hb.last_seen = jiff::Timestamp::now() - Duration::from_secs(60);
    std::fs::write(
        h.runtime_paths.heartbeat_dir.join("sidebar.stale.json"),
        serde_json::to_vec(&stale_hb).expect("serialize stale hb"),
    )
    .expect("write stale hb");

    let item = FeedItem::new(
        h.workspace_id.clone(),
        Surface::NativeUi,
        FeedKind::Generic,
        "wake me up",
        "rimz",
        "cli",
    );
    h.ledger.push_feed_item(&item, "rimz-test").expect("push");

    let mut buf = [0u8; 4096];
    let (n, _) = fresh_recv
        .recv_from(&mut buf)
        .expect("fresh sidebar should receive");
    let parsed: serde_json::Value = serde_json::from_slice(&buf[..n]).expect("parse envelope");
    assert_eq!(parsed["kind"], "ledger_delta");
    assert_eq!(
        parsed["workspace_id"],
        serde_json::to_value(&h.workspace_id).expect("ws json"),
    );
    assert_eq!(parsed["protocol_version"], "rimz.plugin.v1");

    let mut buf2 = [0u8; 4096];
    let stale_result = stale_recv.recv_from(&mut buf2);
    assert!(
        stale_result.is_err(),
        "stale sidebar must not receive a wakeup (got: {stale_result:?})",
    );
}

#[test]
fn wake_sidebars_restat_skips_when_mtime_aged_past_ttl() {
    // TOCTOU re-stat: the heartbeat JSON has a fresh `last_seen` so the
    // content check passes, but the file's mtime is backdated past the TTL.
    // The re-stat must skip the send — the content alone is not enough to
    // trust the wakeup_socket path on disk.
    use std::os::unix::net::UnixDatagram;
    use std::time::SystemTime;

    let h = common::Harness::new();
    if common::af_unix_bind_sandboxed(&h.runtime_paths.sock_dir) {
        tracing::warn!("skipping: AF_UNIX bind is forbidden in this sandbox");
        return;
    }

    let sock_path = h.runtime_paths.sock_dir.join("sidebar.toctou.sock");
    let recv = UnixDatagram::bind(&sock_path).expect("bind toctou");
    recv.set_read_timeout(Some(Duration::from_millis(200)))
        .expect("set read timeout");
    let hb = SidebarHeartbeat::new(
        h.workspace_id.clone(),
        SidebarInstanceId::new(),
        MuxName::Tmux,
        "rimz-test-toctou",
        sock_path,
    );
    let hb_path = h.runtime_paths.heartbeat_dir.join("sidebar.toctou.json");
    let file = std::fs::File::create(&hb_path).expect("create hb");
    serde_json::to_writer(&file, &hb).expect("write hb");
    file.sync_all().expect("sync hb");
    // Backdate mtime 60s into the past so the re-stat trips even though
    // `last_seen` inside the JSON is current.
    let aged = SystemTime::now() - Duration::from_secs(60);
    file.set_modified(aged).expect("backdate mtime");
    drop(file);

    let item = FeedItem::new(
        h.workspace_id.clone(),
        Surface::NativeUi,
        FeedKind::Generic,
        "wake me",
        "rimz",
        "cli",
    );
    h.ledger.push_feed_item(&item, "rimz-test").expect("push");

    let mut buf = [0u8; 4096];
    let recv_result = recv.recv_from(&mut buf);
    assert!(
        recv_result.is_err(),
        "aged-out mtime must block the wakeup even with fresh last_seen (got: {recv_result:?})",
    );
}
