//! `rimz doctor` integration tests. Inject store and heartbeat state directly,
//! then run the binary and assert the report. The JSON report is the stable
//! contract checked here; a smoke test pins the human report's shape.

use jiff::Timestamp;
use rimz::agents::lifecycle::LifecycleSignal;
use rimz::agents::{AgentLifecycleObservation, LaunchParams};
use rimz::diag::DiagSink;
use rimz::diag::record::{DiagEnvelope, DiagEvent, RendererExitCause};
use rimz::ids::{AgentKind, MuxName, PaneId, SidebarInstanceId};
use rimz::store::event::{
    EventEnvelope, LastDeathMarker, MessageEventMethod, SessionDeathAgent, SessionDeathCause,
};
use rimz::store::message::{DeliveryGate, MessageRecord, MessageSender, MessageStatus};
use rimz::wakeup::heartbeat::SidebarHeartbeat;
use serde_json::{Value, json};

use crate::common::{Env, trust_codex_hooks, trust_codex_preflight_hooks};
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;

#[test]
fn doctor_reports_recorded_skipped_settings_in_room_json_and_text() {
    let env = Env::new();
    env.record(&env.project_root);
    let workspace = env.resolve_workspace(&env.project_root);
    let skipped = vec![rimz::workspace::record::SkippedSetting {
        setting: "agents.shell".to_owned(),
        message: "missing shell; fix agents.shell".to_owned(),
    }];
    env.store()
        .set_skipped_settings(&workspace, skipped.clone())
        .unwrap();
    let stub_dir = stub_mux_rooms(&env, StubRoomState::Absent, StubRoomState::Absent);
    let report = doctor_with_mux(&env, &stub_dir, "--zellij");
    assert_eq!(report["mux"]["ready"]["room"]["skipped"], json!(skipped));
    let output = env
        .rimz()
        .args(["--zellij", "doctor"])
        .env("PATH", path_with_only(&[stub_dir]))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("agents.shell") && text.contains("missing shell; fix agents.shell"),
        "{text}"
    );
}

#[test]
fn doctor_empty_dir_warns_only_for_configured_profiles_and_ungranted_accounts() {
    let env = Env::new();
    let report = doctor_json(&env.rimz().args(["doctor", "--json"]).output().unwrap());
    assert_eq!(report["empty_dir"]["rows"], json!([]));
    crate::common::write_definition(
        &env,
        "subagents",
        "research",
        "description: Research\nagent: claude\ntools: [Read]\nempty-dir: true",
        "Research.",
    );
    env.rimz()
        .args(["accounts", "add", "claude", "work"])
        .output()
        .unwrap();
    let report = doctor_json(&env.rimz().args(["doctor", "--json"]).output().unwrap());
    let rows = report["empty_dir"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{report}");
    for row in rows {
        assert_eq!(row["kind"], "claude");
        assert_eq!(row["profiles"], json!(["research"]));
        assert_eq!(row["state"], "undecided");
        assert!(
            row["fix"]
                .as_str()
                .unwrap()
                .contains("rimz trust grant --empty-dir --agents claude")
        );
    }
    let output = env.rimz().arg("doctor").output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("EMPTY-DIR") && text.contains("parent's directory instead"),
        "{text}"
    );
    let bin = crate::common::write_env_dump_shim(&env, "claude");
    env.rimz()
        .env("PATH", crate::common::path_with_front(&bin))
        .args(["trust", "grant", "--empty-dir"])
        .output()
        .unwrap();
    let report = doctor_json(&env.rimz().args(["doctor", "--json"]).output().unwrap());
    assert_eq!(report["empty_dir"]["rows"], json!([]));
}

#[test]
fn doctor_empty_dir_reports_dirty_root_without_workspace() {
    let env = Env::new();
    let root = env.rimz_home().join("empty");
    std::fs::create_dir_all(&root).unwrap();
    let stray = root.join("CLAUDE.md");
    std::fs::write(&stray, "instructions").unwrap();
    let report = doctor_json(
        &env.rimz()
            .current_dir(&env.home_root)
            .args(["doctor", "--json"])
            .output()
            .unwrap(),
    );
    let error = report["empty_dir"]["check"]["unavailable"]["error"]
        .as_str()
        .expect("dirty-root diagnostic");
    assert!(
        error.contains(stray.to_str().unwrap()) && error.contains("remove it"),
        "{error}"
    );
    let output = env
        .rimz()
        .current_dir(&env.home_root)
        .arg("doctor")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&output.stdout).contains(stray.to_str().unwrap()));
    std::fs::remove_file(stray).unwrap();
    let paths = env.store().paths().clone();
    std::fs::create_dir_all(paths.empty_room_dir()).unwrap();
    let stray = paths.empty_room_dir().join("CLAUDE.md");
    std::fs::write(&stray, "instructions").unwrap();
    let report = doctor_json(&env.rimz().args(["doctor", "--json"]).output().unwrap());
    let error = report["empty_dir"]["check"]["unavailable"]["error"]
        .as_str()
        .expect("dirty-room diagnostic");
    assert!(
        error.contains(stray.to_str().unwrap()) && error.contains("remove it"),
        "{error}"
    );
}

#[test]
fn doctor_reports_installed_but_disabled_copilot_hooks() {
    let env = Env::new();
    env.install_agent_hooks("copilot");
    let settings = env.home_root.join(".copilot/settings.json");
    let mut document: Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
    document["disableAllHooks"] = serde_json::json!(true);
    std::fs::write(&settings, serde_json::to_vec(&document).unwrap()).unwrap();
    let output = env.rimz().args(["doctor", "--json"]).output().unwrap();
    let report = doctor_json(&output);
    let row = hook(report["hooks"].as_array().unwrap(), "copilot");
    assert_eq!(row["status"]["state"], "installed_disabled");
    for part in [settings.to_str().unwrap(), "disableAllHooks", "false"] {
        assert!(fix(row).contains(part), "{row}");
    }
    let output = env.rimz().arg("doctor").output().unwrap();
    let rendered = String::from_utf8_lossy(&output.stdout);
    assert!(rendered.contains("installed, disabled"), "{rendered}");
}

#[test]
fn doctor_reports_saved_untrusted_without_a_grant_fix() {
    let env = Env::new();
    let bin = crate::common::write_env_dump_shim(&env, "codex");
    let config = env.home_root.join("codex.toml");
    let original = format!(
        "[projects.{}]\ntrust_level = \"untrusted\"\n",
        toml::Value::String(env.project_root.display().to_string())
    );
    std::fs::write(&config, &original).unwrap();
    let output = env
        .rimz()
        .args(["doctor", "--json"])
        .env("PATH", &bin)
        .env("RIMZ_CODEX_CONFIG", &config)
        .output()
        .unwrap();
    let report = doctor_json(&output);
    let row = &report["folder_trust"]["ready"]["rows"][0];
    assert_eq!(row["state"], "restricted");
    assert!(row["fix"].as_str().unwrap().contains("Open restricted"));
    let output = env
        .rimz()
        .arg("doctor")
        .env("PATH", &bin)
        .env("RIMZ_CODEX_CONFIG", &config)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("restricted (saved untrusted)"), "{stdout}");
    assert_eq!(std::fs::read_to_string(config).unwrap(), original);
}

