use super::*;

#[cfg(unix)]
fn exited(code: i32) -> AttachOutcome {
    use std::os::unix::process::ExitStatusExt;
    AttachOutcome::Exited(std::process::ExitStatus::from_raw(code << 8))
}

#[cfg(unix)]
#[test]
fn early_refusal_reattaches_once_inside_one_terminal_bracket() {
    let spec = rimz::mux::CommandSpec::new("zellij");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut spawns = 0;
    let mut observations = 0;
    let mut events = Vec::new();
    let outcome = run_attach_command(
        &spec,
        &mut stdout,
        true,
        Some(OsStr::new("1")),
        true,
        |output| {
            run_attach_attempts(
                Some("zellij 0.45.1"),
                &mut stderr,
                || {
                    spawns += 1;
                    output.write_all(b"child")?;
                    Ok(if spawns == 1 {
                        (exited(1), Duration::from_millis(50))
                    } else {
                        (exited(23), Duration::from_millis(2001))
                    })
                },
                || {
                    observations += 1;
                    RemoteRoomObservation::Live
                },
                |event| events.push(event),
            )
        },
    )
    .unwrap();
    assert_eq!(spawns, 2, "an early refusal needs re-attach");
    assert!(matches!(outcome, AttachOutcome::Exited(status) if status.code() == Some(23)));
    assert_eq!(observations, 2);
    assert_eq!(
        stdout,
        [
            ALTERNATE_SCROLL_SAVE,
            ALTERNATE_SCROLL_DISABLE,
            rimz::remote::tty::ROOM_OPEN_SENTINEL.as_bytes(),
            b"childchild",
            ALTERNATE_SCROLL_RESTORE
        ]
        .concat()
    );
    let notice = String::from_utf8(stderr).unwrap();
    assert_eq!(notice.lines().count(), 1);
    assert!(notice.contains("refused the client right after it started"));
    assert!(notice.contains("before 0.46"));
    assert!(notice.contains("1/2"));
    assert_eq!(events.len(), 1);
    assert_eq!(
        serde_json::to_value(&events[0]).unwrap(),
        serde_json::json!({
            "kind": "client_reattached", "attempt": 1, "exit_code": 1,
            "elapsed_ms": 50, "zellij_version": "zellij 0.45.1",
        })
    );
}

#[cfg(unix)]
#[test]
fn early_exits_exhaust_two_reattaches_and_preserve_the_last_status() {
    let mut stderr = Vec::new();
    let mut spawns = 0;
    let mut observations = 0;
    let mut events = Vec::new();
    let outcome = run_attach_attempts(
        Some("zellij 0.45.1"),
        &mut stderr,
        || {
            spawns += 1;
            Ok((exited(spawns), Duration::from_millis(50)))
        },
        || {
            observations += 1;
            RemoteRoomObservation::Live
        },
        |event| events.push(event),
    )
    .unwrap();
    assert_eq!(spawns, 3, "the retry bound is two, not zero or unlimited");
    assert!(matches!(outcome, AttachOutcome::Exited(status) if status.code() == Some(3)));
    assert_eq!(observations, 5);
    let notices = String::from_utf8(stderr).unwrap();
    assert_eq!(notices.lines().count(), 3);
    assert!(notices.contains("2/2"));
    assert!(notices.contains("still running"));
    assert!(notices.contains("rimz attach"));
    assert_eq!(events.len(), 2);
    assert_eq!(serde_json::to_value(&events[1]).unwrap()["attempt"], 2);
}

#[cfg(unix)]
#[test]
fn attach_does_not_retry_outside_the_early_live_hazard() {
    for (version, elapsed, observation, reads) in [
        (Some("zellij 0.45.1"), 2001, RemoteRoomObservation::Live, 0),
        (Some("zellij 0.45.1"), 2000, RemoteRoomObservation::Live, 0),
        (Some("zellij 0.45.1"), 50, RemoteRoomObservation::Gone, 1),
        (Some("zellij 0.45.1"), 50, RemoteRoomObservation::Unknown, 1),
        (None, 50, RemoteRoomObservation::Live, 0),
    ] {
        let mut spawns = 0;
        let mut observations = 0;
        let mut stderr = Vec::new();
        let outcome = run_attach_attempts(
            version,
            &mut stderr,
            || {
                spawns += 1;
                Ok((exited(7), Duration::from_millis(elapsed)))
            },
            || {
                observations += 1;
                observation
            },
            |_| panic!("no re-attach diagnostic without a re-attach"),
        )
        .unwrap();
        assert_eq!(spawns, 1);
        assert!(matches!(outcome, AttachOutcome::Exited(status) if status.code() == Some(7)));
        assert_eq!(observations, reads);
        assert!(stderr.is_empty());
    }
}

