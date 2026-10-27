//! `rimz sidebar snapshot` integration tests for provider dashboard facts.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use rimz::agents::lifecycle::LifecycleSignal;
use rimz::agents::{AgentLifecycleObservation, LaunchParams};
use rimz::store::event::EventEnvelope;
use sha2::{Digest, Sha256};

use crate::common::{Env, tmux_pane};

#[test]
fn ended_rows_keep_labels_without_prompt_history_or_pane_argv() {
    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    let now = jiff::Timestamp::now();
    let parent_pane = tmux_pane("%7", "claude", &env.project_root);
    let mut parent = rimz::testkit::agent_state("claude", "parent", now);
    parent.pane = Some(parent_pane.clone());
    parent.worktree_path = Some(env.project_root.display().to_string());
    let mut child = rimz::testkit::agent_state("claude", "child", now);
    child.parent_agent_id = Some(parent.agent_id.clone());
    child.launch_depth = Some(1);
    child.ended_at = Some(now);
    child.first_prompt = Some(format!("first\n {}", "x".repeat(300)));
    child.prompt = Some("latest\n request".to_owned());
    child.recent_prompts = vec!["full history".to_owned()];
    let mut child_pane = tmux_pane("%8", "claude", &env.project_root);
    child_pane.spawn_command = Some("claude --prompt full history".to_owned());
    child_pane.foreground_cmdline = Some("claude --prompt full history".to_owned());
    child.pane = Some(child_pane);
    std::fs::create_dir_all(store.paths().agents_carryover.parent().unwrap()).unwrap();
    std::fs::write(
        &store.paths().agents_carryover,
        serde_json::to_vec(&serde_json::json!({"agents": [parent, child]})).unwrap(),
    )
    .unwrap();
    rimz::store::event_log::append(
        &store.paths().events_log,
        &EventEnvelope::new(
            env.workspace_id.clone(),
            "rimz-test",
            "rimz",
            "cli",
            "test.noop",
            serde_json::json!({}),
        ),
    )
    .unwrap();

    let snapshot = env.snapshot_json_with_panes(&[parent_pane]);
    let child = snapshot["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["agent_id"] == "child")
        .expect("retained ended child");
    assert_eq!(child["prompt"], "latest request");
    assert_eq!(child["first_prompt"], format!("first {}", "x".repeat(154)));
    assert!(child.get("recent_prompts").is_none());
    assert!(child["pane"].get("spawn_command").is_none());
    assert!(child["pane"].get("foreground_cmdline").is_none());
}

#[test]
fn resolution_fallback_only_reports_live_or_unknown_sessions() {
    use crate::common::wait::{agent_wait, register_calling_agent};
    use rimz::diag::record::DiagEvent;

    let mut results = Vec::new();
    for mode in ["absent", "live", "unknown", "success"] {
        let env = Env::new();
        env.record(&env.project_root);
        register_calling_agent(&env);
        let session = env.resolve_workspace(&env.project_root).session_name;
        let bin = env.home_root.join("resolution-bin");
        crate::common::write_path_shim(
            &bin,
            "tmux",
            r#"if [ "$1" = "-S" ]; then shift 3; fi
printf '%s\n' "$1" >> "$RIMZ_TEST_RESOLUTION_LOG"
case "$1" in
    list-panes) printf 'pane read failed\n' >&2; exit 1 ;;
    list-sessions)
        case "$RIMZ_TEST_RESOLUTION_MODE" in
            absent) exit 0 ;;
            live) printf '%s\n' "$RIMZ_TEST_RESOLUTION_SESSION" ;;
            unknown) printf 'permission denied\n' >&2; exit 1 ;;
            *) exit 2 ;;
        esac ;;
    *) exit 2 ;;
