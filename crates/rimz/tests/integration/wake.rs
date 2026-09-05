//! Integration coverage for the agent-facing `rimz wake` doorway.

use crate::common::Env;
use rimz::agents::{AgentLifecycleObservation, LaunchParams, LifecycleSignal};
use rimz::config::Tasks;
use rimz::ids::{AgentKind, AgentSessionId};
use rimz::store::event::{AgentLaunchPayload, AgentLaunchState, EventEnvelope};
use rimz::store::writer::AgentLifecycleIntent;

#[test]
fn wake_signal_arms_standing_instance_for_the_calling_agent() {
    let env = Env::new();
    register_calling_agent(&env);
    let output = agent_wake(&env)
        .args([
            "wake",
            "--signal",
            "deploy.failed",
            "--match",
            "branch=feature",
            "--prompt",
            "inspect CI",
        ])
        .output()
        .expect("arm signal wake");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("armed wake-"), "{stdout}");
    assert!(stdout.contains("on deploy.failed [branch=feature]"));
    assert!(stdout.contains("→ @planner"));

    let tasks: Tasks = serde_json::from_slice(
        &std::fs::read(rimz::harness::schedule::catalog::instances_path(
            &env.state_root(),
        ))
        .expect("wake instance store"),
    )
    .expect("wake instances JSON");
    let (name, entry) = tasks.0.iter().next().expect("one wake row");
    assert!(name.starts_with("wake-"));
    assert_eq!(entry.signal.as_deref(), Some("deploy.failed"));
    assert_eq!(
        entry
            .matches
            .as_ref()
            .and_then(|matches| matches.get("branch"))
            .map(String::as_str),
        Some("feature")
    );
    assert_eq!(entry.once, None);
    assert_eq!(entry.timeout.as_deref(), Some("59m"));
    let meta = entry.wake_meta.as_ref().expect("wake provenance");
    assert!(meta.last_observed_at.is_none());
    assert_eq!(
        entry
            .deadline
            .unwrap()
            .duration_since(meta.armed_at)
            .as_secs(),
        59 * 60
    );
    assert_eq!(entry.prompt.as_deref(), Some("inspect CI"));
    let target = entry.wake.as_ref().expect("pinned wake target");
    assert_eq!(target.kind, "claude");
    assert_eq!(target.session, "provider-session");
    assert_eq!(target.handle, "@planner");
}

#[test]
fn calling_agent_can_list_and_cancel_human_armed_wake_by_launch_identity() {
    let env = Env::new();
    register_calling_agent(&env);
    let armed = env
        .rimz()
        .args(["wake", "@planner", "--signal", "deploy.failed"])
        .output()
        .expect("arm wake from human shell");
    assert!(
        armed.status.success(),
        "{}",
        String::from_utf8_lossy(&armed.stderr)
    );
    let stdout = String::from_utf8_lossy(&armed.stdout);
    let name = stdout
        .strip_prefix("armed ")
        .and_then(|receipt| receipt.split_once(':'))
        .map(|(name, _)| name)
        .expect("armed wake name");

    let listed = agent_wake(&env)
        .args(["wake", "list", "--json"])
        .output()
        .expect("list wake as target agent");
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let rows: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("wake list JSON");
    assert_eq!(rows.as_array().expect("wake rows").len(), 1);
    assert_eq!(rows[0]["name"], name);

    let canceled = agent_wake(&env)
        .args(["wake", "cancel", name])
        .output()
        .expect("cancel wake as target agent");
    assert!(
        canceled.status.success(),
        "{}",
        String::from_utf8_lossy(&canceled.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&canceled.stdout),
        format!("canceled {name}\n")
    );
}

#[test]
fn wake_without_target_refuses_a_plain_shell() {
    let env = Env::new();
    let output = env
        .rimz()
        .args(["wake", "--in", "5m"])
        .output()
        .expect("run wake from shell");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("without an explicit @target is only available to an agent"),
        "{stderr}"
    );
}

#[test]
fn wake_rejects_delays_the_minute_scheduler_cannot_represent() {
    let env = Env::new();
    let output = agent_wake(&env)
        .args(["wake", "--in", "1d"])
        .output()
        .expect("run wake with long delay");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--in must be less than 24h"), "{stderr}");
}

#[test]
fn wake_wait_reports_a_watched_failure_and_settles_its_message() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let store = env.store();

    let output = agent_wake(&env)
        .args([
            "wake",
            "--wait=5s",
            "--",
            "sh",
            "-c",
            "printf watched; exit 3",
        ])
        .output()
        .expect("wait for watched wake");
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("delivered · exit 3"), "{stdout}");
    assert!(stdout.contains("watched"), "{stdout}");
    assert!(store.list_pending_messages().unwrap().is_empty());

    let instances: Tasks = serde_json::from_slice(
        &std::fs::read(rimz::harness::schedule::catalog::instances_path(
            &env.state_root(),
        ))
        .expect("wake instance store"),
    )
    .expect("wake instances JSON");
    assert!(instances.0.is_empty());
}

