//! Integration coverage for `rimz gc`.

use std::time::{Duration, SystemTime};

use assert_cmd::assert::OutputAssertExt;
use predicates::str::contains;
use rimz::schema::heartbeat::ResolverHeartbeat;
use rimz::{ResolverId, RuntimePaths, SidebarInstanceId, WorkspaceId};
use serde_json::json;

use crate::common::Env;

#[test]
fn gc_removes_stale_runtime_heartbeat() {
    let env = Env::new();
    let rt = RuntimePaths::under(env.workspace_id.clone(), &env.runtime_root).expect("runtime");
    rt.ensure_dirs().expect("runtime dirs");

    let resolver_id: ResolverId = "opus-policy".parse().expect("resolver id");
    let heartbeat = ResolverHeartbeat::new(env.workspace_id.clone(), resolver_id);
    let heartbeat_path = rt.heartbeat_dir.join("resolver.opus-policy.json");
    std::fs::write(&heartbeat_path, serde_json::to_vec(&heartbeat).unwrap())
        .expect("write heartbeat");
    let old = SystemTime::now() - Duration::from_secs(7200);
    std::fs::File::open(&heartbeat_path)
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
    for sub in ["feed", "locks", "snapshots"] {
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
        .stdout(contains("abandoned scaffold"))
        .stdout(contains("retained"));

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
    let config_path = config_dir.join("agents.toml");
    std::fs::write(
        &config_path,
        format!(
            "[agents.loop.tasks.dead]\n\
             to = {{ kind = \"claude\", session = \"sess-dead\", handle = \"@claude\" }}\n\
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
        .stdout(contains("schedules reaped: 1"));

    let config = std::fs::read_to_string(config_path).expect("read agents config");
    assert!(
        !config.contains("[agents.loop.tasks.dead]"),
        "dead schedule should be removed"
    );
}

#[test]
fn gc_keeps_spawn_and_live_loop_schedules() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-live", "feature-live");
    let config_dir = env.config_root().join("rimz");
    std::fs::create_dir_all(&config_dir).expect("mkdir config");
    let config_path = config_dir.join("agents.toml");
    std::fs::write(
        &config_path,
        format!(
            "[agents.loop.tasks.spawn]\n\
             spec = \"claude\"\n\
             prompt = \"spawn wake\"\n\
             root = \"{}\"\n\
             at = \"07:00\"\n\
             \n\
             [agents.loop.tasks.live]\n\
             to = {{ kind = \"claude\", session = \"sess-live\", handle = \"@claude\" }}\n\
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
        config.contains("[agents.loop.tasks.spawn]"),
        "spawn schedule should be kept: {config}"
    );
    assert!(
        config.contains("[agents.loop.tasks.live]"),
        "live delivery schedule should be kept: {config}"
    );
}

fn register_running_agent(env: &Env, session_id: &str, branch: &str) {
    run_hook(
        env,
        json!({
            "hook_event_name": "SessionStart",
            "session_id": session_id,
            "worktree_branch": branch,
        }),
    );
    run_hook(
        env,
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": session_id,
            "prompt": "work",
            "worktree_branch": branch,
        }),
    );
}

fn run_hook(env: &Env, payload: serde_json::Value) {
    let payload = serde_json::to_string(&payload).expect("payload");
    let output = env.run_installed_hook_in_pane("claude", &payload, &[]);
    assert!(
        output.status.success(),
        "hook failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