#[test]
fn doctor_reports_folder_trust_without_writing() {
    let env = Env::new();
    std::fs::create_dir(env.project_root.join(".git")).unwrap();
    std::fs::write(env.project_root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let bin = crate::common::write_env_dump_shim(&env, "codex");
    let config = env.home_root.join("codex.toml");
    std::fs::write(&config, "").unwrap();
    let output = env
        .rimz()
        .args(["doctor", "--json"])
        .env("PATH", &bin)
        .env("RIMZ_CODEX_CONFIG", &config)
        .output()
        .unwrap();
    let report = doctor_json(&output);
    let row = &report["folder_trust"]["ready"]["rows"][0];
    assert_eq!(row["kind"], "codex");
    assert_eq!(row["login"], "default");
    assert_eq!(row["state"], "undecided");
    assert!(
        row["fix"]
            .as_str()
            .unwrap()
            .contains("rimz trust grant --agents codex")
    );
    assert_eq!(std::fs::read_to_string(config).unwrap(), "");
    let output = env.rimz().arg("doctor").env("PATH", &bin).output().unwrap();
    assert!(String::from_utf8_lossy(&output.stdout).contains("FOLDER TRUST"));
}

#[test]
fn doctor_accounts_report_dangling_pins_and_blocked_inheritance() {
    doctor_failed_account(false);
}

#[test]
fn doctor_accounts_report_blocked_inheritance() {
    doctor_failed_account(true);
}

fn doctor_failed_account(blocked_only: bool) {
    let env = Env::new();
    env.record(&env.project_root);
    env.store()
        .switch_room_login(
            &env.resolve_workspace(&env.project_root),
            &AgentKind::new_unchecked("claude"),
            &"gone".parse().unwrap(),
        )
        .unwrap();
    env.write_config(&env.project_root, "[accounts]\ncodex = 'team'\n");
    let output = crate::common::hermetic_providers(&env, &mut env.rimz())
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let report = doctor_json(&output);
    let rows = report["accounts"]["ready"]["rows"].as_array().unwrap();
    if !blocked_only {
        let gone = rows
            .iter()
            .find(|row| row["kind"] == "claude" && row["name"] == "gone")
            .expect("doctor retains the dangling pin");
        assert_eq!(gone["room"], false);
        assert!(
            gone["problem"]
                .as_str()
                .unwrap()
                .contains("unknown claude account")
        );
        let output = crate::common::hermetic_providers(&env, &mut env.rimz())
            .arg("doctor")
            .output()
            .unwrap();
        assert!(output.status.success());
        let human = String::from_utf8_lossy(&output.stdout);
        assert!(
            human.lines().any(|line| line.contains("claude")
                && line.contains("gone")
                && line.contains("this room")),
            "{human}"
        );
    }
    let blocked = rows
        .iter()
        .find(|row| row["kind"] == "codex" && !row["problem"].is_null())
        .expect("doctor reports the blocked inherited kind");
    assert!(blocked["problem"].as_str().unwrap().contains("untrusted"));
    assert_eq!(blocked["room"], false);
}

fn inject_lifecycle(
    env: &Env,
    agent_kind: &str,
    agent_id: &str,
    signal: LifecycleSignal,
    branch: Option<&str>,
) {
    inject_lifecycle_with_launch(
        env,
        agent_kind,
        agent_id,
        signal,
        branch,
        LaunchParams::default(),
    );
}

fn inject_lifecycle_with_launch(
    env: &Env,
    agent_kind: &str,
    agent_id: &str,
    signal: LifecycleSignal,
    branch: Option<&str>,
    launch: LaunchParams,
) {
    let obs = AgentLifecycleObservation {
        ask_queue: None,
        agent_id: Some(agent_id.into()),
        agent_name: None,
        launch,
        signal,
        agent_pid: None,
        account_key: None,
        agent_process_start: None,
        runtime_owner: None,
        worktree_path: Some(env.project_root.display().to_string()),
        worktree_branch: branch.map(ToOwned::to_owned),
        task: None,
        prompt: None,
        description: None,
        transcript_path: None,
        origin: None,
        compacted_from: None,
        usage: rimz::agents::AgentUsageSummary::default(),
        pane_id: None,
        pane_stamp: None,
        parent_agent_id: None,
        background_shells: None,
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
fn doctor_account_standing_is_cache_only_and_empty_paid_credits_are_not_exhaustion() {
    let env = Env::new();
    env.record(&env.project_root);
    let output = env
        .rimz()
        .args(["accounts", "add", "codex", "team"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let now = Timestamp::now();
    let reset = now + std::time::Duration::from_secs(3600);
    inject_lifecycle_with_launch(
        &env,
        "codex",
        "limit-agent",
        LifecycleSignal::TurnStarted { turn_id: None },
        Some("main"),
        LaunchParams {
            login: Some("team".parse().unwrap()),
            ..LaunchParams::default()
        },
    );
    let mut context = rimz::agents::AgentContext::new("codex", now);
    context.turn_error = Some(rimz::agents::AgentTurnError {
        class: rimz::agents::TurnErrorClass::PausedRateLimit,
        at: Timestamp::now(),
        label: Some("limit reached".into()),
    });
    rimz::store::agent_context::write(&env.runtime_paths(), "codex", "limit-agent", &context, None)
        .unwrap();
    let key = "codex@team".parse().unwrap();
    let mut cache = rimz::agents::account::RateLimitsCache::default();
    cache.entries.insert(key, serde_json::from_value(json!({
        "limits": {"windows": [{"duration_mins": 300, "used_percentage": 100, "resets_at": reset, "observed_at": now}]}
    })).unwrap());
    env.publish_rate_limits(&cache);
    let credit_time = now - std::time::Duration::from_secs(60);
    let credits = serde_json::to_vec(&json!({
        "refreshed_at_ms": credit_time.as_millisecond(),
        "logins": {"codex@team": {"ok": true, "observed_at_ms": credit_time.as_millisecond(),
            "extra_credits": {"known": {"remaining_usd": 0.0}}}}
    }))
    .unwrap();
    let credits_path = env.runtime_paths().shared_credits_path();
    std::fs::write(&credits_path, &credits).unwrap();
    for (used, resets_at, expected) in [
        (100, Some(reset), "spent"),
        (100, None, "exhausted"),
        (84, Some(reset), "ready"),
    ] {
        let entry = cache.entries.values_mut().next().unwrap();
        entry.limits.windows[0].used_percentage = Some(used);
        entry.limits.windows[0].resets_at = resets_at;
        env.publish_rate_limits(&cache);
        let report = doctor_json(&env.rimz().args(["doctor", "--json"]).output().unwrap());
        let row = report["accounts"]["ready"]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["kind"] == "codex" && row["name"] == "team")
            .unwrap();
        assert_eq!(row["usage"]["standing"], expected, "{row}");
        assert_eq!(row["usage"]["read_at"], json!(now));
        let agent = &report["agents"]["rows"][0];
        assert_eq!(agent["account"], "codex@team", "{report}");
        assert_eq!(
            agent["attributed_to_account"],
            expected != "ready",
            "{report}"
        );
        assert_eq!(
            row["usage"]["agents"].as_array().unwrap().len(),
            usize::from(expected != "ready")
        );
        assert_eq!(
            row["usage"]["resets_at"],
            json!(if expected == "spent" { resets_at } else { None })
        );
        assert_eq!(std::fs::read(&credits_path).unwrap(), credits);
    }
    cache.entries.values_mut().next().unwrap().limits.windows[0].observed_at = None;
    env.publish_rate_limits(&cache);
    let report = doctor_json(&env.rimz().args(["doctor", "--json"]).output().unwrap());
    let row = report["accounts"]["ready"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["kind"] == "codex" && row["name"] == "team")
        .unwrap();
    assert_eq!(
        row["usage"]["read_at"],
        json!(Timestamp::from_millisecond(credit_time.as_millisecond()).unwrap())
    );
}

#[test]
fn doctor_audit_widens_loop_and_language_server_tables() {
    let env = Env::new();
    std::fs::write(
        env.rimz_home().join("loop.toml"),
        format!(
            "[tasks.context-only]\ncheck = 'true'\nroot = '{}'\nsignal = 'ci.done'\n",
            env.project_root.display()
        ),
    )
    .unwrap();
    let directory = env
        .runtime_root
        .join("rimz/lsp")
        .join(rimz::lsp::registry::key(&env.project_root, "context-server").unwrap());
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("entry.json"), serde_json::to_vec(&json!({
        "root": env.project_root, "server": "context-server", "nonce": "nonce", "broker_pid": std::process::id(),
        "broker_start_token": rimz::proc::process_start_token(std::process::id()).unwrap(),
        "state": {"dormant": {"since_ms": 0, "reason": "memory pressure"}},
        "started_at_ms": 0, "estimate_bytes": 0, "settings_hash": "hash", "request_count": 0, "peak_rss_kb": 0, "leases": []
    })).unwrap()).unwrap();
    let report = doctor_json(&env.rimz().args(["doctor", "--json"]).output().unwrap());
    assert_eq!(report["loop_tasks"]["tasks"][0]["spec"], "check");
    for (args, expanded) in [(vec!["doctor"], false), (vec!["doctor", "--audit"], true)] {
        let output = env.rimz().args(args).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let text = String::from_utf8(output.stdout).unwrap();
        assert_eq!(text.contains("context-only"), expanded, "{text}");
        assert_eq!(text.contains("context-server"), expanded, "{text}");
        if !expanded {
            assert!(
                text.contains("1 task, none need you — rimz loop list"),
                "{text}"
            );
            assert!(
                text.contains("1 more dormant or not started — rimz lsp list --all"),
                "{text}"
            );
        }
    }
}

#[test]
fn doctor_keeps_same_named_cross_root_instance_and_its_worktree_hold() {
    if crate::common::git::git_missing() {
        return;
    }
    let env = Env::new();
    crate::common::git::init_repo(&env.project_root);
    env.record(&env.project_root);
    assert!(
        env.rimz()
            .args(["worktree", "new", "followup"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let worktree = env.home_root.join("project-worktrees/followup");
    let other = env.home_root.join("other");
    std::fs::create_dir(&other).unwrap();
    std::fs::write(
        env.rimz_home().join("loop.toml"),
        format!("[tasks.same]\nroot={other:?}\ncheck='true'\nevery='1h'\n"),
    )
    .unwrap();
    let instances = env
        .state_path_for(&env.project_root)
        .root
        .join("records/loop-instances.json");
    std::fs::create_dir_all(instances.parent().unwrap()).unwrap();
    std::fs::write(
        instances,
        serde_json::to_vec(&json!({"same": {
            "root": env.project_root, "dir": worktree, "check": "true", "signal": "ci.failed", "once": true
        }}))
        .unwrap(),
    )
    .unwrap();

    let report = doctor_json(&env.rimz().args(["doctor", "--json"]).output().unwrap());
    let tasks = report["loop_tasks"]["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 2, "{report}");
    let instance = tasks
        .iter()
        .find(|task| task["root"] == json!(env.project_root))
        .unwrap();
    assert_eq!(instance["name"], "same");
    assert_eq!(instance["holds_worktrees"], json!([worktree]));
    let machine = tasks
        .iter()
        .find(|task| task["root"] == json!(other))
        .unwrap();
    assert_eq!(machine["name"], "same");
    assert_eq!(machine["holds_worktrees"], json!([]));
}

#[test]
fn doctor_names_loop_worktree_holds_in_json_and_normal_text() {
    if crate::common::git::git_missing() {
        return;
    }
    let env = Env::new();
    crate::common::git::init_repo(&env.project_root);
    env.record(&env.project_root);
    assert!(
        env.rimz()
            .args(["worktree", "new", "followup"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let path = env.home_root.join("project-worktrees/followup");
    let subdir = path.join("src");
    std::fs::create_dir(&subdir).unwrap();
    std::fs::write(env.rimz_home().join("loop.toml"), format!("[tasks.nightly]\nagent = 'claude'\nroot = '{}'\ndir = '{}'\nsignal = 'ci.failed'\n[tasks.project]\nagent = 'claude'\nroot = '{}'\nsignal = 'ci.failed'\n", path.display(), subdir.display(), env.project_root.display())).unwrap();
    let report = doctor_json(&env.rimz().args(["doctor", "--json"]).output().unwrap());
    assert_eq!(
        report["loop_tasks"]["tasks"][0]["holds_worktrees"],
        json!([path])
    );
    assert_eq!(
        report["loop_tasks"]["tasks"][1]["holds_worktrees"],
        json!([])
    );
    std::fs::write(
        env.rimz_home().join("loop.toml"),
        format!(
            "[tasks.nightly]\nagent = 'claude'\nroot = '{}'\ndir = '{}'\nsignal = 'ci.failed'\n",
            env.project_root.display(),
            subdir.join("not-created-yet").display()
        ),
    )
    .unwrap();
    let report = doctor_json(&env.rimz().args(["doctor", "--json"]).output().unwrap());
    assert_eq!(
        report["loop_tasks"]["tasks"][0]["holds_worktrees"],
        json!([path])
    );
    let output = env.rimz().arg("doctor").output().unwrap();
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("holds worktree followup")
    );
}

#[test]
fn doctor_flags_clock_tasks_without_a_scheduler() {
    let env = Env::new();
    std::fs::write(env.rimz_home().join("loop.toml"), format!(
        "[tasks.morning]\nagent = 'claude'\nroot = '{}'\nevery = '1m'\n[tasks.signal]\nagent = 'claude'\nroot = '{}'\nsignal = 'ci.failed'\n", env.project_root.display(), env.project_root.display()
    )).unwrap();
    let report = doctor_json(&env.rimz().args(["doctor", "--json"]).output().unwrap());
    assert_eq!(report["loop_tasks"]["timer"]["state"], "not_installed");
    assert_eq!(report["loop_tasks"]["tasks"][0]["room_open"], false);
    assert_eq!(report["loop_tasks"]["unscheduled"], json!(["morning"]));
    let output = env.rimz().arg("doctor").output().unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("rimz loop timer install")
    );

    crate::common::room::seed_sidebar_heartbeat(
        &env.runtime_paths(),
        MuxName::Tmux,
        "rimz-test",
        "sidebar",
    );
    let report = doctor_json(&env.rimz().args(["doctor", "--json"]).output().unwrap());
    assert_eq!(report["loop_tasks"]["tasks"][0]["room_open"], true);
    assert_eq!(report["loop_tasks"]["unscheduled"], json!([]));
}

#[test]
fn doctor_checks_selected_servers_and_reports_startup_causes() {
    let env = Env::new();
    std::fs::write(env.project_root.join("Cargo.toml"), "").unwrap();
    let stub = crate::common::cargo_bin("lsp-server-stub", env!("CARGO_BIN_EXE_lsp-server-stub"));
    let mut machine = rimz::config::MachineConfig::default();
    for (name, command, marker) in [
        (
            "broken",
            json!([
                "sh",
                "-c",
                "echo \"error: Unknown binary 'rust-analyzer' in official toolchain '1.98.1-x86_64-unknown-linux-gnu'.\" >&2; exit 1"
            ]),
            "Cargo.toml",
        ),
        ("healthy", json!([stub]), "Cargo.toml"),
        ("skipped", json!(["does-not-exist"]), "absent.marker"),
        (
            "missing",
            json!(["rimz-doctor-missing-server"]),
            "Cargo.toml",
        ),
    ] {
        machine.lsp.servers.insert(
            name.into(),
            serde_json::from_value(json!({
                "command": command, "extensions": ["rs"], "root-markers": [marker]
            }))
            .unwrap(),
        );
    }
    std::fs::write(
        env.rimz_home().join("config.toml"),
        toml::to_string(&std::collections::BTreeMap::from([("lsp", &machine.lsp)])).unwrap(),
    )
    .unwrap();
    let report = doctor_json(&env.rimz().args(["doctor", "--json"]).output().unwrap());
    let checks = report["lsp"]["ready"]["checks"]
        .as_array()
        .expect("startup checks");
    assert_eq!(checks.len(), 3);
    assert_eq!(checks[0]["server"], "broken");
    assert_eq!(checks[0]["outcome"], "failed");
    assert_eq!(checks[0]["cause"]["exit_code"], 1);
    assert!(
        checks[0]["cause"]["stderr_tail"]
            .as_str()
            .unwrap()
            .contains("Unknown binary")
    );
    assert_eq!(
        checks[0]["fix"],
        "rustup component add rust-analyzer --toolchain 1.98.1-x86_64-unknown-linux-gnu"
    );
    assert_eq!(checks[1]["outcome"], "started");
    assert_eq!(checks[2]["outcome"], "failed");
    assert_eq!(
        checks[2]["fix"],
        "rimz-doctor-missing-server not found on PATH; install it or remove [lsp.servers.missing]"
    );
    let output = env.rimz().arg("doctor").output().unwrap();
    let human = String::from_utf8(output.stdout).unwrap();
    assert!(human.contains("broken cannot start in"), "{human}");
    assert!(human.contains("exit code 1"), "{human}");
    assert!(human.contains("healthy starts"), "{human}");
    assert!(human.contains("fix: rustup component add"), "{human}");
    assert!(human.contains("problems"), "{human}");
}

#[test]
fn doctor_does_not_execute_untrusted_project_servers() {
    let env = Env::new();
    std::fs::create_dir_all(env.project_root.join(".rimz")).unwrap();
    std::fs::write(env.project_root.join("Cargo.toml"), "").unwrap();
    std::fs::write(env.project_root.join(".rimz/config.toml"), "[lsp.servers.untrusted]\ncommand = ['sh', '-c', 'touch doctor-sentinel']\nextensions = ['rs']\nroot-markers = ['Cargo.toml']\n").unwrap();
    let report = doctor_json(&env.rimz().args(["doctor", "--json"]).output().unwrap());
    assert_eq!(report["lsp"]["ready"]["checks"][0]["outcome"], "untrusted");
    assert_eq!(report["lsp"]["ready"]["checks"][0]["fix"], "run rimz trust");
    assert!(!env.project_root.join("doctor-sentinel").exists());
}

#[test]
fn doctor_keeps_the_lsp_table_when_project_config_is_malformed() {
    let env = Env::new();
    std::fs::create_dir_all(env.project_root.join(".rimz")).unwrap();
    std::fs::write(
        env.project_root.join(".rimz/config.toml"),
        "[lsp.servers.broken\n",
    )
    .unwrap();
    let report = doctor_json(&env.rimz().args(["doctor", "--json"]).output().unwrap());
    let lsp = &report["lsp"]["ready"];
    assert!(lsp["servers"].is_array(), "{report}");
    assert!(
        lsp["checks_error"]
            .as_str()
            .is_some_and(|error| error.contains("config.toml")),
        "{report}"
    );
    let human = String::from_utf8(env.rimz().arg("doctor").output().unwrap().stdout).unwrap();
    assert!(
        human.contains("cannot check this checkout's language servers"),
        "{human}"
    );
}

#[test]
fn doctor_does_not_create_state_for_an_unopened_project() {
    let env = Env::new();
    let state = env.state_path_for(&env.project_root);
    assert!(!state.root.exists());
    // Probing tmux is what used to create the socket directory, and a host
    // without tmux on PATH never reaches that read path, so the backend is
    // stubbed rather than left to whatever the machine happens to have.
    let tmux = path_with_only(&[stub_mux_version(&env, "mux-bin", "tmux", "-V", "tmux 3.5")]);

    for (args, path) in [
        (vec!["doctor"], None),
        (vec!["doctor", "--json", "--audit"], None),
        (vec!["doctor", "--mux", "tmux"], Some(&tmux)),
    ] {
        let home_before = tree_listing(&env.rimz_home());
        let runtime_before = tree_listing(&env.runtime_root);
        let mut command = env.rimz();
        command.args(&args);
        if let Some(path) = path {
            command.env("PATH", path);
        }
        let output = command.output().expect("spawn doctor");
        assert!(output.status.success(), "{output:?}");
        assert_eq!(tree_listing(&env.rimz_home()), home_before);
        assert_eq!(tree_listing(&env.runtime_root), runtime_before);
        assert!(!state.root.exists());

        if args.contains(&"--json") {
            let report = doctor_json(&output);
            assert_eq!(report["agents"]["state"], "none");
            assert_eq!(
                report["messages"]["ready"],
                json!({
                    "open": {"queued": 0, "claimed": 0, "sent": 0},
                    "stuck": [],
                    "recent_failures": []
                    ,"failure_counts": {}
                })
            );
        } else {
            let report = String::from_utf8(output.stdout).expect("human report");
            assert!(report.contains("AGENTS\n  none observed"), "{report}");
            assert!(report.contains("MESSAGES\n  no open messages"), "{report}");
        }
    }
}

/// Every entry's size and modification time, so a same-size rewrite and a
/// directory that gained and lost an entry both show up as a difference.
fn tree_listing(root: &Path) -> Vec<(PathBuf, Option<u64>, std::time::SystemTime)> {
    let mut listing = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let metadata = std::fs::symlink_metadata(&path).expect("tree metadata");
        listing.push((
            path.strip_prefix(root)
                .expect("relative path")
                .to_path_buf(),
            metadata.is_file().then_some(metadata.len()),
            metadata.modified().expect("entry mtime"),
        ));
        if metadata.is_dir() {
            for entry in std::fs::read_dir(path).expect("list directory") {
                pending.push(entry.expect("directory entry").path());
            }
        }
    }
    listing.sort();
    listing
}

#[test]
fn doctor_preserves_existing_workspace_record() {
    let env = Env::new();
    inject_lifecycle(
        &env,
        "claude",
        "read-only-agent",
        LifecycleSignal::TurnEnded {
            errored: true,
            parked_on_background: false,
            turn_id: None,
        },
        Some("main"),
    );
    env.record(&env.project_root);
    let record = env.state_path_for(&env.project_root).workspace_record;
    let before = std::fs::read(&record).expect("workspace record before doctor");
    let report = doctor_json(
        &env.rimz()
            .args(["doctor", "--json"])
            .output()
            .expect("spawn doctor"),
    );
    assert_eq!(
        std::fs::read(record).expect("workspace record after doctor"),
        before
    );
    assert_eq!(report["agents"]["state"], "observed");
    assert_eq!(report["agents"]["counts"]["failed"], 1);
    assert_eq!(report["agents"]["rows"][0]["agent_id"], "read-only-agent");
    for args in [["doctor", "--no-log-text"], ["doctor", "--audit"]] {
        let home_before = tree_listing(&env.rimz_home());
        let runtime_before = tree_listing(&env.runtime_root);
        let output = env.rimz().args(args).output().expect("spawn doctor");
        assert!(output.status.success(), "{output:?}");
        assert_eq!(tree_listing(&env.rimz_home()), home_before);
        assert_eq!(tree_listing(&env.runtime_root), runtime_before);
    }
}

#[test]
fn doctor_json_reports_legacy_roots_and_an_agents_home_override() {
    let env = Env::new();
    let legacy = env.state_root().join("rimz");
    std::fs::create_dir_all(&legacy).expect("seed legacy root");
    let agents_home = env.home_root.join("agents-home");
    std::fs::create_dir_all(&agents_home).expect("mkdir agents home");

    let report = doctor_json(
        &env.rimz()
            .env("RIMZ_AGENTS_HOME", &agents_home)
            .args(["doctor", "--json"])
            .output()
            .expect("spawn doctor"),
    );
    let home = &report["home"];
    assert_eq!(home["path"], env.rimz_home().display().to_string());
    assert_eq!(home["from_env"], true);
    assert_eq!(
        home["agents_home_override"],
        agents_home.display().to_string()
    );
    assert_eq!(home["legacy_roots"], json!([legacy.display().to_string()]));
    let fix = home["fix"].as_str().expect("fix beside legacy roots");
    assert!(fix.contains("moving-from-the-xdg-roots"), "{fix}");
    let output = env.rimz().arg("doctor").output().unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    let (body, closing) = text.split_at(text.rfind("\n✗ ").or_else(|| text.rfind("\n! ")).unwrap());
    assert!(
        body.contains("legacy roots ~/state/rimz are still on disk and not read"),
        "{body}"
    );
    assert!(!body.contains("fix:"), "{body}");
    assert!(
        closing.contains("move what you still need from ~/state/rimz"),
        "{closing}"
    );
}

#[test]
fn doctor_reports_a_legacy_cache_for_deletion_without_blocking_start() {
    let env = Env::new();
    let cache = env.cache_root().join("rimz");
    std::fs::create_dir_all(&cache).expect("seed legacy cache");
    let report = doctor_json(
        &env.rimz()
            .args(["doctor", "--json"])
            .output()
            .expect("spawn doctor"),
    );
    assert_eq!(report["home"]["legacy_roots"], json!([cache]));
    let fix = report["home"]["fix"].as_str().expect("cache fix");
    assert!(fix.contains("delete the obsolete cache"), "{fix}");
    assert!(fix.contains("moving-from-the-xdg-roots"), "{fix}");
    assert!(!fix.contains("move"), "{fix}");
    assert!(!fix.contains("or set RIMZ_HOME"), "{fix}");

    // Workspace resolution still refuses a missing project before any birth.
    let missing_project = env.home_root.join("missing-project");
    assert!(!env.rimz_home().join("config.toml").exists());
    let output = env
        .rimz()
        .args(["start", "--no-attach"])
        .arg(&missing_project)
        .output()
        .expect("start with cache only");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("resolving workspace at"), "{stderr}");
    assert!(
        stderr.contains(&missing_project.display().to_string()),
        "{stderr}"
    );
    assert!(!stderr.contains("legacy RimZ roots exist"), "{stderr}");

    let config = env.config_root().join("rimz");
    std::fs::create_dir_all(&config).expect("seed legacy config");
    let output = env
        .rimz()
        .args(["start", "--no-attach"])
        .arg(&missing_project)
        .output()
        .expect("start with legacy config");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("resolving workspace at"), "{stderr}");
    assert!(
        stderr.contains(&missing_project.display().to_string()),
        "{stderr}"
    );
    assert!(!env.rimz_home().join("config.toml").exists());
}

#[test]
fn doctor_json_folds_one_row_per_agent() {
    let env = Env::new();
    env.record(&env.project_root);
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
        LifecycleSignal::TurnStarted { turn_id: None },
        Some("main"),
    );
    inject_lifecycle(
        &env,
        "claude",
        "claude-session-abc",
        LifecycleSignal::TurnEnded {
            errored: true,
            parked_on_background: false,
            turn_id: None,
        },
        Some("main"),
    );
    inject_lifecycle(
        &env,
        "codex",
        "codex-session-xyz",
        LifecycleSignal::TurnStarted { turn_id: None },
        Some("feature-migration"),
    );
    let now = Timestamp::now();
    let mut context = rimz::agents::AgentContext::new("claude", now);
    context.turn_error = Some(rimz::agents::AgentTurnError {
        class: rimz::agents::TurnErrorClass::Failed,
        at: now,
        label: Some("request rejected".into()),
    });
    rimz::store::agent_context::write(
        &env.runtime_paths(),
        "claude",
        "claude-session-abc",
        &context,
        None,
    )
    .unwrap();

    let output = env
        .rimz()
        .args(["doctor", "--json"])
        .output()
        .expect("spawn doctor");
    let report = doctor_json(&output);
    assert_eq!(report["schema"], "rimz.doctor.v1");
    assert_eq!(report["audit"], false);

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
    assert_eq!(rows[0]["handle"], "@claude");
    assert_eq!(rows[0]["account"], "claude@default");
    assert_eq!(rows[0]["turn_error"], "request rejected");
    assert_eq!(rows[0]["attributed_to_account"], false);

    let audit = doctor_json(
        &env.rimz()
            .args(["doctor", "--audit", "--json"])
            .output()
            .expect("spawn audit doctor"),
    );
    let rows = audit["agents"]["rows"].as_array().expect("audit rows");
    assert_eq!(audit["audit"], true);
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
fn doctor_audit_does_not_turn_ended_failures_into_findings() {
    let env = Env::new();
    env.record(&env.project_root);
    for id in ["active", "ended"] {
        inject_lifecycle(
            &env,
            "claude",
            id,
            LifecycleSignal::TurnStarted { turn_id: None },
            None,
        );
        inject_lifecycle(
            &env,
            "claude",
            id,
            LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: false,
                turn_id: None,
            },
            None,
        );
    }
    inject_lifecycle(&env, "claude", "ended", LifecycleSignal::Ended, None);
    for signal in [
        LifecycleSignal::Registered,
        LifecycleSignal::TurnStarted { turn_id: None },
        LifecycleSignal::TurnEnded {
            errored: true,
            parked_on_background: false,
            turn_id: None,
        },
        LifecycleSignal::Ended,
    ] {
        inject_lifecycle_with_launch(
            &env,
            "codex",
            "ended-child",
            signal,
            None,
            LaunchParams {
                parent_agent_id: Some("active".into()),
                parent_agent_kind: Some(AgentKind::new_unchecked("claude")),
                launch_depth: Some(1),
                ..Default::default()
            },
        );
    }
    let runtime = env
        .store()
        .runtime_projection(rimz::RuntimeScope::Runtime)
        .unwrap();
    let child = runtime
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "ended-child")
        .expect("ended launched child remains visible under its live parent");
    assert!(
        child.ended_at.is_some() && child.is_launched_child(),
        "{child:?}"
    );
    assert_eq!(child.status, rimz::agents::AgentStatus::Failed);
    let closing = |audit| {
        let mut command = env.rimz();
        command.arg("doctor");
        if audit {
            command.arg("--audit");
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let text = String::from_utf8(output.stdout).unwrap();
        let start = text
            .lines()
            .position(|line| {
                line.contains(" problems in ")
                    || line.contains(" problem in ")
                    || line.contains(" warnings in ")
                    || line.contains(" warning in ")
                    || line.starts_with("✓ everything")
            })
            .unwrap();
        let closing = text.lines().skip(start).collect::<Vec<_>>().join("\n");
        assert!(!closing.contains("on codex@default is failed"), "{closing}");
        closing
    };
    assert_eq!(closing(false), closing(true));
}

#[test]
fn doctor_counts_an_invalid_loop_action() {
    let env = Env::new();
    std::fs::write(
        env.rimz_home().join("loop.toml"),
        format!(
            "[tasks.broken]\nroot = '{}'\nsignal = 'ci.done'\n",
            env.project_root.display()
        ),
    )
    .unwrap();
    let report = doctor_json(&env.rimz().args(["doctor", "--json"]).output().unwrap());
    assert_eq!(report["loop_tasks"]["tasks"][0]["valid"], false, "{report}");
    let output = env.rimz().arg("doctor").output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("✗ LOOP TASKS: broken:"), "{text}");
}

#[test]
fn doctor_json_reports_agent_hook_install_and_trust_states() {
    let env = Env::new();
    let stub_dir = stub_agent_binaries(&env, &["codex", "claude", "kiro-cli"]);
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
    assert_eq!(kiro["status"]["state"], "not_installed");
    assert!(
        fix(kiro).contains("rimz hooks install kiro"),
        "names the kiro wiring command: {kiro}"
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
    let stub_dir = stub_agent_binaries(&env, &["codex"]);
    env.install_agent_hooks("codex");
    trust_codex_preflight_hooks(&env);
    let report = doctor_json(
        &env.rimz()
            .args(["doctor", "--json"])
            .env("PATH", &stub_dir)
            .output()
            .expect("spawn"),
    );
    let codex = hook(report["hooks"].as_array().expect("hooks array"), "codex");
    assert_eq!(codex["status"]["state"], "installed_untrusted");
    assert_eq!(codex["status"]["events"], json!(["Interrupt"]));

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
        "TEMP UNITS",
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
        "the closing block carries the wiring command:\n{stdout}"
    );
    let (body, closing) = stdout.split_at(stdout.rfind("\n✗ ").unwrap());
    assert!(
        !body.contains("fix:")
            && !body
                .lines()
                .any(|line| line.split_whitespace().any(|word| word == "FIX")),
        "{body}"
    );
    assert!(
        closing.contains("✗ HOOKS: claude not installed")
            && closing.contains("fix: run `rimz hooks install claude`"),
        "{closing}"
    );
    let not_detected = stdout
        .lines()
        .find(|line| line.contains("not found on this machine:"))
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
fn doctor_default_json_omits_unmeasured_disk_bytes() {
    let env = Env::new();
    let report = doctor_json(&env.rimz().args(["doctor", "--json"]).output().unwrap());
    assert!(report.get("disk_usage").is_none(), "{report}");
    let units = &report["temp_units"]["ready"];
    assert!(units.is_object(), "{report}");
    assert!(units.get("reclaimable_bytes").is_none(), "{units}");
    assert!(units.get("in_use_bytes").is_none(), "{units}");
}

#[test]
fn doctor_audit_json_keeps_measured_disk_bytes() {
    let env = Env::new();
    let report = doctor_json(
        &env.rimz()
            .args(["doctor", "--audit", "--json"])
            .output()
            .unwrap(),
    );
    assert!(report["disk_usage"]["total_bytes"].is_u64(), "{report}");
    let units = &report["temp_units"]["ready"];
    assert!(units["reclaimable_bytes"].is_u64(), "{units}");
    assert!(units["in_use_bytes"].is_u64(), "{units}");
}

#[test]
fn doctor_reclaimable_empty_units_warn_in_both_modes() {
    let env = Env::new();
    env.record(&env.project_root);
    write_machine_config(
        &env,
        "[gc]\ntmp_floor_percent = 10\ntmp_floor_min = \"0\"\n",
    );
    std::fs::create_dir_all(
        env.state_path_for(&env.project_root)
            .temp_unit_dir(Some("empty")),
    )
    .unwrap();
    for audit in [false, true] {
        let mut command = env.rimz();
        command.arg("doctor").env("RIMZ_TEST_DISK_SPACE", "1,100");
        if audit {
            command.arg("--audit");
        }
        let output = command.output().unwrap();
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(
            text.contains("rimz gc --tmp-floor"),
            "audit={audit}: {text}"
        );
        assert!(!text.contains("stop or restart"), "audit={audit}: {text}");
    }
}

#[test]
fn doctor_temp_units_reports_pressure_without_writing_state() {
    let env = Env::new();
    env.record(&env.project_root);
    write_machine_config(
        &env,
        "[gc]\ntmp_floor_percent = 10\ntmp_floor_min = \"0\"\n",
    );
    let paths = env.state_path_for(&env.project_root);
    let unit = paths.temp_unit_dir(Some("orphan"));
    std::fs::create_dir_all(&unit).unwrap();
    std::fs::write(unit.join("scratch"), "data").unwrap();
    let before = tree_listing(&env.rimz_home());
    let output = env
        .rimz()
        .args(["doctor", "--json"])
        .env("RIMZ_TEST_DISK_SPACE", "1,100")
        .output()
        .unwrap();
    let report = doctor_json(&output);
    assert_eq!(report["temp_units"]["ready"]["free_bytes"], 1);
    assert_eq!(report["temp_units"]["ready"]["floor_bytes"], 10);
    assert_eq!(report["temp_units"]["ready"]["reclaimable_units"], 1);
    assert_eq!(report["temp_units"]["ready"]["past_grace_units"], 0);
    assert_eq!(report["temp_units"]["ready"]["in_use_units"], 0);
    assert_eq!(tree_listing(&env.rimz_home()), before);
    let text = env
        .rimz()
        .arg("doctor")
        .env("RIMZ_TEST_DISK_SPACE", "1,100")
        .output()
        .unwrap();
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains("TEMP UNITS"), "{text}");
    assert!(text.contains("rimz gc --tmp-floor"), "{text}");
    std::fs::remove_dir_all(&unit).unwrap();
    let text = env
        .rimz()
        .arg("doctor")
        .env("RIMZ_TEST_DISK_SPACE", "1,100")
        .output()
        .unwrap();
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains("stop or restart"), "{text}");
}

#[test]
fn doctor_reports_unparseable_machine_config_in_json_and_human_output() {
    let env = Env::new();
    let path = env.rimz_home().join("theme.toml");
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
    assert_eq!(
        broken[0]["error"],
        format!(
            "line 3: `max_cols` is defined more than once in the same table; fix: remove the extra `max_cols` at {}:3, then re-run",
            path.display()
        ),
        "classified parse error: {broken:?}",
    );
    assert_eq!(report["remote_control"]["state"], "unavailable");
    assert_eq!(
        report["remote_control"]["error"],
        format!(
            "line 3: `max_cols` is defined more than once in the same table; fix: remove the extra `max_cols` at {}:3, then re-run",
            path.display()
        )
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
        stdout.contains("line 3: `max_cols` is defined more than once"),
        "{stdout}",
    );
    let remote = stdout
        .lines()
        .find(|line| line.contains("config unavailable"))
        .expect("remote-control config failure");
    assert!(!remote.contains("fix:"), "{remote}");
    let (body, closing) = stdout.split_at(stdout.rfind("\n✗ ").unwrap());
    assert!(!body.contains("fix:"), "{body}");
    assert!(
        closing.contains("fix: remove the extra `max_cols`"),
        "{closing}"
    );
}

#[test]
fn doctor_classifies_an_unparseable_project_config() {
    let env = Env::new();
    let path = env.project_root.join(".rimz/config.toml");
    env.write_config(
        &env.project_root,
        "[profiles.planner]\nagent = \"claude\"\nagent = \"codex\"\n",
    );

    let report = doctor_json(
        &env.rimz()
            .args(["doctor", "--json"])
            .output()
            .expect("spawn doctor"),
    );

    assert_eq!(
        report["trust"]["unavailable"]["error"],
        format!(
            "line 3: `agent` is defined more than once in the same table; fix: remove the extra `agent` at {}:3, then re-run",
            path.display()
        )
    );
}

#[test]
fn doctor_ttyd_follows_web_enabled_on_both_backends() {
    for (mux, version_arg, version) in [
        ("zellij", "--version", "zellij 0.44.3"),
        ("tmux", "-V", "tmux 3.5"),
    ] {
        let env = Env::new();
        let stub_dir = stub_mux_version(&env, "mux-bin", mux, version_arg, version);
        let path = path_with_only(&[stub_dir]);

        for enabled in [true, false] {
            if !enabled {
                write_machine_config(&env, "[web]\nenabled = false\n");
            }
            let report = doctor_json(
                &env.rimz()
                    .args(["doctor", "--json", "--mux", mux])
                    .env("PATH", &path)
                    .output()
                    .expect("spawn doctor"),
            );
            let ready = &report["mux"]["ready"];
            assert_eq!(ready["name"], mux, "{report}");
            assert_eq!(ready.get("ttyd").is_some(), enabled, "{report}");
        }
    }
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
        concat!(
            "INFO boot\nWARN first warning\nINFO WARN mid-line ignored\nERROR failed\nPanic occured: boom\n",
            "ERROR a non-fatal error occured\nCaused by:\n    0: reading /private/doctor-path-marker\n    1: doctor-cause-marker\n",
        ),
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
    assert_eq!(log["scope"]["kind"], "host_user");
    assert_eq!(log["scope"]["uid"], uid);
    assert_eq!(log["logical_records"], 6);
    assert_eq!(log["problem_records"], 4);
    assert_eq!(log["log_text_omitted"], false);
    assert_eq!(log["omitted_issue_groups"], 0);
    let issues = log["issues"].as_array().expect("issues");
    assert_eq!(issues.len(), 4, "{issues:?}");
    assert_eq!(issues[0]["source_severity"], "warn");
    assert_eq!(issues[0]["state"], "investigate");
    assert_eq!(issues[0]["impact"], "warn");
    assert_eq!(issues[0]["samples"][0], "WARN first warning");
    assert_eq!(issues[1]["source_severity"], "error");
    assert_eq!(issues[1]["samples"][0], "ERROR failed");
    assert_eq!(issues[2]["source_severity"], "panic");
    assert_eq!(issues[2]["impact"], "alarm");
    assert_eq!(issues[2]["samples"][0], "Panic occured: boom");
    assert_eq!(
        issues[3]["summary"],
        "reading /private/doctor-path-marker: doctor-cause-marker"
    );
    assert!(
        issues[3]["samples"][0]
            .as_str()
            .unwrap()
            .contains("Caused by:\n")
    );

    let stripped = doctor_json(
        &env.rimz()
            .args(["doctor", "--json", "--no-log-text"])
            .env("PATH", path_with_only(std::slice::from_ref(&stub_dir)))
            .env("TMPDIR", &tmp)
            .output()
            .expect("spawn stripped doctor"),
    );
    let serialized = serde_json::to_string(&stripped).expect("serialize report");
    for marker in [
        "first warning",
        "ERROR failed",
        "Panic occured: boom",
        "/private/doctor-path-marker",
        "doctor-cause-marker",
        "Caused by:",
    ] {
        assert!(!serialized.contains(marker), "leaked {marker}");
    }
    let stripped_log = &stripped["mux"]["ready"]["log"];
    assert_eq!(stripped_log["log_text_omitted"], true);
    assert_eq!(stripped_log["problem_records"], log["problem_records"]);
    let stripped_issues = stripped_log["issues"].as_array().expect("stripped issues");
    assert_eq!(stripped_issues.len(), issues.len());
    for (included, omitted) in issues.iter().zip(stripped_issues) {
        assert_eq!(included["occurrences"], omitted["occurrences"]);
        assert!(omitted["samples"].as_array().unwrap().is_empty());
        assert_eq!(omitted["evidence_truncated"], false);
    }

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
    assert_eq!(clean["mux"]["ready"]["log"]["problem_records"], 0);
    assert_eq!(
        clean["mux"]["ready"]["log"]["issues"]
            .as_array()
            .expect("issues")
            .len(),
        0
    );
}

#[test]
fn doctor_json_surfaces_stuck_and_failed_messages() {
    let env = Env::new();
    env.record(&env.project_root);
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
    assert_eq!(stuck_rows[0]["target"], "@codex#main");
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
    assert_eq!(messages["failure_counts"], json!({"errored": 1}));
    assert!(
        !failures.iter().any(|row| row["message_id"] == delivered_id),
        "delivered terminal events stay out of doctor health rows: {failures:?}"
    );
    let listed = env
        .rimz()
        .args(["message", "list", "--all", "--system"])
        .output()
        .unwrap();
    assert!(listed.status.success(), "{listed:?}");
    let listed = String::from_utf8(listed.stdout).unwrap();
    assert!(listed.contains(&failed_id), "{listed}");
    for (args, show_failure) in [(vec!["doctor"], false), (vec!["doctor", "--audit"], true)] {
        let output = env.rimz().args(args).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let text = String::from_utf8(output.stdout).unwrap();
        assert_eq!(text.contains(&failed_id), show_failure, "{text}");
        assert!(text.contains("1 recent failure: 1 errored"), "{text}");
    }
    for _ in 0..11 {
        let mut failure = MessageRecord::new(
            env.workspace_id.clone(),
            agent,
            "timed out".into(),
            DeliveryGate::Done,
        );
        failure.status = MessageStatus::TimedOut;
        let event = EventEnvelope::message_event(
            &failure,
            "test-session",
            MessageEventMethod::TimedOut,
            Some("deadline passed"),
        );
        env.store().append_event(&event).unwrap();
        env.store().append_event(&event).unwrap();
    }
    for (args, expected_rows) in [
        (vec!["doctor", "--json"], 10),
        (vec!["doctor", "--json", "--audit"], 12),
    ] {
        let report = doctor_json(&env.rimz().args(args).output().unwrap());
        let messages = &report["messages"]["ready"];
        assert_eq!(
            messages["failure_counts"],
            json!({"errored": 1, "timed_out": 11})
        );
        assert_eq!(
            messages["recent_failures"].as_array().unwrap().len(),
            expected_rows
        );
    }
}

#[test]
fn doctor_clear_dismisses_recorded_history() {
    let env = Env::new();
    env.record(&env.project_root);
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
        report["diagnostics"]["incidents"]
            .as_array()
            .expect("diagnostic incidents")
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
        next["diagnostics"]["incidents"]
            .as_array()
            .expect("diagnostic incidents")
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
        fresh["diagnostics"]["incidents"]
            .as_array()
            .expect("diagnostic incidents")
            .len(),
        1,
        "post-clear records remain visible: {fresh}"
    );
}

/// `--clear` covers the multiplexer's own log too: a server log spans days of
/// unrelated sessions, so a report that ignores the watermark keeps re-raising
/// history the operator already dismissed.
#[test]
fn doctor_clear_cuts_the_multiplexer_log_at_the_watermark() {
    let env = Env::new();
    let stub_dir = stub_mux_version(&env, "mux-bin", "zellij", "--version", "zellij 0.44.3");
    let tmp = env.home_root.join("tmp");
    let uid = nix::unistd::Uid::current().as_raw();
    let log_path = tmp
        .join(format!("zellij-{uid}"))
        .join("zellij-log")
        .join("zellij.log");
    std::fs::create_dir_all(log_path.parent().expect("log parent")).expect("mkdir log dir");

    // Zellij stamps records in local wall-clock time with no offset.
    let stamp = |offset: jiff::Span| {
        jiff::Zoned::now()
            .checked_add(offset)
            .expect("shifted time")
            .strftime("%Y-%m-%d %H:%M:%S%.3f")
            .to_string()
    };
    let record = |at: &str, message: &str| {
        format!(
            "ERROR  |zellij_server::pty       | {at} [pty] zellij-server/src/pty.rs:9: {message}\n"
        )
    };
    std::fs::write(
        &log_path,
        record(&stamp(jiff::Span::new().hours(-2)), "old trouble"),
    )
    .expect("write zellij log");

    let cleared = env
        .rimz()
        .args(["doctor", "--clear", "--json"])
        .env("PATH", path_with_only(std::slice::from_ref(&stub_dir)))
        .env("TMPDIR", &tmp)
        .output()
        .expect("spawn clear doctor");
    let cleared = doctor_json(&cleared);
    let log = &cleared["mux"]["ready"]["log"];
    assert_eq!(log["records_before_cutoff"], 1);
    assert_eq!(
        log["problem_records"], 0,
        "the dismissed record stops counting: {log}"
    );

    let mut appended = std::fs::read_to_string(&log_path).expect("read log");
    appended.push_str(&record(&stamp(jiff::Span::new().minutes(5)), "new trouble"));
    std::fs::write(&log_path, appended).expect("append zellij log");

    let after = doctor_json(
        &env.rimz()
            .args(["doctor", "--json"])
            .env("PATH", path_with_only(&[stub_dir]))
            .env("TMPDIR", &tmp)
            .output()
            .expect("spawn doctor after clear"),
    );
    let log = &after["mux"]["ready"]["log"];
    assert_eq!(log["records_before_cutoff"], 1);
    assert_eq!(log["problem_records"], 1);
    let issues = log["issues"].as_array().expect("issues");
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(issues[0]["summary"], "new trouble");
}

#[test]
fn doctor_labels_stale_build_diagnostics() {
    let env = Env::new();
    env.record(&env.project_root);
    write_diag_record(&env, rimz::utils::time::unix_now_ms(), Some("stale-build"));

    let report = doctor_json(
        &env.rimz()
            .args(["doctor", "--json"])
            .output()
            .expect("spawn doctor"),
    );
    let rows = report["diagnostics"]["incidents"]
        .as_array()
        .expect("diagnostic incidents");
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
    rimz::disk::atomic::write_temp_then_rename(&paths.last_death_marker, &marker)
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
    write_diag_event(
        env,
        at_ms,
        build,
        DiagEvent::RendererExit {
            cause: RendererExitCause::DegradedGaveUp,
        },
    );
}

fn write_diag_event(env: &Env, at_ms: u64, build: Option<&str>, event: DiagEvent) {
    let mut record = DiagEnvelope::new(
        env.workspace_id.clone(),
        "test-session".to_owned(),
        None,
        at_ms,
        event,
    );
    record.build = build.map(ToOwned::to_owned);
    let mut line = serde_json::to_vec(&record).expect("encode diagnostic record");
    line.push(b'\n');
    rimz::disk::atomic::append_record_bytes(&diag_log_path(env), &line)
        .expect("append diagnostic record");
}

fn diag_log_path(env: &Env) -> PathBuf {
    let paths = env.state_path_for(&env.project_root);
    DiagSink::under(paths.root, env.workspace_id.clone(), "test-session", None)
        .log_path()
        .expect("diagnostic log path")
}

fn hook_lag() -> DiagEvent {
    DiagEvent::HookDrainLag {
        ingress: rimz::ids::EventId::new(),
        hook_event: Some("Stop".into()),
        lag_ms: 1_399_944,
        pending_bytes: 5_474_624,
    }
}

#[test]
fn doctor_recent_hook_lag_counts_a_diagnostics_warning() {
    let env = Env::new();
    env.record(&env.project_root);
    write_diag_event(
        &env,
        rimz::utils::time::unix_now_ms() - 60_000,
        None,
        hook_lag(),
    );
    let output = env.rimz().arg("doctor").output().unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.lines().any(|line| line.contains("warning")
            && line.contains(" in ")
            && line.contains("DIAGNOSTICS")),
        "{text}"
    );
    let report = doctor_json(&env.rimz().args(["doctor", "--json"]).output().unwrap());
    let incident = &report["diagnostics"]["incidents"][0];
    assert_eq!(incident["state"], "investigate");
    assert_eq!(incident["impact"], "warn");
    assert_eq!(
        incident["drain_lag"],
        json!({"worst_lag_ms": 1_399_944, "peak_pending_bytes": 5_474_624})
    );
}

