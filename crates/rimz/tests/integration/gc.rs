//! Integration coverage for `rimz gc`.

use std::time::{Duration, SystemTime};

use assert_cmd::assert::OutputAssertExt;
use predicates::str::contains;
use rimz::schema::heartbeat::ResolverHeartbeat;
use rimz::{ResolverId, RuntimePaths};

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
