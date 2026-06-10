//! Integration coverage for `rimz gc`.

use std::time::{Duration, SystemTime};

use assert_cmd::assert::OutputAssertExt;
use predicates::str::contains;
use rimz::schema::heartbeat::ResolverHeartbeat;
use rimz::{ResolverId, RuntimePaths, SidebarInstanceId, WorkspaceId};

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
        .stdout(contains("gc complete"))
        .stdout(contains("heartbeats    : 1"));

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
        .stdout(contains("gc complete"))
        .stdout(contains("sidecars      : 1"));

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
        .stdout(contains("gc complete"));

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
        .stdout(contains("gc complete"))
        .stdout(contains("abandoned scaffold"))
        .stdout(contains("retained      : 1"));

    assert!(!scaffold.exists(), "abandoned scaffold should be reaped");
    assert!(
        history.exists(),
        "unreadable record with history should be kept"
    );
}
