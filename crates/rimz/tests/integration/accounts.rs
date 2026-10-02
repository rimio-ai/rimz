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
fn room_account_switch_changes_only_the_room_default() {
    let env = Env::new();
    succeeded(&accounts(&env, &["add", "claude", "work"]));
    env.record(&env.project_root);
    let workspace = env.resolve_workspace(&env.project_root);
    let store = env.store();
    let config = env.rimz_home().join("config.toml");
    let switch_with = |flags: &[&str]| {
        env.rimz()
            .envs(rimz::workspace::pin_env(
                &workspace.workspace_id,
                &workspace.project_root,
            ))
            .args(["accounts", "use"])
            .args(flags)
            .args(["claude", "work"])
            .output()
            .unwrap()
    };
    let global = succeeded(&switch_with(&["--global"]));
    assert!(
        global.contains("new rooms now use claude account `work`"),
        "{global}"
    );
    assert!(
        rimz::workspace::record::read(&store.paths().workspace_record)
            .unwrap()
            .logins
            .is_none()
    );
    let before = std::fs::read(&config).unwrap();
    let switch = || switch_with(&[]);
    let output = succeeded(&switch());
    assert!(output.contains("this room now launches claude on account `work`; no running claude agent is on `default`"), "{output}");
    assert_eq!(std::fs::read(&config).unwrap(), before);
    let record = rimz::workspace::record::read(&store.paths().workspace_record).unwrap();
    assert_eq!(
        record
            .logins
            .unwrap()
            .get(&rimz::ids::AgentKind::new_unchecked("claude"))
            .unwrap()
            .as_str(),
        "work"
    );
    assert_eq!(
        succeeded(&switch()).trim(),
        "this room already launches claude on `work`"
    );

    let claude_rows = |command: &mut std::process::Command| -> Vec<Value> {
        let output = command
            .env_remove("CLAUDE_CONFIG_DIR")
            .args(["accounts", "list", "--json"])
            .output()
            .unwrap();
        let rows: Value = serde_json::from_str(&succeeded(&output)).unwrap();
        rows.as_array()
            .unwrap()
            .iter()
            .filter(|row| row["kind"] == "claude")
            .cloned()
            .collect()
    };
    let in_room = || {
        let mut command = env.rimz();
        command.envs(rimz::workspace::pin_env(
            &workspace.workspace_id,
            &workspace.project_root,
        ));
        claude_rows(&mut command)
    };
    let row = |rows: &[Value], name: &str| {
        rows.iter()
            .find(|row| row["name"] == name)
            .cloned()
            .unwrap()
    };
    let rows = in_room();
    assert_eq!(row(&rows, "work")["active"], true);
    assert_eq!(
        row(&rows, "work")["default_for"],
        json!(["this_room", "new_rooms"])
    );
    succeeded(&accounts(&env, &["use", "--global", "claude", "default"]));
    let rows = in_room();
    assert_eq!(row(&rows, "work")["active"], true);
    assert_eq!(row(&rows, "work")["default_for"], json!(["this_room"]));
    assert_eq!(row(&rows, "default")["active"], false);
    assert_eq!(row(&rows, "default")["default_for"], json!(["new_rooms"]));

    let elsewhere = env.home_root.join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let rows = claude_rows(env.rimz().current_dir(&elsewhere));
    assert_eq!(row(&rows, "default")["active"], true);
    assert_eq!(row(&rows, "work")["active"], false);
    assert_eq!(row(&rows, "work")["default_for"], json!([]));
}

#[test]
fn room_account_switch_refuses_a_missing_home() {
    let env = Env::new();
    let home = env.home_root.join("missing-home");
    succeeded(&accounts(
        &env,
        &["add", "claude", "work", "--home", home.to_str().unwrap()],
    ));
    std::fs::remove_dir_all(home).unwrap();
    env.record(&env.project_root);
    let workspace = env.resolve_workspace(&env.project_root);
    let output = env
        .rimz()
        .envs(rimz::workspace::pin_env(
            &workspace.workspace_id,
            &workspace.project_root,
        ))
        .args(["accounts", "use", "claude", "work"])
        .output()
        .unwrap();
    assert!(failed(&output).contains("rimz accounts add claude work"));
    assert!(
        rimz::workspace::record::read(&env.store().paths().workspace_record)
            .unwrap()
            .logins
            .is_none()
    );
}

