//! `rimz doctor` agent rollup integration tests. Inject an `agent.lifecycle`
//! event directly into the ledger, then run the binary and assert the
//! rendered mode pill matches the closure of the unattended-runs audit story
//! in `docs/guide/product.md`.

use rimz::agents::AgentLifecycleObservation;
use rimz::feed::{AgentMode, AgentStatus};
use rimz::ids::{MuxName, ResolverId, SidebarInstanceId};
use rimz::schema::event::EventEnvelope;
use rimz::schema::heartbeat::{ResolverHeartbeat, SidebarHeartbeat};

use crate::common::Env;

fn inject_lifecycle(
    env: &Env,
    agent_kind: &str,
    agent_id: &str,
    status: AgentStatus,
    mode: AgentMode,
    branch: Option<&str>,
) {
    let obs = AgentLifecycleObservation {
        agent_id: Some(agent_id.to_owned()),
        status,
        mode,
        worktree_path: Some(env.project_root.display().to_string()),
        worktree_branch: branch.map(ToOwned::to_owned),
        task: None,
        model: None,
        effort: None,
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
    let output = env.rimz().arg("doctor").output().expect("spawn doctor");
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
fn doctor_renders_mode_pill_per_agent() {
    let env = Env::new();
    inject_lifecycle(
        &env,
        "claude",
        "claude-session-abc",
        AgentStatus::Waiting,
        AgentMode::Bypass,
        Some("main"),
    );
    inject_lifecycle(
        &env,
        "codex",
        "codex-session-xyz",
        AgentStatus::Running,
        AgentMode::Auto,
        Some("feature-migration"),
    );

    let output = env.rimz().arg("doctor").output().expect("spawn doctor");
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

    // Per-agent row: agent id + worktree + status + mode pill.
    assert!(stdout.contains("claude-session-abc"));
    assert!(stdout.contains("main"));
    assert!(stdout.contains("waiting"));
    assert!(stdout.contains("bypass"));

    assert!(stdout.contains("codex-session-xyz"));
    assert!(stdout.contains("feature-migration"));
    assert!(stdout.contains("running"));
    assert!(stdout.contains("auto"));
}

#[test]
fn doctor_keeps_latest_mode_per_agent_id() {
    let env = Env::new();
    inject_lifecycle(
        &env,
        "claude",
        "claude-session-abc",
        AgentStatus::Idle,
        AgentMode::Interactive,
        Some("main"),
    );
    inject_lifecycle(
        &env,
        "claude",
        "claude-session-abc",
        AgentStatus::Waiting,
        AgentMode::Bypass,
        Some("main"),
    );

    let output = env.rimz().arg("doctor").output().expect("spawn doctor");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8");

    assert!(
        stdout.contains("waiting") && stdout.contains("bypass"),
        "rollup should reflect the latest observation, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("interactive"),
        "old mode pill should be replaced, got:\n{stdout}"
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
        "protocols     : event rimz.event.v1; sidebar rimz.plugin.v1; resolver rimz.resolver.v1",
    ));
    assert!(stdout.contains(
        "protocol warn : event log schema rimz.event.v0 seen 1 record (expected rimz.event.v1)",
    ));
    assert!(stdout.contains(
        "protocol warn : sidebar heartbeat sidebar.old.json uses rimz.plugin.v0 (expected rimz.plugin.v1)",
    ));
    assert!(stdout.contains(
        "protocol warn : resolver heartbeat resolver.opus-policy.json uses rimz.resolver.v0 (expected rimz.resolver.v1)",
    ));
}
