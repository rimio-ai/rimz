//! `rimz doctor` integration tests. Inject ledger and heartbeat state directly,
//! then run the binary and assert the report. The JSON report is the stable
//! contract checked here; a smoke test pins the human report's shape.

use rimz::agents::AgentLifecycleObservation;
use rimz::agents::lifecycle::LifecycleSignal;
use rimz::ids::{MuxName, ResolverId, SidebarInstanceId};
use rimz::message::{DeliveryGate, MessageRecord, MessageStatus};
use rimz::schema::event::{EventEnvelope, MessageEventMethod};
use rimz::schema::heartbeat::{ResolverHeartbeat, SidebarHeartbeat};
use serde_json::Value;

use crate::common::Env;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;

fn inject_lifecycle(
    env: &Env,
    agent_kind: &str,
    agent_id: &str,
    signal: LifecycleSignal,
    branch: Option<&str>,
) {
    let obs = AgentLifecycleObservation {
        agent_id: Some(agent_id.into()),
        agent_name: None,
        role: None,
        team: None,
        launch_group: None,
        launch_ordinal: None,
        channel: None,
        profile: None,
        kind_ordinal: None,
        signal,
        agent_pid: None,
        agent_process_start: None,
        runtime_owner: None,
        worktree_path: Some(env.project_root.display().to_string()),
        worktree_branch: branch.map(ToOwned::to_owned),
        task: None,
        prompt: None,
        transcript_path: None,
        origin: None,
        model: None,
        effort: None,
        context_pct: None,
        context_window: None,
        total_tokens: None,
        turn_error: None,
        cache_read_input_tokens: None,
        cache_write_input_tokens: None,
        fresh_input_tokens: None,
        output_tokens: None,
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

/// Run `rimz doctor --json …` and parse the report, failing loudly on a non-zero
/// exit or non-JSON stdout.
fn doctor_json(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("doctor --json emits valid json")
}

#[test]
fn doctor_json_folds_one_row_per_agent() {
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
        .args(["doctor", "--json"])
        .output()
        .expect("spawn doctor");
    let report = doctor_json(&output);

    let agents = &report["agents"];
    assert_eq!(agents["state"], "observed");
    assert_eq!(agents["counts"]["running"], 1);
    assert_eq!(agents["counts"]["failed"], 1);
    let rows = agents["rows"].as_array().expect("rows array");
    assert_eq!(
        rows.len(),
        1,
        "default doctor shows only problem rows: {rows:?}"
    );
    assert_eq!(rows[0]["kind"], "claude");
    assert_eq!(rows[0]["agent_id"], "claude-session-abc");
    assert_eq!(rows[0]["branch"], "main");
    assert_eq!(rows[0]["status"], "failed");

    let audit = doctor_json(
        &env.rimz()
            .args(["doctor", "--audit", "--json"])
            .output()
            .expect("spawn audit doctor"),
    );
    let rows = audit["agents"]["rows"].as_array().expect("audit rows");
    assert_eq!(
        rows.len(),
        2,
        "audit widens to every observed session: {rows:?}"
    );
    let codex_row = rows
        .iter()
        .find(|row| row["kind"] == "codex")
        .expect("codex row");
    assert_eq!(codex_row["agent_id"], "codex-session-xyz");
    assert_eq!(codex_row["branch"], "feature-migration");
    assert_eq!(codex_row["status"], "running");
}

#[test]
fn doctor_json_reports_agent_hook_install_and_trust_states() {
    let env = Env::new();
    let report = doctor_json(
        &env.rimz()
            .args(["doctor", "--json"])
            .output()
            .expect("spawn"),
    );
    let hooks = report["hooks"].as_array().expect("hooks array");
    let codex = hook(hooks, "codex");
    assert_eq!(codex["status"]["state"], "not_installed");
    assert!(
        fix(codex).contains("rimz hooks install codex"),
        "names the codex wiring command: {codex}"
    );
    assert!(
        fix(hook(hooks, "claude")).contains("rimz hooks install claude"),
        "names the claude wiring command"
    );

    let env = Env::new();
    env.install_agent_hooks("codex");
    let report = doctor_json(
        &env.rimz()
            .args(["doctor", "--json"])
            .output()
            .expect("spawn"),
    );
    let hooks = report["hooks"].as_array().expect("hooks array");
    let codex = hook(hooks, "codex");
    assert_eq!(
        codex["status"]["state"], "installed_untrusted",
        "freshly installed hooks are untrusted until /hooks"
    );
    assert!(
        fix(codex).contains("run /hooks inside codex and trust the Rimz hooks"),
        "doctor names the trust fix: {codex}"
    );
    assert_eq!(
        hook(hooks, "claude")["status"]["state"],
        "not_installed",
        "claude is still unwired"
    );

    let env = Env::new();
    env.install_agent_hooks("codex");
    trust_codex_hooks(&env);
    let report = doctor_json(
        &env.rimz()
            .args(["doctor", "--json"])
            .output()
            .expect("spawn"),
    );
    let hooks = report["hooks"].as_array().expect("hooks array");
    assert_eq!(
        hook(hooks, "codex")["status"]["state"],
        "installed",
        "trusted hooks read plain installed"
    );
}

fn hook<'a>(hooks: &'a [Value], kind: &str) -> &'a Value {
    hooks
        .iter()
        .find(|hook| hook["kind"] == kind)
        .unwrap_or_else(|| panic!("hook row for {kind}"))
}