#[test]
fn remote_room_ended_never_reattaches() {
    let mut spawns = 0;
    let mut stderr = Vec::new();
    let outcome = run_attach_attempts(
        Some("zellij 0.45.1"),
        &mut stderr,
        || {
            spawns += 1;
            Ok((AttachOutcome::RoomEnded, Duration::from_millis(50)))
        },
        || panic!("RoomEnded must not probe liveness"),
        |_| panic!("RoomEnded must not emit a re-attach diagnostic"),
    )
    .unwrap();
    assert!(matches!(outcome, AttachOutcome::RoomEnded));
    assert_eq!(spawns, 1);
    assert!(stderr.is_empty());
}

#[cfg(unix)]
#[test]
fn early_success_preserves_detach_without_reattaching() {
    let mut spawns = 0;
    let mut observations = 0;
    let mut stderr = Vec::new();
    let mut events = Vec::new();
    let outcome = run_attach_attempts(
        Some("zellij 0.45.1"),
        &mut stderr,
        || {
            spawns += 1;
            Ok((exited(0), Duration::from_millis(50)))
        },
        || {
            observations += 1;
            RemoteRoomObservation::Live
        },
        |event| events.push(event),
    )
    .unwrap();
    assert_eq!(spawns, 1, "a successful detach must not re-attach");
    assert!(matches!(outcome, AttachOutcome::Exited(status) if status.success()));
    assert_eq!(observations, 0);
    assert!(stderr.is_empty());
    assert!(events.is_empty());
}

#[cfg(unix)]
#[test]
fn early_signal_preserves_the_signal_without_reattaching() {
    use std::os::unix::process::ExitStatusExt;
    let mut spawns = 0;
    let mut observations = 0;
    let mut stderr = Vec::new();
    let mut events = Vec::new();
    let outcome = run_attach_attempts(
        Some("zellij 0.45.1"),
        &mut stderr,
        || {
            spawns += 1;
            Ok((
                AttachOutcome::Exited(std::process::ExitStatus::from_raw(15)),
                Duration::from_millis(50),
            ))
        },
        || {
            observations += 1;
            RemoteRoomObservation::Live
        },
        |event| events.push(event),
    )
    .unwrap();
    assert_eq!(spawns, 1, "a signal exit must not re-attach");
    assert!(matches!(outcome, AttachOutcome::Exited(status) if status.signal() == Some(15)));
    assert_eq!(observations, 0);
    assert!(stderr.is_empty());
    assert!(events.is_empty());
}

#[cfg(unix)]
#[test]
fn reattach_rechecks_liveness_after_diagnostic_io() {
    let live = std::cell::Cell::new(true);
    let mut spawns = 0;
    let mut observations = 0;
    let mut stderr = Vec::new();
    let outcome = run_attach_attempts(
        Some("zellij 0.45.1"),
        &mut stderr,
        || {
            spawns += 1;
            Ok((exited(4), Duration::from_millis(50)))
        },
        || {
            observations += 1;
            if live.get() {
                RemoteRoomObservation::Live
            } else {
                RemoteRoomObservation::Gone
            }
        },
        |_| live.set(false),
    )
    .unwrap();
    assert_eq!(observations, 2, "diagnostic I/O can outlive the room");
    assert_eq!(spawns, 1);
    assert!(
        stderr.is_empty(),
        "a cancelled attempt does not announce itself"
    );
    assert!(matches!(outcome, AttachOutcome::Exited(status) if status.code() == Some(4)));
}

