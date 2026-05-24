//! Integration coverage for `rimz gc`.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use assert_cmd::{assert::OutputAssertExt, cargo::CommandCargoExt};
use predicates::str::contains;
use rimz::schema::heartbeat::ResolverHeartbeat;
use rimz::{ResolverId, RuntimePaths, WorkspaceId};
use tempfile::TempDir;

struct Env {
    home: TempDir,
}

impl Env {
    fn new() -> Self {
        let home = TempDir::new().expect("tempdir");
        std::fs::create_dir_all(home.path().join("runtime")).expect("mkdir runtime");
        Self { home }
    }

    fn root(&self) -> &Path {
        self.home.path()
    }

    fn runtime_root(&self) -> PathBuf {
        self.root().join("runtime")
    }

    fn rimz(&self) -> std::process::Command {
        let mut cmd = std::process::Command::cargo_bin("rimz").expect("cargo-bin");
        cmd.env("XDG_RUNTIME_DIR", self.runtime_root())
            .env("HOME", self.root())
            .env_remove("RUST_LOG")
            .current_dir(self.root());
        cmd
    }
}

#[test]
fn gc_removes_stale_runtime_heartbeat() {
    let env = Env::new();
    let workspace_id = WorkspaceId::from_project_root(env.root());
    let rt = RuntimePaths::under(workspace_id.clone(), &env.runtime_root()).expect("runtime");
    rt.ensure_dirs().expect("runtime dirs");

    let resolver_id: ResolverId = "opus-policy".parse().expect("resolver id");
    let heartbeat = ResolverHeartbeat::new(workspace_id, resolver_id);
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