#[test]
fn watch_retires_without_delivery_when_its_polarity_does_not_match() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_calling_agent(&env);

    let output = agent_wake(&env)
        .args(["wake", "--on", "fail", "--wait=5s", "--", "true"])
        .output()
        .expect("wait for successful watched command");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("skipped · exit 0"), "{stdout}");
    assert!(env.store().list_pending_messages().unwrap().is_empty());
    let instances: Tasks = serde_json::from_slice(
        &std::fs::read(rimz::harness::schedule::catalog::instances_path(
            &env.state_root(),
        ))
        .expect("wake instance store"),
    )
    .expect("wake instances JSON");
    assert!(instances.0.is_empty());
}

#[test]
fn signal_wake_observes_siblings_delivers_repeated_matches_and_lapses_silently() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let name = arm_subscription(&env);

    wake_ok(
        &env,
        &[
            "events",
            "emit",
            "deploy.passed",
            "--json",
            r#"{"branch":"other"}"#,
        ],
    );
    assert!(wake_records(&env).is_empty());
    assert!(
        wake_instances(&env).0[&name]
            .wake_meta
            .as_ref()
            .unwrap()
            .last_observed_at
            .is_none()
    );

    wake_ok(
        &env,
        &[
            "events",
            "emit",
            "deploy.passed",
            "--json",
            r#"{"branch":"feature"}"#,
        ],
    );
    let records = wait_for_wake_records(&env, 1);
    assert_eq!(records[0].result.label(), "skipped");
    assert!(records[0].message_id.is_none());
    assert!(env.store().list_pending_messages().unwrap().is_empty());
    let observed = wake_instances(&env).0[&name]
        .wake_meta
        .as_ref()
        .unwrap()
        .last_observed_at;
    assert!(observed.is_some());

    for count in [2, 3] {
        wake_ok(
            &env,
            &[
                "events",
                "emit",
                "deploy.failed",
                "--json",
                r#"{"branch":"feature","reason":"red"}"#,
            ],
        );
        let records = wait_for_wake_records(&env, count);
        let record = records.last().unwrap();
        assert_eq!(record.result.label(), "delivered");
        let message_id = record.message_id.as_ref().expect("delivered message");
        let message = wake_ok(&env, &["message", "show", message_id.as_str()]);
        assert!(message.contains("deploy.failed"), "{message}");
        assert!(message.contains("feature"), "{message}");
        assert!(wake_instances(&env).0.contains_key(&name));
    }

    expire_subscription(&env, &name);
    wake_ok(&env, &["loop", "tick"]);
    let records = wait_for_wake_records(&env, 4);
    assert_eq!(records[3].result.label(), "expired");
    assert!(records[3].message_id.is_none());
    assert!(!wake_instances(&env).0.contains_key(&name));
    assert_eq!(env.store().list_pending_messages().unwrap().len(), 2);
}

#[test]
fn unobserved_signal_wake_expires_with_a_delivered_wake() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let name = arm_subscription(&env);
    expire_subscription(&env, &name);
    wake_ok(&env, &["loop", "tick"]);
    let records = wait_for_wake_records(&env, 1);
    assert_eq!(records[0].result.label(), "expired");
    let message_id = records[0].message_id.as_ref().expect("expiry message");
    let message = wake_ok(&env, &["message", "show", message_id.as_str()]);
    assert!(message.contains(&format!("{name} expired:")), "{message}");
    assert!(message.contains("deploy.failed"), "{message}");
    assert!(!wake_instances(&env).0.contains_key(&name));
}

#[test]
fn signal_wake_rearm_and_cancel_race_with_lapse() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let name = arm_subscription(&env);
    let receipt = wake_ok(
        &env,
        &[
            "wake",
            "--signal",
            "deploy.failed",
            "--match",
            "branch=feature",
            "--json",
        ],
    );
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    assert_eq!(receipt["name"], name);
    wake_ok(&env, &["loop", "run", &name, "--expired"]);
    assert!(wake_records(&env).is_empty());
    assert!(wake_instances(&env).0.contains_key(&name));
    wake_ok(&env, &["wake", "cancel", &name]);
    wake_ok(&env, &["loop", "run", &name, "--expired"]);
    assert!(wake_records(&env).is_empty());
    assert!(env.store().list_pending_messages().unwrap().is_empty());
}

