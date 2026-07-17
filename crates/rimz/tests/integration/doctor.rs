//! `rimz doctor` integration tests. Inject store and heartbeat state directly,
//! then run the binary and assert the report. The JSON report is the stable
//! contract checked here; a smoke test pins the human report's shape.

use jiff::Timestamp;
use rimz::agents::lifecycle::LifecycleSignal;
use rimz::agents::{AgentLifecycleObservation, LaunchParams};
use rimz::diag::DiagSink;
use rimz::diag::record::{DiagEnvelope, DiagEvent, RendererExitCause};
use rimz::ids::{AgentKind, MuxName, PaneId, SidebarInstanceId};
use rimz::message::{DeliveryGate, MessageRecord, MessageSender, MessageStatus};
use rimz::sidebar::heartbeat::SidebarHeartbeat;
use rimz::store::event::{
    EventEnvelope, LastDeathMarker, MessageEventMethod, SessionDeathAgent, SessionDeathCause,
};
use serde_json::Value;

use crate::common::Env;
use std::ffi::OsString;
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
        launch: LaunchParams::default(),
        signal,
        agent_pid: None,
        agent_process_start: None,
        runtime_owner: None,
        worktree_path: Some(env.project_root.display().to_string()),
        worktree_branch: branch.map(ToOwned::to_owned),
        task: None,
        prompt: None,
        description: None,
        transcript_path: None,
        origin: None,
        context_pct: None,
        context_window: None,
        total_tokens: None,
        turn_error: None,
        cache_read_input_tokens: None,
        cache_write_input_tokens: None,
        fresh_input_tokens: None,
        output_tokens: None,
        pane_id: None,
        pane_stamp: None,
        parent_agent_id: None,
    };
    let envelope = EventEnvelope::agent_lifecycle(
        env.workspace_id.clone(),
        "test-session",
        agent_kind,
        "SessionStart",
        &obs,
    );
    env.store().append_event(&envelope).expect("append");
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
    let stub_dir = stub_agent_binaries(&env, &["codex", "claude", "kiro"]);
    let report = doctor_json(
        &env.rimz()
            .args(["doctor", "--json"])
            .env("PATH", &stub_dir)
            .output()
            .expect("spawn"),
    );
    let hooks = report["hooks"].as_array().expect("hooks array");
    let codex = hook(hooks, "codex");
    assert_eq!(codex["status"]["state"], "not_installed");
    assert_eq!(codex["detected"], true);
    assert!(
        fix(codex).contains("rimz hooks install codex"),
        "names the codex wiring command: {codex}"
    );
    assert!(
        fix(hook(hooks, "claude")).contains("rimz hooks install claude"),
        "names the claude wiring command"
    );
    let kiro = hook(hooks, "kiro");
    assert_eq!(kiro["status"]["state"], "unsupported");
    assert!(
        kiro["status"]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("does not execute standalone hook configs")),
        "Kiro reports the verified hook limitation: {kiro}"
    );
    let grok = hook(hooks, "grok");
    assert_eq!(grok["status"]["state"], "not_detected");
    assert_eq!(grok["detected"], false);

    let env = Env::new();
    let stub_dir = stub_agent_binaries(&env, &["codex", "claude"]);
    env.install_agent_hooks("codex");
    let report = doctor_json(
        &env.rimz()
            .args(["doctor", "--json"])
            .env("PATH", &stub_dir)
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
        fix(codex).contains("run /hooks inside codex and trust the RimZ hooks"),
        "doctor names the trust fix: {codex}"
    );
    assert_eq!(
        hook(hooks, "claude")["status"]["state"],
        "not_installed",
        "claude is still unwired"
    );

    let env = Env::new();
    let stub_dir = stub_agent_binaries(&env, &["codex", "claude"]);
    env.install_agent_hooks("codex");
    trust_codex_hooks(&env);
    let report = doctor_json(
        &env.rimz()
            .args(["doctor", "--json"])
            .env("PATH", &stub_dir)
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
    let stub_dir = stub_agent_binaries(&env, &["claude"]);
    let output = env
        .rimz()
        .arg("doctor")
        .env("PATH", &stub_dir)
        .output()
        .expect("spawn doctor");
    assert!(
        output.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");

    for title in [
        "WORKSPACE",
        "MACHINE CONFIG",
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
    let not_detected = stdout
        .lines()
        .find(|line| line.contains("not detected on this machine:"))
        .expect("absent-agent footer");
    assert!(not_detected.contains("grok"), "{not_detected}");
    assert!(
        !stdout
            .lines()
            .any(|line| line.contains('✗') && line.contains("grok")),
        "absent grok is not an alarm row:\n{stdout}"
    );
    assert!(
        stdout.contains("problems")
            || stdout.contains("warnings")
            || stdout.contains("no problems found"),
        "the report ends with a verdict line:\n{stdout}"
    );
}

#[test]
fn doctor_reports_unparseable_machine_config_in_json_and_human_output() {
    let env = Env::new();
    let path = env.config_root().join("rimz/theme.toml");
    std::fs::create_dir_all(path.parent().expect("config parent")).expect("mkdir config");
    std::fs::write(&path, "[theme.display]\nmax_cols = 64\nmax_cols = 72\n")
        .expect("write broken theme config");

    let report = doctor_json(
        &env.rimz()
            .args(["doctor", "--json"])
            .output()
            .expect("spawn doctor"),
    );
    let broken = report["machine_config"]["broken_files"]
        .as_array()
        .expect("broken files array");
    assert_eq!(broken.len(), 1, "one broken config file: {broken:?}");
    assert_eq!(broken[0]["path"], path.display().to_string());
    assert!(
        broken[0]["error"]
            .as_str()
            .expect("error string")
            .contains("duplicate key")
            && broken[0]["error"]
                .as_str()
                .expect("error string")
                .contains("max_cols"),
        "precise parse error: {broken:?}",
    );

    let output = env.rimz().arg("doctor").output().expect("spawn doctor");
    assert!(
        output.status.success(),
        "doctor succeeds with broken config"
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8 human report");
    assert!(stdout.contains("MACHINE CONFIG"), "{stdout}");
    assert!(stdout.contains("theme.toml is unparseable"), "{stdout}");
    assert!(
        stdout.contains("duplicate key") && stdout.contains("max_cols"),
        "{stdout}",
    );
}

#[test]
fn doctor_reports_duplicate_mux_binaries() {
    let env = Env::new();
    let first_dir = stub_mux_version(&env, "mux-one", "zellij", "--version", "zellij 0.44.3-a");
    let second_dir = stub_mux_version(&env, "mux-two", "zellij", "--version", "zellij 0.44.3-b");
    let path = path_with_only(&[first_dir.clone(), second_dir.clone()]);

    let report = doctor_json(
        &env.rimz()
            .args(["doctor", "--json"])
            .env("PATH", &path)
            .output()
            .expect("spawn doctor"),
    );
    let mux = &report["mux"]["ready"];
    assert_eq!(mux["name"], "zellij");
    assert_eq!(
        mux["binaries"]["active"]["path"],
        first_dir.join("zellij").display().to_string()
    );
    assert_eq!(mux["binaries"]["active"]["version"], "zellij 0.44.3-a");
    let duplicates = mux["binaries"]["duplicates"]
        .as_array()
        .expect("duplicates");
    assert_eq!(duplicates.len(), 1, "{duplicates:?}");
    assert_eq!(
        duplicates[0]["path"],
        second_dir.join("zellij").display().to_string()
    );
    assert_eq!(duplicates[0]["version"], "zellij 0.44.3-b");

    let output = env
        .rimz()
        .arg("doctor")
        .env("PATH", &path)
        .output()
        .expect("spawn doctor");
    assert!(
        output.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    assert!(
        stdout.contains("multiple zellij binaries on PATH"),
        "human report names duplicate mux binaries:\n{stdout}"
    );
}

#[test]
fn doctor_reports_zellij_server_log_excerpt() {
    let env = Env::new();
    let stub_dir = stub_mux_version(&env, "mux-bin", "zellij", "--version", "zellij 0.44.3");
    let tmp = env.home_root.join("tmp");
    let uid = nix::unistd::Uid::current().as_raw();
    let log_path = tmp
        .join(format!("zellij-{uid}"))
        .join("zellij-log")
        .join("zellij.log");
    std::fs::create_dir_all(log_path.parent().expect("log parent")).expect("mkdir log dir");
    std::fs::write(
        &log_path,
        "INFO boot\nWARN first warning\nINFO WARN mid-line ignored\nERROR failed\nPanic occured: boom\n",
    )
    .expect("write zellij log");

    let report = doctor_json(
        &env.rimz()
            .args(["doctor", "--json"])
            .env("PATH", path_with_only(std::slice::from_ref(&stub_dir)))
            .env("TMPDIR", &tmp)
            .output()
            .expect("spawn doctor"),
    );
    let log = &report["mux"]["ready"]["log"];
    assert_eq!(log["state"], "ready");
    assert_eq!(log["path"], log_path.display().to_string());
    assert_eq!(log["matched"], 3);
    let entries = log["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 3, "{entries:?}");
    assert_eq!(entries[0]["severity"], "warn");
    assert_eq!(entries[0]["line"], "WARN first warning");
    assert_eq!(entries[1]["severity"], "error");
    assert_eq!(entries[1]["line"], "ERROR failed");
    assert_eq!(entries[2]["severity"], "panic");
    assert_eq!(entries[2]["line"], "Panic occured: boom");

    std::fs::write(&log_path, "INFO boot\nINFO WARN still ignored\n").expect("rewrite zellij log");
    let clean = doctor_json(
        &env.rimz()
            .args(["doctor", "--json"])
            .env("PATH", path_with_only(&[stub_dir]))
            .env("TMPDIR", &tmp)
            .output()
            .expect("spawn doctor"),
    );
    assert_eq!(clean["mux"]["ready"]["log"]["state"], "ready");
    assert_eq!(clean["mux"]["ready"]["log"]["matched"], 0);
    assert_eq!(
        clean["mux"]["ready"]["log"]["entries"]
            .as_array()
            .expect("entries")
            .len(),
        0
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
    let snapshot = env.store().snapshot_cached().expect("snapshot");
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
    env.store()
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
    env.store()
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
    env.store()
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
    assert_eq!(stuck_rows[0]["target"], "@codex#project");
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
fn doctor_clear_dismisses_recorded_history() {
    let env = Env::new();
    write_diag_record(&env, 0, None);
    let _ = write_last_death_marker(&env, SessionDeathCause::Crash, Some(2));
    let mut failed = EventEnvelope::unresolved_message_event(
        env.workspace_id.clone(),
        "test-session",
        "@missing".to_owned(),
        None,
        MessageSender::Human,
        6,
        "receiver not found".to_owned(),
    );
    failed.timestamp = Timestamp::UNIX_EPOCH;
    env.store()
        .append_event(&failed)
        .expect("append failed message event");

    let report = doctor_json(
        &env.rimz()
            .args(["doctor", "--clear", "--json"])
            .output()
            .expect("spawn clear doctor"),
    );
    let cleared_at: Timestamp = report["history_cleared_at"]
        .as_str()
        .expect("history watermark")
        .parse()
        .expect("watermark timestamp");
    assert!(
        report["diagnostics"]["records"]
            .as_array()
            .expect("diagnostic records")
            .is_empty(),
        "old diagnostics are dismissed: {report}"
    );
    assert!(
        report.get("last_incident").is_none(),
        "old incident is dismissed: {report}"
    );
    assert!(
        report["messages"]["ready"]["recent_failures"]
            .as_array()
            .expect("recent failures")
            .is_empty(),
        "old message failure is dismissed: {report}"
    );

    let paths = env.state_path_for(&env.project_root);
    assert!(paths.doctor_watermark.exists(), "watermark is durable");
    assert!(
        paths.last_death_marker.exists(),
        "incident evidence remains"
    );
    assert!(diag_log_path(&env).exists(), "diagnostic evidence remains");
    assert!(paths.events_log.exists(), "event evidence remains");

    let next = doctor_json(
        &env.rimz()
            .args(["doctor", "--json"])
            .output()
            .expect("spawn doctor after clear"),
    );
    assert_eq!(next["history_cleared_at"], report["history_cleared_at"]);
    assert!(
        next["diagnostics"]["records"]
            .as_array()
            .expect("diagnostic records")
            .is_empty()
    );
    assert!(next.get("last_incident").is_none());
    assert!(
        next["messages"]["ready"]["recent_failures"]
            .as_array()
            .expect("recent failures")
            .is_empty()
    );

    write_diag_record(
        &env,
        u64::try_from(cleared_at.as_millisecond() + 1).expect("positive timestamp"),
        None,
    );
    let fresh = doctor_json(
        &env.rimz()
            .args(["doctor", "--json"])
            .output()
            .expect("spawn doctor with fresh diagnostic"),
    );
    assert_eq!(
        fresh["diagnostics"]["records"]
            .as_array()
            .expect("diagnostic records")
            .len(),
        1,
        "post-clear records remain visible: {fresh}"
    );
}

#[test]
fn doctor_labels_stale_build_diagnostics() {
    let env = Env::new();
    write_diag_record(
        &env,
        rimz::sidebar::timing::unix_now_ms(),
        Some("stale-build"),
    );

    let report = doctor_json(
        &env.rimz()
            .args(["doctor", "--json"])
            .output()
            .expect("spawn doctor"),
    );
    let rows = report["diagnostics"]["records"]
        .as_array()
        .expect("diagnostic records");
    assert_eq!(rows.len(), 1, "one diagnostic row: {rows:?}");
    assert_eq!(rows[0]["build"], "stale-build");
    assert_eq!(rows[0]["stale_build"], true);

    let output = env.rimz().arg("doctor").output().expect("spawn doctor");
    assert!(
        output.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8 human report");
    assert!(stdout.contains("old build stale-build"), "{stdout}");
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

#[test]
fn doctor_reports_last_incident() {
    let env = Env::new();
    let archive = write_last_death_marker(&env, SessionDeathCause::Crash, Some(2))
        .expect("crash marker creates archive");
    let report = doctor_json(
        &env.rimz()
            .args(["doctor", "--json"])
            .output()
            .expect("spawn"),
    );
    let incident = &report["last_incident"];
    assert_eq!(incident["cause"], "crash");
    assert_eq!(incident["recovered"], 2);
    assert_eq!(
        incident["lost_agents"]
            .as_array()
            .expect("lost agents")
            .len(),
        2
    );
    assert!(
        incident["forensics"]
            .as_str()
            .expect("forensics path")
            .ends_with(archive.file_name().expect("archive name").to_str().unwrap()),
        "{incident}"
    );

    let env = Env::new();
    let _ = write_last_death_marker(&env, SessionDeathCause::Reboot, Some(1));
    let report = doctor_json(
        &env.rimz()
            .args(["doctor", "--json"])
            .output()
            .expect("spawn"),
    );
    let incident = report["last_incident"]
        .as_object()
        .expect("last incident object");
    assert_eq!(incident["cause"], "reboot");
    assert_eq!(incident["recovered"], 1);
    assert!(
        !incident.contains_key("forensics"),
        "reboot has no crash archive pointer: {incident:?}"
    );
}

fn write_last_death_marker(
    env: &Env,
    cause: SessionDeathCause,
    recovered: Option<usize>,
) -> Option<PathBuf> {
    let paths = env.state_path_for(&env.project_root);
    let marker = LastDeathMarker {
        cause,
        lost_agents: vec![
            SessionDeathAgent {
                kind: AgentKind::new_unchecked("claude"),
                agent_id: "sess-a".into(),
                name: Some("lucid-atlas".to_owned()),
            },
            SessionDeathAgent {
                kind: AgentKind::new_unchecked("codex"),
                agent_id: "sess-b".into(),
                name: Some("quiet-comet".to_owned()),
            },
        ],
        at: Timestamp::UNIX_EPOCH,
        recovered,
    };
    rimz::store::atomic::write_temp_then_rename(&paths.last_death_marker, &marker)
        .expect("write last death marker");
    (cause == SessionDeathCause::Crash).then(|| {
        std::fs::create_dir_all(paths.crashes_dir.join("20260708T082717Z"))
            .expect("mkdir older crash archive");
        let archive = paths.crashes_dir.join("20260709T082717Z");
        std::fs::create_dir_all(&archive).expect("mkdir crash archive");
        archive
    })
}

fn write_diag_record(env: &Env, at_ms: u64, build: Option<&str>) {
    let mut record = DiagEnvelope::new(
        env.workspace_id.clone(),
        "test-session".to_owned(),
        None,
        at_ms,
        DiagEvent::RendererExit {
            cause: RendererExitCause::DegradedGaveUp,
        },
    );
    record.build = build.map(ToOwned::to_owned);
    rimz::diag::JsonlLog::new(diag_log_path(env), 1_048_576).append(&record);
}

fn diag_log_path(env: &Env) -> PathBuf {
    let paths = env.state_path_for(&env.project_root);
    DiagSink::under(paths.root, env.workspace_id.clone(), "test-session", None)
        .log_path()
        .expect("diagnostic log path")
}

/// Append `[hooks.state]` trust entries for every RimZ-installed codex event,
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

fn stub_agent_binaries(env: &Env, agents: &[&str]) -> PathBuf {
    let dir = env.home_root.join("bin");
    std::fs::create_dir_all(&dir).expect("mkdir agent bin");
    for agent in agents {
        let path = dir.join(agent);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").expect("write agent stub");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod agent stub");
    }
    dir
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

fn stub_mux_version(
    env: &Env,
    dir_name: &str,
    program: &str,
    version_arg: &str,
    version: &str,
) -> PathBuf {
    let dir = env.home_root.join(dir_name);
    std::fs::create_dir_all(&dir).expect("mkdir mux bin");
    let path = dir.join(program);
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nif [ \"${{1:-}}\" = \"{version_arg}\" ]; then printf '%s\\n' '{version}'; exit 0; fi\nexit 0\n"
        ),
    )
    .expect("write mux stub");
    let mut perms = std::fs::metadata(&path).expect("metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod mux stub");
    dir
}

fn path_with_stub_first(stub_dir: &Path) -> String {
    let current = std::env::var_os("PATH").unwrap_or_default();
    format!("{}:{}", stub_dir.display(), current.to_string_lossy())
}

fn path_with_only(dirs: &[PathBuf]) -> OsString {
    std::env::join_paths(dirs).expect("join PATH")
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
    let agents: Vec<(&str, &str)> = remote["agents"]
        .as_array()
        .expect("agents")
        .iter()
        .map(|agent| {
            (
                agent["kind"].as_str().expect("kind"),
                agent["detail"].as_str().expect("detail"),
            )
        })
        .collect();
    assert!(
        agents.contains(&("claude", "enabled, blocked")),
        "{agents:?}"
    );
    assert!(
        agents.contains(&("codex", "enabled, standalone install missing")),
        "{agents:?}"
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
    env.store().append_event(&event).expect("append old event");

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
    rimz::store::atomic::write_temp_then_rename(
        &rt.heartbeat_dir.join("sidebar.old.json"),
        &sidebar,
    )
    .expect("write old sidebar heartbeat");

    let report = doctor_json(
        &env.rimz()
            .args(["doctor", "--json"])
            .output()
            .expect("spawn"),
    );
    let protocols = &report["protocols"];
    assert_eq!(protocols["event"], "rimz.event.v2");
    assert_eq!(protocols["sidebar"], "rimz.plugin.v5");

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
}

#[test]
fn doctor_reports_mixed_build_writers() {
    let env = Env::new();
    let rt = env.runtime_paths();
    rt.ensure_dirs().expect("runtime dirs");

    for (name, build, pane) in [("a", "0f3a9c21d4be", "%31"), ("b", "8e7d6c5b4a39", "%32")] {
        let mut sidebar = SidebarHeartbeat::new(
            env.workspace_id.clone(),
            SidebarInstanceId::new(),
            MuxName::Tmux,
            "rimz-test",
            rt.sock_dir.join(format!("sidebar.{name}.sock")),
            Some(PaneId::from_parts(MuxName::Tmux, pane)),
        );
        sidebar.build = Some(build.to_owned());
        rimz::store::atomic::write_temp_then_rename(
            &rt.heartbeat_dir.join(format!("sidebar.{name}.json")),
            &sidebar,
        )
        .expect("write sidebar heartbeat");
    }

    let report = doctor_json(
        &env.rimz()
            .args(["doctor", "--json"])
            .output()
            .expect("spawn"),
    );
    let writers = report["protocols"]["build_drift"]["writers"]
        .as_array()
        .expect("build drift writers");
    assert!(writers.len() >= 2, "{writers:?}");
    for (build, pane) in [("0f3a9c21d4be", "tmux:%31"), ("8e7d6c5b4a39", "tmux:%32")] {
        let writer = writers
            .iter()
            .find(|writer| writer["build"] == build)
            .unwrap_or_else(|| panic!("writer for {build}: {writers:?}"));
        assert_eq!(writer["sidebar_count"], 1);
        assert_eq!(
            writer["pane_ids"].as_array().expect("pane ids"),
            &[Value::String(pane.to_owned())]
        );
    }

    let output = env.rimz().arg("doctor").output().expect("spawn doctor");
    assert!(
        output.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    assert!(
        stdout.contains("mixed rimz builds writing this workspace")
            && stdout.contains("rimz reload"),
        "human report names mixed builds and remedy:\n{stdout}"
    );
}
