//! Third-party agent plugin CLI and registry integration.

use std::fs;
use std::path::PathBuf;

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
        report["capabilities"]["agents"]
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

#[test]
fn plugin_check_replays_example_envelopes_and_rejects_bad_input() {
    let env = Env::new();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/agent-plugin");
    let plugin_dir = env.config_root().join("rimz/agents.d/scriptbot");
    fs::create_dir_all(plugin_dir.join("fixtures")).expect("create plugin fixture directory");
    fs::copy(source.join("README.md"), plugin_dir.join("README.md")).expect("copy setup doc");
    fs::copy(
        source.join("fixtures/envelopes.jsonl"),
        plugin_dir.join("fixtures/envelopes.jsonl"),
    )
    .expect("copy replay fixture");
    let manifest = fs::read_to_string(source.join("agent.toml")).expect("read example manifest");
    let manifest = manifest
        .split_once("[probes]")
        .map_or(manifest.as_str(), |(manifest, _)| manifest);
    fs::write(plugin_dir.join("agent.toml"), manifest).expect("write probe-free test manifest");

    let replay = plugin_dir.join("fixtures/envelopes.jsonl");
    let output = env
        .rimz()
        .args([
            "agents",
            "check",
            "scriptbot",
            "--replay",
            replay.to_str().expect("utf-8 replay path"),
        ])
        .bounded_output()
        .expect("check plugin replay");
    assert!(
        output.status.success(),
        "check failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout)
        .expect("utf-8 report")
        .replace(plugin_dir.to_str().expect("utf-8 plugin path"), "$PLUGIN");
    assert_eq!(
        stdout,
        r#"plugin `scriptbot`
manifest: valid ($PLUGIN/agent.toml)
coverage: 9 wired, 1 partial, 7 unsupported
lifecycle: 8 native, 1 derived, 2 absent
probes: none declared
replay: $PLUGIN/fixtures/envelopes.jsonl
LINE  EVENT           SIGNAL          STATE              RESULT
1     session_start   registered      idle               ok
2     turn_start      turn_started    running/reasoning  ok
3     context         context         running/reasoning  ok
4     tool_use        tool_used       running/acting     ok
5     awaiting_input  awaiting_input  waiting            ok
6     turn_end        turn_ended      success            ok
final AgentState:
  example-session: status=success, phase=idle, compacting=false
"#
    );

    let bad_replay = plugin_dir.join("fixtures/bad-envelopes.jsonl");
    fs::write(
        &bad_replay,
        r#"{"protocol":1,"hook_event_name":"awaiting_input","session_id":"example-session","ask":"unsupported"}
"#,
    )
    .expect("write bad replay");
    let output = env
        .rimz()
        .args([
            "agents",
            "check",
            "scriptbot",
            "--replay",
            bad_replay.to_str().expect("utf-8 replay path"),
        ])
        .bounded_output()
        .expect("check bad replay");
    assert!(!output.status.success(), "bad envelope passed check");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("awaiting_input"), "{stdout}");
    assert!(stdout.contains("unknown variant `unsupported`"), "{stdout}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("agent plugin check failed"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
