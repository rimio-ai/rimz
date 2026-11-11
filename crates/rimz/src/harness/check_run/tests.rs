use std::collections::BTreeMap;
use std::fs::File;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::*;

#[test]
fn run_check_captures_output_status_and_timeout() {
    let dir = tempfile::tempdir().expect("tempdir");
    let check = |cmd: &str, timeout| {
        run_check(
            dir.path(),
            cmd,
            timeout,
            CheckEcho::Capture,
            &BTreeMap::new(),
        )
        .expect("check ran")
    };

    let passed = check("printf out; printf err >&2", Duration::from_secs(1));
    assert!(passed.passed);
    assert_eq!(passed.code, Some(0));
    assert!(passed.output.contains("out"), "stdout is captured");
    assert!(passed.output.contains("err"), "stderr is captured too");
    assert_eq!(passed.stderr_line(), "err");

    let failed = check("printf nope; exit 1", Duration::from_secs(1));
    assert!(!failed.passed);
    assert!(!failed.timed_out);
    assert_eq!(failed.code, Some(1));
    assert!(failed.output.contains("nope"));

    let expired = check("(sleep 1; printf leaked) & wait", Duration::from_millis(50));
    assert!(!expired.passed);
    assert!(expired.timed_out);
    assert!(
        expired.output.is_empty(),
        "timed-out descendants cannot keep writing to the check pipes"
    );
    let orphan = check(
        "( (sleep 1; printf orphaned) & ); sleep 30",
        Duration::from_millis(50),
    );
    assert!(orphan.timed_out);
    assert!(
        orphan.output.is_empty(),
        "a reparented pipe holder cannot prevent timeout recording"
    );
}

#[test]
fn hosted_check_publishes_its_process_group_and_tees_both_streams() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("check.log");
    let mut published = None;
    let output = run_command(
        dir.path(),
        "printf out; printf err >&2; exit 7",
        WatchDeadline::Hosted {
            argv: None,
            interrupted: &|| false,
            spawned: &mut |pid| {
                assert_eq!(
                    nix::unistd::getpgid(Some(Pid::from_raw(pid as i32)))?.as_raw(),
                    pid as i32
                );
                published = Some(pid);
                Ok(())
            },
        },
        CheckEcho::Tee {
            file: File::create(&path).unwrap(),
        },
        &BTreeMap::new(),
        |_, _| panic!("hosted checks have no check-in"),
        &mut None,
    );
    assert!(
        output.is_ok(),
        "hosted checks must run and publish their PID"
    );
    let output = output.unwrap();
    assert!(published.is_some());
    assert_eq!(output.code, Some(7));
    assert!(!output.passed);
    assert!(!output.timed_out);
    assert!(!output.interrupted);
    let log = std::fs::read_to_string(path).unwrap();
    assert!(log.contains("out") && log.contains("err"), "{log}");
}

