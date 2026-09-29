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
fn accounts_add_creates_a_hooked_home_and_remove_forgets_only_the_entry() {
    let env = Env::new();
    let home = env.home_root.join("claude work");
    let home_arg = home.display().to_string();

    let added = succeeded(&accounts(
        &env,
        &["add", "claude", "work", "--home", &home_arg],
    ));
    assert!(home.join("settings.json").is_file(), "{added}");
    assert!(
        added.contains(&format!(
            "log in once   CLAUDE_CONFIG_DIR='{home_arg}' claude"
        )),
        "{added}"
    );
    let rerun = succeeded(&accounts(&env, &["add", "claude", "work"]));
    assert!(rerun.contains("hooks up to date"), "{rerun}");
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
