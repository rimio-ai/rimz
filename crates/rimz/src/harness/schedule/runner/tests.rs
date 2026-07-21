use std::path::Path;

use super::*;

#[test]
fn spawn_timeout_prefers_task_then_config_then_builtin() {
    use LoopRunMode::{Manual, Scheduled};
    let task = Duration::from_secs(30);
    let configured = Duration::from_secs(60);

    assert_eq!(
        effective_spawn_timeout(Scheduled, Some(task), Some(configured)),
        Some(task),
        "task timeout outranks config"
    );
    assert_eq!(
        effective_spawn_timeout(Scheduled, None, Some(configured)),
        Some(configured),
        "config timeout applies when the task is silent"
    );
    assert_eq!(
        effective_spawn_timeout(Scheduled, None, None),
        Some(SCHEDULED_RUN_DEFAULT_TIMEOUT),
        "scheduled runs always carry a deadline"
    );
    assert_eq!(
        effective_spawn_timeout(Manual, None, Some(configured)),
        None,
        "manual runs stay untimed even with a configured default"
    );
}

#[test]
fn budget_refusal_finishes_as_a_recorded_gate() {
    let check = CheckRecord {
        code: Some(1),
        timed_out: false,
        output: "guard failed".to_owned(),
    };
    let mut record = LoopRunRecord::new(
        "nightly",
        LoopRunResult::Completed,
        LoopRunMode::Scheduled,
        12,
    );

    let (presentation, notice) = finish_spawn_effect(
        &mut record,
        SupervisedRunOutcome::BudgetExceeded {
            reason: "room budget reached".to_owned(),
        },
        Some(check.clone()),
        true,
    );

    assert_eq!(record.result, LoopRunResult::BudgetSkipped);
    assert_eq!(
        record.check.as_ref(),
        Some(&check),
        "the guard record survives the refusal"
    );
    assert_eq!(record.error.as_deref(), Some("room budget reached"));
    assert_eq!(presentation, LoopRunPresentation::default());
    assert!(matches!(
        notice,
        TaskFireNotice::Gate { reason } if reason == "room budget reached"
    ));
}

#[test]
fn deadline_expiry_and_relative_age_use_injected_time() {
    let now = Timestamp::from_second(200_000).expect("now");
    let expired = TaskEntry {
        deadline: Some(Timestamp::from_second(199_999).expect("deadline")),
        ..TaskEntry::default()
    };

    assert!(deadline_expired_at(&expired, now));
    assert_eq!(
        relative_age(Timestamp::from_second(198_500).expect("started"), now),
        "25m ago"
    );
}

fn surplus_entry(surplus: Option<&str>, surplus_after: Option<&str>) -> TaskEntry {
    TaskEntry {
        surplus: surplus.map(ToOwned::to_owned),
        surplus_after: surplus_after.map(ToOwned::to_owned),
        ..TaskEntry::default()
    }
}

fn reading(elapsed_days: i64, headroom: f64) -> WindowSurplus {
    WindowSurplus {
        duration_mins: 7 * 24 * 60,
        elapsed: jiff::SignedDuration::from_secs(elapsed_days * 86_400),
        headroom,
    }
}

#[test]
fn surplus_gate_covers_every_branch() {
    // Reason `surplus`/`surplus-after` gave for holding a fire back, if any.
    let gate = |surplus, after, reading| {
        surplus_gate_in(&surplus_entry(surplus, after), "claude", reading)
    };

    assert_eq!(
        surplus_gate_in(&TaskEntry::default(), "claude", None),
        None,
        "an ungated task never consults the window"
    );
    assert_eq!(
        gate(Some("1.5x"), None, None).as_deref(),
        Some("no claude budget-window reading; surplus gate stays closed"),
        "a gate with no reading fails closed"
    );
    assert_eq!(
        gate(Some("1.5x"), Some("3d"), Some(reading(2, 2.0))).as_deref(),
        Some("claude 7d window 2d elapsed; fires after 3d"),
        "ample headroom still waits for the elapsed floor"
    );
    assert_eq!(
        gate(Some("1.5x"), Some("3d"), Some(reading(4, 1.4))).as_deref(),
        Some("claude 7d window surplus 1.4x below 1.5x")
    );
    assert_eq!(
        gate(Some("1.5x"), Some("3d"), Some(reading(4, 1.5))),
        None,
        "headroom exactly at the threshold fires"
    );
    assert_eq!(
        surplus_gate_in(
            &surplus_entry(None, Some("3d")),
            "codex",
            Some(reading(4, 0.9))
        )
        .as_deref(),
        Some("codex 7d window surplus 0.9x below 1.0x"),
        "a bare elapsed floor still demands sustainable headroom"
    );
}

