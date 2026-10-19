//! CLI accounting for daily sweeps and per-unit pressure evictions.

use assert_cmd::assert::OutputAssertExt;
use serde_json::json;

use crate::common::Env;

#[test]
fn stats_counts_tmp_evictions_separately_from_sweeps() {
    let env = Env::new();
    let now = jiff::Timestamp::now();
    let mut records = Vec::new();
    for error in [None, None, Some("removal failed")] {
        records.push(json!({
            "at": now, "assist": "tmp_evict", "workspace_id": env.workspace_id,
            "handle": "fox", "reclaimed_bytes": if error.is_none() { 2048 } else { 0 },
            "free_bytes_after": 4096, "floor_bytes": 8192, "error": error
        }));
    }
    for error in [None, Some("sweep failed")] {
        records.push(json!({
            "at": now, "assist": "auto_gc", "workspace_id": env.workspace_id,
            "scope": "machine", "older_than_secs": 604800,
            "reclaimed_bytes": if error.is_none() { 1024 } else { 0 },
            "worktrees_removed": 0, "workspaces_pruned": 0, "files_removed": 0,
            "worktree_paths": ["/worktrees/removed"],
            "messages_archived": 0, "problems": 0, "error": error
        }));
    }
    let logs = env.rimz_home().join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    let wire = records
        .iter()
        .map(|record| serde_json::to_string(record).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(logs.join("assists.log.jsonl"), wire).unwrap();

    let output = env
        .rimz()
        .args(["stats", "--assists", "--all", "--json"])
        .assert()
        .success();
    let report: serde_json::Value = serde_json::from_slice(&output.get_output().stdout).unwrap();
    assert_eq!(report["rollup"]["sweeps"], 1, "{report}");
    assert_eq!(report["rollup"]["tmp_evictions"], 2, "{report}");
    assert_eq!(report["rollup"]["held"]["sweeps"], 1, "{report}");
    assert_eq!(report["rollup"]["held"]["tmp_evictions"], 1, "{report}");
    assert_eq!(report["rollup"]["reclaimed_bytes"], 5120, "{report}");
    assert_eq!(report["events"].as_array().unwrap().len(), 5, "{report}");
    assert!(
        report["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|event| event.get("worktree_paths").is_none()),
        "{report}"
    );

    let output = env
        .rimz()
        .args(["stats", "--assists", "--all"])
        .assert()
        .success();
    let text = String::from_utf8_lossy(&output.get_output().stdout);
    assert!(text.contains("Auto-gc: 1 (5 KB, 1 held)"), "{text}");
    assert!(text.contains("Temp evictions: 2 (1 held)"), "{text}");
}
