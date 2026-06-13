//! Integration coverage for `rimz list-themes`.

use assert_cmd::assert::OutputAssertExt;
use predicates::str::contains;

use crate::common::Env;

#[test]
fn list_themes_prints_bundled_names() {
    let env = Env::new();

    env.rimz()
        .arg("list-themes")
        .assert()
        .success()
        .stdout(contains("TokyoNight Night"))
        .stdout(contains("Catppuccin Mocha"));
}

#[test]
fn list_themes_json_is_an_array_of_names() {
    let env = Env::new();

    let output = env
        .rimz()
        .args(["list-themes", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let names: Vec<String> =
        serde_json::from_slice(&output).expect("--json emits a JSON array of names");
    assert!(names.iter().any(|name| name == "TokyoNight Night"));
}
