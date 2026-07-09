//! Third-party agent plugin CLI and registry integration.

use std::fs;

use serde_json::Value;

use crate::common::{CommandTimeoutExt, Env};

#[test]
fn plugin_scaffold_registry_doctor_and_start_validation_work_end_to_end() {
    let env = Env::new();
    let output = env
        .rimz()
        .args(["agents", "register", "testbot"])
        .bounded_output()
        .expect("register plugin");
    assert!(
        output.status.success(),
        "register failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let plugin_dir = env.config_root().join("rimz/agents.d/testbot");
    for path in [
        "agent.toml",
        "README.md",
        "shim.sh",
        "probes/spend",
        "probes/account",
    ] {
        assert!(plugin_dir.join(path).is_file(), "missing scaffold {path}");
    }

    let output = env
        .rimz()
        .args(["agents", "register", "--check"])
        .bounded_output()
        .expect("check plugin");
    assert!(
        output.status.success(),
        "check failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = env
        .rimz()
        .args(["coverage", "--json"])
        .bounded_output()
        .expect("plugin coverage");
    assert!(output.status.success(), "coverage failed");
    let report: Value = serde_json::from_slice(&output.stdout).expect("coverage json");
    assert!(
        report["coverage"]["agents"]
            .as_array()
            .is_some_and(|agents| agents.iter().any(|kind| kind == "testbot")),
        "plugin missing from coverage: {report}"
    );

    let output = env
        .rimz()
        .args(["doctor", "--json"])
        .bounded_output()
        .expect("plugin doctor");
    assert!(output.status.success(), "doctor failed");
    let report: Value = serde_json::from_slice(&output.stdout).expect("doctor json");
    assert_eq!(report["plugins"][0]["kind"], "testbot");
    assert_eq!(report["plugins"][0]["valid"], true);

    let output = env.run_hook(
        "testbot",
        r#"{"protocol":1,"hook_event_name":"session_start","session_id":"plugin-session","cwd":"/tmp/project"}"#,
    );
    assert!(
        output.status.success(),
        "plugin hook failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = env
        .rimz()
        .args(["doctor", "--audit", "--json"])
        .bounded_output()
        .expect("plugin rollup audit");
    let report: Value = serde_json::from_slice(&output.stdout).expect("doctor audit json");
    assert_eq!(report["agents"]["rows"][0]["kind"], "testbot");
    assert_eq!(report["agents"]["rows"][0]["agent_id"], "plugin-session");
    assert_eq!(report["agents"]["rows"][0]["status"], "idle");

    let manifest_path = plugin_dir.join("agent.toml");
    let manifest = fs::read_to_string(&manifest_path).expect("read manifest");
    fs::write(
        &manifest_path,
        manifest.replace("protocol = 1", "protocol = 9"),
    )
    .expect("corrupt manifest");

    let output = env
        .rimz()
        .arg("start")
        .bounded_output()
        .expect("start validation");
    assert!(!output.status.success(), "start accepted invalid plugin");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("agent plugin validation failed"),
        "{stderr}"
    );
    assert!(stderr.contains("agent.toml"), "{stderr}");
    assert!(stderr.contains("register --check"), "{stderr}");

    let output = env
        .rimz()
        .args(["hooks", "feed", "--source", "testbot"])
        .bounded_output()
        .expect("invalid plugin hook feed");
    assert!(
        output.status.success(),
        "invalid plugin broke hook path: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
