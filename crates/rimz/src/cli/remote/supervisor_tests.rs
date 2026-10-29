use std::time::{Duration, Instant};

use super::*;

#[test]
fn exhausted_retries_report_one_held_assist_then_a_success() {
    let mut sync = PortSync::default();
    let mut assists = Vec::new();
    for (now_ms, available) in [(1_000, false), (2_000, false), (3_000, true)] {
        for action in sync.observe(now_ms, &[3000], Some(PortsScope::Room)) {
            let (local, report_exhaustion) =
                choose_local_forward_port(&mut sync, now_ms, 3000, |_| available, || None);
            if let Some(local) = local {
                sync.mark_opened(now_ms, 3000);
                assists.extend(port_forward_assist(action, "dev-box", Some(local), None));
            } else if report_exhaustion {
                assists.extend(port_forward_assist(
                    action,
                    "dev-box",
                    None,
                    Some("no free local port".into()),
                ));
            }
        }
    }
    assert_eq!(
        serde_json::to_value(assists).unwrap(),
        serde_json::json!([
            {"assist":"port_forward","host":"dev-box","remote":3000,
             "error":"no free local port"},
            {"assist":"port_forward","host":"dev-box","remote":3000,"local":3000}
        ]),
        "persistent exhaustion records one held attempt, then a successful forward"
    );
}

#[test]
fn report_opens_record_assists_but_master_reopens_and_closes_do_not() {
    let assist = port_forward_assist(PortAction::Open(3000), "dev-box", Some(3001), None);
    assert_eq!(
        assist.map(|assist| serde_json::to_value(assist).unwrap()),
        Some(serde_json::json!({
            "assist":"port_forward","host":"dev-box","remote":3000,"local":3001
        }))
    );
    let refused = port_forward_assist(
        PortAction::Open(9000),
        "dev-box",
        None,
        Some("forwarding disabled".into()),
    )
    .unwrap();
    let refused = serde_json::to_value(refused).unwrap();
    assert_eq!(refused["error"], "forwarding disabled");
    assert!(refused.get("local").is_none());
    for action in [
        PortAction::Reopen(3000),
        PortAction::Close {
            remote: 3000,
            local: 3001,
        },
    ] {
        assert!(port_forward_assist(action, "dev-box", Some(3001), None).is_none());
    }
}

