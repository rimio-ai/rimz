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
    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    rimz::testkit::fleet::seed_history_carryover(&store, 12, 256).unwrap();
    for index in 0..12 {
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
    assert_eq!(snapshot.fenced_sessions.len(), 12);
    env
}

fn cold_frame(payload: Value, assemblies: u64, config_loads: u64) {
    let env = fixture();
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
        "rollup_base_copies",
        "snapshot_publishes",
        "latest_bytes_parsed",
    ] {
        assert!(row[field].is_u64(), "{field}: {row}");
    }
    // handoff-perf-hook-flat.md: reuse cold-child fleet projections to lower this ratchet.
    assert_eq!(row["snapshot_assemblies"], assemblies, "{row}");
    // Prompt and Read applies read only the attention settings; Stop still loads the whole
    // machine config. handoff-perf-hook-flat.md owns lowering the Stop count.
    assert_eq!(row["machine_config_loads"], config_loads, "{row}");
    // handoff-perf-hook-flat.md: keep one cold rollup parse until the apply child is flattened.
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
}

#[test]
fn cold_prompt_reports_process_work() {
    cold_frame(
        json!({"hook_event_name": "UserPromptSubmit", "prompt": "inspect the repository"}),
        6,
        0,
    );
}

#[test]
fn cold_read_reports_process_work() {
    cold_frame(
        json!({"hook_event_name": "PostToolUse", "tool_name": "Read",
        "tool_input": {"file_path": "README.md"}, "tool_response": "contents"}),
        5,
        0,
    );
}

#[test]
fn cold_stop_reports_process_work() {
    cold_frame(
        json!({"hook_event_name": "Stop", "last_assistant_message": "done"}),
        5,
        1,
    );
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