#[test]
fn loop_signal_siblings_preserve_forever_and_once_subscriptions() {
    for once in [false, true] {
        let env = Env::new();
        env.install_agent_hooks("claude");
        register_calling_agent(&env);
        let mut args = vec![
            "loop",
            "add",
            "deployment",
            "--wake",
            "@planner",
            "--signal",
            "deploy.failed",
            "--prompt",
            "inspect deployment",
        ];
        if once {
            args.push("--once");
        }
        wake_ok(&env, &args);
        wake_ok(&env, &["events", "emit", "deploy.passed"]);
        let records = wake_records(&env);
        assert_eq!(records.len(), 1, "skip is recorded before emit returns");
        assert_eq!(
            serde_json::to_value(records[0].result).unwrap(),
            "signal_skipped"
        );
        assert_eq!(
            records[0].signal.as_ref().unwrap().name.as_str(),
            "deploy.passed"
        );
        assert!(env.store().list_pending_messages().unwrap().is_empty());
        wake_ok(&env, &["loop", "show", "deployment"]);
        wake_ok(&env, &["events", "emit", "deploy.failed"]);
        let records = wait_for_wake_records(&env, 2);
        assert_eq!(records[1].result.label(), "delivered");
        if once {
            assert!(!wake_instances(&env).0.contains_key("deployment"));
        } else {
            wake_ok(&env, &["loop", "show", "deployment"]);
            wake_ok(&env, &["events", "emit", "deploy.failed"]);
            assert_eq!(
                wait_for_wake_records(&env, 3)[2].result.label(),
                "delivered"
            );
        }
    }
}

fn wake_ok(env: &Env, args: &[&str]) -> String {
    let output = agent_wake(env)
        .args(args)
        .output()
        .expect("run wake command");
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn arm_subscription(env: &Env) -> String {
    let receipt = wake_ok(
        env,
        &[
            "wake",
            "--signal",
            "deploy.failed",
            "--match",
            "branch=feature",
            "--json",
        ],
    );
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    receipt["name"].as_str().unwrap().to_owned()
}

fn wake_instances(env: &Env) -> Tasks {
    let path = rimz::harness::schedule::catalog::instances_path(&env.state_root());
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn expire_subscription(env: &Env, name: &str) {
    let mut tasks = wake_instances(env);
    tasks.0.get_mut(name).unwrap().deadline = Some(jiff::Timestamp::UNIX_EPOCH);
    let path = rimz::harness::schedule::catalog::instances_path(&env.state_root());
    std::fs::write(path, serde_json::to_vec(&tasks).unwrap()).unwrap();
}

fn wake_records(env: &Env) -> Vec<rimz::harness::schedule::run_log::LoopRunRecord> {
    let path = rimz::harness::schedule::run_log::log_path(&env.state_root());
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn wait_for_wake_records(
    env: &Env,
    count: usize,
) -> Vec<rimz::harness::schedule::run_log::LoopRunRecord> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let records = wake_records(env);
        if records.len() >= count {
            return records;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "expected {count} records, got {records:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

fn register_calling_agent(env: &Env) {
    let store = env.store();
    let workspace =
        rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("workspace resolves");
    store
        .append_event(&EventEnvelope::agent_launched(
            workspace.workspace_id,
            &workspace.session_name,
            &AgentKind::new_unchecked("claude"),
            AgentLaunchPayload {
                agent_id: AgentSessionId::from("provider-session"),
                launch_id: Some(AgentSessionId::from("launch-session")),
                agent_name: "planner".to_owned(),
                agent_name_explicit: true,
                launch: LaunchParams::default(),
                state: AgentLaunchState::Bound,
                run_id: None,
                pane_id: None,
                runtime_owner: None,
                worktree_path: Some(env.project_root.display().to_string()),
                worktree_branch: Some("main".to_owned()),
                prompt: None,
                description: None,
            },
        ))
        .expect("seed launched target");
    let mut observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("provider-session")),
        LifecycleSignal::Registered,
    );
    observation.agent_name = Some("planner".to_owned());
    store
        .append_agent_lifecycle(AgentLifecycleIntent {
            session_name: "rimz-test",
            agent_kind: AgentKind::new_unchecked("claude"),
            event_name: "test",
            observation: &observation,
            spawned_subagents: &[],
        })
        .expect("register target");
}

fn agent_wake(env: &Env) -> std::process::Command {
    let mut command = env.rimz();
    command
        .env("RIMZ_AGENT_KIND", "claude")
        .env("RIMZ_AGENT_ID", "launch-session")
        .env("RIMZ_AGENT_NAME", "planner");
    command
}
