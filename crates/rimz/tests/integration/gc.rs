//! Integration coverage for `rimz gc`.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

use assert_cmd::assert::OutputAssertExt;
use predicates::str::contains;
use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
use rimz::ids::AgentSessionId;
use rimz::message::{DeliveryGate, MessageRecord};
use rimz::sidebar::heartbeat::SidebarHeartbeat;
use rimz::sidebar::timing::{SESSION_PROBE_MARKER_PREFIX, SESSION_PROBE_MARKER_TTL};
use rimz::{MuxName, RuntimePaths, SidebarInstanceId, WorkspaceId};
use serde_json::json;

use crate::common::Env;

#[test]
fn sidebar_snapshot_does_not_create_an_abandoned_state_scaffold() {
    let env = Env::new();

    let output = env
        .rimz()
        .args([
            "sidebar",
            "snapshot",
            "--workspace-id",
            env.workspace_id.as_str(),
            "--no-produce",
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
    assert_eq!(snapshot["agents"], json!([]));

    let workspace_state = env
        .state_root()
        .join("rimz")
        .join("workspaces")
        .join(env.workspace_id.as_str());
    assert!(
        !workspace_state.exists(),
        "read-only snapshot should not create a state scaffold"
    );
}

#[test]
fn gc_removes_stale_sidebar_heartbeat_and_leaves_unknown_file() {
    let env = Env::new();
    let rt = RuntimePaths::under(env.workspace_id.clone(), &env.runtime_root).expect("runtime");
    rt.ensure_dirs().expect("runtime dirs");

    let heartbeat = SidebarHeartbeat::new(
        env.workspace_id.clone(),
        SidebarInstanceId::new(),
        MuxName::Tmux,
        "rimz-test",
        rt.sock_dir.join("sidebar.old.sock"),
        None,
    );
    let heartbeat_path = rt.heartbeat_dir.join("sidebar.old.json");
    rimz::store::atomic::write_temp_then_rename(&heartbeat_path, &heartbeat)
        .expect("write heartbeat");
    let unknown_file = rt.heartbeat_dir.join("unknown.opus-policy.json");
    std::fs::write(&unknown_file, br#"{"legacy":true}"#).expect("write unknown file");
    let old = SystemTime::now() - Duration::from_secs(7200);
    std::fs::File::open(&heartbeat_path)
        .unwrap()
        .set_modified(old)
        .unwrap();
    std::fs::File::open(&unknown_file)
        .unwrap()
        .set_modified(old)
        .unwrap();

    env.rimz()
        .args(["gc", "--older-than", "1h"])
        .assert()
        .success()
        .stdout(contains("reclaimed"))
        .stdout(contains("heartbeat"));

    assert!(
        !heartbeat_path.exists(),
        "stale heartbeat should be removed"
    );
    assert!(
        unknown_file.exists(),
        "unknown heartbeat file is not a runtime GC target"
    );
}

#[test]
fn gc_removes_stale_sidebar_read_marks() {
    let env = Env::new();
    let rt = RuntimePaths::under(env.workspace_id.clone(), &env.runtime_root).expect("runtime");
    rt.ensure_dirs().expect("runtime dirs");

    let read_marks_path = rt.sidebar_read_marks_path(&SidebarInstanceId::new());
    std::fs::write(&read_marks_path, br#"{"marks":{"row-a":1000}}"#).expect("write read marks");
    let old = SystemTime::now() - Duration::from_secs(7200);
    std::fs::File::open(&read_marks_path)
        .unwrap()
        .set_modified(old)
        .unwrap();

    env.rimz()
        .args(["gc", "--older-than", "1h"])
        .assert()
        .success()
        .stdout(contains("reclaimed"))
        .stdout(contains("sidecar"));

    assert!(
        !read_marks_path.exists(),
        "stale read marks should be removed"
    );
}

#[test]
fn gc_prunes_dead_root_workspace() {
    let env = Env::new();
    let gone_root = env.project_root.join("gone-project");
    env.record(&gone_root);
    let gone_paths = env.state_path_for(&gone_root);
    std::fs::remove_dir_all(&gone_root).expect("remove gone root");

    // `gc` is the global garbage collector: it reaps provably-dead workspaces
    // alongside runtime liveness hints.
    env.rimz()
        .args(["gc", "--older-than", "1h"])
        .assert()
        .success()
        .stdout(contains("reclaimed"));

    assert!(
        !gone_paths.root.exists(),
        "gc should reap the workspace whose project root is gone"
    );
}

#[test]
fn gc_reaps_scaffold_but_keeps_unreadable_history() {
    let env = Env::new();
    let workspaces = env.state_root().join("rimz").join("workspaces");
    std::fs::create_dir_all(&workspaces).expect("mkdir workspaces");

    // An abandoned `rimz start` scaffold: empty subdirs, no workspace.json.
    let scaffold =
        workspaces.join(WorkspaceId::from_project_root(std::path::Path::new("/scaffold")).as_str());
    for sub in ["snapshots", "runs", "locks"] {
        std::fs::create_dir_all(scaffold.join(sub)).expect("mkdir scaffold sub");
    }

    // An unreadable record that still holds history: kept and reported.
    let history =
        workspaces.join(WorkspaceId::from_project_root(std::path::Path::new("/history")).as_str());
    std::fs::create_dir_all(&history).expect("mkdir history");
    std::fs::write(history.join("workspace.json"), b"{ not json").expect("garbled record");
    std::fs::write(history.join("events.log.jsonl"), b"{}\n").expect("history");

    env.rimz()
        .args(["gc", "--older-than", "1h"])
        .assert()
        .success()
        .stdout(contains("reclaimed"))
        .stdout(contains("abandoned setup, never used"))
        .stdout(contains("kept with unreadable record"));

    assert!(!scaffold.exists(), "abandoned scaffold should be reaped");
    assert!(
        history.exists(),
        "unreadable record with history should be kept"
    );
}

#[test]
fn gc_reaps_dead_loop_delivery_schedule() {
    let env = Env::new();
    let config_dir = env.config_root().join("rimz");
    std::fs::create_dir_all(&config_dir).expect("mkdir config");
    let config_path = config_dir.join("loop.toml");
    std::fs::write(
        &config_path,
        format!(
            "[tasks.dead]\n\
             wake = {{ kind = \"claude\", session = \"sess-dead\", handle = \"@claude\" }}\n\
             prompt = \"wake up\"\n\
             root = \"{}\"\n\
             at = \"07:00\"\n",
            env.project_root.display()
        ),
    )
    .expect("write agents config");

    env.rimz()
        .args(["gc", "--older-than", "1h"])
        .assert()
        .success()
        .stdout(contains("loop schedules"))
        .stdout(contains("1 dead reaped"));

    let config = std::fs::read_to_string(config_path).expect("read agents config");
    assert!(
        !config.contains("[tasks.dead]"),
        "dead schedule should be removed"
    );
}

#[test]
fn gc_sweeps_orphan_temps_and_probe_markers() {
    let env = Env::new();
    let rt = RuntimePaths::under(env.workspace_id.clone(), &env.runtime_root).expect("runtime");
    rt.ensure_dirs().expect("runtime dirs");
    let state = env.state_path_for(&env.project_root);
    std::fs::create_dir_all(&state.snapshots_dir).expect("mkdir snapshots");
    let state_shared = env.state_root().join("rimz").join("shared");
    std::fs::create_dir_all(&state_shared).expect("mkdir state shared");

    let nonce = "00000000000000000000000000000000";
    let old_state_shared = state_shared.join(format!("spending.json.tmp.1.{nonce}"));
    let old_state_rollup = state
        .snapshots_dir
        .join(format!("rollup.json.tmp.1.{nonce}"));
    let old_runtime_shared = rt
        .shared_root
        .join(format!("rate_limits.json.tmp.1.{nonce}"));
    let fresh_temp = state_shared.join(format!("fresh.json.tmp.1.{nonce}"));
    for path in [
        &old_state_shared,
        &old_state_rollup,
        &old_runtime_shared,
        &fresh_temp,
    ] {
        std::fs::write(path, b"temp").expect("write temp");
    }

    let old_session_marker = rt
        .shared_root
        .join(format!("{SESSION_PROBE_MARKER_PREFIX}{nonce}"));
    let recent_session_marker = rt.shared_root.join(format!(
        "{SESSION_PROBE_MARKER_PREFIX}11111111111111111111111111111111"
    ));
    let spending_lock = rt.shared_root.join("spending.lock");
    for path in [&old_session_marker, &recent_session_marker] {
        std::fs::write(path, b"probe").expect("write probe marker");
    }
    std::fs::write(&spending_lock, b"lock").expect("write lock");

    let old = SystemTime::now() - Duration::from_secs(7200);
    for path in [
        &old_state_shared,
        &old_state_rollup,
        &old_runtime_shared,
        &old_session_marker,
        &spending_lock,
    ] {
        std::fs::File::open(path)
            .unwrap()
            .set_modified(old)
            .unwrap();
    }
    let recently_dead = SystemTime::now() - (SESSION_PROBE_MARKER_TTL + Duration::from_secs(1));
    std::fs::File::open(&recent_session_marker)
        .unwrap()
        .set_modified(recently_dead)
        .unwrap();

    env.rimz()
        .args(["gc", "--older-than", "1h"])
        .assert()
        .success()
        .stdout(contains("reclaimed"))
        .stdout(contains("temp"))
        .stdout(contains("probe"));

    assert!(!old_state_shared.exists());
    assert!(!old_state_rollup.exists());
    assert!(!old_runtime_shared.exists());
    assert!(!old_session_marker.exists());
    assert!(!recent_session_marker.exists());
    assert!(fresh_temp.exists());
    assert!(spending_lock.exists());
}

#[test]
fn gc_json_emits_report() {
    let env = Env::new();

    let assert = env.rimz().args(["gc", "--json"]).assert().success();
    let value: serde_json::Value =
        serde_json::from_slice(&assert.get_output().stdout).expect("gc json");

    assert_eq!(value["dry_run"], false);
    assert!(
        value.get("reclaimed_bytes").is_some(),
        "json includes reclaimed_bytes: {value}"
    );
}

#[test]
fn gc_keeps_spawn_and_live_loop_schedules() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-live", "feature-live");
    let config_dir = env.config_root().join("rimz");
    std::fs::create_dir_all(&config_dir).expect("mkdir config");
    let config_path = config_dir.join("loop.toml");
    std::fs::write(
        &config_path,
        format!(
            "[tasks.spawn]\n\
             agent = \"claude\"\n\
             prompt = \"spawn wake\"\n\
             root = \"{}\"\n\
             at = \"07:00\"\n\
             \n\
             [tasks.live]\n\
             wake = {{ kind = \"claude\", session = \"sess-live\", handle = \"@claude\" }}\n\
             prompt = \"live wake\"\n\
             root = \"{}\"\n\
             at = \"07:00\"\n",
            env.project_root.display(),
            env.project_root.display()
        ),
    )
    .expect("write agents config");

    env.rimz()
        .args(["gc", "--older-than", "1h"])
        .assert()
        .success();

    let config = std::fs::read_to_string(config_path).expect("read agents config");
    assert!(
        config.contains("[tasks.spawn]"),
        "spawn schedule should be kept: {config}"
    );
    assert!(
        config.contains("[tasks.live]"),
        "live delivery schedule should be kept: {config}"
    );
}

#[test]
fn gc_keeps_worktree_with_live_agent() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);

    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let worktree = env.home_root.join("project-worktrees").join("demo");
    register_running_agent_at(&env, "sess-worktree-live", "demo", &worktree, &[]);

    let assert = env
        .rimz()
        .args(["gc", "--older-than", "1h"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);

    assert!(worktree.exists(), "live agent should keep the worktree");
    assert!(
        stdout.contains("1 kept — 1 in use"),
        "gc should report why the worktree was kept: {stdout}"
    );
    assert!(
        !stdout.contains("removed: demo"),
        "gc should not remove a live worktree: {stdout}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn gc_dry_run_previews_worktree_after_agent_dies() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);

    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let worktree = env.home_root.join("project-worktrees").join("demo");
    register_running_agent_at(
        &env,
        "sess-worktree-dead",
        "demo",
        &worktree,
        &[("RIMZ_AGENT_PID", &u32::MAX.to_string())],
    );

    env.rimz()
        .args(["gc", "--older-than", "1h", "--dry-run"])
        .assert()
        .success()
        .stdout(contains("would remove: demo"))
        .stdout(contains("dry run"));

    assert!(worktree.exists(), "dry-run should keep the worktree");

    env.rimz()
        .args(["gc", "--older-than", "1h"])
        .assert()
        .success()
        .stdout(contains("removed: demo"));

    assert!(!worktree.exists(), "real gc should sweep the worktree");
}

#[cfg(target_os = "linux")]
#[test]
fn gc_sweeps_worktree_after_agent_dies() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);

    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let worktree = env.home_root.join("project-worktrees").join("demo");
    register_running_agent_at(
        &env,
        "sess-worktree-dead",
        "demo",
        &worktree,
        &[("RIMZ_AGENT_PID", &u32::MAX.to_string())],
    );
    let message_id = queue_channel_message(&env, "demo", "old work");

    env.rimz()
        .args(["gc", "--older-than", "1h"])
        .assert()
        .success()
        .stdout(contains("worktrees"))
        .stdout(contains("1 removed"))
        .stdout(contains("removed: demo"));

    assert!(!worktree.exists(), "dead agent should release the worktree");
    assert!(env.store().list_messages().expect("messages").is_empty());
    let archived = env
        .read_events()
        .into_iter()
        .find(|event| {
            event.method == "message.archived"
                && event.params_value()["message_id"] == message_id.as_str()
        })
        .expect("archived message");
    assert_eq!(archived.params_value()["reason"], "worktree removed");
    let store = env.store();
    let audit = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("audit projection");
    assert!(audit.agents.iter().any(|agent| {
        agent.agent_id.as_str() == "sess-worktree-dead" && agent.ended_at.is_some()
    }));
    let runtime = store
        .runtime_projection(rimz::RuntimeScope::Runtime)
        .expect("runtime projection");
    assert!(
        runtime
            .agents
            .iter()
            .all(|agent| agent.agent_id.as_str() != "sess-worktree-dead")
    );
}

fn queue_channel_message(env: &Env, channel: &str, text: &str) -> rimz::MessageId {
    let session_id = AgentSessionId::from(format!("sess-{channel}-message"));
    let mut observation =
        AgentLifecycleObservation::new(Some(session_id.clone()), LifecycleSignal::Registered);
    observation.worktree_branch = Some(channel.to_owned());
    let event = rimz::EventEnvelope::agent_lifecycle(
        env.workspace_id.clone(),
        "rimz-test",
        "claude",
        "SessionStart",
        &observation,
    );
    env.store().append_event(&event).expect("append agent");
    let snapshot = env.store().snapshot_cached().expect("snapshot");
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id == session_id)
        .expect("agent");
    let message = MessageRecord::new(
        env.workspace_id.clone(),
        agent,
        text.to_owned(),
        true,
        DeliveryGate::Done,
    )
    .with_channel(Some(channel.to_owned()));
    let message_id = message.message_id.clone();
    env.store()
        .queue_message(&message, "rimz-test")
        .expect("queue message");
    message_id
}