#[test]
fn doctor_quiet_hook_lag_is_settled_without_a_diagnostics_warning() {
    let env = Env::new();
    env.record(&env.project_root);
    write_diag_event(
        &env,
        rimz::utils::time::unix_now_ms() - 1_200_000,
        None,
        hook_lag(),
    );
    let report = doctor_json(&env.rimz().args(["doctor", "--json"]).output().unwrap());
    let incident = &report["diagnostics"]["incidents"][0];
    assert_eq!(incident["state"], "recovered");
    assert_eq!(incident["impact"], "info");
    let output = env.rimz().arg("doctor").output().unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.contains("1 handled and closed themselves: hook_drain_lag"),
        "{text}"
    );
    assert!(
        !text.lines().any(|line| line.contains("warning")
            && line.contains(" in ")
            && line.contains("DIAGNOSTICS")),
        "{text}"
    );
}

#[test]
fn doctor_clear_dismisses_recent_hook_lag() {
    let env = Env::new();
    env.record(&env.project_root);
    write_diag_event(
        &env,
        rimz::utils::time::unix_now_ms() - 60_000,
        None,
        hook_lag(),
    );
    let report = doctor_json(
        &env.rimz()
            .args(["doctor", "--clear", "--json"])
            .output()
            .unwrap(),
    );
    assert!(
        report["diagnostics"]["incidents"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let output = env.rimz().arg("doctor").output().unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.contains("hook_drain_lag"), "{text}");
    assert!(
        diag_log_path(&env).is_file(),
        "clearing history preserves the evidence"
    );
}

fn write_machine_config(env: &Env, text: &str) -> PathBuf {
    let dir = env.rimz_home();
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
            "#!/bin/sh\nif [ \"${{1:-}}\" = \"-S\" ]; then shift 3; fi\nif [ \"${{1:-}}\" = \"{version_arg}\" ]; then printf '%s\\n' '{version}'; exit 0; fi\nexit 0\n"
        ),
    )
    .expect("write mux stub");
    let mut perms = std::fs::metadata(&path).expect("metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod mux stub");
    dir
}

