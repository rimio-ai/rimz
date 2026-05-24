//! Integration coverage for `rimz trust {status,grant,revoke}` and the
//! auto-stale path.

use std::path::Path;

use assert_cmd::{assert::OutputAssertExt, cargo::CommandCargoExt};
use predicates::str::contains;
use tempfile::TempDir;

struct Env {
    home: TempDir,
    project: std::path::PathBuf,
}

impl Env {
    fn new() -> Self {
        let home = TempDir::new().expect("tempdir");
        for d in ["state", "runtime", "config"] {
            std::fs::create_dir_all(home.path().join(d)).expect("mkdir env root");
        }
        let project = home.path().join("billing-service");
        std::fs::create_dir_all(&project).expect("mkdir project");
        Self { home, project }
    }

    fn root(&self) -> &Path {
        self.home.path()
    }

    fn write_config(&self, body: &str) {
        let dir = self.project.join(".rimz");
        std::fs::create_dir_all(&dir).expect("mkdir .rimz");
        std::fs::write(dir.join("config.toml"), body).expect("write config");
    }

    fn rimz(&self) -> std::process::Command {
        let mut cmd = std::process::Command::cargo_bin("rimz").expect("cargo-bin");
        cmd.env("XDG_STATE_HOME", self.root().join("state"))
            .env("XDG_RUNTIME_DIR", self.root().join("runtime"))
            .env("XDG_CONFIG_HOME", self.root().join("config"))
            .env("HOME", self.root())
            .env_remove("RUST_LOG")
            .current_dir(&self.project);
        cmd
    }
}

#[test]
fn trust_status_grant_revoke_lifecycle() {
    let env = Env::new();
    env.write_config("[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n");

    env.rimz()
        .args(["trust", "status"])
        .assert()
        .success()
        .stdout(contains("trust: untrusted"));

    env.rimz()
        .args(["trust", "grant"])
        .assert()
        .success()
        .stdout(contains("trust: trusted"));

    env.rimz()
        .args(["trust", "status"])
        .assert()
        .success()
        .stdout(contains("trust: trusted"));

    env.rimz()
        .args(["trust", "revoke"])
        .assert()
        .success()
        .stdout(contains("trust: untrusted"));
}

#[test]
fn trust_auto_revokes_when_executable_surface_drifts() {
    let env = Env::new();
    env.write_config("[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n");

    env.rimz().args(["trust", "grant"]).assert().success();

    // Mutate the hook command — a command-running field.
    env.write_config(
        "[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude --telemetry\"\n",
    );

    env.rimz()
        .args(["trust", "status"])
        .assert()
        .success()
        .stdout(contains("trust: stale"));
}

#[test]
fn trust_ignores_non_command_field_edits() {
    let env = Env::new();
    env.write_config(
        "display_name = \"Billing\"\n\n[[layout.initial_panes]]\ncommand = \"$SHELL\"\n",
    );

    env.rimz().args(["trust", "grant"]).assert().success();

    // Edit a non-command field — `display_name` and `sidebar` are not in the
    // executable surface.
    env.write_config(
        "display_name = \"Billing service\"\nsidebar = true\n\n[[layout.initial_panes]]\ncommand = \"$SHELL\"\n",
    );

    env.rimz()
        .args(["trust", "status"])
        .assert()
        .success()
        .stdout(contains("trust: trusted"));
}

#[test]
fn trust_status_json_emits_canonical_fields() {
    let env = Env::new();
    env.write_config("[notifications]\ncommand = \"notify-send rimz\"\n");

    let output = env
        .rimz()
        .args(["trust", "status", "--json"])
        .output()
        .expect("run rimz");
    assert!(
        output.status.success(),
        "rimz trust status --json failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("parse json");
    assert_eq!(parsed["state"], "untrusted");
    assert!(
        parsed["current_hash"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(parsed["granted_hash"].is_null());
    assert!(parsed["granted_at"].is_null());
}

#[test]
fn trust_no_config_reports_no_config() {
    let env = Env::new();
    env.rimz()
        .args(["trust", "status"])
        .assert()
        .success()
        .stdout(contains("trust: no project config"));
}