#[test]
fn run_lock_reports_holder_metadata_and_accepts_empty_legacy_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let open = |path: &Path, create: bool| {
        std::fs::OpenOptions::new()
            .create(create)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .expect("open lock")
    };

    let missing_path = dir.path().join("missing.lock");
    assert!(matches!(
        probe_run_lock_path(&missing_path).expect("probe missing lock"),
        RunLockState::Available
    ));
    assert!(
        !missing_path.exists(),
        "probing should not create a lock file"
    );

    let path = dir.path().join("task.lock");
    let guard = match acquire_run_lock_file(open(&path, true), &path).expect("acquire lock") {
        RunLockAttempt::Acquired(guard) => guard,
        RunLockAttempt::Held(_) => panic!("fresh lock should be acquired"),
    };
    let written: RunLockInfo =
        serde_json::from_slice(&std::fs::read(&path).expect("read lock")).expect("parse lock info");
    assert_eq!(written.pid, std::process::id());

    match acquire_run_lock_file(open(&path, false), &path).expect("contend for lock") {
        RunLockAttempt::Held(Some(info)) => assert_eq!(info, written),
        RunLockAttempt::Held(None) => panic!("holder metadata should be readable"),
        RunLockAttempt::Acquired(_) => panic!("held lock should reject contender"),
    }

    drop(guard);
    let before_probe = std::fs::read(&path).expect("read lock before probe");
    assert!(matches!(
        probe_run_lock_file(open(&path, false), &path).expect("probe available lock"),
        RunLockState::Available
    ));
    assert_eq!(
        std::fs::read(&path).expect("read lock after probe"),
        before_probe,
        "probing an available lock should not rewrite its metadata"
    );

    let empty_path = dir.path().join("legacy.lock");
    let holder = open(&empty_path, true);
    holder.try_lock().expect("hold empty lock");
    assert!(
        matches!(
            probe_run_lock_file(open(&empty_path, false), &empty_path).expect("probe empty lock"),
            RunLockState::Held(None)
        ),
        "a held lock with no metadata is still held"
    );
}

#[test]
fn wait_for_run_lock_release_observes_guard_drop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("task.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .expect("open lock");
    let guard = match acquire_run_lock_file(file, &path).expect("acquire lock") {
        RunLockAttempt::Acquired(guard) => guard,
        RunLockAttempt::Held(_) => panic!("fresh lock should be acquired"),
    };
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        drop(guard);
    });

    assert!(
        wait_for_run_lock_release_path(&path, Duration::from_secs(1)).expect("wait for release")
    );
    releaser.join().expect("release thread");
}

#[test]
fn stop_ladder_cancels_then_signals_then_reports_manual_recovery() {
    let info = RunLockInfo {
        pid: 42,
        started_at: Timestamp::from_second(1).expect("timestamp"),
    };
    let held = RunLockState::Held(Some(info));

    assert_eq!(
        next_stop_action(&RunLockState::Available, true, false, false),
        StopAction::Done
    );
    assert_eq!(
        next_stop_action(&held, true, false, false),
        StopAction::CancelRun
    );
    assert_eq!(
        next_stop_action(&held, true, true, false),
        StopAction::Signal(info)
    );
    assert_eq!(
        next_stop_action(&held, true, true, true),
        StopAction::Manual
    );
    assert_eq!(
        next_stop_action(&RunLockState::Held(None), false, false, false),
        StopAction::Manual,
        "an unidentifiable holder can only be cleared by hand"
    );
}

#[test]
fn check_polarity_truth_table() {
    let outcome = |passed, timed_out, code| CheckOutcome {
        passed,
        timed_out,
        output: String::new(),
        code,
    };
    let passed = outcome(true, false, Some(0));
    let failed = outcome(false, false, Some(1));
    let timed_out = outcome(false, true, None);

    assert!(!polarity_fires(Some(CheckOn::Fail), &passed));
    assert!(polarity_fires(Some(CheckOn::Fail), &failed));
    assert!(polarity_fires(Some(CheckOn::Fail), &timed_out));
    assert!(polarity_fires(Some(CheckOn::Success), &passed));
    assert!(!polarity_fires(Some(CheckOn::Success), &failed));
    assert!(!polarity_fires(Some(CheckOn::Success), &timed_out));
}

#[test]
fn run_check_captures_output_status_and_timeout() {
    let dir = tempfile::tempdir().expect("tempdir");
    let check = |cmd: &str, timeout| {
        run_check(dir.path(), cmd, timeout, CheckEcho::Capture).expect("check ran")
    };

    let passed = check("printf out; printf err >&2", Duration::from_secs(1));
    assert!(passed.passed);
    assert_eq!(passed.code, Some(0));
    assert!(passed.output.contains("out"), "stdout is captured");
    assert!(passed.output.contains("err"), "stderr is captured too");

    let failed = check("printf nope; exit 1", Duration::from_secs(1));
    assert!(!failed.passed);
    assert!(!failed.timed_out);
    assert_eq!(failed.code, Some(1));
    assert!(failed.output.contains("nope"));

    let expired = check("sleep 1", Duration::from_millis(50));
    assert!(!expired.passed);
    assert!(expired.timed_out);
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
