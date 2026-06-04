//! `rimz doctor` agent rollup integration tests. Inject an `agent.lifecycle`
//! event directly into the ledger, then run the binary and assert the rendered
//! per-agent rows.

use rimz::agents::AgentLifecycleObservation;
use rimz::agents::lifecycle::LifecycleSignal;
use rimz::ids::{MuxName, ResolverId, SidebarInstanceId};
use rimz::schema::event::EventEnvelope;
use rimz::schema::heartbeat::{ResolverHeartbeat, SidebarHeartbeat};

use crate::common::Env;

fn inject_lifecycle(
    env: &Env,
    agent_kind: &str,
    agent_id: &str,
    signal: LifecycleSignal,
    branch: Option<&str>,
) {
    let obs = AgentLifecycleObservation {
        agent_id: Some(agent_id.to_owned()),
        signal,
        agent_pid: None,
        agent_process_start: None,
        runtime_owner: None,
        worktree_path: Some(env.project_root.display().to_string()),
        worktree_branch: branch.map(ToOwned::to_owned),
        task: None,
        prompt: None,
        model: None,
        effort: None,
        context_pct: None,
        context_window: None,
        total_tokens: None,
        todo_done: None,
        todo_total: None,
        pane_id: None,
        parent_agent_id: None,
    };
    let envelope = EventEnvelope::agent_lifecycle(
        env.workspace_id.clone(),
        "test-session",
        agent_kind,
        "SessionStart",
        &obs,
    );
    env.ledger().append_event(&envelope).expect("append");
}

#[test]
fn doctor_reports_no_agents_when_none_observed() {
    let env = Env::new();
    let output = env
        .rimz()
        .args(["doctor", "--audit"])
        .output()
        .expect("spawn doctor");
    assert!(
        output.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    assert!(
        stdout.contains("agents        : none observed"),
        "missing 'none observed' row, got:\n{stdout}"
    );
}

#[test]
fn doctor_renders_status_row_per_agent() {
    let env = Env::new();
    inject_lifecycle(
        &env,
        "claude",
        "claude-session-abc",
        LifecycleSignal::TurnEnded {
            errored: true,
            parked_on_background: false,
        },
        Some("main"),
    );
    inject_lifecycle(
        &env,
        "codex",
        "codex-session-xyz",
        LifecycleSignal::TurnStarted,
        Some("feature-migration"),
    );

    let output = env
        .rimz()
        .args(["doctor", "--audit"])
        .output()
        .expect("spawn doctor");
    assert!(
        output.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");

    // Group headers — one per kind, sorted lexically.
    assert!(
        stdout.contains("agent (claude)"),
        "missing claude group header in:\n{stdout}"
    );
    assert!(
        stdout.contains("agent (codex)"),
        "missing codex group header in:\n{stdout}"
    );

    // Per-agent row: agent id + worktree + status.
    assert!(stdout.contains("claude-session-abc"));
    assert!(stdout.contains("main"));
    assert!(stdout.contains("failed"));

    assert!(stdout.contains("codex-session-xyz"));
    assert!(stdout.contains("feature-migration"));
    assert!(stdout.contains("running"));
}

#[test]
fn doctor_keeps_latest_status_per_agent_id() {
    let env = Env::new();
    inject_lifecycle(
        &env,
        "claude",
        "claude-session-abc",
        LifecycleSignal::Registered,
        Some("main"),
    );
    inject_lifecycle(
        &env,
        "claude",
        "claude-session-abc",
        LifecycleSignal::TurnStarted,
        Some("main"),
    );

    let output = env
        .rimz()
        .args(["doctor", "--audit"])
        .output()
        .expect("spawn doctor");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8");

    assert_eq!(
        stdout.matches("claude-session-abc").count(),
        1,
        "the rollup folds both events into one row, got:\n{stdout}"
    );
    assert!(
        stdout.contains("running"),
        "rollup should reflect the latest observation, got:\n{stdout}"
    );
}

#[test]
fn doctor_reports_agent_hooks_not_installed() {
    // A fresh machine has no agent hooks wired. Running codex/claude in a Rimz
    // room then registers nothing — and the only signal the user gets must be
    // here: doctor names each un-wired agent and the command that wires it.
    let env = Env::new();
    let output = env.rimz().arg("doctor").output().expect("spawn doctor");
    assert!(
        output.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");

    assert!(
        stdout.contains("agent hooks"),
        "doctor must report agent hook install status:\n{stdout}"
    );
    assert!(
        stdout.contains("not installed"),
        "an un-onboarded machine must read 'not installed':\n{stdout}"
    );
    assert!(
        stdout.contains("rimz hooks install claude") && stdout.contains("rimz hooks install codex"),
        "doctor must name the wiring command for each missing agent:\n{stdout}"
    );
}

#[test]
fn doctor_reports_agent_hooks_installed_after_wiring() {
    let env = Env::new();
    env.install_agent_hooks("codex");
    let output = env.rimz().arg("doctor").output().expect("spawn doctor");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8");

    assert!(
        stdout.contains("codex installed"),
        "a wired agent must read 'installed':\n{stdout}"
    );
    assert!(
        stdout.contains("rimz hooks install claude"),
        "claude is still unwired, so its install hint stays:\n{stdout}"
    );
}

#[test]
fn doctor_reports_protocol_version_mismatches() {
    let env = Env::new();

    let mut event = EventEnvelope::new(
        env.workspace_id.clone(),
        "test-session",
        "rimz",
        "cli",
        "event.emit",
        serde_json::json!({ "kind": "build.started" }),
    );
    event.schema_version = "rimz.event.v0".to_owned();
    env.ledger().append_event(&event).expect("append old event");

    let rt = env.runtime_paths();
    rt.ensure_dirs().expect("runtime dirs");

    let mut sidebar = SidebarHeartbeat::new(
        env.workspace_id.clone(),
        SidebarInstanceId::new(),
        MuxName::Tmux,
        "rimz-test",
        rt.sock_dir.join("sidebar.old.sock"),
        None,
    );
    sidebar.protocol_version = "rimz.plugin.v0".to_owned();
    rimz::ledger::atomic::write_temp_then_rename(
        &rt.heartbeat_dir.join("sidebar.old.json"),
        &sidebar,
    )
    .expect("write old sidebar heartbeat");

    let resolver_id: ResolverId = "opus-policy".parse().expect("resolver id");
    let mut resolver = ResolverHeartbeat::new(env.workspace_id.clone(), resolver_id);
    resolver.protocol_version = "rimz.resolver.v0".to_owned();
    rimz::ledger::atomic::write_temp_then_rename(
        &rt.heartbeat_dir.join("resolver.opus-policy.json"),
        &resolver,
    )
    .expect("write old resolver heartbeat");

    let output = env.rimz().arg("doctor").output().expect("spawn doctor");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8");

    assert!(stdout.contains(
        "protocols     : event rimz.event.v2; sidebar rimz.plugin.v3; resolver rimz.resolver.v1",
    ));
    assert!(stdout.contains(
        "protocol warn : event log schema rimz.event.v0 seen 1 record (expected rimz.event.v2)",
    ));
    assert!(stdout.contains(
        "protocol warn : sidebar heartbeat sidebar.old.json uses rimz.plugin.v0 (expected rimz.plugin.v3)",
    ));
    assert!(stdout.contains(
        "protocol warn : resolver heartbeat resolver.opus-policy.json uses rimz.resolver.v0 (expected rimz.resolver.v1)",
    ));
}
