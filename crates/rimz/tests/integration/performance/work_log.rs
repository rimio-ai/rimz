use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use rimz::agents::{AgentContext, AgentLifecycleObservation, LifecycleSignal};
use rimz::ids::{AgentKind, EventId};
use rimz::store::event::EventEnvelope;
use rimz::store::{agent_context, event_log, ingress::HookIngress};
use serde_json::{Value, json};

use crate::common::Env;

fn records(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).expect("complete work record"))
        .collect()
}

fn command(env: &Env, index: usize) -> Command {
    let mut command = env.rimz();
    command
        .args(["hooks", "apply"])
        .env("RIMZ_WORKSPACE_ID", env.workspace_id.as_str())
        .env("RIMZ_PROJECT_ROOT", &env.project_root)
        .env("RIMZ_AGENT_PID", env.agent_owner_pid().to_string())
        .env("TMUX_PANE", format!("%{}", index + 1))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn apply(env: &Env, index: usize, payload: Value, log: Option<&Path>) -> u32 {
    let mut command = command(env, index);
    if let Some(log) = log {
        command.env("RIMZ_TEST_WORK_LOG", log);
    }
    let frame = ingress(env, index, payload, &command);
    let child = env.spawn_payload(command, &serde_json::to_string(&frame).unwrap());
    let pid = child.id();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    pid
}

/// The frame a hook captured from `command`'s environment.
fn ingress(env: &Env, index: usize, mut payload: Value, command: &Command) -> HookIngress {
    payload["cwd"] = json!(env.project_root);
    payload["session_id"] = json!(format!("work-{index}"));
    let captured: BTreeMap<_, _> = command
        .get_envs()
        .filter_map(|(key, value)| {
            value.map(|value| {
                (
                    key.to_str().unwrap().to_owned(),
                    value.to_str().unwrap().to_owned(),
                )
            })
        })
        .collect();
    HookIngress {
        boot_id: rimz::proc::boot_id(),
        schema_version: "1".into(),
        event_id: EventId::new(),
        ts: Timestamp::now(),
        source: AgentKind::new_unchecked("claude"),
        event: Some(payload["hook_event_name"].as_str().unwrap().into()),
        payload: payload.to_string(),
        cwd: env.project_root.clone(),
        hook_pid: std::process::id(),
        env: captured,
    }
}

/// Queue one Stop per session index, drain them inline, and return the frames and the
/// `hooks apply` rows. Holding the spawn lock keeps a spawned drainer out, so the frames drain in
/// the `--once` process, which has reaped its apply children when it returns.
fn drain_queued(
    env: &Env,
    sessions: &[usize],
    log: &Path,
    drain_env: &[(&str, &str)],
) -> (Vec<HookIngress>, Vec<Value>) {
    let frames: Vec<_> = sessions
        .iter()
        .map(|&index| {
            let mut command = command(env, index);
            command.env("RIMZ_TEST_WORK_LOG", log);
            let payload = json!({"hook_event_name": "Stop", "last_assistant_message": "done"});
            crate::common::append_frame(env, ingress(env, index, payload, &command)).0
        })
        .collect();
    let store = env.store();
    let _spawning =
        rimz::disk::lock::WorkspaceLock::acquire(&store.runtime_paths().hook_drainer_spawn_lock())
            .unwrap();
    let output = env
        .rimz()
        .args(["hooks", "drain", "--project-root"])
        .arg(&env.project_root)
        .arg("--once")
        .env("RIMZ_TEST_HOOK_DRAIN_TIMING", "1")
        .envs(drain_env.iter().copied())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let timings: Vec<_> = String::from_utf8_lossy(&output.stderr)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|record| record.get("hook_drain_timing").cloned())
        .collect();
    assert_eq!(timings.len(), frames.len(), "{timings:?}");
    assert!(
        timings.iter().all(|timing| timing["applied"] == true),
        "{timings:?}"
    );
    let events = env.read_events();
    for frame in &frames {
        let derived = events
            .iter()
            .filter(|event| event.ingress.as_ref() == Some(&frame.event_id))
            .filter(|event| {
                matches!(
                    event.kind(),
                    rimz::store::event::EventKind::AgentLifecycle(_)
                )
            })
            .count();
        assert_eq!(derived, 1, "{events:?}");
    }
    let rows: Vec<_> = records(log)
        .into_iter()
        .filter(|row| row["command"] == "hooks apply")
        .collect();
    for row in &rows {
        let pid = row["pid"].as_u64().unwrap() as u32;
        assert!(
            !rimz::proc::process_is_live(pid, None),
            "no apply child outlives the drain: {row}"
        );
    }
    (frames, rows)
}