#[test]
fn room_account_switch_refuses_outside_a_room() {
    let env = Env::new();
    let output = accounts(&env, &["use", "claude", "default"]);
    assert!(failed(&output).contains(
        "`rimz accounts use` changes the running room it is run inside; run it inside one, or pass --global to set the machine default for new rooms"
    ));
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
        &json!({
            "kind": "claude",
            "name": "work",
            "home": home_arg,
            "history": "shared",
            "machine_default": false,
            "status": "ready",
            "active": false,
            "default_for": [],
            "agents": 0
        })
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
fn accounts_add_history_writes_only_that_field() {
    let env = Env::new();
    let config = env.rimz_home().join("config.toml");
    let home = env.home_root.join("work-home");
    let listed = |name: &str| -> serde_json::Value {
        let rows: Vec<serde_json::Value> =
            serde_json::from_str(&succeeded(&accounts(&env, &["list", "--json"]))).unwrap();
        rows.into_iter()
            .find(|row| row["kind"] == "claude" && row["name"] == name)
            .unwrap_or_else(|| panic!("no claude row `{name}`"))["history"]
            .clone()
    };

    succeeded(&accounts(
        &env,
        &["add", "claude", "work", "--home", home.to_str().unwrap()],
    ));
    let declared = std::fs::read_to_string(&config).unwrap();
    assert!(!declared.contains("\nhistory = "), "{declared}");
    assert_eq!(listed("work"), "shared");
    assert_eq!(listed("default"), serde_json::Value::Null);

    succeeded(&accounts(
        &env,
        &["add", "claude", "work", "--history", "standalone"],
    ));
    let home_line = format!("home = \"{}\"\n", home.display());
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        declared.replace(
            &home_line,
            &format!("{home_line}history = \"standalone\"\n")
        )
    );
    assert_eq!(listed("work"), "standalone");

    succeeded(&accounts(&env, &["add", "claude", "work"]));
    assert_eq!(
        listed("work"),
        "standalone",
        "a rerun leaves the mode alone"
    );

    succeeded(&accounts(
        &env,
        &["add", "claude", "solo", "--history", "standalone"],
    ));
    assert_eq!(listed("solo"), "standalone");
    let error = failed(&accounts(
        &env,
        &["add", "claude", "other", "--history", "mine"],
    ));
    assert!(
        error.contains("shared") && error.contains("standalone"),
        "{error}"
    );
}

#[test]
fn machine_account_switch_and_removal_preserve_declared_accounts() {
    let env = Env::new();
    let missing = failed(&accounts(&env, &["use", "--global", "codex", "missing"]));
    assert!(
        missing.contains("rimz accounts add codex missing"),
        "{missing}"
    );
    succeeded(&accounts(&env, &["add", "claude", "work"]));
    let used = succeeded(&accounts(&env, &["use", "--global", "claude", "work"]));
    assert!(
        used.contains("new rooms")
            && used.contains("existing rooms, running or stopped")
            && used.contains("`rimz accounts use claude work`"),
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
    succeeded(&accounts(&env, &["use", "--global", "claude", "default"]));

    // A hand-set entry for a kind without named accounts is cleared by the
    // fix its refusal names.
    let set = env
        .rimz()
        .args(["config", "set", "accounts.use.grok", "work"])
        .output()
        .expect("run rimz config set");
    succeeded(&set);
    succeeded(&accounts(&env, &["use", "--global", "grok", "default"]));
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

#[test]
fn a_broken_project_config_warns_and_marks_no_account() {
    let env = Env::new();
    let project = env.project_root.join(".rimz");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("config.toml"), "[accounts").unwrap();
    let empty_path = env.home_root.join("empty-path");
    std::fs::create_dir_all(&empty_path).unwrap();
    for args in [
        &["accounts", "list", "--json"][..],
        &["providers", "--json", "--all"][..],
    ] {
        let output = env
            .rimz()
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME")
            .env("PATH", &empty_path)
            .env("RIMZ_OAUTH_USAGE_OFFLINE", "1")
            .args(args)
            .output()
            .expect("run rimz");
        let rows: Value = serde_json::from_str(&succeeded(&output)).expect("json rows");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(stderr.matches("warning").count(), 1, "{args:?}: {stderr}");
        assert!(stderr.contains("config.toml"), "{args:?}: {stderr}");
        let rows = rows.as_array().unwrap();
        assert!(
            rows.iter().all(|row| row["active"] == false),
            "{args:?}: no account is marked: {rows:?}"
        );
        let claude_default = rows
            .iter()
            .find(|row| {
                row["kind"] == "claude" && (row["name"] == "default" || row["account"] == "default")
            })
            .unwrap_or_else(|| panic!("{args:?}: no claude default row in {rows:?}"));
        assert_eq!(
            claude_default["default_for"],
            json!(["new_rooms"]),
            "{args:?}: machine scopes stay"
        );
    }
}
