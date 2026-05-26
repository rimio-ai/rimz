//! Integration coverage for `rimz list`.

use std::path::Path;

use rimz::WorkspaceId;

use crate::common::Env;

#[test]
fn list_with_no_workspaces_prints_nothing() {
    let env = Env::new();
    let output = env.rimz().arg("list").output().expect("run");
    assert!(output.status.success());
    assert!(
        output.stdout.is_empty(),
        "expected empty stdout, got: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn list_shows_known_workspaces_with_session_and_root() {
    let env = Env::new();
    env.record(&env.project_root.join("query-engine"));
    env.record(&env.project_root.join("invoicing"));

    let output = env.rimz().arg("list").output().expect("run");
    assert!(
        output.status.success(),
        "rimz list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    assert!(
        stdout.contains("WORKSPACE"),
        "header line missing:\n{stdout}"
    );
    assert!(
        stdout.contains("query-engine"),
        "query-engine project missing:\n{stdout}"
    );
    assert!(
        stdout.contains("invoicing"),
        "invoicing project missing:\n{stdout}"
    );
}

#[test]
fn list_json_emits_canonical_fields() {
    let env = Env::new();
    env.record(&env.project_root.join("query-engine"));

    let output = env.rimz().args(["list", "--json"]).output().expect("run");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    let rows = parsed.as_array().expect("rows array");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert!(row["workspace_id"].as_str().unwrap().starts_with("ws_"));
    assert!(
        row["project_root"]
            .as_str()
            .unwrap()
            .contains("query-engine")
    );
    assert!(row["session_name"].as_str().unwrap().starts_with("rimz-"));
    // No real mux session is bound; expect None.
    assert!(row["running_on"].is_null());
    // Activity should be populated from workspace.json mtime even without events.
    assert!(row["last_activity"].is_string());
}

#[test]
fn list_skips_workspaces_with_unreadable_record() {
    let env = Env::new();
    env.record(&env.project_root.join("query-engine"));

    // Add a sibling dir under workspaces with a garbled workspace.json.
    let mut bogus_dir = env.state_root();
    bogus_dir.push("rimz");
    bogus_dir.push("workspaces");
    bogus_dir.push(WorkspaceId::from_project_root(Path::new("/nope")).as_str());
    std::fs::create_dir_all(&bogus_dir).expect("mkdir bogus");
    std::fs::write(bogus_dir.join("workspace.json"), b"{ not json").expect("write bogus");

    let output = env
        .rimz()
        .args(["list", "--json"])
        .output()
        .expect("run rimz");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&stdout).expect("json");
    assert_eq!(rows.len(), 1, "garbled record should be skipped");
    assert!(
        rows[0]["project_root"]
            .as_str()
            .unwrap()
            .contains("query-engine")
    );
}
