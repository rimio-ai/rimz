use std::collections::BTreeMap;
use std::path::Path;

use super::*;
use crate::agents::account::{RateLimitCacheEntry, RateLimitsCache};
use crate::agents::context::WindowSource;
use crate::agents::{
    AgentRateLimits, ProviderAccountBinding, ProviderAccountScope, RateLimitWindow,
};

fn runtime_in(dir: &Path) -> RuntimePaths {
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(dir), dir).expect("runtime paths");
    runtime.ensure_dirs().expect("runtime dirs");
    runtime
}

/// Publish `windows` as `kind`'s shared rate-limit cache, carrying whatever
/// account scope `entry` binds them to.
fn write_rate_limits(
    runtime: &RuntimePaths,
    kind: &str,
    entry: RateLimitCacheEntry,
    windows: Vec<RateLimitWindow>,
) {
    let cache = RateLimitsCache {
        entries: BTreeMap::from([(
            kind.to_owned(),
            RateLimitCacheEntry {
                limits: AgentRateLimits { windows },
                ..entry
            },
        )]),
        ..Default::default()
    };
    crate::store::atomic::write_temp_then_rename_cache(&runtime.shared_rate_limits_path(), &cache)
        .expect("rate-limit cache");
}

fn window(duration_mins: u32, resets_at: Timestamp) -> RateLimitWindow {
    RateLimitWindow {
        used_percentage: Some(40),
        resets_at: Some(resets_at),
        duration_mins: Some(duration_mins),
        ..Default::default()
    }
}

fn scope_in(runtime: &RuntimePaths, kind: &str) -> FireScope {
    FireScope::new(
        crate::ids::AgentKind::new_unchecked(kind),
        runtime.clone(),
        None,
    )
}

fn now() -> Timestamp {
    Timestamp::from_second(1_000_000).expect("now")
}

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

/// Ping tasks exist to warm a budget window that is not yet counting down, so
/// the gate closes whenever the window is already running and when the provider
/// stopped enforcing it at all.
#[test]
fn ping_gate_reason_reports_lifted_running_and_cold_windows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime = runtime_in(dir.path());
    let now = now();
    let reset_ping = TaskEntry {
        every: Some("reset".to_owned()),
        ..TaskEntry::default()
    };

    assert_eq!(
        scope_in(&runtime, "claude").ping_gate_reason(&reset_ping, now),
        None,
        "no cached capacity leaves the gate open"
    );

    write_rate_limits(
        &runtime,
        "claude",
        RateLimitCacheEntry::default(),
        vec![RateLimitWindow {
            used_percentage: Some(1),
            ..window(300, now + jiff::SignedDuration::from_secs(300))
        }],
    );
    assert_eq!(
        scope_in(&runtime, "claude")
            .ping_gate_reason(&TaskEntry::default(), now)
            .as_deref(),
        Some("claude budget window already counting down")
    );

    write_rate_limits(
        &runtime,
        "claude",
        RateLimitCacheEntry::default(),
        vec![RateLimitWindow {
            duration_mins: Some(7 * 24 * 60),
            source: WindowSource::Authoritative,
            lifted: true,
            ..Default::default()
        }],
    );
    assert_eq!(
        scope_in(&runtime, "claude")
            .ping_gate_reason(&reset_ping, now)
            .as_deref(),
        Some("claude budget window is not enforced"),
        "a lifted window needs no primer"
    );
}

