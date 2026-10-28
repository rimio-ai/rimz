//! Sidebar wakeup fanout against real store writes — no subprocess.

use std::time::Duration;

use rimz::wakeup::heartbeat::SidebarHeartbeat;
use rimz::{MuxName, SidebarInstanceId};

#[test]
fn wake_sidebars_drops_datagrams_when_receiver_queue_is_full() {
    use std::os::unix::net::UnixDatagram;

    let h = crate::common::Harness::new();
    if h.skip_if_sandboxed() {
        return;
    }

    let sock_path = h.runtime_paths.sock_dir.join("event.full.sock");
    let recv = UnixDatagram::bind(&sock_path).expect("bind full receiver");
    recv.set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set read timeout");
    let hb = SidebarHeartbeat::new(
        h.workspace_id.clone(),
        SidebarInstanceId::new(),
        MuxName::Tmux,
        "rimz-test-full",
        sock_path,
        None,
    );
    std::fs::write(
        h.runtime_paths.heartbeat_dir.join("sidebar.full.json"),
        serde_json::to_vec(&hb).expect("serialize full hb"),
    )
    .expect("write full hb");
    crate::common::room::date_heartbeat_as_live(
        &h.runtime_paths.heartbeat_dir.join("sidebar.full.json"),
    );

    for _ in 0..2_000 {
        rimz::wakeup::wake_store_delta(&h.runtime_paths, None, None).expect("wake sidebars");
    }

    let mut buf = [0u8; 4096];
    let received = recv.recv(&mut buf).expect("receiver holds sent prefix");
    assert!(received > 0);
}

#[test]
fn wake_sidebars_dispatches_to_fresh_heartbeats_and_skips_stale_or_wrong_protocol() {
    use std::os::unix::net::UnixDatagram;

    let h = crate::common::Harness::new();
    if h.skip_if_sandboxed() {
        return;
    }

    let fresh_sock_path = h.runtime_paths.sock_dir.join("event.fresh.sock");
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
        None,
    );
    std::fs::write(
        h.runtime_paths.heartbeat_dir.join("sidebar.fresh.json"),
        serde_json::to_vec(&fresh_hb).expect("serialize fresh hb"),
    )
    .expect("write fresh hb");
    crate::common::room::date_heartbeat_as_live(
        &h.runtime_paths.heartbeat_dir.join("sidebar.fresh.json"),
    );

    let mut sibling_hb = fresh_hb.clone();
    sibling_hb.instance_id = SidebarInstanceId::new();
    std::fs::write(
        h.runtime_paths.heartbeat_dir.join("sidebar.sibling.json"),
        serde_json::to_vec(&sibling_hb).expect("serialize sibling hb"),
    )
    .expect("write sibling hb");
    crate::common::room::date_heartbeat_as_live(
        &h.runtime_paths.heartbeat_dir.join("sidebar.sibling.json"),
    );

    let stale_sock_path = h.runtime_paths.sock_dir.join("event.stale.sock");
    let stale_recv = UnixDatagram::bind(&stale_sock_path).expect("bind stale");
    // The wakeup walk runs synchronously inside the writer, so a wrongly sent
    // datagram would already be buffered by the time we read — a short timeout
    // proves the negative just as well as a long one.
    stale_recv
        .set_read_timeout(Some(Duration::from_millis(50)))
        .expect("set read timeout stale");
    let stale_hb = SidebarHeartbeat::new(
        h.workspace_id.clone(),
        SidebarInstanceId::new(),
        MuxName::Tmux,
        "rimz-test-stale",
        stale_sock_path,
        None,
    );
    std::fs::write(
        h.runtime_paths.heartbeat_dir.join("sidebar.stale.json"),
        serde_json::to_vec(&stale_hb).expect("serialize stale hb"),
    )
    .expect("write stale hb");
    std::fs::File::open(h.runtime_paths.heartbeat_dir.join("sidebar.stale.json"))
        .unwrap()
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(60))
        .unwrap();

    let wrong_protocol_sock_path = h.runtime_paths.sock_dir.join("event.wrong-protocol.sock");
    let wrong_protocol_recv =
        UnixDatagram::bind(&wrong_protocol_sock_path).expect("bind wrong protocol");
    wrong_protocol_recv
        .set_read_timeout(Some(Duration::from_millis(50)))
        .expect("set read timeout wrong protocol");
    let mut wrong_protocol_hb = SidebarHeartbeat::new(
        h.workspace_id.clone(),
        SidebarInstanceId::new(),
        MuxName::Tmux,
        "rimz-test-wrong-protocol",
        wrong_protocol_sock_path,
        None,
    );
    wrong_protocol_hb.protocol_version = "rimz.plugin.v0".to_owned();
    std::fs::write(
        h.runtime_paths
            .heartbeat_dir
            .join("sidebar.wrong-protocol.json"),
        serde_json::to_vec(&wrong_protocol_hb).expect("serialize wrong protocol hb"),
    )
    .expect("write wrong protocol hb");

    h.store
        .append_event(&crate::common::lifecycle_event(
            &h,
            "rimz-test",
            "SessionStart",
            "wake-me",
        ))
        .expect("append event");

    let mut buf = [0u8; 4096];
    let (n, _) = fresh_recv
        .recv_from(&mut buf)
        .expect("fresh sidebar should receive");
    let parsed: serde_json::Value = serde_json::from_slice(&buf[..n]).expect("parse envelope");
    assert_eq!(parsed["v"], rimz::wakeup::events::SIDEBAR_EVENT_VERSION);
    assert_eq!(
        parsed["workspace_id"],
        serde_json::to_value(&h.workspace_id).expect("ws json"),
    );
    assert_eq!(parsed["event"]["kind"], "store_delta");
    assert!(parsed["sent_at_ms"].as_u64().is_some());

    fresh_recv
        .set_nonblocking(true)
        .expect("nonblocking receive");
    assert_eq!(
        fresh_recv.recv(&mut buf).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "one host receives only one datagram for its two panes",
    );

    let mut buf2 = [0u8; 4096];
    let stale_result = stale_recv.recv_from(&mut buf2);
    assert!(
        stale_result.is_err(),
        "stale sidebar must not receive a wakeup (got: {stale_result:?})",
    );

    let wrong_protocol_result = wrong_protocol_recv.recv_from(&mut buf2);
    assert!(
        wrong_protocol_result.is_err(),
        "wrong-protocol sidebar must not receive a wakeup (got: {wrong_protocol_result:?})",
    );
}

