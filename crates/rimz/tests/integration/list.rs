//! Integration coverage for `rimz list`.

use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::common::Env;

#[test]
fn legacy_rooms_are_omitted_from_list_and_pruned_by_gc() {
    let env = Env::new();
    let room = env.rimz_home().join("ws/legacy-abcd");
    std::fs::create_dir_all(&room).unwrap();
    let record = br#"{"layout":1}"#;
    std::fs::write(room.join("workspace.json"), record).unwrap();
    let runtime_room = env
        .runtime_paths()
        .root
        .parent()
        .unwrap()
        .join("legacy-abcd");
    std::fs::create_dir_all(&runtime_room).unwrap();
    for args in [vec!["list", "--all"], vec!["list", "--all", "--json"]] {
        let output = env.rimz().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(!stdout.contains("legacy-abcd"), "{stdout}");
        assert_eq!(std::fs::read(room.join("workspace.json")).unwrap(), record);
    }
    let output = env
        .rimz()
        .args(["gc", "--all", "--dry-run", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        json["workspaces"]["removed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|room| {
                room["dir_name"] == "legacy-abcd" && room["reason"] == "incompatible_layout"
            })
    );
    assert!(room.exists());
    assert!(runtime_room.exists());
    let output = env.rimz().args(["gc", "--all"]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!room.exists());
    assert!(!runtime_room.exists());
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
    assert_eq!(
        row["session_name"],
        env.state_path_for(&env.project_root.join("query-engine"))
            .dir_name
            .as_str()
    );
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
    let bogus_dir = env.rimz_home().join("ws").join("nope-abcd");
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

#[test]
fn list_hides_dormant_workspaces_unless_all() {
    let env = Env::new();
    env.record(&env.project_root.join("query-engine"));

    // Backdate the workspace's files past the 24h recency window so it counts
    // as dormant. It is not running, so the default view should drop it.
    let workspaces = env.rimz_home().join("ws");
    let ws_dir = std::fs::read_dir(&workspaces)
        .expect("read workspaces")
        .next()
        .expect("one workspace dir")
        .expect("entry")
        .path();
    backdate_tree(
        &ws_dir,
        SystemTime::now() - Duration::from_secs(48 * 60 * 60),
    );

    let default = env.rimz().arg("list").output().expect("run");
    assert!(default.status.success());
    let default_out = String::from_utf8(default.stdout).expect("utf8");
    assert!(
        !default_out.contains("query-engine"),
        "dormant workspace should be hidden by default:\n{default_out}"
    );

    let all = env.rimz().args(["list", "--all"]).output().expect("run");
    assert!(all.status.success());
    let all_out = String::from_utf8(all.stdout).expect("utf8");
    assert!(
        all_out.contains("query-engine"),
        "--all should reveal the dormant workspace:\n{all_out}"
    );
}

/// Recursively set every file's mtime under `dir`, so `activity_for`'s
/// newest-mtime probe reports the workspace as dormant.
fn backdate_tree(dir: &Path, when: SystemTime) {
    for entry in std::fs::read_dir(dir).expect("read dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            backdate_tree(&path, when);
        } else {
            std::fs::File::open(&path)
                .expect("open file")
                .set_modified(when)
                .expect("set mtime");
        }
    }
}
