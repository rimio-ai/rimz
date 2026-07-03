use std::path::PathBuf;

use crate::common::{CommandTimeoutExt, Env};

fn zellij_shim() -> PathBuf {
    crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace"))
}

#[test]
fn web_status_json_parses_zellij_status_and_token_count() {
    let env = Env::new();
    let log = env.project_root.join("zellij-web.log");
    let output = env
        .rimz()
        .args(["--mux", "zellij", "web", "status", "--json"])
        .env("RIMZ_ZELLIJ_BIN", zellij_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &log)
        .env(
            "RIMZ_TEST_ZELLIJ_WEB_STATUS",
            "Web server online with version: 0.44.3. Checked: http://127.0.0.1:8082\n",
        )
        .env("RIMZ_TEST_ZELLIJ_WEB_TOKENS", "default 2026-07-03\n")
        .bounded_output()
        .expect("run rimz web status");

    assert!(
        output.status.success(),
        "status succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("status json parses");
    assert_eq!(json["version"], "rimz.web.v1");
    assert_eq!(json["online"], true);
    assert_eq!(json["base_url"], "http://127.0.0.1:8082");
    assert_eq!(json["token_count"], 1);

    let log = std::fs::read_to_string(log).expect("read zellij log");
    assert!(log.contains("web\t--help"), "{log}");
    assert!(log.contains("web\t--status"), "{log}");
    assert!(log.contains("web\t--list-tokens"), "{log}");
}

#[test]
fn web_refuses_tmux_backend() {
    let env = Env::new();
    let output = env
        .rimz()
        .args(["--mux", "tmux", "web", "status"])
        .bounded_output()
        .expect("run rimz web status on tmux");

    assert!(!output.status.success(), "tmux should be unsupported");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("supports Zellij only"),
        "stderr should name the backend boundary: {stderr}"
    );
}