#[derive(Clone, Copy)]
enum StubRoomState {
    Live,
    Exited,
    Absent,
    Unavailable,
}

fn workspace_session_name(env: &Env) -> String {
    env.state_path_for(&env.project_root).dir_name.to_string()
}

fn stub_mux_rooms(env: &Env, zellij: StubRoomState, tmux: StubRoomState) -> PathBuf {
    let dir = env.home_root.join("mux-rooms");
    std::fs::create_dir_all(&dir).expect("mkdir mux rooms");
    let session = workspace_session_name(env);
    let zellij_list = match zellij {
        StubRoomState::Live => format!("printf '%s [Created 1m ago]\\n' '{session}'; exit 0"),
        StubRoomState::Exited => format!(
            "printf '%s [Created 1m ago] (EXITED - attach to resurrect)\\n' '{session}'; exit 0"
        ),
        StubRoomState::Absent => {
            "printf '%s\\n' 'No active zellij sessions found.' >&2; exit 1".to_owned()
        }
        StubRoomState::Unavailable => "printf '%s\\n' 'permission denied' >&2; exit 1".to_owned(),
    };
    write_executable(
        &dir.join("zellij"),
        &format!(
            "#!/bin/sh\nif [ \"${{1:-}}\" = \"--version\" ]; then printf '%s\\n' 'zellij 0.44.3'; exit 0; fi\nif [ \"${{1:-}}\" = \"list-sessions\" ]; then {zellij_list}; fi\nexit 1\n"
        ),
    );

    let tmux_list = match tmux {
        StubRoomState::Live | StubRoomState::Exited => {
            format!("printf '%s\\n' '{session}'; exit 0")
        }
        StubRoomState::Absent => "printf '%s\\n' 'no server running' >&2; exit 1".to_owned(),
        StubRoomState::Unavailable => "printf '%s\\n' 'permission denied' >&2; exit 1".to_owned(),
    };
    // RimZ addresses its own server, so every tmux argv leads with
    // `-S <socket> -u`; the stub steps past it before matching the verb.
    write_executable(
        &dir.join("tmux"),
        &format!(
            "#!/bin/sh\nif [ \"${{1:-}}\" = \"-S\" ]; then shift 3; fi\nif [ \"${{1:-}}\" = \"-V\" ]; then printf '%s\\n' 'tmux 3.5'; exit 0; fi\nif [ \"${{1:-}}\" = \"list-sessions\" ]; then {tmux_list}; fi\nexit 1\n"
        ),
    );
    dir
}