#[test]
fn consecutive_equal_environment_frames_share_one_apply_child() {
    let env = fixture();
    let log = env.home_root.join("run-work.jsonl");
    let rollup_bytes = std::fs::metadata(env.store().paths().rollup_cache().unwrap())
        .unwrap()
        .len();
    let (_, rows) = drain_queued(&env, &[0, 0, 0], &log, &[]);
    assert_eq!(rows.len(), 1, "one run, one apply child: {rows:?}");
    assert_eq!(
        rows[0]["rollup_bytes_parsed"].as_u64().unwrap(),
        rollup_bytes,
        "later frames reuse the child's held fold: {}",
        rows[0]
    );
}

#[test]
fn an_apply_child_never_applies_another_environments_frame() {
    let env = fixture();
    let log = env.home_root.join("run-work.jsonl");
    let sessions = [0, 0, 1, 0];
    let (frames, rows) = drain_queued(&env, &sessions, &log, &[]);
    assert_eq!(
        rows.len(),
        2,
        "A's idle child takes its last frame after B's commit: {rows:?}"
    );
    let events = env.read_events();
    for (frame, index) in frames.iter().zip(sessions) {
        let lifecycle = events
            .iter()
            .find(|event| event.ingress.as_ref() == Some(&frame.event_id))
            .map(|event| serde_json::to_value(event).unwrap())
            .unwrap();
        assert_eq!(
            lifecycle["params"]["pane_id"],
            format!("tmux:%{}", index + 1),
            "each frame applies under its own environment: {lifecycle}"
        );
    }
}

#[test]
fn a_full_pool_closes_the_apply_child_unused_longest() {
    let env = fixture();
    let log = env.home_root.join("run-work.jsonl");
    let (_, rows) = drain_queued(
        &env,
        &[0, 1, 0, 2, 0],
        &log,
        &[("RIMZ_TEST_HOOK_APPLY_POOL_CAP", "2")],
    );
    assert_eq!(rows.len(), 3, "C evicts B, not A: {rows:?}");
}

#[test]
fn a_pool_of_one_starts_a_child_on_every_environment_change() {
    let env = fixture();
    let log = env.home_root.join("run-work.jsonl");
    let (_, rows) = drain_queued(
        &env,
        &[0, 1, 0],
        &log,
        &[("RIMZ_TEST_HOOK_APPLY_POOL_CAP", "1")],
    );
    assert_eq!(rows.len(), 3, "A | B | A: {rows:?}");
}

#[test]
fn the_run_cap_ends_an_apply_child() {
    let env = fixture();
    let log = env.home_root.join("run-work.jsonl");
    let (_, rows) = drain_queued(
        &env,
        &[0, 0, 0],
        &log,
        &[("RIMZ_TEST_HOOK_APPLY_RUN_CAP", "2")],
    );
    assert_eq!(rows.len(), 2, "cap + 1 frames: {rows:?}");
}

fn fixture() -> Env {
    fixture_with_history(12)
}

