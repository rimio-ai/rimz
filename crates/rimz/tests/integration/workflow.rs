use crate::common::Env;
use serde_json::{Value, json};
use std::fs;

#[test]
fn validate_json_shape_errors_and_exit_status() {
    let env = Env::new();
    let dir = env.rimz_home().join("workflows/review");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("workflow.toml"), "[workflow]\nscope = 'worktree'\nstart = 'pr'\n[checks.pr]\nrun = 'true'\non.ok = 'missing'\n").unwrap();
    let output = env
        .rimz()
        .args(["workflow", "validate", "--json"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["warnings"], json!([]));
    assert_eq!(
        report["rows"],
        json!([{
            "workflow": "review", "node": "pr", "kind": "check", "runner": "run", "run": "true", "agent": null,
            "edges": [
                {"outcome":"deadline", "target":"@user", "source":"default"},
                {"outcome":"invalid", "target":"@user", "source":"default"},
                {"outcome":"ok", "target":"missing", "source":"explicit"},
                {"outcome":"timeout", "target":"@user", "source":"default"}
            ]
        }])
    );
    assert_eq!(report["errors"].as_array().unwrap().len(), 1);
    assert_eq!(report["errors"][0]["path"], "checks.pr.on.ok");
    assert!(
        report["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("pr")
    );
    fs::write(dir.join("workflow.toml"), "[workflow]\nscope = 'worktree'\nstart = 'pr'\n[checks.pr]\nrun = 'true'\non.ok = '@done'\n").unwrap();
    assert!(
        env.rimz()
            .args(["workflow", "validate"])
            .status()
            .unwrap()
            .success()
    );
    let schema = env.rimz().args(["workflow", "schema"]).output().unwrap();
    assert!(schema.status.success());
    let schema: Value = serde_json::from_slice(&schema.stdout).unwrap();
    assert!(schema["properties"]["checks"].is_object());
    assert!(
        !env.rimz_home().join("ws").exists(),
        "validation must not birth a room"
    );
}
