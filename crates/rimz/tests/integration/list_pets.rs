//! Integration coverage for `rimz list-pets`.

use assert_cmd::assert::OutputAssertExt;
use predicates::str::contains;

use crate::common::Env;

const BUILTIN_IDS: [&str; 8] = [
    "codex",
    "dewey",
    "fireball",
    "rocky",
    "seedy",
    "stacky",
    "bsod",
    "null-signal",
];

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
        ids.iter().map(String::as_str).collect::<Vec<_>>(),
        BUILTIN_IDS
    );
}

#[test]
fn list_pets_json_includes_installed_petdex_slugs_after_bundled_ids() {
    let env = Env::new();
    let pet = env.home_root.join(".codex/pets/wall-e");
    std::fs::create_dir_all(&pet).expect("mkdir installed pet");
    std::fs::write(
        pet.join("pet.json"),
        br#"{"id":"wall-e","spritesheetPath":"spritesheet.webp"}"#,
    )
    .expect("write installed pet manifest");

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
        ids.iter()
            .take(BUILTIN_IDS.len())
            .map(String::as_str)
            .collect::<Vec<_>>(),
        BUILTIN_IDS
    );
    assert!(ids.contains(&"wall-e".to_owned()));
}