fn write_executable(path: &Path, contents: &str) {
    std::fs::write(path, contents).expect("write executable stub");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod executable stub");
}

/// The stubs list a live Zellij room they can never answer a health probe
/// for, so bound the probe: at the default budget every doctor call on a live
/// stub waits the full 8 s before reporting the same outcome.
fn doctor_with_mux(env: &Env, stub_dir: &Path, flag: &str) -> Value {
    doctor_json(
        &env.rimz()
            .args([flag, "doctor", "--json"])
            .env("PATH", path_with_only(&[stub_dir.to_path_buf()]))
            .env("RIMZ_TEST_ZELLIJ_HEALTH_PROBE_MS", "100")
            .output()
            .expect("spawn doctor with mux stubs"),
    )
}

fn write_presence_stamp(env: &Env, mux: Option<MuxName>, session_name: Option<&str>) {
    let runtime = env.runtime_paths();
    runtime.ensure_dirs().expect("runtime dirs");
    let stamp = rimz::sidebar::cache::PresenceStamp {
        written_at_ms: rimz::utils::time::unix_now_ms(),
        mux,
        session_name: session_name.map(ToOwned::to_owned),
    };
    rimz::disk::atomic::write_temp_then_rename_cache(
        &rimz::sidebar::cache::presence_stamp_path(&runtime),
        &stamp,
    )
    .expect("write presence stamp");
}

