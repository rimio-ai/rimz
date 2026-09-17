//! `rimz paths` integration tests: the home and workspace dirs a project
//! resolves to, as the binary reports them.

use std::path::Path;
use std::process::Command;

use assert_cmd::assert::OutputAssertExt;
use serde_json::Value;

use crate::common::{Env, canonical};

fn paths_json(command: &mut Command) -> Value {
    let output = command
        .args(["paths", "--json"])
        .output()
        .expect("spawn paths");
    assert!(
        output.status.success(),
        "paths failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("paths --json emits valid json")
}

fn path_of<'a>(report: &'a Value, key: &str) -> &'a Path {
    Path::new(report[key].as_str().expect("path string"))
}

fn workspace_hex(env: &Env) -> &str {
    env.workspace_id
        .as_str()
        .strip_prefix("ws_")
        .expect("workspace id prefix")
}

#[test]
fn paths_names_one_workspace_dir_in_both_trees() {
    let env = Env::new();
    env.record(&env.project_root);
    let state = env.state_path_for(&env.project_root);

    let report = paths_json(&mut env.rimz());
    assert_eq!(report["schema"], "rimz.paths.v1");
    assert_eq!(path_of(&report, "home"), env.rimz_home());
    assert_eq!(report["workspace_id"], env.workspace_id.as_str());
    let name = report["workspace_dir"].as_str().expect("workspace dir");
    assert_eq!(name, format!("project-{}", &workspace_hex(&env)[..4]));
    assert_eq!(path_of(&report, "state_dir"), state.root);
    assert_eq!(
        path_of(&report, "state_dir"),
        env.rimz_home().join("ws").join(name)
    );
    assert_eq!(
        path_of(&report, "runtime_dir"),
        env.runtime_root.join("rimz/ws").join(name)
    );
    assert_eq!(report["scratch_agent_view"], report["scratch"]);

    let sandboxed = paths_json(env.rimz().env("RIMZ_ISOLATION", "sandbox"));
    assert_eq!(sandboxed["scratch_agent_view"], "/tmp/scratchpad");

    env.rimz()
        .arg("paths")
        .assert()
        .success()
        .stdout(predicates::str::contains("workspace dir"));
}

#[test]
fn a_taken_basename_prefix_mints_a_longer_workspace_dir() {
    let env = Env::new();
    let hex = workspace_hex(&env);
    let taken = env
        .rimz_home()
        .join("ws")
        .join(format!("project-{}", &hex[..4]));
    std::fs::create_dir_all(&taken).expect("mkdir neighbour");
    let neighbour = format!("ws_{}{}", &hex[..4], "f".repeat(20));
    assert_ne!(neighbour, env.workspace_id.as_str());
    std::fs::write(
        taken.join("workspace.json"),
        format!(r#"{{"workspace_id":"{neighbour}"}}"#),
    )
    .expect("record neighbour");

    let report = paths_json(&mut env.rimz());
    assert_eq!(
        report["workspace_dir"],
        format!("project-{}", &hex[..6]),
        "a neighbour's dir is never adopted"
    );
}

#[test]
fn the_default_home_is_no_project_marker() {
    let env = Env::new();
    let home = env.home_root.join(".rimz");
    std::fs::create_dir_all(&home).expect("mkdir home");
    std::fs::write(home.join("config.toml"), "[agents]\nisolation = \"host\"\n")
        .expect("write machine config");
    let plain = env.home_root.join("plain");
    std::fs::create_dir_all(&plain).expect("mkdir plain dir");

    let report = paths_json(env.rimz().env_remove("RIMZ_HOME").current_dir(&plain));
    assert_eq!(path_of(&report, "home"), home);
    assert_eq!(path_of(&report, "project_root"), canonical(&plain));

    env.rimz()
        .env_remove("RIMZ_HOME")
        .current_dir(&env.home_root)
        .args(["trust", "status"])
        .assert()
        .success()
        .stdout(predicates::str::contains("trust: no project config"));
}
