//! Integration coverage for `rimz accounts add|list|remove`.

use std::process::Output;

use serde_json::{Value, json};

use crate::common::{Env, hermetic_providers as hermetic, provider_bin};

fn accounts(env: &Env, args: &[&str]) -> Output {
    hermetic(env, &mut env.rimz())
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
        let output = hermetic(&env, command)
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
                assert!(!home.join(".rimz-aside").exists());
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
    assert!(rerun.contains("already linked to"), "{rerun}");
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
            "agents": 0,
            "windows": [],
            "metered": null
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

/// The one timestamp directory under an account home's set-aside root.
fn aside(home: &std::path::Path) -> std::path::PathBuf {
    let mut stamps = std::fs::read_dir(home.join(".rimz-aside"))
        .expect("set-aside root")
        .map(|entry| entry.unwrap().path());
    let stamp = stamps.next().expect("one set-aside directory");
    assert_eq!(stamps.next(), None);
    stamp
}

#[test]
fn accounts_add_shares_everything_but_credentials_and_sets_conflicts_aside() {
    let env = Env::new();
    let home = env.home_root.join("adopted");
    let native = env.home_root.join(".claude");
    std::fs::create_dir_all(native.join("projects")).unwrap();
    std::fs::create_dir_all(home.join("todos")).unwrap();
    std::fs::write(native.join("settings.json"), "{}").unwrap();
    std::fs::write(native.join(".credentials.json"), "default").unwrap();
    let settings = home.join("settings.json");
    let original = "{\"model\":\"mine\"}";
    std::fs::write(&settings, original).unwrap();
    std::fs::write(home.join(".credentials.json"), "work").unwrap();
    std::fs::write(home.join("todos/one"), "mine").unwrap();
    std::fs::create_dir_all(home.join("projects/repo")).unwrap();
    std::fs::write(home.join("projects/repo/session.jsonl"), "old").unwrap();
    let added = succeeded(&accounts(
        &env,
        &["add", "claude", "work", "--home", home.to_str().unwrap()],
    ));
    let kept = aside(&home).join("settings.json");
    assert_eq!(std::fs::read_to_string(&kept).unwrap(), original);
    assert_eq!(
        std::fs::read_link(&settings).unwrap(),
        native.join("settings.json")
    );
    assert!(
        added.contains(&format!(
            "moved {} aside to {}",
            settings.display(),
            kept.display()
        )),
        "{added}"
    );
    for name in ["projects", "todos"] {
        assert_eq!(
            std::fs::read_link(home.join(name)).unwrap(),
            native.join(name)
        );
    }
    assert_eq!(
        std::fs::read_to_string(native.join("todos/one")).unwrap(),
        "mine"
    );
    // No room is live, so the account's own history directory moves aside.
    assert_eq!(
        std::fs::read_to_string(aside(&home).join("projects/repo/session.jsonl")).unwrap(),
        "old"
    );
    assert!(!home.join(".credentials.json").is_symlink());
    assert_eq!(
        std::fs::read_to_string(home.join(".credentials.json")).unwrap(),
        "work"
    );
    assert_eq!(
        std::fs::read_to_string(native.join(".credentials.json")).unwrap(),
        "default"
    );

    let standalone = succeeded(&accounts(
        &env,
        &["add", "claude", "work", "--history", "standalone"],
    ));
    assert!(standalone.contains("unlinked from"), "{standalone}");
    for name in ["projects", "todos"] {
        assert!(!home.join(name).exists(), "{standalone}");
        assert!(native.join(name).is_dir(), "{standalone}");
    }
    assert!(settings.is_symlink());
    succeeded(&accounts(
        &env,
        &["add", "claude", "work", "--history", "shared"],
    ));
    assert_eq!(
        std::fs::read_link(home.join("projects")).unwrap(),
        native.join("projects")
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

#[cfg(unix)]
#[test]
fn accounts_add_history_keeps_the_declared_mode_when_the_switch_is_refused() {
    let env = Env::new();
    let config = env.rimz_home().join("config.toml");
    let home = env.home_root.join("work-home");
    let native = env.home_root.join(".claude");
    succeeded(&accounts(
        &env,
        &[
            "add",
            "claude",
            "work",
            "--home",
            home.to_str().unwrap(),
            "--history",
            "standalone",
        ],
    ));
    let declared = std::fs::read_to_string(&config).unwrap();
    assert!(declared.contains("history = \"standalone\""), "{declared}");

    // Sharing must set the account's own `projects` aside, which a file in
    // the aside directory's place prevents (for root too, unlike a mode).
    std::fs::create_dir_all(home.join("projects")).unwrap();
    std::fs::create_dir_all(native.join("projects")).unwrap();
    std::fs::write(home.join(".rimz-aside"), "").unwrap();
    let refused = accounts(&env, &["add", "claude", "work", "--history", "shared"]);

    let error = failed(&refused);
    assert!(error.contains("cannot link"), "{error}");
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        declared,
        "a refused switch leaves the config as it was"
    );
    assert!(home.join("projects").is_dir() && !home.join("projects").is_symlink());
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
    for args in [
        &["accounts", "list", "--json"][..],
        &["providers", "--json", "--all"][..],
    ] {
        let output = hermetic(&env, &mut env.rimz())
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

/// A fake `claude` whose `auth status` answers from a file the test rewrites,
/// and logs each probe.
#[cfg(unix)]
fn write_fake_claude(env: &Env) -> (std::path::PathBuf, std::path::PathBuf) {
    let answer = env.home_root.join("claude-auth");
    let log = env.home_root.join("claude-probes");
    crate::common::write_path_shim(
        &provider_bin(env),
        "claude",
        &format!(
            "case \"$*\" in\n  \"auth status\") echo probe >> '{log}'; . '{answer}';;\n  *) echo 0.0.0;;\nesac",
            log = log.display(),
            answer = answer.display()
        ),
    );
    (answer, log)
}

#[cfg(unix)]
#[test]
fn list_probes_a_logged_out_account_again_on_every_run() {
    const LOGGED_OUT: &str = "/bin/echo '{\"loggedIn\": false, \"authMethod\": \"none\"}'; exit 1";
    const LOGGED_IN: &str = "/bin/echo '{\"loggedIn\": true, \"authMethod\": \"claude.ai\", \"subscriptionType\": \"max\"}'";
    let env = Env::new();
    let home = env.home_root.join("work home");
    succeeded(&accounts(
        &env,
        &["add", "claude", "work", "--home", home.to_str().unwrap()],
    ));
    let (answer, log) = write_fake_claude(&env);
    let work = || -> Value {
        let rows: Value =
            serde_json::from_str(&succeeded(&accounts(&env, &["list", "--json"]))).unwrap();
        rows.as_array()
            .unwrap()
            .iter()
            .find(|row| row["kind"] == "claude" && row["name"] == "work")
            .cloned()
            .expect("work row")
    };
    let probes = || {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .count()
    };

    std::fs::write(&answer, LOGGED_OUT).unwrap();
    let row = work();
    assert_eq!(row["status"], "logged_out", "{row}");
    assert_eq!(
        row["problem"],
        format!(
            "claude account `work` is logged out; log in once: CLAUDE_CONFIG_DIR='{}' claude",
            home.display()
        )
    );
    let text = succeeded(&accounts(&env, &["list"]));
    assert!(text.contains("  logged out  "), "{text}");
    assert!(text.lines().all(|line| line.trim_end() == line), "{text:?}");
    // Both accounts answered logged out within the TTL, and both are asked again.
    let cold = probes();
    assert_eq!(work()["status"], "logged_out");
    assert_eq!(probes(), cold + 2, "each cached logout is probed once more");

    std::fs::write(&answer, LOGGED_IN).unwrap();
    let row = work();
    assert_eq!(row["status"], "ready", "a login made since shows at once");
    assert_eq!(row.get("problem"), None, "{row}");
    assert_eq!(row["metered"], true, "{row}");
    let settled = probes();
    assert_eq!(work()["status"], "ready");
    assert_eq!(probes(), settled, "a logged-in record keeps the due rule");
}

#[test]
fn a_history_entry_on_another_filesystem_stays_in_the_account_home_and_out_of_the_pool() {
    use std::os::unix::fs::MetadataExt;

    let device = |path: &std::path::Path| std::fs::metadata(path).map(|meta| meta.dev()).ok();
    let probe = Env::new();
    let home_device = device(&probe.home_root);
    let Some(foreign) = ["/dev/shm", "/tmp", "/var/tmp"]
        .into_iter()
        .map(std::path::Path::new)
        .filter(|root| device(root).is_some_and(|dev| Some(dev) != home_device))
        .find_map(|root| tempfile::tempdir_in(root).ok())
    else {
        crate::common::skip("no writable filesystem apart from the fixture HOME");
        return;
    };

    // One fixture per layout: the account home beside the default home, then
    // on another filesystem. Returns the default pool's spend and `add`'s stdout.
    let run = |env: &Env, home: &std::path::Path| -> (String, String) {
        let session = home.join("projects/repo/session.jsonl");
        std::fs::create_dir_all(session.parent().unwrap()).unwrap();
        let now = rimz::agents::spending::unix_secs_now();
        let tod = now % 86_400;
        std::fs::write(
            &session,
            format!(
                r#"{{"timestamp":"{}T{:02}:{:02}:{:02}.000Z","costUSD":0.25,"requestId":"req-1","message":{{"id":"msg-1","usage":{{"input_tokens":1200,"output_tokens":80}}}}}}"#,
                rimz::agents::spending::utc_date(now),
                tod / 3_600,
                (tod % 3_600) / 60,
                tod % 60
            ) + "\n",
        )
        .unwrap();
        std::fs::create_dir_all(env.home_root.join(".claude")).unwrap();
        let added = succeeded(&accounts(
            env,
            &["add", "claude", "work", "--home", home.to_str().unwrap()],
        ));
        succeeded(&env.rimz().arg("stats").output().unwrap());
        let budget = env
            .rimz()
            .args(["budget", "--account", "claude"])
            .output()
            .unwrap();
        (succeeded(&budget), added)
    };

    let local = Env::new();
    let (spend, added) = run(&local, &local.home_root.join("work"));
    assert!(spend.contains("spend:  $0.25 today"), "{spend}\n{added}");

    let env = Env::new();
    let native = env.home_root.join(".claude");
    let home = foreign.path().join("work");
    let (spend, added) = run(&env, &home);
    assert!(
        added.contains(&format!(
            "{} stays in the account home; it cannot move to {} on another filesystem",
            home.join("projects").display(),
            native.display()
        )),
        "{added}"
    );
    let entry = std::fs::symlink_metadata(home.join("projects")).unwrap();
    assert!(entry.is_dir(), "{added}");
    assert!(std::fs::symlink_metadata(native.join("projects")).is_err());
    assert!(spend.contains("spend:  $0.00 today"), "{spend}\n{added}");
}

/// Answer `thread/loaded/list` on the control socket under a Codex `home` with
/// the ids currently in the returned list, as the account's daemon would.
fn serve_loaded_threads(home: &std::path::Path) -> std::sync::Arc<std::sync::Mutex<Vec<String>>> {
    use tungstenite::Message;

    let control = home.join("app-server-control");
    std::fs::create_dir_all(&control).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(control.join("app-server-control.sock"))
        .expect("bind the stand-in control socket");
    let loaded = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let served = loaded.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut peer) = tungstenite::accept(stream.unwrap()) else {
                continue;
            };
            while let Ok(Message::Text(frame)) = peer.read() {
                let request: Value = serde_json::from_str(&frame).unwrap();
                let result = match request["method"].as_str() {
                    Some("initialize") => json!({"userAgent": "codex/0.154.0"}),
                    Some("thread/loaded/list") => json!({"data": *served.lock().unwrap()}),
                    _ => continue,
                };
                let response = json!({"jsonrpc": "2.0", "id": request["id"], "result": result});
                if peer
                    .send(Message::Text(response.to_string().into()))
                    .is_err()
                {
                    break;
                }
            }
        }
    });
    loaded
}