esac"#,
        );
        let log = env.home_root.join("resolution-calls.log");
        let mut command = agent_wait(&env);
        command
            .args(["--mux", "tmux", "wait", "list"])
            .env("PATH", &bin)
            .env("RIMZ_TEST_RESOLUTION_LOG", &log)
            .env("RIMZ_TEST_RESOLUTION_MODE", mode)
            .env("RIMZ_TEST_RESOLUTION_SESSION", &session);
        if mode == "success" {
            let fixture = env.write_pane_fixture(&[tmux_pane("%1", "claude", &env.project_root)]);
            command.env("RIMZ_TEST_PANE_LIST", fixture);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "@planner#main · no waits\n",
            "{mode}"
        );
        let records = env.diag_records(&session);
        let fallbacks = records
            .iter()
            .filter(|record| matches!(record.event, DiagEvent::ResolutionFallback { .. }))
            .count();
        let rejected = records
            .iter()
            .filter(|record| matches!(record.event, DiagEvent::FrameRejected { .. }))
            .count();
        results.push((
            mode,
            fallbacks,
            rejected,
            std::fs::read_to_string(log).unwrap_or_default(),
        ));
    }
    assert_eq!(
        results,
        vec![
            ("absent", 0, 1, "list-panes\nlist-sessions\n".into()),
            ("live", 1, 1, "list-panes\nlist-sessions\n".into()),
            ("unknown", 1, 1, "list-panes\nlist-sessions\n".into()),
            ("success", 0, 0, String::new()),
        ]
    );
}

#[test]
fn play_refuses_a_pipe_before_terminal_output() {
    let env = Env::new();
    let pilot = Path::new(env!("CARGO_MANIFEST_DIR")).join("scenarios/pilot.scn");
    let output = env
        .rimz()
        .args(["sidebar", "play"])
        .arg(pilot)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--frames"));
    assert!(output.stdout.is_empty());
}

#[test]
fn play_exports_repeatable_frames_and_seeks_through_the_same_loop() {
    let env = Env::new();
    let pilot = Path::new(env!("CARGO_MANIFEST_DIR")).join("scenarios/pilot.scn");
    let first = env.project_root.join("first");
    let second = env.project_root.join("second");
    for dir in [&first, &second] {
        let output = env
            .rimz()
            .args(["sidebar", "play"])
            .arg(&pilot)
            .arg("--frames")
            .arg(dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let first = first.join("pilot");
    let second = second.join("pilot");
    let index = std::fs::read_to_string(first.join("index.jsonl")).unwrap();
    let frames: Vec<serde_json::Value> = index
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        frames.len() > 10,
        "a real filmstrip, not just the opening frame"
    );
    assert_eq!(frames[0]["t_ms"], 0);
    assert_eq!(frames[0]["key"], true);
    assert!(frames.iter().any(|frame| {
        frame["changed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|label| label == "@coder#main")
    }));
    for entry in std::fs::read_dir(&first).unwrap() {
        let entry = entry.unwrap();
        assert_eq!(
            std::fs::read(entry.path()).unwrap(),
            std::fs::read(second.join(entry.file_name())).unwrap(),
            "{}",
            entry.path().display()
        );
    }
    for frame in &frames {
        let text = std::fs::read_to_string(first.join(frame["text"].as_str().unwrap())).unwrap();
        assert_eq!(text.lines().count(), 34);
        assert!(!text.contains('\x1b'));
    }
    let frame = &frames[frames.len() / 2];
    let at = format!("{}ms", frame["t_ms"].as_u64().unwrap());
    let output = env
        .rimz()
        .args(["sidebar", "play"])
        .arg(&pilot)
        .args(["--at", &at])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = std::fs::read_to_string(first.join(frame["text"].as_str().unwrap())).unwrap();
    let output = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        output.lines().skip(1).collect::<Vec<_>>().join("\n"),
        expected.trim_end_matches('\n')
    );
    let output = env
        .rimz()
        .args(["sidebar", "play"])
        .arg(&pilot)
        .args(["--at", &at, "--section", "@coder#main", "--height", "50"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.lines()
            .skip(1)
            .all(|line| line.starts_with("@coder#main"))
    );
    assert!(text.lines().count() > 1);
    let output = env
        .rimz()
        .args(["sidebar", "play"])
        .arg(&pilot)
        .args(["--at", "1s", "--ansi"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stdout.contains(&0x1b));
    assert_eq!(String::from_utf8_lossy(&output.stdout).lines().count(), 34);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("== pilot"));
}

#[test]
fn play_check_parses_every_checked_in_scenario_and_reports_all_errors() {
    let env = Env::new();
    let scenarios = Path::new(env!("CARGO_MANIFEST_DIR")).join("scenarios");
    let files: Vec<_> = std::fs::read_dir(scenarios)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "scn"))
        .collect();
    assert!(!files.is_empty());
    let output = env
        .rimz()
        .args(["sidebar", "play", "--check"])
        .args(&files)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| line.starts_with("ok "))
            .count(),
        files.len()
    );
    let bad = env.project_root.join("bad.scn");
    let other = env.project_root.join("other.scn");
    std::fs::write(&bad, "agent @a claude\n0s @a spwans @b codex").unwrap();
    std::fs::write(&other, "0s @unknown idle").unwrap();
    let output = env
        .rimz()
        .args(["sidebar", "play", "--check"])
        .args([&bad, &files[0], &other])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let errors = String::from_utf8_lossy(&output.stderr);
    assert!(errors.contains("bad.scn: line 2, column 7:"), "{errors}");
    assert!(errors.contains("other.scn: line 1, column 4:"), "{errors}");
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("ok "));
    let output = env.rimz().args(["sidebar", "play"]).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("crates/rimz/scenarios/"));
}

