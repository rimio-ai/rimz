//! Integration coverage for `rimz channel`.

use assert_cmd::assert::OutputAssertExt;
use predicates::str::contains;
use serde_json::Value;

use crate::common::Env;

#[test]
fn channel_new_list_and_remove_round_trip() {
    let env = Env::new();

    env.rimz()
        .args(["channel", "new", "design"])
        .assert()
        .success()
        .stdout(contains("created design"));

    let out = env
        .rimz()
        .args(["channel", "list", "--json"])
        .output()
        .expect("spawn list");
    assert!(out.status.success(), "channel list succeeds");
    let parsed: Value = serde_json::from_slice(&out.stdout).expect("json");
    let entries = parsed.as_array().expect("array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["channel"], "design");
    assert_eq!(entries[0]["backing"], "named");
    assert_eq!(entries[0]["agents"].as_array().expect("agents").len(), 0);

    env.rimz()
        .args(["channel", "rm", "design"])
        .assert()
        .success()
        .stdout(contains("removed design"));

    let out = env
        .rimz()
        .args(["channel", "list", "--json"])
        .output()
        .expect("spawn list");
    assert!(out.status.success(), "channel list succeeds");
    let parsed: Value = serde_json::from_slice(&out.stdout).expect("json");
    assert!(parsed.as_array().expect("array").is_empty());
}

#[test]
fn channel_new_validates_bare_names() {
    let env = Env::new();

    env.rimz()
        .args(["channel", "new", "bad/name"])
        .assert()
        .failure()
        .stderr(contains("invalid channel name"));
}
