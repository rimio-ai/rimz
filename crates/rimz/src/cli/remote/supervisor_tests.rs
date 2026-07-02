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
