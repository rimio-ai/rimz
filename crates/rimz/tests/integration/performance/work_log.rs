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

fn apply(env: &Env, index: usize, mut payload: Value, log: Option<&Path>) -> u32 {
    payload["cwd"] = json!(env.project_root);
    payload["session_id"] = json!(format!("work-{index}"));
    let mut command = command(env, index);
    if let Some(log) = log {
        command.env("RIMZ_TEST_WORK_LOG", log);
    }
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
    let frame = HookIngress {
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
    };
    let child = env.spawn_payload(command, &serde_json::to_string(&frame).unwrap());
    let pid = child.id();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    pid
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
