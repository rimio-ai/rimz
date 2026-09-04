//! Integration coverage for the agent-facing `rimz wake` doorway.

use crate::common::Env;
use rimz::agents::{AgentLifecycleObservation, LaunchParams, LifecycleSignal};
use rimz::config::Tasks;
use rimz::ids::{AgentKind, AgentSessionId};
use rimz::store::event::{AgentLaunchPayload, AgentLaunchState, EventEnvelope};
use rimz::store::writer::AgentLifecycleIntent;

#[test]
fn wake_signal_arms_one_shot_instance_for_the_calling_agent() {
    let env = Env::new();
    register_calling_agent(&env);
    let output = agent_wake(&env)
        .args([
            "wake",
            "--signal",
            "ci.finished",
            "--match",
            "conclusion=failure",
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
    assert!(stdout.contains("on ci.finished [conclusion=failure]"));
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
    assert_eq!(entry.signal.as_deref(), Some("ci.finished"));
    assert_eq!(
        entry
            .matches
            .as_ref()
            .and_then(|matches| matches.get("conclusion"))
            .map(String::as_str),
        Some("failure")
    );
    assert_eq!(entry.once, Some(true));
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
        .args(["wake", "@planner", "--signal", "ci.finished"])
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