fn fix(hook: &Value) -> &str {
    hook["status"]["fix"].as_str().expect("fix string")
}

#[test]
fn doctor_human_report_renders_titled_sections() {
    let env = Env::new();
    let output = env.rimz().arg("doctor").output().expect("spawn doctor");
    assert!(
        output.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");

    for title in [
        "WORKSPACE",
        "HOOKS",
        "STORAGE",
        "PROTOCOLS",
        "AGENTS",
        "MESSAGES",
    ] {
        assert!(stdout.contains(title), "missing section {title}:\n{stdout}");
    }
    assert!(
        !stdout.contains("ROOMS"),
        "rooms inventory moved out:\n{stdout}"
    );
    assert!(
        !stdout.contains("renderer:"),
        "static sidebar renderer line is gone:\n{stdout}"
    );
    assert!(
        !stdout.contains("AGENT COVERAGE") && !stdout.contains("HOOKS MATRIX"),
        "static adapter matrices moved to `rimz coverage`:\n{stdout}"
    );
    assert!(
        stdout.contains("rimz hooks install claude"),
        "the hooks table carries the wiring command:\n{stdout}"
    );
    assert!(
        stdout.contains("problems")
            || stdout.contains("warnings")
            || stdout.contains("no problems found"),
        "the report ends with a verdict line:\n{stdout}"
    );
}

#[test]
fn doctor_json_surfaces_stuck_and_failed_messages() {
    let env = Env::new();
    inject_lifecycle(
        &env,
        "codex",
        "sess-doctor-message",
        LifecycleSignal::Registered,
        Some("messages"),
    );
    let snapshot = env.ledger().snapshot_cached().expect("snapshot");
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "sess-doctor-message")
        .expect("message agent");

    let mut stuck = MessageRecord::new(
        env.workspace_id.clone(),
        agent,
        "stuck".to_owned(),
        true,
        DeliveryGate::Done,
    );
    stuck.agent_name = Some("coder".to_owned());
    stuck.attempts = 3;
    stuck.last_error = Some("pane rejected".to_owned());
    let stuck_id = stuck.message_id.to_string();
    env.ledger()
        .queue_message(&stuck, "test-session")
        .expect("queue stuck message");

    let mut failed = MessageRecord::new(
        env.workspace_id.clone(),
        agent,
        "failed".to_owned(),
        true,
        DeliveryGate::Done,
    );
    failed.status = MessageStatus::Errored;
    let failed_id = failed.message_id.to_string();
    env.ledger()
        .append_event(&EventEnvelope::message_event(
            &failed,
            "test-session",
            MessageEventMethod::Errored,
            Some("pane rejected input"),
        ))
        .expect("append failed message event");

    let mut delivered = MessageRecord::new(
        env.workspace_id.clone(),
        agent,
        "delivered".to_owned(),
        true,
        DeliveryGate::Done,
    );
    delivered.status = MessageStatus::Delivered;
    let delivered_id = delivered.message_id.to_string();
    env.ledger()
        .append_event(&EventEnvelope::message_event(
            &delivered,
            "test-session",
            MessageEventMethod::Delivered,
            None,
        ))
        .expect("append delivered message event");

    let report = doctor_json(
        &env.rimz()
            .args(["doctor", "--json"])
            .output()
            .expect("spawn"),
    );
    let messages = &report["messages"]["ready"];
    assert_eq!(messages["open"]["queued"], 1);
    let stuck_rows = messages["stuck"].as_array().expect("stuck rows");
    assert_eq!(stuck_rows.len(), 1, "{stuck_rows:?}");
    assert_eq!(stuck_rows[0]["message_id"], stuck_id);
    assert_eq!(stuck_rows[0]["status"], "queued");
    assert_eq!(stuck_rows[0]["target"], "@coder");
    assert!(
        stuck_rows[0]["problem"]
            .as_str()
            .expect("problem")
            .contains("attempts 3"),
        "{stuck_rows:?}"
    );
    assert!(
        stuck_rows[0]["problem"]
            .as_str()
            .expect("problem")
            .contains("pane rejected"),
        "{stuck_rows:?}"
    );

    let failures = messages["recent_failures"]
        .as_array()
        .expect("failure rows");
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(failures[0]["message_id"], failed_id);
    assert_eq!(failures[0]["status"], "errored");
    assert_eq!(failures[0]["problem"], "pane rejected input");
    assert!(
        !failures.iter().any(|row| row["message_id"] == delivered_id),
        "delivered terminal events stay out of doctor health rows: {failures:?}"
    );
}