#[test]
fn wake_sidebars_restat_skips_when_mtime_aged_past_ttl() {
    // Readable current-protocol JSON is not enough to trust the socket path:
    // the post-read metadata check must reject an expired mtime.
    use std::os::unix::net::UnixDatagram;
    use std::time::SystemTime;

    let h = crate::common::Harness::new();
    if h.skip_if_sandboxed() {
        return;
    }

    let sock_path = h.runtime_paths.sock_dir.join("event.toctou.sock");
    let recv = UnixDatagram::bind(&sock_path).expect("bind toctou");
    // Synchronous walk (see the stale/wrong-protocol test): a short timeout is
    // enough to prove the re-stat blocked the send.
    recv.set_read_timeout(Some(Duration::from_millis(50)))
        .expect("set read timeout");
    let hb = SidebarHeartbeat::new(
        h.workspace_id.clone(),
        SidebarInstanceId::new(),
        MuxName::Tmux,
        "rimz-test-toctou",
        sock_path,
        None,
    );
    let hb_path = h.runtime_paths.heartbeat_dir.join("sidebar.toctou.json");
    let file = std::fs::File::create(&hb_path).expect("create hb");
    serde_json::to_writer(&file, &hb).expect("write hb");
    // Backdate mtime 60s into the past so the freshness check rejects it.
    let aged = SystemTime::now() - Duration::from_secs(60);
    file.set_modified(aged).expect("backdate mtime");
    drop(file);

    h.store
        .append_event(&crate::common::lifecycle_event(
            &h,
            "rimz-test",
            "SessionStart",
            "wake-me",
        ))
        .expect("append event");

    let mut buf = [0u8; 4096];
    let recv_result = recv.recv_from(&mut buf);
    assert!(
        recv_result.is_err(),
        "aged-out mtime must block the wakeup (got: {recv_result:?})",
    );
}
