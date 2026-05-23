//! Round-trip integration tests for `rimz resolver` against a tempdir-rooted
//! `$XDG_CONFIG_HOME/rimz/resolvers.toml`.

mod common;

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

fn rimz(home: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("rimz").expect("cargo-bin");
    cmd.env("XDG_CONFIG_HOME", home.path())
        .env_remove("RUST_LOG");
    cmd
}

#[test]
fn add_list_remove_round_trip() {
    let home = TempDir::new().expect("tempdir");
    rimz(&home)
        .args(["resolver", "list", "--json"])
        .assert()
        .success()
        .stdout(contains("\"resolvers\": []"));

    rimz(&home)
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

    rimz(&home)
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

    let output = rimz(&home)
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

    rimz(&home)
        .args(["resolver", "remove", "opus-policy"])
        .assert()
        .success();

    let output = rimz(&home)
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
    let home = TempDir::new().expect("tempdir");
    rimz(&home)
        .args(["resolver", "add", "opus", "--budget", "30s"])
        .assert()
        .success();
    rimz(&home)
        .args(["resolver", "add", "opus", "--budget", "60s"])
        .assert()
        .failure()
        .stderr(contains("already enrolled"));
}

#[test]
fn reorder_before_swaps_chain_position() {
    let home = TempDir::new().expect("tempdir");
    for (id, order) in [("opus", "10"), ("slack", "20"), ("pager", "30")] {
        rimz(&home)
            .args(["resolver", "add", id, "--order", order, "--budget", "30s"])
            .assert()
            .success();
    }
    rimz(&home)
        .args(["resolver", "reorder", "pager", "--before", "slack"])
        .assert()
        .success();
    let output = rimz(&home)
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