#[test]
fn agent_birth_argv_returned_to_shell_renders_process_row() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let mut pane = tmux_pane("%7", "zsh", &env.project_root);
    pane.spawn_command = Some(format!(
        "{} agents exec claude --worktree-path {}",
        env.rimz_bin().display(),
        env.project_root.display(),
    ));

    let snapshot = env.snapshot_json_with_panes(&[pane]);
    let rows = snapshot["worktree_groups"]
        .as_array()
        .expect("worktree groups")
        .iter()
        .flat_map(|group| group["rows"].as_array().expect("rows"));
    let row = rows.into_iter().next().expect("shell process row");

    assert_eq!(row["row_kind"], "process");
    assert_eq!(row["name"], "zsh");
}

fn inject_lifecycle(env: &Env, agent_kind: &str, agent_id: &str) {
    env.record(&env.project_root);
    let obs = AgentLifecycleObservation {
        on_screen: None,
        state_snapshot: None,
        ask_queue: None,
        agent_id: Some(agent_id.into()),
        agent_name: None,
        launch: LaunchParams::default(),
        signal: LifecycleSignal::Registered,
        agent_pid: None,
        account_key: None,
        agent_process_start: None,
        runtime_owner: None,
        worktree_path: Some(env.project_root.display().to_string()),
        worktree_branch: Some("main".to_owned()),
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

fn claude_remote_control_badge(
    env: &Env,
    settings: &Path,
    path: Option<String>,
) -> serde_json::Value {
    inject_lifecycle(env, "claude", "claude-session-rc");

    let mut command = env.rimz();
    command
        .args([
            "sidebar",
            "snapshot",
            "--workspace-id",
            env.workspace_id.as_str(),
            "--json",
        ])
        .env("RIMZ_CLAUDE_SETTINGS", settings)
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN");
    if let Some(path) = path {
        command.env("PATH", path);
    }
    let output = command.output().expect("spawn sidebar snapshot");
    assert!(
        output.status.success(),
        "snapshot failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("snapshot json");
    json["providers"]
        .as_array()
        .expect("providers")
        .iter()
        .find(|provider| provider["kind"] == "claude")
        .expect("claude provider panel")["remote_control"]
        .clone()
}

#[test]
fn snapshot_exits_cleanly_when_the_consumer_closes_the_pipe() {
    use std::io::Read;
    use std::process::Stdio;

    let env = Env::new();
    inject_lifecycle(&env, "claude", "claude-session-epipe");

    let (reader, writer) = std::io::pipe().expect("pipe");
    drop(reader);

    let mut child = env
        .rimz()
        .args([
            "sidebar",
            "snapshot",
            "--workspace-id",
            env.workspace_id.as_str(),
            "--json",
        ])
        .stdout(Stdio::from(writer))
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sidebar snapshot");

    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr piped")
        .read_to_string(&mut stderr)
        .expect("read stderr");
    let status = child.wait().expect("wait");

    assert!(status.success(), "expected clean exit; stderr: {stderr}");
    assert!(
        !stderr.contains("panicked"),
        "broken pipe must not panic; stderr: {stderr}"
    );
}

#[test]
fn frame_emits_plain_text_over_a_pipe() {
    let env = Env::new();
    let session = "test-session";
    let runtime = env.runtime_paths();
    runtime.ensure_dirs().expect("runtime dirs");
    let published =
        rimz::sidebar::frame::assemble_frame(Vec::new(), rimz::utils::time::unix_now_ms(), session);
    rimz::disk::atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &published)
        .expect("publish pane frame");

    let output = env
        .rimz()
        .args([
            "sidebar",
            "frame",
            "--workspace-id",
            env.workspace_id.as_str(),
            "--session-name",
            session,
            "--width",
            "40",
            "--height",
            "12",
        ])
        .output()
        .expect("spawn sidebar frame");

    assert!(
        output.status.success(),
        "frame failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.stdout.is_empty(), "frame stdout is empty");
    assert!(
        !output.stdout.contains(&b'\x1b'),
        "piped frame contains ANSI escapes: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
}

const ROOM_LESS_ID: &str = "ws_abcdef0123456789abcdef01";

#[test]
fn frame_section_filters_the_full_frame_over_a_pipe() {
    let env = Env::new();
    let frame = |section: Option<&str>, expand: bool| {
        let mut command = env.rimz();
        command.args([
            "sidebar",
            "frame",
            "--workspace-id",
            ROOM_LESS_ID,
            "--width",
            "54",
        ]);
        if expand {
            command.arg("--expand");
        } else {
            command.args(["--height", "24"]);
        }
        if let Some(section) = section {
            command.args(["--section", section]);
        }
        let output = command.output().expect("spawn frame");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!output.stdout.contains(&b'\x1b'));
        String::from_utf8(output.stdout).unwrap()
    };
    for expand in [false, true] {
        let full = frame(None, expand);
        let cockpit = frame(Some("cockpit"), expand);
        assert!(!cockpit.trim().is_empty(), "cockpit rows missing");
        assert!(cockpit.lines().count() < full.lines().count());
        assert!(
            cockpit
                .lines()
                .all(|row| full.lines().any(|full| full == row))
        );
        assert!(!cockpit.contains("? for help"));
        assert!(frame(Some("unknown-section"), expand).is_empty());
    }
}

fn assert_no_dir_named_for_room_less_id(env: &Env) {
    fn walk(dir: &Path, hits: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().contains(&ROOM_LESS_ID[3..]))
            {
                hits.push(path.clone());
            }
            walk(&path, hits);
        }
    }
    let mut hits = Vec::new();
    walk(&env.runtime_root, &mut hits);
    walk(&env.rimz_home(), &mut hits);
    assert!(hits.is_empty(), "room-less read scaffolded: {hits:?}");
}

#[test]
fn snapshot_for_a_room_less_id_is_empty_and_creates_nothing() {
    let env = Env::new();

    let output = env
        .rimz()
        .args([
            "sidebar",
            "snapshot",
            "--workspace-id",
            ROOM_LESS_ID,
            "--json",
        ])
        .output()
        .expect("spawn sidebar snapshot");

    assert!(
        output.status.success(),
        "snapshot failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let snapshot: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("snapshot json");
    assert_eq!(snapshot["agents"], serde_json::json!([]));
    assert_eq!(snapshot["display_name"], ROOM_LESS_ID);
    assert_no_dir_named_for_room_less_id(&env);
}

#[test]
fn snapshot_for_a_room_less_id_carries_the_machine_config() {
    let env = Env::new();
    std::fs::create_dir_all(env.rimz_home()).expect("rimz home");
    std::fs::write(
        env.rimz_home().join("config.toml"),
        "[sidebar]\ntrunk = \"develop\"\n[agents.attention]\nstalled_after_secs = 2700\n",
    )
    .expect("write machine config");
    std::fs::write(
        env.rimz_home().join("theme.toml"),
        "[theme.display]\nprovider_list = [\"codex\"]\n",
    )
    .expect("write theme config");

    let output = env
        .rimz()
        .args([
            "sidebar",
            "snapshot",
            "--workspace-id",
            ROOM_LESS_ID,
            "--json",
        ])
        .output()
        .expect("spawn sidebar snapshot");

    assert!(
        output.status.success(),
        "snapshot failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let snapshot: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("snapshot json");
    assert_eq!(snapshot["sidebar"]["trunk"], "develop");
    assert_eq!(
        snapshot["theme"]["display"]["provider_list"],
        serde_json::json!(["codex"])
    );
    assert_eq!(snapshot["attention"]["stalled_after_secs"], 2700);
    assert_no_dir_named_for_room_less_id(&env);
}

#[test]
fn frame_for_a_room_less_id_renders_and_creates_nothing() {
    let env = Env::new();

    let output = env
        .rimz()
        .args([
            "sidebar",
            "frame",
            "--workspace-id",
            ROOM_LESS_ID,
            "--width",
            "40",
            "--height",
            "12",
        ])
        .output()
        .expect("spawn sidebar frame");

    assert!(
        output.status.success(),
        "frame failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.stdout.is_empty(), "frame stdout is empty");
    assert_no_dir_named_for_room_less_id(&env);
}

#[test]
fn sidebar_lights_claude_rc_badge_from_claude_startup_setting() {
    let env = Env::new();
    let settings = write_claude_settings(&env, r#"{ "remoteControlAtStartup": true }"#);

    assert_eq!(
        claude_remote_control_badge(&env, &settings, None),
        "healthy"
    );
}

#[test]
fn sidebar_suppresses_claude_rc_badge_when_auth_blocks_remote_control() {
    let env = Env::new();
    let settings = write_claude_settings(
        &env,
        r#"{ "remoteControlAtStartup": true, "apiKeyHelper": "op read key" }"#,
    );
    let stub_dir = stub_claude_version(&env, "2.1.173 (Claude Code)");

    assert_eq!(
        claude_remote_control_badge(&env, &settings, Some(path_with_stub_first(&stub_dir))),
        "hidden"
    );
}

#[test]
fn codex_identity_enrichment_preserves_hook_owned_question() {
    let env = Env::new();
    env.record(&env.project_root);
    let session_id = "11111111-1111-4111-8111-111111111111";
    let now = jiff::Timestamp::now();
    let date = now.to_zoned(jiff::tz::TimeZone::UTC).date();
    let sessions = env.home_root.join(format!(
        ".codex/sessions/{:04}/{:02}/{:02}",
        date.year(),
        date.month(),
        date.day()
    ));
    std::fs::create_dir_all(&sessions).expect("create Codex sessions dir");
    let rollout = sessions.join(format!(
        "rollout-{:04}-{:02}-{:02}T00-00-00-{session_id}.jsonl",
        date.year(),
        date.month(),
        date.day()
    ));
    std::fs::write(
        &rollout,
        format!(
            "{}\n",
            serde_json::json!({
                "timestamp": now.to_string(),
                "type": "session_meta",
                "payload": {
                    "id": session_id,
                    "cwd": env.project_root,
                }
            })
        ),
    )
    .expect("write Codex rollout");

    let hook = env.run_installed_hook_in_pane(
        "codex",
        &serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": session_id,
            "tool_name": "request_user_input",
            "tool_input": {
                "questions": [{
                    "id": "shape",
                    "question": "Which fix shape?",
                    "options": [{ "label": "minimal" }, { "label": "broad" }]
                }]
            }
        })
        .to_string(),
        &[("TMUX_PANE", "%7")],
    );
    assert!(
        hook.status.success(),
        "{}",
        String::from_utf8_lossy(&hook.stderr)
    );

    let mut pane = crate::common::tmux_pane("%7", "codex", &env.project_root);
    pane.resumed_session_id = Some(rimz::ids::AgentSessionId::from(session_id));
    let snapshot = env.snapshot_json_with_panes(&[pane]);
    let agent = snapshot["agents"]
        .as_array()
        .expect("agents")
        .iter()
        .find(|agent| agent["agent_id"] == session_id)
        .expect("Codex agent");
    assert_eq!(agent["status"], "waiting");
    assert!(agent["waiting_since"].is_string());
    assert!(agent["open_ask"]["id"].as_str().is_some());

    let row = snapshot["worktree_groups"]
        .as_array()
        .expect("worktree groups")
        .iter()
        .flat_map(|group| group["rows"].as_array().expect("rows"))
        .find(|row| row["id"] == session_id)
        .expect("Codex worktree row");
    assert_eq!(row["status"], "waiting");
}

#[test]
fn codex_guardian_rollout_cannot_displace_the_root_on_its_pane() {
    let env = Env::new();
    env.record(&env.project_root);
    let root = "11111111-1111-4111-8111-111111111111";
    let guardian = "22222222-2222-4222-8222-222222222222";
    let now = std::time::SystemTime::now();
    let date = jiff::Timestamp::try_from(now)
        .unwrap()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date();
    let sessions = env.home_root.join(format!(
        ".codex/sessions/{:04}/{:02}/{:02}",
        date.year(),
        date.month(),
        date.day()
    ));
    std::fs::create_dir_all(&sessions).unwrap();
    for (id, age, source, thread_source, parent) in [
        (root, 20, serde_json::json!("cli"), "user", None),
        (
            guardian,
            10,
            serde_json::json!({ "subagent": { "other": "guardian" } }),
            "guardian_review",
            Some(root),
        ),
    ] {
        let created_at = now - std::time::Duration::from_secs(age);
        let rollout = sessions.join(format!(
            "rollout-{:04}-{:02}-{:02}T00-00-00-{id}.jsonl",
            date.year(),
            date.month(),
            date.day()
        ));
        std::fs::write(
            &rollout,
            serde_json::json!({
                "timestamp": jiff::Timestamp::try_from(created_at).unwrap().to_string(),
                "type": "session_meta",
                "payload": {
                    "id": id,
                    "cwd": env.project_root,
                    "source": source,
                    "thread_source": thread_source,
                    "parent_thread_id": parent,
                }
            })
            .to_string(),
        )
        .unwrap();
        std::fs::File::options()
            .write(true)
            .open(&rollout)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(created_at))
            .unwrap();
    }

    let mut pane = tmux_pane("%7", "codex", &env.project_root);
    pane.pane_process_start =
        Some(jiff::Timestamp::try_from(now - std::time::Duration::from_secs(30)).unwrap());
    let snapshot = env.snapshot_json_with_panes(&[pane]);
    let agents = snapshot["agents"].as_array().expect("agents");
    let ids = agents
        .iter()
        .map(|agent| agent["agent_id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(ids, [root]);
    assert_eq!(agents[0]["pane"]["pane_id"], "tmux:%7");
    let rows = snapshot["worktree_groups"]
        .as_array()
        .expect("worktree groups")
        .iter()
        .flat_map(|group| group["rows"].as_array().expect("rows"))
        .collect::<Vec<_>>();
    assert!(rows.iter().all(|row| row["id"] != guardian));
    let root_row = rows.iter().find(|row| row["id"] == root).expect("root row");
    assert_eq!(root_row["pane"]["pane_id"], "tmux:%7");
}

#[test]
fn kiro_local_store_bootstraps_live_state_and_history_without_events() {
    let env = Env::new();
    env.record(&env.project_root);
    let session_id = "sess_11111111-1111-4111-8111-111111111111";
    let bucket = &hex::encode(Sha256::digest(
        env.project_root.as_os_str().as_encoded_bytes(),
    ))[..16];
    let session_dir = env
        .home_root
        .join(".kiro/sessions")
        .join(bucket)
        .join(session_id);
    std::fs::create_dir_all(&session_dir).expect("create Kiro session dir");
    std::fs::write(
        session_dir.join("session.json"),
        serde_json::json!({
            "id": session_id,
            "schemaVersion": "1.0.0",
            "dataModelVersion": 1,
            "workspacePaths": [env.project_root],
            "createdAt": "2025-01-01T00:00:00Z",
            "lastModifiedAt": "2025-01-01T00:00:00Z"
        })
        .to_string(),
    )
    .expect("write Kiro metadata");
    let records = [
        r#"{"id":"user","timestamp":"2025-01-01T00:00:01Z","payload":{"type":"user","content":"ping"}}"#,
        r#"{"id":"start","timestamp":"2025-01-01T00:00:02Z","payload":{"type":"turn_start","executionId":"turn"}}"#,
        r#"{"id":"pending","timestamp":"2025-01-01T00:00:03Z","payload":{"type":"pending_interaction","interactionType":"tool_approval","toolCallId":"tool","question":"Write File"}}"#,
        r#"{"id":"resolved","timestamp":"2025-01-01T00:00:04Z","payload":{"type":"interaction_resolved","toolCallId":"tool"}}"#,
        r#"{"id":"call","timestamp":"2025-01-01T00:00:05Z","payload":{"type":"tool_call","toolCallId":"tool","toolName":"fs_write","status":"approved"}}"#,
        r#"{"id":"result","timestamp":"2025-01-01T00:00:06Z","payload":{"type":"tool_result","toolCallId":"tool","success":true}}"#,
        r#"{"id":"assistant","timestamp":"2025-01-01T00:00:07Z","payload":{"type":"assistant","operationType":"Say","content":"pong"}}"#,
        r#"{"id":"context","timestamp":"2025-01-01T00:00:07.100Z","payload":{"type":"session_metadata","key":"contextUsage","value":{"usagePercentage":42.4}}}"#,
        r#"{"id":"pause","timestamp":"2025-01-01T00:00:08Z","payload":{"type":"session_event","category":"session_pause","context":{"status":"success"}}}"#,
        r#"{"id":"end","timestamp":"2025-01-01T00:00:08.100Z","payload":{"type":"turn_end","executionId":"turn","stopReason":"end_turn"}}"#,
    ];
    let transcript = session_dir.join("messages.jsonl");
    std::fs::write(&transcript, b"").expect("write empty Kiro transcript");
    let pane = rimz::pane::PaneRef {
        pane_id: rimz::ids::PaneId::from_parts(rimz::ids::MuxName::Tmux, "%kiro"),
        session_name: "rimz-test".to_owned(),
        view_id: Some("@0".to_owned()),
        view_kind: Some(rimz::ids::ViewKind::Window),
        view_name: None,
        title: None,
        is_floating: false,
        command: Some("kiro-cli-chat".to_owned()),
        foreground_cmdline: None,
        spawn_command: None,
        cwd: Some(env.project_root.to_string_lossy().into_owned()),
        pane_pid: None,
        pane_process_start: Some("2024-12-31T23:59:58Z".parse().unwrap()),
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
        hosted_agent_lineage: Vec::new(),
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    };
    let event_log = env.store().paths().events_log.clone();
    let before = std::fs::read(&event_log).unwrap_or_default();
    let pane_fixture = env.write_pane_fixture(std::slice::from_ref(&pane));
    let session_name = env.resolve_workspace(&env.project_root).session_name;
    let run_snapshot = || {
        let output = env
            .rimz()
            .args([
                "sidebar",
                "snapshot",
                "--workspace-id",
                env.workspace_id.as_str(),
                "--mux",
                "tmux",
                "--session-name",
                &session_name,
                "--json",
            ])
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .output()
            .expect("run Kiro snapshot");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };

    let snapshot = run_snapshot();
    let agent = snapshot["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["agent_id"] == session_id)
        .expect("newborn Kiro agent");
    assert_eq!(agent["status"], "idle");
    assert_eq!(agent["phase"], "idle");
    assert!(agent.get("prompt").is_none());
    assert!(agent["context_pct"].is_null());
    assert!(agent["context_window"].is_null());
    assert!(agent["total_tokens"].is_null());
    assert!(agent["context"].get("model_id").is_none());
    assert!(agent["context"].get("tokens").is_none());
    assert!(agent["context"].get("cost").is_none());
    assert!(agent["model"].is_null());
    assert!(agent["effort"].is_null());
    let row = snapshot["worktree_groups"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|group| group["rows"].as_array().unwrap())
        .find(|row| row["id"] == session_id)
        .expect("newborn Kiro row");
    assert_eq!(row["row_kind"], "agent");
    assert_eq!(row["status"], "idle");
    assert!(row.get("prompt").is_none());
    assert!(row["context_pct"].is_null());
    assert!(row.get("total_tokens").is_none());
    assert!(row["context"].get("model_id").is_none());
    assert!(row["context"].get("tokens").is_none());
    assert!(row["context"].get("cost").is_none());
    assert_eq!(std::fs::read(&event_log).unwrap_or_default(), before);

    for (end, expected, phase) in [
        (2, "running", "reasoning"),
        (3, "waiting", "idle"),
        (5, "running", "acting"),
        (10, "success", "idle"),
    ] {
        std::fs::write(&transcript, format!("{}\n", records[..end].join("\n")))
            .expect("grow Kiro transcript");
        let snapshot = run_snapshot();
        let agent = snapshot["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|agent| agent["agent_id"] == session_id)
            .expect("Kiro agent row");
        assert_eq!(agent["status"], expected);
        assert_eq!(agent["phase"], phase);
        if end == records.len() {
            assert_eq!(agent["context_pct"], 42);
        }
    }

    let frame = rimz::sidebar::frame::assemble_frame(
        vec![pane.clone()],
        rimz::utils::time::unix_now_ms(),
        &session_name,
    );
    rimz::disk::atomic::write_temp_then_rename_cache(
        &env.runtime_paths().pane_frame_path(),
        &frame,
    )
    .expect("publish Kiro pane frame");

    let created_at: jiff::Timestamp = "2025-01-01T00:00:00Z".parse().unwrap();
    let observation = rimz::agents::LocalSessionObservation {
        login: Some("default".parse().unwrap()),
        kind: rimz::ids::AgentKind::new_unchecked("kiro"),
        session_id: session_id.into(),
        workspace: env.project_root.clone(),
        transcript_path: transcript.clone(),
        created_at,
        fresh_binding_at: Some(created_at),
        first_event_at: Some("2025-01-01T00:00:01Z".parse().unwrap()),
        last_activity: "2025-01-01T00:00:08.100Z".parse().unwrap(),
        projection: rimz::agents::LocalSessionProjection::IdentityOnly,
    };
    rimz::disk::atomic::write_temp_then_rename_cache(
        &env.runtime_paths().agent_projection_path(),
        &serde_json::json!({
            "session_name": session_name,
            "wiring": { "kinds": ["kiro"], "default_models": {} },
            "inputs": { "by_kind": { "kiro": [env.project_root] } },
            "observations": [observation],
        }),
    )
    .expect("publish Kiro local-session projection");

    let history = env
        .rimz()
        .args(["agents", "history", session_id, "--json"])
        .output()
        .expect("run Kiro history");
    assert!(
        history.status.success(),
        "{}",
        String::from_utf8_lossy(&history.stderr)
    );
    let history: serde_json::Value = serde_json::from_slice(&history.stdout).unwrap();
    assert_eq!(history[0]["prompt"], "ping");
    assert_eq!(history[0]["outcome"], "done");
    assert_eq!(history[0]["fresh_input"], 0);
    assert_eq!(history[0]["output"], 0);
    assert!(history[0]["cost_usd"].is_null());
    assert_eq!(std::fs::read(&event_log).unwrap_or_default(), before);
}