fn fixture_with_history(closed: usize) -> Env {
    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    rimz::testkit::fleet::seed_history_carryover(&store, closed, 256).unwrap();
    for index in 0..closed {
        let id = format!("history-{index}");
        let observation =
            AgentLifecycleObservation::new(Some(id.clone().into()), LifecycleSignal::Ended);
        event_log::append(
            &store.paths().events_log,
            &EventEnvelope::agent_lifecycle(
                env.workspace_id.clone(),
                id,
                "claude",
                "SessionEnd",
                &observation,
            ),
        )
        .unwrap();
    }
    for index in 0..3 {
        apply(
            &env,
            index,
            json!({"hook_event_name": "SessionStart", "source": "startup"}),
            None,
        );
        agent_context::write(
            store.runtime_paths(),
            "claude",
            &format!("work-{index}"),
            &AgentContext::new("claude", Timestamp::now()),
            None,
        )
        .unwrap();
    }
    let snapshot = store.snapshot_cached().unwrap();
    assert_eq!(
        snapshot
            .agents
            .iter()
            .filter(|a| a.ended_at.is_none())
            .count(),
        3
    );
    assert_eq!(snapshot.fenced_sessions.len(), closed);
    env
}

fn cold_frame(payload: Value, assemblies: u64, rows_materialized: u64, config_loads: u64) {
    let small = frame_work(
        12,
        payload.clone(),
        assemblies,
        rows_materialized,
        config_loads,
    );
    let large = frame_work(120, payload, assemblies, rows_materialized, config_loads);
    assert_eq!(
        large, small,
        "(fold row copies, materialized rows) must not grow with retained history"
    );
}

fn frame_work(
    closed: usize,
    payload: Value,
    assemblies: u64,
    rows_materialized: u64,
    config_loads: u64,
) -> (u64, u64) {
    let env = fixture_with_history(closed);
    let log = env.home_root.join("work.jsonl");
    let rollup_bytes = std::fs::metadata(env.store().paths().rollup_cache().unwrap())
        .unwrap()
        .len();
    let pid = apply(&env, 0, payload, Some(&log));
    let rows = records(&log);
    let rows: Vec<_> = rows.iter().filter(|row| row["pid"] == pid).collect();
    assert_eq!(
        rows.len(),
        1,
        "one returning apply writes one record: {rows:?}"
    );
    let row = rows[0];
    assert_eq!(row["command"], "hooks apply");
    assert_eq!(row["ppid"], std::process::id());
    assert!(row["ended_ms"].as_u64().unwrap() >= row["started_ms"].as_u64().unwrap());
    assert!(row["user_us"].as_u64().unwrap() + row["sys_us"].as_u64().unwrap() > 0);
    for field in [
        "max_rss_kb",
        "minor_faults",
        "spawns",
        "fsyncs",
        "event_log_bytes_read",
        "carryover_bytes_parsed",
        "rollup_rows_materialized",
        "fold_row_copies",
        "snapshot_publishes",
        "latest_bytes_parsed",
        "marker_reads",
    ] {
        assert!(row[field].is_u64(), "{field}: {row}");
    }
    // Cached reads assemble once before the lifecycle append and once after its commit.
    assert_eq!(row["snapshot_assemblies"], assemblies, "{row}");
    assert_eq!(row["rollup_rows_materialized"], rows_materialized, "{row}");
    // Settings reads use the memoized core-only and loop-file readers.
    assert_eq!(row["machine_config_loads"], config_loads, "{row}");
    // One cold rollup parse per apply process; later reads share the held fold.
    assert_eq!(
        row["rollup_bytes_parsed"].as_u64().unwrap() / rollup_bytes,
        1,
        "{row}"
    );
    assert_eq!(
        row["rollup_bytes_parsed"].as_u64().unwrap() % rollup_bytes,
        0,
        "{row}"
    );
    (
        row["fold_row_copies"].as_u64().unwrap(),
        row["rollup_rows_materialized"].as_u64().unwrap(),
    )
}

#[test]
fn cold_prompt_reports_process_work() {
    cold_frame(
        json!({"hook_event_name": "UserPromptSubmit", "prompt": "inspect the repository"}),
        2,
        // Two three-row runtime projections and three own-row reads; Audit borrows.
        9,
        0,
    );
}

#[test]
fn cold_read_reports_process_work() {
    cold_frame(
        json!({"hook_event_name": "PostToolUse", "tool_name": "Read",
        "tool_input": {"file_path": "README.md"}, "tool_response": "contents"}),
        2,
        // Two three-row runtime projections and two own-row reads; Audit borrows.
        8,
        0,
    );
}

