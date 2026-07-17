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
fn fatal_session_message_points_missing_remote_rimz_at_setup() {
    let message = fatal_session_message(rimz::remote::REMOTE_RIMZ_MISSING_EXIT, "dev-box", "dev");

    assert!(
        message.contains("rimz is not installed on dev-box"),
        "{message}"
    );
    assert!(message.contains("rimz remote setup dev"), "{message}");
    assert!(
        !message.contains("not reconnecting"),
        "missing binary is not a reconnect-policy error: {message}"
    );
}

#[test]
fn fatal_session_message_keeps_reconnect_tail_for_other_codes() {
    let message = fatal_session_message(2, "dev-box", "dev");

    assert!(
        message.contains("ssh to dev-box exited with status 2"),
        "{message}"
    );
    assert!(message.contains("not reconnecting"), "{message}");
    assert!(!message.contains("remote setup"), "{message}");
}

#[test]
fn direct_dial_plan_selects_outage_age_pacing() {
    let policy = rimz::remote::ReconnectPolicy::default();
    let ladder = Duration::from_secs(30);

    assert_eq!(
        retry_delay(&policy, true, Duration::from_secs(30), ladder),
        Duration::from_secs(2)
    );
    assert_eq!(
        retry_delay(&policy, false, Duration::from_secs(30), ladder),
        ladder
    );
}

#[test]
fn plain_retry_wait_reports_interruption() {
    let stop = AtomicBool::new(true);
    let mut ui = OutageUi::plain_lines("dev-box");

    assert_eq!(
        wait_before_retry(
            None,
            None,
            Duration::from_secs(30),
            Duration::from_secs(30),
            true,
            &mut ui,
            Some(&stop),
        )
        .expect("wait result"),
        WaitOutcome::Interrupted
    );
}

#[test]
fn plain_retry_wait_omits_the_internet_checkpoint() {
    let ui = OutageUi::plain_lines("dev-box");

    assert_eq!(internet_probe_for_wait(&ui), None);
}

#[test]
fn settled_retry_wait_returns_only_attach_or_interrupted() {
    let mut ui = OutageUi::plain_lines("dev-box");

    assert_eq!(
        wait_before_retry(
            None,
            None,
            Duration::ZERO,
            Duration::from_secs(30),
            true,
            &mut ui,
            None,
        )
        .expect("wait result"),
        WaitOutcome::AttachNow {
            network_restored: false
        }
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
            "RimZ: remote link stalled",
            "No probe ack from dev for 8s.",
            LocalLinkNotificationDelivery::TerminalOnly,
            &prefs,
        )
        .is_none(),
        "blackout delivery is terminal-only"
    );

    let notification = local_link_command_notification(
        rimz::sidebar::notify::NotificationKind::LinkLost,
        "RimZ: remote link lost",
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
