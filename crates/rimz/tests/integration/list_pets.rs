//! Integration coverage for `rimz list-pets`.

use assert_cmd::assert::OutputAssertExt;
use predicates::str::contains;

use crate::common::Env;

#[test]
fn list_pets_prints_bundled_ids() {
    let env = Env::new();

    env.rimz()
        .arg("list-pets")
        .env("RIMZ_PETS_OFFLINE", "1")
        .assert()
        .success()
        .stdout(contains("rocky\n"));
}

#[test]
fn list_pets_json_is_an_array_of_ids() {
    let env = Env::new();

    let output = env
        .rimz()
        .args(["list-pets", "--json"])
        .env("RIMZ_PETS_OFFLINE", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let ids: Vec<String> = serde_json::from_slice(&output).expect("--json emits pet id array");
    assert_eq!(
        ids,
        [
            "codex",
            "dewey",
            "fireball",
            "rocky",
            "seedy",
            "stacky",
            "bsod",
            "null-signal",
        ]
    );
}
