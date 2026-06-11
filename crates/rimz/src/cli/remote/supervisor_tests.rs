use std::time::{Duration, Instant};

use super::*;

#[test]
fn disconnected_link_event_channel_keeps_poll_cadence() {
    let (tx, rx) = std::sync::mpsc::channel::<LinkEvent>();
    drop(tx);
    let poll = Duration::from_millis(20);
    let started = Instant::now();

    assert!(recv_link_event(&rx, poll).is_none());

    assert!(
        started.elapsed() >= Duration::from_millis(15),
        "a disconnected probe channel must not hot-poll the ssh child"
    );
}

#[test]
fn probe_respawn_backoff_is_capped_and_resettable() {
    let mut backoff = ProbeRespawnBackoff::default();

    assert_eq!(backoff.next_delay(), Duration::from_secs(1));
    assert_eq!(backoff.next_delay(), Duration::from_secs(2));
    assert_eq!(backoff.next_delay(), Duration::from_secs(4));
    assert_eq!(backoff.next_delay(), Duration::from_secs(8));
    assert_eq!(backoff.next_delay(), Duration::from_secs(16));
    assert_eq!(backoff.next_delay(), Duration::from_secs(30));
    assert_eq!(backoff.next_delay(), Duration::from_secs(30));

    backoff.reset();
    assert_eq!(backoff.next_delay(), Duration::from_secs(1));
}

#[test]
fn finish_probe_stream_drains_tail_ack() {
    let (ack_tx, ack_rx) = std::sync::mpsc::channel::<u64>();
    let (event_tx, event_rx) = std::sync::mpsc::channel::<LinkEvent>();
    let mut window = ProbeWindow::with_timeout(Duration::from_millis(100));
    let mut blackout_latched = false;
    let mut seen_ack = false;
    let sent_at_ms = rimz::sidebar::cache::unix_now_ms().saturating_sub(20);

    window.record_sent(7, sent_at_ms);
    window.record_sent(8, sent_at_ms + 10);
    ack_tx.send(7).expect("send tail ack");
    ack_tx.send(8).expect("send second tail ack");
    drop(ack_tx);

    assert!(finish_probe_stream(
        &ack_rx,
        &event_tx,
        &mut window,
        &mut blackout_latched,
        &mut seen_ack,
        false,
    ));
    assert!(seen_ack);
    assert!(!blackout_latched);
    match event_rx.try_recv().expect("first ack event") {
        LinkEvent::FirstAck => {}
        other => panic!("expected first ack event, got {other:?}"),
    }
    let stats = window.stats();
    assert_eq!(stats.window, 2);
    assert_eq!(stats.miss_pct, 0);
    assert!(stats.rtt_ms.is_some());
    assert!(
        event_rx.try_recv().is_err(),
        "only the first ack emits an event"
    );
}

#[test]
fn ack_drain_reports_when_rtt_becomes_publishable() {
    let (ack_tx, ack_rx) = std::sync::mpsc::channel::<u64>();
    let (event_tx, _event_rx) = std::sync::mpsc::channel::<LinkEvent>();
    let mut window = ProbeWindow::with_timeout(Duration::from_millis(100));
    let mut blackout_latched = false;
    let mut seen_ack = false;
    let now_ms = rimz::sidebar::cache::unix_now_ms();

    window.record_sent(1, now_ms.saturating_sub(50));
    ack_tx.send(1).expect("send first ack");
    let first = drain_probe_acks(
        &ack_rx,
        &event_tx,
        &mut window,
        &mut blackout_latched,
        &mut seen_ack,
    );
    assert!(first.acked);
    assert!(
        !first.reported_rtt_changed,
        "the first ack is accounted but keeps the badge warming"
    );
    assert_eq!(window.stats().rtt_ms, None);

    window.record_sent(2, now_ms.saturating_sub(55));
    ack_tx.send(2).expect("send second ack");
    let second = drain_probe_acks(
        &ack_rx,
        &event_tx,
        &mut window,
        &mut blackout_latched,
        &mut seen_ack,
    );

    assert!(second.acked);
    assert!(
        second.reported_rtt_changed,
        "the second ack seeds the displayed RTT and should publish immediately"
    );
    assert!(window.stats().rtt_ms.is_some());
}