#[test]
fn stderr_drain_keeps_a_bounded_tail_and_the_final_ssh_line() {
    let mut child = std::process::Command::new("sh")
        .args(["-c", "i=0; while [ \"$i\" -lt 12000 ]; do printf 'discarded\\n' >&2; i=$((i + 1)); done; printf 'Killed by signal 2.\\n' >&2"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let drain = drain_stderr(&mut child).unwrap();
    assert!(child.wait().unwrap().success());
    let text = drain.join().unwrap();
    assert!(text.len() <= 8192, "retained {} stderr bytes", text.len());
    assert!(text.starts_with("discarded\n"));
    assert!(text.ends_with("Killed by signal 2.\n"));
}

#[test]
fn remapped_port_forward_notice_reaches_terminal_with_the_local_port() {
    let (title, body) = remapped_port_forward_notice(3000, 3001, 3000);
    assert_eq!(title, "RimZ: port remapped");
    assert_eq!(
        body,
        "Port 3000 forwarded to localhost:3001 (3000 is busy here)"
    );
    assert_eq!(
        remapped_port_forward_notice(3000, 3002, 3001).1,
        "Port 3000 forwarded to localhost:3002 (3001 is busy here)"
    );
    // Only the gate is asserted here, never the payload: `osc::desktop_payload`
    // drops the OSC under an ambient Zellij (`mux::drops_desktop_osc`), so a
    // content assertion would pass or fail with the suite's own terminal.
    let prefs = rimz::config::NotificationsPrefs::default();
    assert!(
        local_link_terminal_notification_bytes(&title, &body, &prefs, false).is_empty(),
        "redirected stderr must not collect notification bytes"
    );
    assert!(
        !local_link_terminal_notification_bytes(&title, &body, &prefs, true).is_empty(),
        "terminal stderr carries the remap notice"
    );
}

#[test]
fn local_forward_port_probe_detects_ipv6_only_listeners() {
    for host in ["127.0.0.1", "0.0.0.0", "::1"] {
        let listener = match TcpListener::bind((host, 0)) {
            Ok(listener) => listener,
            Err(error) if host == "::1" => {
                writeln!(
                    std::io::stderr(),
                    "skipping: IPv6 loopback unavailable: {error}"
                )
                .unwrap();
                return;
            }
            Err(error) => panic!("bind {host}: {error}"),
        };
        let port = listener.local_addr().unwrap().port();
        assert!(
            !local_forward_port_available(port),
            "a listener on {host}:{port} makes the local port busy"
        );
    }
}

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
    let message = fatal_session_message(
        rimz::remote::REMOTE_RIMZ_MISSING_EXIT,
        "dev-box",
        None,
        "dev",
        None,
        FatalPhase::Ssh,
    );

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
fn fatal_session_message_reports_other_codes_without_setup_hint() {
    let message = fatal_session_message(2, "dev-box", None, "dev", None, FatalPhase::Ssh);

    assert!(
        message.contains("ssh to dev-box exited with status 2"),
        "{message}"
    );
    assert!(message.contains("not reconnecting"), "{message}");
    assert!(!message.contains("remote setup"), "{message}");
}

#[test]
fn fatal_session_message_distinguishes_established_room_boundaries() {
    for (phase, expected) in [
        (
            FatalPhase::BeforeRoom,
            "rimz on dev-box exited with status 2 before the room opened; not reconnecting",
        ),
        (
            FatalPhase::Room,
            "rimz on dev-box exited with status 2; not reconnecting",
        ),
    ] {
        assert_eq!(
            fatal_session_message(2, "dev-box", None, "dev", None, phase),
            expected
        );
    }
}

#[test]
fn fatal_session_message_includes_a_captured_attach_error() {
    let message = fatal_session_message(
        2,
        "dev-box",
        None,
        "dev",
        Some("SSH control connection dropped"),
        FatalPhase::Ssh,
    );

    assert!(
        message.ends_with("— SSH control connection dropped"),
        "{message}"
    );
}

#[test]
fn confirmed_transport_exit_maps_filtered_farewell_to_control_drop() {
    assert_eq!(
        session_error_summary(
            Some("Shared connection to dev-box closed.\n"),
            Some(rimz::remote::SSH_TRANSPORT_EXIT),
            true,
        ),
        Some("SSH control connection dropped".to_owned())
    );
    assert_eq!(
        session_error_summary(
            Some("Shared connection to dev-box closed.\n"),
            Some(0),
            true,
        ),
        None
    );
    assert_eq!(
        session_error_summary(Some("Permission denied (publickey).\n"), Some(2), true,),
        Some("Permission denied (publickey).".to_owned())
    );
}

#[test]
fn fatal_session_message_explains_minor_version_bypass() {
    let message = fatal_session_message(
        rimz::remote::REMOTE_VERSION_SKEW_EXIT,
        "dev-box",
        None,
        "dev",
        None,
        FatalPhase::Ssh,
    );

    assert!(message.contains("differ by a minor version"), "{message}");
    assert!(message.contains("rimz remote setup dev"), "{message}");
    assert!(message.contains("--force-version"), "{message}");
}

#[test]
fn fatal_session_message_keeps_major_version_refusal_hard() {
    let message = fatal_session_message(
        rimz::remote::REMOTE_VERSION_INCOMPATIBLE_EXIT,
        "dev-box",
        None,
        "dev",
        None,
        FatalPhase::Ssh,
    );

    assert!(message.contains("differ by a major version"), "{message}");
    assert!(message.contains("upgrade required"), "{message}");
    assert!(message.contains("rimz remote setup dev"), "{message}");
    assert!(!message.contains("--force-version"), "{message}");
}

#[test]
fn fatal_session_message_explains_a_missing_remote_path() {
    let message = fatal_session_message(
        rimz::remote::REMOTE_PATH_MISSING_EXIT,
        "dev-box",
        Some("workspace/missing"),
        "dev",
        Some(
            "remote path does not exist on dev-box: workspace/mis…; check the target with `rimz remote list`",
        ),
        FatalPhase::Ssh,
    );

    assert!(
        message.contains("remote path does not exist on dev-box: workspace/missing"),
        "{message}"
    );
    assert!(message.contains("rimz remote list"), "{message}");
    assert_eq!(message.matches("rimz remote list").count(), 1, "{message}");
}

#[test]
fn missing_path_sentinel_rebuilds_a_long_path_without_stderr() {
    let path =
        "workspace/projects/query-engine/with/a/very/long/path/that/exceeds/the/summary/limit";

    let message = fatal_session_message(
        rimz::remote::REMOTE_PATH_MISSING_EXIT,
        "dev-box",
        Some(path),
        "dev",
        None,
        FatalPhase::Ssh,
    );

    assert!(message.contains(path), "{message}");
    assert_eq!(message.matches("rimz remote list").count(), 1, "{message}");
}

#[test]
fn interrupted_message_preserves_the_last_ssh_error() {
    assert_eq!(
        interrupted_message(
            ConnectStage::Initial,
            "dev-box",
            Some("Permission denied (publickey).")
        ),
        "rimz: connect to dev-box stopped — last error: Permission denied (publickey)."
    );
    assert_eq!(
        interrupted_message(ConnectStage::Recovery, "dev-box", None),
        "rimz: reconnect to dev-box stopped"
    );
}

#[test]
fn path_preflight_classifies_only_exit_one_as_missing() {
    assert!(path_preflight_missing(Some(1)));
    for code in [
        Some(0),
        Some(2),
        Some(rimz::remote::SSH_TRANSPORT_EXIT),
        None,
    ] {
        assert!(!path_preflight_missing(code));
    }
}

#[test]
fn plain_ui_omits_the_internet_checkpoint() {
    let ui = OutageUi::plain_lines(ConnectStage::Recovery, "dev-box");

    assert_eq!(internet_probe_for_wait(&ui), None);
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

#[test]
fn preestablished_probe_preserves_the_master_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let control = dir.path().join("master.sock");
    std::fs::write(&control, b"owned by master guard").expect("write control marker");
    let probe = ProbeHandle {
        stop: Arc::new(AtomicBool::new(true)),
        join: None,
        control_path: Some(control.clone()),
        remove_control_path: false,
    };

    drop(probe);

    assert!(control.exists());
}

#[test]
fn master_guard_transfer_preserves_then_cleans_up_the_control_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let control = dir.path().join("master.sock");
    std::fs::write(&control, b"live control socket").expect("write control marker");
    let attempt = MasterAttempt {
        child: None,
        stderr: None,
        control_path: control.clone(),
        remove_control_path_on_drop: true,
    };

    let guard = attempt.into_guard();
    assert!(
        control.exists(),
        "transferring ownership must preserve the live socket"
    );

    drop(guard);
    assert!(!control.exists(), "the owning guard cleans up the socket");
}
