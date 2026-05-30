//! Round-trip integration tests for `rimz resolver` against a tempdir-rooted
//! `$XDG_CONFIG_HOME/rimz/resolvers.toml`.

use assert_cmd::assert::OutputAssertExt;
use predicates::str::contains;

use crate::common::Env;

#[test]
fn add_list_remove_round_trip() {
    let env = Env::new();
    env.rimz()
        .args(["resolver", "list", "--json"])
        .assert()
        .success()
        .stdout(contains("\"resolvers\": []"));

    env.rimz()
        .args([
            "resolver",
            "add",
            "opus-policy",
            "--order",
            "10",
            "--budget",
            "30s",
        ])
        .assert()
        .success();

    env.rimz()
        .args([
            "resolver",
            "add",
            "slack-on-call",
            "--order",
            "20",
            "--budget",
            "5m",
        ])
        .assert()
        .success();

    let output = env
        .rimz()
        .args(["resolver", "list", "--json"])
        .output()
        .expect("list");
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    let rows = parsed["resolvers"].as_array().expect("array");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["id"], "opus-policy");
    assert_eq!(rows[0]["budget_seconds"], 30);
    assert_eq!(rows[1]["id"], "slack-on-call");
    assert_eq!(rows[1]["budget_seconds"], 300);

    env.rimz()
        .args(["resolver", "remove", "opus-policy"])
        .assert()
        .success();

    let output = env
        .rimz()
        .args(["resolver", "list", "--json"])
        .output()
        .expect("list2");
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    let rows = parsed["resolvers"].as_array().expect("array");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], "slack-on-call");
}

#[test]
fn add_rejects_duplicate_id() {
    let env = Env::new();
    env.rimz()
        .args(["resolver", "add", "opus", "--budget", "30s"])
        .assert()
        .success();
    env.rimz()
        .args(["resolver", "add", "opus", "--budget", "60s"])
        .assert()
        .failure()
        .stderr(contains("already enrolled"));
}

#[test]
fn reorder_before_swaps_chain_position() {
    let env = Env::new();
    for (id, order) in [("opus", "10"), ("slack", "20"), ("pager", "30")] {
        env.rimz()
            .args(["resolver", "add", id, "--order", order, "--budget", "30s"])
            .assert()
            .success();
    }
    env.rimz()
        .args(["resolver", "reorder", "pager", "--before", "slack"])
        .assert()
        .success();
    let output = env
        .rimz()
        .args(["resolver", "list", "--json"])
        .output()
        .expect("list");
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    let ids: Vec<&str> = parsed["resolvers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["opus", "pager", "slack"]);
}
