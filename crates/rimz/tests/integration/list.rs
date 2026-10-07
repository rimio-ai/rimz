//! Integration coverage for `rimz list`.

use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::common::{Env, zellij_trace_shim};

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
    assert_eq!(row["current"], false);
    assert!(row["last_death"].is_null());
    assert!(row["last_death_detail"].is_null());
    let keys = [
        "workspace_id",
        "project_root",
        "session_name",
        "running_on",
        "last_activity",
        "last_death",
        "current",
        "last_death_detail",
    ];
    let positions: Vec<_> = keys
        .iter()
        .map(|key| stdout.find(&format!("\"{key}\":")).expect("key present"))
        .collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn list_empty_states_are_human_only() {
    let env = Env::new();
    for args in [vec!["list"], vec!["list", "--all"]] {
        let output = env.rimz().args(args).output().unwrap();
        assert!(output.status.success());
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "No rooms yet. Run `rimz` in a project to open one.\n"
        );
    }
}

#[test]
fn list_json_empty_is_silent() {
    let env = Env::new();
    for args in [vec!["list", "--json"], vec!["list", "--all", "--json"]] {
        let output = env.rimz().args(args).output().unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout), "[]\n");
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn list_table_leads_with_room_and_marks_only_a_verified_current_pin() {
    let env = Env::new();
    let project = env.home_root.join("query-engine");
    env.record(&project);
    let workspace = env.resolve_workspace(&project);
    backdate_tree(
        &env.state_path_for(&project).root,
        SystemTime::now() - Duration::from_secs(2 * 60 * 60),
    );

    for valid_pin in [true, false] {
        let pin = if valid_pin {
            workspace.workspace_id.as_str()
        } else {
            "ws_000000000000000000000000"
        };
        let output = env
            .rimz()
            .arg("list")
            .env("RIMZ_WORKSPACE_ID", pin)
            .env("RIMZ_PROJECT_ROOT", project.join("."))
            .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
            .env("RIMZ_TEST_ZELLIJ_LOG", env.home_root.join("zellij.log"))
            .env("RIMZ_TEST_ZELLIJ_LIST_SESSIONS", &workspace.session_name)
            .output()
            .unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            stdout
                .lines()
                .next()
                .unwrap()
                .split_whitespace()
                .collect::<Vec<_>>(),
            ["ROOM", "PROJECT", "MUX", "LAST", "ACTIVE"]
        );
        let room = if valid_pin {
            format!("{} (here)", workspace.session_name)
        } else {
            workspace.session_name.clone()
        };
        assert!(
            stdout.lines().nth(1).unwrap().starts_with(&room),
            "{stdout}"
        );
        assert!(stdout.contains("~/query-engine"), "{stdout}");
        assert!(stdout.contains("zellij"), "{stdout}");
        assert!(stdout.contains("2h ago"), "{stdout}");
        assert_eq!(stdout.contains("(here)"), valid_pin, "{stdout}");
        assert!(
            !stdout.contains(workspace.workspace_id.as_str()),
            "{stdout}"
        );

        let json = env
            .rimz()
            .args(["list", "--json"])
            .env("RIMZ_WORKSPACE_ID", pin)
            .env("RIMZ_PROJECT_ROOT", project.join("."))
            .output()
            .unwrap();
        assert!(json.status.success());
        let rows: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
        assert_eq!(rows[0]["current"], valid_pin);
    }
}

#[test]
fn list_death_detail_is_additive_and_human_death_is_relative() {
    let env = Env::new();
    env.record(&env.project_root);
    let paths = env.state_path_for(&env.project_root);
    for (cause, verb, agents) in [("crash", "crashed", 16), ("reboot", "rebooted", 1)] {
        let lost_agents: Vec<_> = (0..agents)
            .map(|index| {
                serde_json::json!({
                    "kind": "claude", "agent_id": format!("sess-{index}"), "name": "helper"
                })
            })
            .collect();
        let marker = serde_json::json!({
            "cause": cause,
            "at": "1970-01-01T00:00:00Z",
            "lost_agents": lost_agents,
            "recovered": 1
        });
        rimz::disk::atomic::write_temp_then_rename(&paths.last_death_marker, &marker).unwrap();

        let output = env.rimz().args(["list", "--json"]).output().unwrap();
        assert!(output.status.success());
        let rows: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let count = if agents == 1 {
            "1 agent".to_owned()
        } else {
            format!("{agents} agents")
        };
        assert_eq!(
            rows[0]["last_death"],
            format!("{verb} · {count} · 1970-01-01 00:00")
        );
        assert_eq!(
            rows[0]["last_death_detail"],
            serde_json::json!({
                "cause": cause, "at": marker["at"], "lost_agents": marker["lost_agents"]
            })
        );

        let human = env.rimz().arg("list").output().unwrap();
        assert!(human.status.success());
        let stdout = String::from_utf8(human.stdout).unwrap();
        assert!(stdout.contains(&format!("{verb} · {count} · ")), "{stdout}");
        assert!(stdout.contains(" ago"), "{stdout}");
        assert!(!stdout.contains("1970-01-01"), "{stdout}");
    }
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
    assert_eq!(
        String::from_utf8_lossy(&default.stderr),
        "No rooms running or active in the last 24h. 1 dormant: rimz list --all\n"
    );
    let json = env.rimz().args(["list", "--json"]).output().unwrap();
    assert!(json.status.success());
    assert_eq!(String::from_utf8_lossy(&json.stdout), "[]\n");
    assert!(json.stderr.is_empty());

    let all = env.rimz().args(["list", "--all"]).output().expect("run");
    assert!(all.status.success());
    let all_out = String::from_utf8(all.stdout).expect("utf8");
    assert!(
        all_out.contains("query-engine"),
        "--all should reveal the dormant workspace:\n{all_out}"
    );
    assert!(all.stderr.is_empty());
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