#[test]
fn cold_stop_reports_process_work() {
    cold_frame(
        json!({"hook_event_name": "Stop", "last_assistant_message": "done"}),
        2,
        // Two three-row runtime projections and two own-row reads; Audit borrows.
        // This cold Stop has no queued dispatch or context-row join.
        8,
        // Core defaults/accounts/attention and the lifecycle task catalog read only their files.
        0,
    );
}

#[test]
fn cold_team_stop_with_pending_message_avoids_machine_config_loads() {
    use rimz::store::message::{DeliveryGate, MessageRecord};

    let env = fixture();
    let store = env.store();
    let mut observation =
        AgentLifecycleObservation::new(Some("work-0".into()), LifecycleSignal::Registered);
    observation.launch.team = Some("forge".to_owned());
    store
        .append_event(&EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            "work-0",
            "claude",
            "SessionStart",
            &observation,
        ))
        .unwrap();
    let message = MessageRecord::new_for_card(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("claude"),
        "work-0".into(),
        None,
        "continue the task".to_owned(),
        true,
        DeliveryGate::Done,
    );
    let workspace = env.resolve_workspace(&env.project_root);
    store
        .queue_message(&message, &workspace.session_name)
        .unwrap();
    assert!(store.audit_rows().unwrap().iter().any(|agent| {
        agent.agent_id.as_str() == "work-0" && agent.team.as_deref() == Some("forge")
    }));
    let log = env.home_root.join("team-work.jsonl");
    let pid = apply(
        &env,
        0,
        json!({"hook_event_name": "Stop", "last_assistant_message": "done"}),
        Some(&log),
    );
    let rows = records(&log);
    let row = rows.iter().find(|row| row["pid"] == pid).unwrap();
    assert_eq!(row["machine_config_loads"], 0, "{row}");
}

#[test]
fn feed_carries_the_work_log_to_the_drainers_apply_child() {
    let env = fixture();
    let log = env.home_root.join("work.jsonl");
    let mut command = env.hook_command("claude");
    command
        .env("RIMZ_TEST_WORK_LOG", &log)
        .env("RIMZ_TEST_HOOK_DRAIN_IDLE_MS", "50")
        .env("TMUX_PANE", "%1");
    let child = env.spawn_payload(
        command,
        &json!({
            "hook_event_name": "Stop", "session_id": "work-0", "cwd": env.project_root,
            "last_assistant_message": "done",
        })
        .to_string(),
    );
    let feed_pid = child.id();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let deadline = Instant::now() + Duration::from_secs(30);
    let rows = loop {
        let rows = records(&log);
        if rows.iter().any(|row| row["command"] == "hooks drain") || Instant::now() >= deadline {
            break rows;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let applies: Vec<_> = rows
        .iter()
        .filter(|row| row["command"] == "hooks apply")
        .collect();
    assert_eq!(
        applies.len(),
        1,
        "one queued frame, one apply record: {rows:?}"
    );
    let drainer = applies[0]["ppid"].as_u64().unwrap() as u32;
    assert_ne!(drainer, feed_pid);
    assert_ne!(drainer, std::process::id());
    assert!(
        rows.iter()
            .any(|row| row["pid"] == drainer && row["command"] == "hooks drain"),
        "the apply child's parent is the returning drainer: {rows:?}"
    );
}

#[test]
fn errors_log_the_parsed_command_without_positional_values() {
    let env = Env::new();
    let log = env.home_root.join("work.jsonl");
    let mut child = env
        .rimz()
        .args(["agents", "show", "@work-log-private-positional"])
        .env("RIMZ_TEST_WORK_LOG", &log)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    assert!(!child.wait().unwrap().success());
    let rows = records(&log);
    assert_eq!(
        rows.len(),
        1,
        "a returning error writes exactly one record: {rows:?}"
    );
    assert_eq!(rows[0]["pid"], pid);
    assert_eq!(rows[0]["command"], "agents show");
    assert!(
        !std::fs::read_to_string(log)
            .unwrap()
            .contains("private-positional")
    );
}