#[test]
fn doctor_writes_json_report_to_file() {
    let env = Env::new();
    let path = env.home_root.join("doctor-report.json");
    let output = env
        .rimz()
        .args(["doctor", "--json", "--output"])
        .arg(&path)
        .output()
        .expect("spawn doctor");
    assert!(
        output.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "a file dump writes nothing to stdout: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    let bytes = std::fs::read(&path).expect("report file on disk");
    let report: Value = serde_json::from_slice(&bytes).expect("valid json on disk");
    assert!(report["hooks"].is_array(), "report is the full document");
}

/// Append `[hooks.state]` trust entries for every Rimz-installed codex event,
/// key-shaped exactly as Codex writes them after the user trusts via /hooks.
fn trust_codex_hooks(env: &Env) {
    let config = env.agent_config_path("codex");
    let mut text = std::fs::read_to_string(&config).expect("read codex config");
    for token in [
        "session_start",
        "user_prompt_submit",
        "subagent_start",
        "subagent_stop",
        "stop",
        "permission_request",
        "pre_tool_use",
        "post_tool_use",
        "pre_compact",
        "post_compact",
    ] {
        text.push_str(&format!(
            "\n[hooks.state.\"{}:{token}:0:0\"]\ntrusted_hash = \"sha256:deadbeef\"\n",
            config.display(),
        ));
    }
    std::fs::write(&config, text).expect("write trust state");
}

fn write_machine_config(env: &Env, text: &str) -> PathBuf {
    let dir = env.config_root().join("rimz");
    std::fs::create_dir_all(&dir).expect("mkdir config");
    let path = dir.join("config.toml");
    std::fs::write(&path, text).expect("write machine config");
    path
}

fn write_claude_settings(env: &Env, text: &str) -> PathBuf {
    let path = env.home_root.join("claude-settings.json");
    std::fs::write(&path, text).expect("write claude settings");
    path
}

fn stub_claude_version(env: &Env, version: &str) -> PathBuf {
    let dir = env.home_root.join("bin");
    std::fs::create_dir_all(&dir).expect("mkdir bin");
    let path = dir.join("claude");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo '{version}'; exit 0; fi\nexit 1\n"
        ),
    )
    .expect("write claude stub");
    let mut perms = std::fs::metadata(&path).expect("metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod claude stub");
    dir
}

