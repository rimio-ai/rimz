//! Integration coverage for `rimz accounts add|list|remove`.

use std::process::Output;

use serde_json::{Value, json};

use crate::common::Env;

fn accounts(env: &Env, args: &[&str]) -> Output {
    env.rimz()
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .arg("accounts")
        .args(args)
        .output()
        .expect("run rimz accounts")
}

fn succeeded(output: &Output) -> String {
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn failed(output: &Output) -> String {
    assert!(!output.status.success(), "unexpected success");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn accounts_add_rejects_default_home_alias_without_declaring_it() {
    let env = Env::new();
    let native = env.home_root.join(".claude");
    let alias = env.home_root.join("alias");
    std::fs::create_dir(&native).unwrap();
    std::os::unix::fs::symlink(&native, &alias).unwrap();
    let error = failed(&accounts(
        &env,
        &["add", "claude", "al", "--home", alias.to_str().unwrap()],
    ));
    assert!(error.contains("provider's own home"), "{error}");
    let rows: Value =
        serde_json::from_str(&succeeded(&accounts(&env, &["list", "--json"]))).unwrap();
    assert!(
        !rows
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["name"] == "al")
    );
    assert_eq!(std::fs::read_dir(&native).unwrap().count(), 0);
}

#[test]
fn accounts_add_rejects_exported_named_homes_before_writing() {
    for (kind, key) in [("claude", "CLAUDE_CONFIG_DIR"), ("codex", "CODEX_HOME")] {
        let env = Env::new();
        let home = env.home_root.join("named");
        succeeded(&accounts(
            &env,
            &["add", kind, "work", "--home", home.to_str().unwrap()],
        ));
        let before = succeeded(&accounts(&env, &["list", "--json"]));
        let alias = env.home_root.join("alias");
        std::os::unix::fs::symlink(&home, &alias).unwrap();
        for exported in [&home, &alias] {
            for name in ["work", "new"] {
                let proposed = env.home_root.join(name);
                let mut command = env.rimz();
                command
                    .env_remove("CODEX_HOME")
                    .env_remove("CLAUDE_CONFIG_DIR")
                    .env(key, exported)
                    .args(["accounts", "add", kind, name]);
                if name == "new" {
                    command.args(["--home", proposed.to_str().unwrap()]);
                }
                let error = failed(&command.output().unwrap());
                assert!(
                    error.contains(&format!(
                        "unset `{key}` so `default` resolves to {kind}'s own home"
                    )),
                    "{error}"
                );
                assert!(error.contains("account `work`"), "{error}");
                assert!(
                    error.contains(&format!("env -u {key} rimz accounts add {kind} {name}")),
                    "{error}"
                );
                assert!(!proposed.exists());
                assert_eq!(succeeded(&accounts(&env, &["list", "--json"])), before);
                assert!(!home.join("config.toml.orig").exists());
                assert!(!home.join("settings.json.orig").exists());
            }
        }
        let native = env.home_root.join(format!(".{kind}"));
        succeeded(
            &env.rimz()
                .env_remove("CODEX_HOME")
                .env_remove("CLAUDE_CONFIG_DIR")
                .env(key, native)
                .args(["accounts", "add", kind, "native-export"])
                .output()
                .unwrap(),
        );
    }
}

#[test]
fn accounts_add_creates_a_hooked_home_and_remove_forgets_only_the_entry() {
    let env = Env::new();
    let home = env.home_root.join("claude work");
    let home_arg = home.display().to_string();

    let added = succeeded(&accounts(
        &env,
        &["add", "claude", "work", "--home", &home_arg],
    ));
    assert!(home.join("settings.json").is_file(), "{added}");
    let target = env.home_root.join(".claude/settings.json");
    assert!(home.join("settings.json").is_symlink(), "{added}");
    assert_eq!(
        std::fs::read_link(home.join("settings.json")).unwrap(),
        target
    );
    assert!(std::fs::read_to_string(&target).unwrap().contains("rimz"));
    assert!(
        added.contains(&format!(
            "log in once   CLAUDE_CONFIG_DIR='{home_arg}' claude"
        )),
        "{added}"
    );
    let rerun = succeeded(&accounts(&env, &["add", "claude", "work"]));
    assert!(rerun.contains("hooks up to date"), "{rerun}");
    assert!(rerun.contains("settings already shared"), "{rerun}");
    assert_eq!(
        std::fs::read_link(home.join("settings.json")).unwrap(),
        target
    );
    let moved = failed(&accounts(
        &env,
        &["add", "claude", "work", "--home", "/elsewhere"],
    ));
    assert!(moved.contains("already lives at"), "{moved}");
    assert!(failed(&accounts(&env, &["add", "claude", "default"])).contains("needs no declaring"));
    assert!(failed(&accounts(&env, &["add", "grok", "work"])).contains("claude, codex"));

    let listed: Value =
        serde_json::from_str(&succeeded(&accounts(&env, &["list", "--json"]))).expect("json");
    let work = listed
        .as_array()
        .expect("rows")
        .iter()
        .find(|row| row["kind"] == "claude" && row["name"] == "work")
        .expect("work row");
    assert_eq!(
        work,
        &json!({"kind": "claude", "name": "work", "home": home_arg, "machine_default": false})
    );

    let removed = succeeded(&accounts(&env, &["remove", "claude", "work"]));
    assert!(removed.contains("stay on disk"), "{removed}");
    assert!(home.join("settings.json").is_file());
    let again = succeeded(&accounts(&env, &["remove", "claude", "work"]));
    assert!(again.contains("nothing to remove"), "{again}");
    let started = env
        .rimz()
        .args(["start", "--account", "claude=work"])
        .output()
        .expect("run rimz start");
    assert!(failed(&started).contains("run `rimz accounts add claude work`"));
}

#[test]
fn accounts_add_adopts_existing_settings_without_discarding_them() {
    let env = Env::new();
    let home = env.home_root.join("adopted");
    std::fs::create_dir(&home).unwrap();
    let settings = home.join("settings.json");
    let original = "{\"model\":\"mine\"}";
    std::fs::write(&settings, original).unwrap();
    let added = succeeded(&accounts(
        &env,
        &["add", "claude", "work", "--home", home.to_str().unwrap()],
    ));
    let orig = home.join("settings.json.orig");
    assert!(orig.is_file(), "{added}");
    assert_eq!(std::fs::read_to_string(&orig).unwrap(), original);
    assert!(settings.is_symlink());
    assert!(
        added.contains(&format!("{} → {}", settings.display(), orig.display())),
        "{added}"
    );
}

#[test]
fn machine_account_switch_and_removal_preserve_declared_accounts() {
    let env = Env::new();
    let missing = failed(&accounts(&env, &["use", "codex", "missing"]));
    assert!(
        missing.contains("rimz accounts add codex missing"),
        "{missing}"
    );
    succeeded(&accounts(&env, &["add", "claude", "work"]));
    let used = succeeded(&accounts(&env, &["use", "claude", "work"]));
    assert!(
        used.contains("new rooms") && used.contains("rimz reset"),
        "{used}"
    );
    let listed: Value =
        serde_json::from_str(&succeeded(&accounts(&env, &["list", "--json"]))).unwrap();
    let work = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "work")
        .unwrap();
    assert_eq!(work["machine_default"], true);
    let removed = succeeded(&accounts(&env, &["remove", "claude", "work"]));
    assert!(
        removed.contains("new rooms") && removed.contains("default"),
        "{removed}"
    );
    let listed: Value =
        serde_json::from_str(&succeeded(&accounts(&env, &["list", "--json"]))).unwrap();
    let native = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["kind"] == "claude" && row["name"] == "default")
        .unwrap();
    assert_eq!(native["machine_default"], true);
    succeeded(&accounts(&env, &["use", "claude", "default"]));

    // A hand-set entry for a kind without named accounts is cleared by the
    // fix its refusal names.
    let set = env
        .rimz()
        .args(["config", "set", "accounts.use.grok", "work"])
        .output()
        .expect("run rimz config set");
    succeeded(&set);
    succeeded(&accounts(&env, &["use", "grok", "default"]));
    let listed: Value =
        serde_json::from_str(&succeeded(&accounts(&env, &["list", "--json"]))).unwrap();
    assert!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["kind"] != "grok"),
        "{listed}"
    );
}