#[test]
fn hosted_check_argv_preserves_literal_words_without_a_shell() {
    let dir = tempfile::tempdir().unwrap();
    let argv: Vec<std::ffi::OsString> = [
        "sh",
        "-c",
        "printf '%s\\n' \"$@\"; test ! -t 0",
        "script",
        "two words",
        "$HOME",
        "*",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    let output = run_command(
        dir.path(),
        "printf shell-was-used",
        WatchDeadline::Hosted {
            argv: Some(&argv),
            interrupted: &|| false,
            spawned: &mut |_| Ok(()),
        },
        CheckEcho::Capture,
        &BTreeMap::new(),
        |_, _| panic!("hosted commands have no check-in"),
        &mut None,
    )
    .unwrap();
    assert_eq!(output.output, "two words\n$HOME\n*\n");
    assert!(output.passed, "{}", output.output);
}

#[test]
fn hosted_check_keeps_an_exited_childs_status_when_the_interrupt_is_due() {
    let dir = tempfile::tempdir().unwrap();
    let output = run_command(
        dir.path(),
        "exec >/dev/null 2>&1; sleep 0.1; exit 7",
        WatchDeadline::Hosted {
            argv: None,
            interrupted: &|| true,
            spawned: &mut |pid| {
                let deadline = Instant::now() + Duration::from_secs(5);
                while crate::proc::process_is_live(pid, None) {
                    assert!(Instant::now() < deadline, "child {pid} did not exit");
                    std::thread::sleep(CHECK_POLL_INTERVAL);
                }
                Ok(())
            },
        },
        CheckEcho::Capture,
        &BTreeMap::new(),
        |_, _| panic!("hosted commands have no check-in"),
        &mut None,
    )
    .unwrap();
    assert_eq!(output.code, Some(7));
    assert!(!output.interrupted, "an exited child was not interrupted");
}

#[test]
fn hosted_check_interrupt_kills_the_group_without_a_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let polls = std::cell::Cell::new(0);
    let mut published = None;
    let output = run_command(
        dir.path(),
        "sleep 60 & echo $! > descendant.pid; wait",
        WatchDeadline::Hosted {
            argv: None,
            interrupted: &|| {
                polls.set(polls.get() + 1);
                polls.get() >= 3
                    && std::fs::read_to_string(dir.path().join("descendant.pid"))
                        .ok()
                        .is_some_and(|pid| pid.trim().parse::<u32>().is_ok())
            },
            spawned: &mut |pid| {
                published = Some(pid);
                Ok(())
            },
        },
        CheckEcho::Capture,
        &BTreeMap::new(),
        |_, _| panic!("hosted checks have no check-in"),
        &mut None,
    );
    assert!(output.is_ok(), "hosted interruption must return an outcome");
    let output = output.unwrap();
    assert!(output.interrupted);
    assert!(!output.timed_out);
    assert!(!output.passed);
    assert!(!crate::proc::process_is_live(published.unwrap(), None));
    let descendant = std::fs::read_to_string(dir.path().join("descendant.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        wait_for_process_exit(descendant, Duration::from_secs(2)),
        "descendant {descendant} outlived the group kill"
    );
}

#[test]
fn hosted_check_interrupts_pipe_holders_after_the_shell_exits() {
    let dir = tempfile::tempdir().unwrap();
    let polls = std::cell::Cell::new(0);
    let output = run_command(
        dir.path(),
        "sleep 60 & echo $! > descendant.pid",
        WatchDeadline::Hosted {
            argv: None,
            interrupted: &|| {
                polls.set(polls.get() + 1);
                polls.get() >= 3
                    && std::fs::read_to_string(dir.path().join("descendant.pid"))
                        .ok()
                        .is_some_and(|pid| pid.trim().parse::<u32>().is_ok())
            },
            spawned: &mut |_| Ok(()),
        },
        CheckEcho::Capture,
        &BTreeMap::new(),
        |_, _| panic!("hosted checks have no check-in"),
        &mut None,
    );
    assert!(
        output.is_ok(),
        "hosted checks must keep polling while draining"
    );
    let output = output.unwrap();
    assert!(output.interrupted);
    assert!(!output.passed);
    let descendant = std::fs::read_to_string(dir.path().join("descendant.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        wait_for_process_exit(descendant, Duration::from_secs(2)),
        "descendant {descendant} outlived the group kill"
    );
}

#[test]
fn hosted_check_publication_error_kills_the_group_and_returns_the_error() {
    let dir = tempfile::tempdir().unwrap();
    let mut published = None;
    let output = run_command(
        dir.path(),
        "sleep 60 & echo $! > descendant.pid; wait",
        WatchDeadline::Hosted {
            argv: None,
            interrupted: &|| false,
            spawned: &mut |pid| {
                published = Some(pid);
                let deadline = Instant::now() + Duration::from_secs(5);
                while !std::fs::read_to_string(dir.path().join("descendant.pid"))
                    .ok()
                    .is_some_and(|pid| pid.trim().parse::<u32>().is_ok())
                {
                    assert!(Instant::now() < deadline, "child did not start");
                    std::thread::sleep(CHECK_POLL_INTERVAL);
                }
                Err(std::io::Error::other("publication refused"))
            },
        },
        CheckEcho::Capture,
        &BTreeMap::new(),
        |_, _| panic!("hosted checks have no check-in"),
        &mut None,
    );
    assert!(
        published.is_some(),
        "hosted checks must call the publication hook"
    );
    assert!(output.is_err_and(|error| format!("{error:#}").contains("publication refused")));
    assert!(!crate::proc::process_is_live(published.unwrap(), None));
    let descendant = std::fs::read_to_string(dir.path().join("descendant.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        wait_for_process_exit(descendant, Duration::from_secs(2)),
        "descendant {descendant} outlived the group kill"
    );
}

#[test]
fn polled_watch_probe_runs_to_exit_without_checkin() {
    let dir = tempfile::tempdir().unwrap();
    let output = run_command(
        dir.path(),
        "printf probe; exit 3",
        WatchDeadline::Watch(None),
        CheckEcho::Capture,
        &BTreeMap::new(),
        |_, _| panic!("a probe never checks in"),
        &mut None,
    )
    .unwrap();
    assert_eq!(output.code, Some(3));
    assert_eq!(output.output, "probe");
    assert!(!output.timed_out);
}

#[test]
fn watch_exit_during_checkin_delivery_is_not_lost() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("watch.log");
    let mut notices = 0;
    let output = run_command(
        dir.path(),
        "printf interim; printf '%s' \"$$\" > command.pid; while [ ! -e release ]; do sleep 0.01; done; printf final; exit 3",
        WatchDeadline::Watch(Some(Duration::from_millis(100))),
        CheckEcho::Tee { file: File::create(&path).unwrap() },
        &BTreeMap::new(),
        |elapsed_ms, tail| {
            notices += 1;
            assert!(elapsed_ms >= 100);
            assert_eq!(tail, "interim");
            std::fs::write(dir.path().join("release"), "").unwrap();
            let pid = std::fs::read_to_string(dir.path().join("command.pid")).unwrap().parse().unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while crate::proc::process_is_live(pid, None) {
                assert!(Instant::now() < deadline, "command did not exit during notice");
                std::thread::sleep(Duration::from_millis(10));
            }
        },
        &mut None,
    ).unwrap();
    assert_eq!(notices, 1);
    assert_eq!(output.code, Some(3));
    assert!(!output.timed_out);
    assert_eq!(output.output, "interimfinal");
    assert_eq!(std::fs::read_to_string(path).unwrap(), output.output);
}

#[test]
fn watched_check_keeps_full_output_and_a_bounded_tail() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("watch.log");
    let output = run_check(
        dir.path(),
        "seq 1 5000; printf stderr >&2",
        Duration::from_secs(5),
        CheckEcho::Tee {
            file: File::create(&path).unwrap(),
        },
        &BTreeMap::new(),
    )
    .unwrap();
    let full = std::fs::read_to_string(path).unwrap();
    assert!(output.passed());
    assert!(full.contains("1\n2\n3\n"));
    assert!(full.contains("4999\n5000\n"));
    assert!(full.contains("stderr"));
    assert!(full.len() > WAIT_TAIL_CAP);
    assert!(output.output.len() <= WAIT_TAIL_CAP);
    assert!(full.ends_with(&output.output));
    assert!(output.output.contains("5000"));
}

#[test]
fn check_capture_bounds_chatty_chunks_before_decoding() {
    let mut capture = CheckCapture {
        file: None,
        tail: Vec::new(),
        cap: WAIT_TAIL_CAP,
    };
    for chunk in [
        vec![b'a'; WAIT_TAIL_CAP * 3],
        vec![b'b'; 17],
        vec![b'c'; WAIT_TAIL_CAP],
    ] {
        capture.push(&chunk).unwrap();
        assert!(capture.tail.len() <= WAIT_TAIL_CAP);
    }
    assert_eq!(capture.tail, vec![b'c'; WAIT_TAIL_CAP]);
}

#[test]
fn capacity_first_stderr_line_truncates_at_a_character_boundary() {
    let line = format!("{}é rest\n", "x".repeat(1023));
    let capture = Arc::new(Mutex::new(CheckCapture {
        file: None,
        tail: Vec::new(),
        cap: CHECK_OUTPUT_CAP,
    }));
    let first = drain_pipe(Some(std::io::Cursor::new(line.into_bytes())), capture, None)
        .join()
        .unwrap()
        .unwrap();
    assert_eq!(first, "x".repeat(1023));
}

#[test]
fn pipe_forward_buffers_partial_lines_and_terminates_the_tail() {
    let mut pending = b"first".to_vec();
    assert_eq!(take_complete_line(&mut pending), None);

    pending.extend_from_slice(b" line\nsecond");
    assert_eq!(
        take_complete_line(&mut pending),
        Some(b"first line\n".to_vec())
    );
    assert_eq!(pending, b"second");
    assert_eq!(take_trailing_line(&mut pending), Some(b"second\n".to_vec()));
    assert!(pending.is_empty());
}

/// Whether `pid` exits within `grace`, polled as the scheduler's stop ladder polls a holder.
fn wait_for_process_exit(pid: u32, grace: Duration) -> bool {
    let deadline = Instant::now() + grace;
    while crate::proc::process_is_live(pid, None) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10).min(remaining));
    }
    true
}