fn path_with_stub_first(stub_dir: &Path) -> String {
    let current = std::env::var_os("PATH").unwrap_or_default();
    format!("{}:{}", stub_dir.display(), current.to_string_lossy())
}

#[test]
fn doctor_json_reports_remote_control_refusals_and_skips() {
    let env = Env::new();
    write_machine_config(&env, "[remote_control]\nclaude = true\ncodex = true\n");
    let settings = write_claude_settings(&env, r#"{ "disableRemoteControl": true }"#);
    let stub_dir = stub_claude_version(&env, "2.1.173 (Claude Code)");
    let codex_home = env.home_root.join("codex-home");

    let output = env
        .rimz()
        .args(["doctor", "--json"])
        .env("PATH", path_with_stub_first(&stub_dir))
        .env("CODEX_HOME", &codex_home)
        .env("RIMZ_CLAUDE_SETTINGS", &settings)
        .output()
        .expect("spawn doctor");
    let report = doctor_json(&output);

    let remote = &report["remote_control"];
    assert_eq!(remote["state"], "on");
    let labels: Vec<&str> = remote["agents"]
        .as_array()
        .expect("agents")
        .iter()
        .map(|agent| agent["label"].as_str().expect("label"))
        .collect();
    assert!(labels.contains(&"claude enabled, blocked"), "{labels:?}");
    assert!(
        labels.contains(&"codex enabled, standalone install missing"),
        "{labels:?}"
    );

    let refusals = remote["refusals"]
        .as_array()
        .expect("refusals")
        .iter()
        .map(|refusal| refusal.as_str().expect("refusal string"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        refusals.contains("`disableRemoteControl: true`"),
        "{refusals}"
    );
    assert!(
        refusals.contains("[remote_control] claude = false"),
        "{refusals}"
    );
    assert!(
        !refusals.contains("managed standalone Codex install is missing"),
        "{refusals}"
    );

    let skipped = remote["skipped"]
        .as_array()
        .expect("skipped")
        .iter()
        .map(|skip| skip.as_str().expect("skipped string"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        skipped.contains("managed standalone Codex install is missing"),
        "{skipped}"
    );
    assert!(
        skipped.contains("[remote_control] codex = false"),
        "{skipped}"
    );
}

#[test]
fn doctor_json_reports_protocol_version_mismatches() {
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

    let report = doctor_json(
        &env.rimz()
            .args(["doctor", "--json"])
            .output()
            .expect("spawn"),
    );
    let protocols = &report["protocols"];
    assert_eq!(protocols["event"], "rimz.event.v2");
    assert_eq!(protocols["sidebar"], "rimz.plugin.v5");
    assert_eq!(protocols["resolver"], "rimz.resolver.v1");

    let warnings = protocols["warnings"]
        .as_array()
        .expect("warnings")
        .iter()
        .map(|warning| warning.as_str().expect("warning string"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        warnings.contains("event log schema rimz.event.v0 seen 1 record (expected rimz.event.v2)"),
        "{warnings}"
    );
    assert!(
        warnings.contains(
            "sidebar heartbeat sidebar.old.json uses rimz.plugin.v0 (expected rimz.plugin.v5)"
        ),
        "{warnings}"
    );
    assert!(
        warnings.contains(
            "resolver heartbeat resolver.opus-policy.json uses rimz.resolver.v0 (expected rimz.resolver.v1)"
        ),
        "{warnings}"
    );
}