#[test]
fn doctor_scopes_room_health_and_presence_to_selected_backend() {
    let env = Env::new();
    let stub_dir = stub_mux_rooms(&env, StubRoomState::Live, StubRoomState::Absent);

    let report = doctor_with_mux(&env, &stub_dir, "--tmux");
    let mux = &report["mux"]["ready"];
    assert_eq!(mux["room"]["selected_state"]["state"], "absent");
    assert_eq!(mux["room"]["live_on"], serde_json::json!(["zellij"]));
    assert!(mux.get("session_health").is_none(), "{mux}");
    assert_eq!(mux["presence"]["mode"], "not_applicable");
    assert!(
        mux["presence"]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("live on zellij, not tmux")),
        "{mux}"
    );
    assert_eq!(mux["version"]["state"], "reported");
    assert!(mux.get("capabilities").is_some(), "{mux}");
    assert!(mux.get("binaries").is_some(), "{mux}");
    assert!(mux.get("log").is_some(), "{mux}");
}

#[test]
fn doctor_accepts_only_matching_or_unambiguous_legacy_presence_stamps() {
    let env = Env::new();
    let stub_dir = stub_mux_rooms(&env, StubRoomState::Live, StubRoomState::Absent);
    let session = workspace_session_name(&env);

    write_presence_stamp(&env, Some(MuxName::Zellij), Some(&session));
    let report = doctor_with_mux(&env, &stub_dir, "--zellij");
    assert_eq!(
        report["mux"]["ready"]["room"]["selected_state"]["state"],
        "live"
    );
    assert_eq!(report["mux"]["ready"]["presence"]["mode"], "event");

    write_presence_stamp(&env, Some(MuxName::Zellij), None);
    let report = doctor_with_mux(&env, &stub_dir, "--zellij");
    assert_eq!(report["mux"]["ready"]["presence"]["mode"], "event");

    for (mux, stamp_session) in [
        (Some(MuxName::Tmux), Some(session.as_str())),
        (Some(MuxName::Zellij), Some("rimz-wrong")),
    ] {
        write_presence_stamp(&env, mux, stamp_session);
        let report = doctor_with_mux(&env, &stub_dir, "--zellij");
        assert_ne!(
            report["mux"]["ready"]["presence"]["mode"], "event",
            "mismatched stamp must not enable event mode: {report}"
        );
    }

    write_presence_stamp(&env, None, None);
    let report = doctor_with_mux(&env, &stub_dir, "--zellij");
    assert_eq!(report["mux"]["ready"]["presence"]["mode"], "event");
}