#[test]
fn accounts_add_refuses_a_history_switch_only_while_the_codex_daemon_has_sessions() {
    let env = Env::new();
    // A short name: the control socket under this home must fit a socket path.
    let home = env.home_root.join("cw");
    let home_arg = home.display().to_string();
    succeeded(&accounts(
        &env,
        &["add", "codex", "work", "--home", &home_arg],
    ));
    let sessions = home.join("sessions");
    let linked = std::fs::read_link(&sessions).expect("a shared account links its sessions");

    // The record Codex keeps for its app-server, naming a process this test owns.
    let mut daemon = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("spawn the stand-in daemon");
    let pid = daemon.id().to_string();
    let started = std::process::Command::new("ps")
        .args(["-p", &pid, "-o", "lstart="])
        .output()
        .expect("ps");
    let state_dir = home.join("app-server-daemon");
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::write(
        state_dir.join("app-server.pid"),
        json!({
            "pid": daemon.id(),
            "processStartTime": String::from_utf8_lossy(&started.stdout).trim(),
        })
        .to_string(),
    )
    .unwrap();

    // The daemon check reads the stand-in's start time with the host `ps`,
    // which the hermetic provider `PATH` would hide; `add` probes no provider.
    let switch = || {
        env.rimz()
            .args([
                "accounts",
                "add",
                "codex",
                "work",
                "--history",
                "standalone",
            ])
            .output()
            .expect("run rimz accounts add")
    };
    let toggle = "`rimz config set remote_control.codex false`, rerun, then set it back to `true`";
    let config = "or remove `history = \"standalone\"` from `[accounts.codex.work]`";

    // A daemon that cannot be asked may hold a session.
    let silent = switch();
    let after_silent = std::fs::read_link(&sessions);
    let loaded = serve_loaded_threads(&home);
    loaded.lock().unwrap().push("thread-a".to_owned());
    let holding = switch();
    let after_holding = std::fs::read_link(&sessions);
    loaded.lock().unwrap().clear();
    let idle = switch();
    let still_running = daemon.try_wait().expect("poll the stand-in daemon");
    daemon.kill().expect("kill the stand-in daemon");
    daemon.wait().expect("reap the stand-in daemon");

    let error = failed(&silent);
    assert!(error.contains("cannot unlink `"), "{error}");
    assert!(
        error.contains(&format!(
            "the remote-control daemon on the account did not report its sessions, so it may write through that link, which would be removed under it; stop it with {toggle}, {config}"
        )),
        "{error}"
    );
    assert_eq!(after_silent.unwrap(), linked);

    let error = failed(&holding);
    assert!(error.contains("cannot unlink `"), "{error}");
    assert!(
        error.contains(&format!(
            "the remote-control daemon on the account holds 1 live session(s) that write through that link, which would be removed under them; close them in the remote client and rerun once the daemon has unloaded them, or stop the daemon with {toggle}, {config}"
        )),
        "{error}"
    );
    assert_eq!(after_holding.unwrap(), linked);

    // Idle, the daemon does not block the switch and is left running.
    succeeded(&idle);
    assert!(still_running.is_none(), "{still_running:?}");
    assert!(std::fs::symlink_metadata(&sessions).is_err());
}
