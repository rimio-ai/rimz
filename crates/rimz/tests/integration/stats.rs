//! CLI accounting for daily sweeps and per-unit pressure evictions.

use assert_cmd::assert::OutputAssertExt;
use serde_json::json;

use crate::common::Env;

fn publish_team_effort(env: &Env) {
    use rimz::agents::spending::{
        EffortTally, EffortWindow, ProviderSpendingCache, RoleTally, SpendTally, SpendWindow,
        write_provider_spending_cache,
    };

    let now = jiff::Timestamp::now().as_second() as u64;
    let mut cache = ProviderSpendingCache {
        timezone: "UTC".into(),
        refreshed_at_ms: now * 1000,
        day_cutoff_secs: now,
        ..Default::default()
    };
    let spend = SpendWindow {
        usd: 12.0,
        tokens: 100,
        sessions: 2,
        ..Default::default()
    };
    let tally = SpendTally {
        day: spend.clone(),
        week: spend.clone(),
        month: spend.clone(),
        year: spend,
        ..Default::default()
    };
    let effort = EffortWindow {
        active_ms: 45_000,
        from_user: 1,
        from_teammates: 2,
        to_teammates: 7,
    };
    let entry = RoleTally {
        tally: tally.clone(),
        effort: EffortTally {
            day: effort,
            week: effort,
            month: effort,
            year: effort,
        },
    };
    cache.spending.total = tally;
    cache.roles.teams = (0..5)
        .map(|i| {
            (
                format!("team-{i}"),
                std::collections::BTreeMap::from([("coder".into(), entry.clone())]),
            )
        })
        .collect();
    cache.roles.recorded_since_secs = Some(now);
    cache.roles.effort_since_secs = Some(now);
    assert!(write_provider_spending_cache(
        &env.runtime_paths().shared_provider_spending_path(),
        &cache
    ));
}

#[test]
fn stats_panel_shows_teams_with_effort_and_keeps_roles_in_json_only() {
    let env = Env::new();
    publish_team_effort(&env);
    let output = env.rimz().arg("stats").assert().success();
    let text = String::from_utf8_lossy(&output.get_output().stdout);
    for expected in [
        "Teams (today)",
        "ACTIVE",
        "MESSAGES",
        "2 other teams",
        "3 (1 you)",
    ] {
        assert!(text.contains(expected), "missing {expected}: {text}");
    }
    assert!(!text.contains("coder"), "{text}");
    let output = env.rimz().args(["stats", "--json"]).assert().success();
    let value: serde_json::Value = serde_json::from_slice(&output.get_output().stdout).unwrap();
    assert_eq!(value["teams"].as_array().unwrap().len(), 5);
    assert_eq!(value["teams"][0]["roles"][0]["role"], "coder");
    assert_eq!(value["agents"], json!([]));
}

#[test]
fn stats_teams_envelope_is_uncapped_and_empty_view_names_the_window() {
    let env = Env::new();
    publish_team_effort(&env);
    let output = env
        .rimz()
        .args(["stats", "--teams", "--json"])
        .assert()
        .success();
    let value: serde_json::Value = serde_json::from_slice(&output.get_output().stdout).unwrap();
    assert_eq!(
        value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["as_of", "effort_since", "roles_since", "teams", "window"]
    );
    assert_eq!(value["teams"].as_array().unwrap().len(), 5);
    assert_eq!(value["teams"][0]["active_secs"], 45);
    assert_eq!(
        value["teams"][0]["messages"],
        json!({"total": 3, "from_user": 1})
    );
    assert_eq!(value["teams"][0]["roles"][0]["to_teammates"], 7);
    let output = env.rimz().args(["stats", "--teams"]).assert().success();
    let text = String::from_utf8_lossy(&output.get_output().stdout);
    for name in [
        "team-0", "team-1", "team-2", "team-3", "team-4", "INPUT", "OUTPUT", "CACHED", "HIT",
        "SESSIONS", "ACTIVE", "MESSAGES",
    ] {
        assert!(text.contains(name), "missing {name}: {text}");
    }
    assert!(
        !text.contains("other teams") && !text.contains("coder"),
        "{text}"
    );
    let mut cache = rimz::agents::spending::read_provider_spending_cache(
        &env.runtime_paths().shared_provider_spending_path(),
    );
    cache.roles.teams.clear();
    assert!(rimz::agents::spending::write_provider_spending_cache(
        &env.runtime_paths().shared_provider_spending_path(),
        &cache
    ));
    let output = env
        .rimz()
        .args(["stats", "--teams", "--window", "week"])
        .assert()
        .success();
    let text = String::from_utf8_lossy(&output.get_output().stdout);
    assert!(text.contains("No team spend recorded (week)."), "{text}");
    assert!(
        text.contains("active time and messages recorded since"),
        "{text}"
    );
}

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