#[test]
fn doctor_reports_cross_backend_room_conflicts_and_duplicate_groups() {
    let env = Env::new();
    let stub_dir = stub_mux_rooms(&env, StubRoomState::Live, StubRoomState::Live);
    let runtime = env.runtime_paths();
    let session = workspace_session_name(&env);
    for (name, mux, pane) in [
        ("zellij", MuxName::Zellij, "zellij:terminal_1"),
        ("tmux", MuxName::Tmux, "tmux:%2"),
    ] {
        let heartbeat = SidebarHeartbeat::new(
            env.workspace_id.clone(),
            SidebarInstanceId::new(),
            mux,
            &session,
            runtime.sock_dir.join(format!("event.{name}.sock")),
            Some(PaneId::parse(pane).expect("pane id")),
        );
        let path = runtime.heartbeat_dir.join(format!("sidebar.{name}.json"));
        rimz::disk::atomic::write_temp_then_rename(&path, &heartbeat).expect("write heartbeat");
        crate::common::room::date_heartbeat_as_live(&path);
    }

    let report = doctor_with_mux(&env, &stub_dir, "--tmux");
    let mux = &report["mux"]["ready"];
    assert_eq!(mux["room"]["conflict"], true);
    assert_eq!(
        mux["room"]["live_on"],
        serde_json::json!(["zellij", "tmux"])
    );
    let groups = mux["duplicate_sessions"]["ready"]["groups"]
        .as_array()
        .expect("duplicate groups");
    assert_eq!(groups.len(), 2, "{groups:?}");
    assert!(groups.iter().any(|group| group["mux"] == "zellij"));
    assert!(groups.iter().any(|group| group["mux"] == "tmux"));
}