/// Qwen quota is per Alibaba account, so a ping only trusts a cache it can
/// prove is its own: an unresolved or mismatched binding closes the gate rather
/// than priming a window against someone else's balance.
#[test]
fn ping_gate_refuses_qwen_capacity_it_cannot_attribute() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime = runtime_in(dir.path());
    let now = now();
    let scope = ProviderAccountScope::sub_provider("alibaba", "international");
    write_rate_limits(
        &runtime,
        "qwen",
        RateLimitCacheEntry {
            scope: scope.clone(),
            account_key: Some("owner".to_owned()),
            ..Default::default()
        },
        vec![window(
            7 * 24 * 60,
            now + jiff::SignedDuration::from_secs(300),
        )],
    );
    let reason = |launch| {
        let mut fire_scope = scope_in(&runtime, "qwen");
        fire_scope.managed_launch = launch;
        fire_scope.ping_gate_reason(&TaskEntry::default(), now)
    };
    let bound = |key: &str| {
        ManagedLaunchState::Bound(
            ProviderAccountBinding::new(scope.clone(), key.to_owned()).expect("binding"),
        )
    };

    assert_eq!(
        reason(ManagedLaunchState::Unresolved).as_deref(),
        Some("Qwen launch has no exact Alibaba account binding")
    );
    assert_eq!(
        reason(bound("other")).as_deref(),
        Some("Qwen cached quota belongs to a different Alibaba account")
    );
    assert_eq!(
        reason(bound("owner")).as_deref(),
        Some("qwen budget window already counting down"),
        "a matching binding reads its own running window"
    );
}

#[test]
fn ping_window_outcome_selects_shortest_and_longest_duration() {
    let capacity = ProviderCapacity::from_windows(vec![
        window(10_080, Timestamp::from_second(20_000).unwrap()),
        window(300, Timestamp::from_second(10_000).unwrap()),
    ]);

    let outcome = ping_window_outcome(&capacity).expect("window outcome");
    assert_eq!(outcome.shortest.unwrap().duration_mins, Some(300));
    assert_eq!(outcome.longest.unwrap().duration_mins, Some(10_080));
}

/// A sub-provider window belongs to one account: priming reads its capacity
/// only through a binding that matches, never a neighbour's or an unbound
/// launch. Kinds without sub-provider accounts read shared capacity.
#[test]
fn reset_signal_requires_a_matching_account_binding() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime = runtime_in(dir.path());
    let now = now();
    let reset = now + jiff::SignedDuration::from_secs(2 * 86_400);
    let scope = ProviderAccountScope::sub_provider("alibaba", "international");
    write_rate_limits(
        &runtime,
        "qwen",
        RateLimitCacheEntry {
            scope: scope.clone(),
            account_key: Some("owner".to_owned()),
            ..Default::default()
        },
        vec![window(7 * 24 * 60, reset)],
    );
    let signal_for = |key: &str| {
        let binding = ProviderAccountBinding::new(scope.clone(), key.to_owned()).expect("binding");
        reset_signal_for_binding(&runtime, "qwen", &ManagedLaunchState::Bound(binding), now)
    };
    let unbound = TaskEntry {
        agent: Some("qwen-ping".to_owned()),
        root: dir.path().to_path_buf(),
        worktree: Some("isolated".to_owned()),
        ..TaskEntry::default()
    };

    assert_eq!(
        window_reset_signal_in(&runtime, &unbound, "qwen", now),
        ResetSignal::Unknown,
        "an unbound qwen launch reads no account's cache"
    );
    assert_eq!(
        signal_for("other"),
        ResetSignal::Unknown,
        "a neighbour account's cached quota is not ours"
    );
    assert_eq!(signal_for("owner"), ResetSignal::At(reset));

    let dir = tempfile::tempdir().expect("tempdir");
    let runtime = runtime_in(dir.path());
    let reset = now + jiff::SignedDuration::from_hours(5);
    write_rate_limits(
        &runtime,
        "claude",
        RateLimitCacheEntry::default(),
        vec![window(5 * 60, reset)],
    );
    let claude_ping = TaskEntry {
        agent: Some("claude-ping".to_owned()),
        root: dir.path().to_path_buf(),
        every: Some("reset".to_owned()),
        ..TaskEntry::default()
    };
    assert_eq!(
        window_reset_signal_in(&runtime, &claude_ping, "claude", now),
        ResetSignal::At(reset),
        "kinds without sub-provider accounts read shared capacity"
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