#[test]
fn probe_blackout_requires_prior_ack_threshold_and_latches() {
    let (tx, rx) = std::sync::mpsc::channel::<LinkEvent>();
    let mut window = ProbeWindow::with_timeout(Duration::from_millis(100));
    let mut blackout_latched = false;
    let blackout_after_ms = LINK_BLACKOUT_AFTER.as_millis() as u64;

    window.record_sent(1, 1_000);
    maybe_send_probe_blackout_at(
        &tx,
        &mut window,
        &mut blackout_latched,
        false,
        1_000 + blackout_after_ms,
    );
    assert!(rx.try_recv().is_err());
    assert!(!blackout_latched);

    assert!(window.record_ack(1, 1_020));

    maybe_send_probe_blackout_at(
        &tx,
        &mut window,
        &mut blackout_latched,
        true,
        1_020 + blackout_after_ms - 1,
    );
    assert!(rx.try_recv().is_err());
    assert!(!blackout_latched);

    maybe_send_probe_blackout_at(
        &tx,
        &mut window,
        &mut blackout_latched,
        true,
        1_020 + blackout_after_ms,
    );
    match rx.try_recv().expect("blackout event") {
        LinkEvent::Blackout(duration) => assert_eq!(duration, LINK_BLACKOUT_AFTER),
        other => panic!("expected blackout event, got {other:?}"),
    }
    assert!(blackout_latched);

    maybe_send_probe_blackout_at(
        &tx,
        &mut window,
        &mut blackout_latched,
        true,
        1_020 + blackout_after_ms + 1_000,
    );
    assert!(
        rx.try_recv().is_err(),
        "latched blackout events are not repeated"
    );
}

#[test]
fn link_notifications_respect_command_and_terminal_gates() {
    let prefs = rimz::config::NotificationsPrefs {
        enabled: true,
        command: Some("notify-send rimz".to_owned()),
        ..rimz::config::NotificationsPrefs::default()
    };

    assert!(
        local_link_command_notification(
            rimz::sidebar::notify::NotificationKind::LinkLost,
            "Rimz: remote link stalled",
            "No probe ack from dev for 8s.",
            LocalLinkNotificationDelivery::TerminalOnly,
            &prefs,
        )
        .is_none(),
        "blackout delivery is terminal-only"
    );

    let notification = local_link_command_notification(
        rimz::sidebar::notify::NotificationKind::LinkLost,
        "Rimz: remote link lost",
        "SSH to dev dropped; reconnecting.",
        LocalLinkNotificationDelivery::TerminalAndCommand,
        &prefs,
    )
    .expect("lost-link delivery spawns the configured command");
    assert_eq!(notification.kind_env(), "link_lost");

    assert!(
        local_link_terminal_notification_bytes("Title", "Body", &prefs, false).is_empty(),
        "redirected stderr must not collect OSC or BEL bytes"
    );
    assert!(
        !local_link_terminal_notification_bytes("Title", "Body", &prefs, true).is_empty(),
        "terminal stderr keeps the configured terminal notification bytes"
    );
}

#[cfg(unix)]
#[test]
fn prepare_control_path_hardens_control_directories() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let runtime = dir.path().join("runtime");
    let rimz_dir = runtime.join("rimz");
    let link_dir = rimz_dir.join("link");
    std::fs::create_dir_all(&link_dir).expect("mkdir link dir");
    for path in [&runtime, &rimz_dir, &link_dir] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o777))
            .expect("make dir world-accessible");
    }
    let control = link_dir.join("link.sock");

    prepare_control_path(&control).expect("prepare control path");

    for path in [&runtime, &rimz_dir, &link_dir] {
        let mode = std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "{} is private", path.display());
    }
}