fn register_running_agent(env: &Env, session_id: &str, branch: &str) {
    register_running_agent_at(env, session_id, branch, &env.project_root, &[]);
}

fn register_running_agent_at(
    env: &Env,
    session_id: &str,
    branch: &str,
    cwd: &Path,
    pane_env: &[(&str, &str)],
) {
    let cwd = cwd.display().to_string();
    run_hook(
        env,
        json!({
            "hook_event_name": "SessionStart",
            "session_id": session_id,
            "worktree_branch": branch,
            "cwd": cwd.clone(),
        }),
        pane_env,
    );
    run_hook(
        env,
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": session_id,
            "prompt": "work",
            "worktree_branch": branch,
            "cwd": cwd,
        }),
        pane_env,
    );
}

fn run_hook(env: &Env, payload: serde_json::Value, pane_env: &[(&str, &str)]) {
    let payload = serde_json::to_string(&payload).expect("payload");
    let output = env.run_installed_hook_in_pane("claude", &payload, pane_env);
    assert!(
        output.status.success(),
        "hook failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_missing() -> bool {
    Command::new("git").arg("--version").output().is_err()
}

fn init_repo(path: &Path) {
    git(path, &["init", "-b", "main"]);
    git(path, &["config", "user.email", "rimz@example.com"]);
    git(path, &["config", "user.name", "RimZ Test"]);
    commit_file(path, "README.md", "fixture\n", "initial");
}

fn commit_file(repo: &Path, name: &str, contents: &str, message: &str) {
    std::fs::write(repo.join(name), contents).expect("write committed file");
    git(repo, &["add", name]);
    git(repo, &["commit", "-m", message]);
}

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {} failed\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