#[test]
fn attach_action_matrix_and_nested_report_policy() {
    for (stdin, stdout, inside) in [
        (true, true, false),
        (false, true, false),
        (true, false, false),
        (false, false, false),
        (true, true, true),
        (false, false, true),
    ] {
        assert_eq!(
            attach_action(AttachMode::Attach, stdin, stdout, inside),
            AttachAction::Launch
        );
        assert_eq!(
            attach_action(AttachMode::Print, stdin, stdout, inside),
            AttachAction::Print(PrintCause::Explicit)
        );
    }
    assert_eq!(
        attach_action(AttachMode::Auto, true, true, false),
        AttachAction::Launch,
    );
    assert_eq!(
        attach_action(AttachMode::Auto, false, true, false),
        AttachAction::Print(PrintCause::NoTerminal),
    );
    assert_eq!(
        attach_action(AttachMode::Auto, true, false, false),
        AttachAction::Print(PrintCause::NoTerminal),
    );
    assert_eq!(
        attach_action(AttachMode::Auto, true, true, true),
        AttachAction::Print(PrintCause::InsideMux),
    );
    assert_eq!(
        attach_action(AttachMode::Attach, false, false, true),
        AttachAction::Launch,
    );
    assert_eq!(
        attach_action(AttachMode::Print, true, true, false),
        AttachAction::Print(PrintCause::Explicit),
    );

    assert!(should_report_already_inside(AttachMode::Auto, true));
    assert!(!should_report_already_inside(AttachMode::Auto, false));
    assert!(!should_report_already_inside(AttachMode::Print, true));
    assert!(!should_report_already_inside(AttachMode::Attach, true));
}

#[test]
fn room_open_sentinel_requires_a_nonempty_supervised_request_not_a_tty() {
    let spec = rimz::mux::CommandSpec::new("mux");
    for supervised in [None, Some(OsStr::new("")), Some(OsStr::new("1"))] {
        let mut output = Vec::new();
        run_attach_command(&spec, &mut output, false, supervised, true, |stdout| {
            stdout.write_all(b"child")?;
            Ok(AttachOutcome::RoomEnded)
        })
        .expect("run piped attach");
        let expected: &[u8] = if supervised.is_some_and(|value| !value.is_empty()) {
            b"\x1b]7717;rimz-room-open\x07child"
        } else {
            b"child"
        };
        assert_eq!(output, expected, "supervised={supervised:?}");
    }
}

#[test]
fn outer_scroll_marker_does_not_reach_the_multiplexer_environment() {
    let command = attach_command(&rimz::mux::CommandSpec::new("mux"));

    assert!(command.get_envs().any(|(key, value)| {
        key == OsStr::new(rimz::remote::OUTER_SCROLL_BRACKET_ENV) && value.is_none()
    }));
}

#[test]
fn outer_scroll_marker_suppresses_a_nested_bracket() {
    assert!(alternate_scroll_bracket_enabled(None));
    assert!(alternate_scroll_bracket_enabled(Some(OsStr::new(""))));
    assert!(!alternate_scroll_bracket_enabled(Some(OsStr::new("1"))));
}

#[test]
fn alternate_scroll_sequences_are_exact() {
    assert_eq!(ALTERNATE_SCROLL_SAVE, b"\x1b[?1007s");
    assert_eq!(ALTERNATE_SCROLL_DISABLE, b"\x1b[?1007l");
    assert_eq!(ALTERNATE_SCROLL_RESTORE, b"\x1b[?1007r");
}

#[test]
fn remote_session_loss_translates_only_missing_sessions() {
    assert!(remote_session_lost_exit(RemoteRoomObservation::Gone));
    assert!(!remote_session_lost_exit(RemoteRoomObservation::Live));
    assert!(!remote_session_lost_exit(RemoteRoomObservation::Unknown));
}

#[test]
fn remote_room_watchdog_interval_accepts_only_positive_milliseconds() {
    assert_eq!(
        remote_room_watchdog_interval(Some("25")),
        Duration::from_millis(25)
    );
    for raw in [None, Some("0"), Some("invalid")] {
        assert_eq!(
            remote_room_watchdog_interval(raw),
            REMOTE_ROOM_WATCHDOG_INTERVAL,
            "{raw:?}"
        );
    }
}

#[test]
fn remote_room_watchdog_uses_a_custom_interval() {
    let interval = Duration::from_millis(25);
    let mut watchdog = RemoteRoomWatchdog {
        interval,
        ..RemoteRoomWatchdog::default()
    };

    assert_eq!(watchdog.next_observation_in(Duration::ZERO), Duration::ZERO);
    assert!(!watchdog.observe(Duration::ZERO, RemoteRoomObservation::Live));
    assert_eq!(watchdog.next_observation_in(Duration::ZERO), interval);
    assert_eq!(watchdog.next_observation_in(interval), Duration::ZERO);
    assert!(watchdog.observe(interval, RemoteRoomObservation::Gone));
}