#[test]
fn doctor_keeps_rival_probe_failures_unavailable() {
    let env = Env::new();
    let stub_dir = stub_mux_rooms(&env, StubRoomState::Unavailable, StubRoomState::Absent);

    let report = doctor_with_mux(&env, &stub_dir, "--tmux");
    let room = &report["mux"]["ready"]["room"];
    assert_eq!(room["selected_state"]["state"], "absent");
    assert_eq!(room["zellij"]["state"], "unavailable");
    assert!(
        room["zellij"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("permission denied")),
        "{room}"
    );
}

#[test]
fn doctor_reports_exited_zellij_room_preflight_health() {
    let env = Env::new();
    let stub_dir = stub_mux_rooms(&env, StubRoomState::Exited, StubRoomState::Absent);

    let report = doctor_with_mux(&env, &stub_dir, "--zellij");
    let mux = &report["mux"]["ready"];
    assert_eq!(mux["room"]["selected_state"]["state"], "exited");
    assert_eq!(mux["session_health"]["ready"]["state"], "stuck");
    assert_eq!(mux["presence"]["mode"], "not_applicable");
}

fn path_with_stub_first(stub_dir: &Path) -> String {
    let current = std::env::var_os("PATH").unwrap_or_default();
    format!("{}:{}", stub_dir.display(), current.to_string_lossy())
}

fn path_with_only(dirs: &[PathBuf]) -> OsString {
    std::env::join_paths(dirs).expect("join PATH")
}

#[test]
fn doctor_json_reports_uninstalled_claude_remote_control() {
    let env = Env::new();
    write_machine_config(&env, "[remote_control]\nclaude = true\n");
    let output = env
        .rimz()
        .args(["doctor", "--json"])
        .env("PATH", path_with_only(&[]))
        .output()
        .expect("spawn doctor");
    let report = doctor_json(&output);
    let remote = &report["remote_control"];
    assert_eq!(
        remote["agents"],
        serde_json::json!([
            {"kind": "claude", "detail": "enabled, not on PATH", "ready": false}
        ])
    );
    assert_eq!(remote["refusals"], serde_json::json!([]));
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
        agents.contains(&("codex", "enabled, managed install missing")),
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
        refusals.contains("Remove `disableRemoteControl` or set it to false"),
        "{refusals}"
    );
    assert!(
        !refusals.contains("managed Codex install is missing"),
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
        skipped.contains("managed Codex install is missing"),
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
        rt.sock_dir.join("event.old.sock"),
        None,
    );
    sidebar.protocol_version = "rimz.plugin.v0".to_owned();
    rimz::disk::atomic::write_temp_then_rename(
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
            rt.sock_dir.join(format!("event.{name}.sock")),
            Some(PaneId::from_parts(MuxName::Tmux, pane)),
        );
        sidebar.build = Some(build.to_owned());
        let path = rt.heartbeat_dir.join(format!("sidebar.{name}.json"));
        rimz::disk::atomic::write_temp_then_rename(&path, &sidebar)
            .expect("write sidebar heartbeat");
        crate::common::room::date_heartbeat_as_live(&path);
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