#[test]
fn remote_room_watchdog_arms_on_live_and_fires_on_gone() {
    let mut watchdog = RemoteRoomWatchdog::default();

    assert_eq!(watchdog.next_observation_in(Duration::ZERO), Duration::ZERO);
    assert!(!watchdog.observe(Duration::ZERO, RemoteRoomObservation::Live));
    assert_eq!(
        watchdog.next_observation_in(Duration::ZERO),
        REMOTE_ROOM_WATCHDOG_INTERVAL
    );
    assert_eq!(
        watchdog.next_observation_in(REMOTE_ROOM_WATCHDOG_INTERVAL),
        Duration::ZERO
    );
    assert!(watchdog.observe(REMOTE_ROOM_WATCHDOG_INTERVAL, RemoteRoomObservation::Gone,));
}

#[test]
fn remote_room_watchdog_fails_open_until_live_is_observed() {
    let mut watchdog = RemoteRoomWatchdog::default();

    assert!(!watchdog.observe(Duration::ZERO, RemoteRoomObservation::Unknown));
    assert!(!watchdog.observe(REMOTE_ROOM_WATCHDOG_INTERVAL, RemoteRoomObservation::Gone,));
}

#[cfg(unix)]
#[test]
fn attach_brackets_child_output_and_preserves_its_failure() {
    use std::os::unix::process::ExitStatusExt;

    struct FlushedOutput<'a> {
        bytes: Vec<u8>,
        flushes: &'a std::cell::Cell<usize>,
    }

    impl Write for FlushedOutput<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes.write(bytes)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.flushes.set(self.flushes.get() + 1);
            Ok(())
        }
    }

    let spec = rimz::mux::CommandSpec::new("sh").args(["-c", "printf child; exit 23"]);
    let flushes = std::cell::Cell::new(0);
    let mut output = FlushedOutput {
        bytes: Vec::new(),
        flushes: &flushes,
    };
    let status = run_attach_command(
        &spec,
        &mut output,
        true,
        Some(OsStr::new("1")),
        true,
        |stdout| {
            assert_eq!(
                flushes.get(),
                3,
                "scroll bracket and sentinel flush before mux launch"
            );
            stdout.write_all(b"child")?;
            Ok(AttachOutcome::Exited(std::process::ExitStatus::from_raw(
                23 << 8,
            )))
        },
    )
    .expect("child status");

    assert_eq!(
        output.bytes,
        [
            ALTERNATE_SCROLL_SAVE,
            ALTERNATE_SCROLL_DISABLE,
            b"\x1b]7717;rimz-room-open\x07",
            b"child",
            ALTERNATE_SCROLL_RESTORE,
        ]
        .concat(),
    );
    let AttachOutcome::Exited(status) = status else {
        panic!("expected child exit")
    };
    assert_eq!(status.code(), Some(23));
}

#[test]
fn attach_restores_terminal_bracket_when_the_room_ends() {
    let spec = rimz::mux::CommandSpec::new("mux");
    let mut output = Vec::new();
    let outcome = run_attach_command(&spec, &mut output, true, None, true, |_| {
        Ok(AttachOutcome::RoomEnded)
    })
    .expect("room-ended outcome");

    assert!(matches!(outcome, AttachOutcome::RoomEnded));
    assert_eq!(
        output,
        [
            ALTERNATE_SCROLL_SAVE,
            ALTERNATE_SCROLL_DISABLE,
            ALTERNATE_SCROLL_RESTORE,
        ]
        .concat(),
    );
}

#[test]
fn attach_restores_after_launch_error_and_skips_non_terminal_sequences() {
    let spec = rimz::mux::CommandSpec::new("mux");
    let mut terminal_output = Vec::new();
    let result = run_attach_command(&spec, &mut terminal_output, true, None, true, |_| {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "missing mux",
        ))
    });
    assert_eq!(
        terminal_output,
        [
            ALTERNATE_SCROLL_SAVE,
            ALTERNATE_SCROLL_DISABLE,
            ALTERNATE_SCROLL_RESTORE,
        ]
        .concat(),
    );
    assert!(result.unwrap_err().to_string().contains("running `mux`"));

    let mut piped_output = Vec::new();
    let result = run_attach_command(&spec, &mut piped_output, false, None, true, |stdout| {
        stdout.write_all(b"child")?;
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "missing mux",
        ))
    });
    assert_eq!(piped_output, b"child");
    assert!(result.unwrap_err().to_string().contains("running `mux`"));

    let mut nested_output = Vec::new();
    let result = run_attach_command(&spec, &mut nested_output, true, None, false, |stdout| {
        stdout.write_all(b"child")?;
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "missing mux",
        ))
    });
    assert_eq!(nested_output, b"child");
    assert!(result.unwrap_err().to_string().contains("running `mux`"));
}
