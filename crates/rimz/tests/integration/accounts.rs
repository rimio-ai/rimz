//! Integration coverage for `rimz accounts add|list|remove`.

use std::path::Path;
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

#[test]
fn accounts_and_providers_json_keep_day_and_month_budget_fields_separate() {
    use rimz::agents::spending::{
        ProviderSpendingCache, SpendTally, SpendWindow, utc_month_start_secs,
        write_provider_spending_cache,
    };
    let env = Env::new();
    env.record(&env.project_root);
    std::fs::write(
        env.rimz_home().join("config.toml"),
        "[accounts.budget]\nclaude = '10/month'\n",
    )
    .unwrap();
    crate::common::write_path_shim(
        &provider_bin(&env),
        "claude",
        concat!(
            "case \"$1\" in\n--version) echo '2.1.0 (Claude Code)';;\n",
            "auth) echo '{\"loggedIn\":true,\"authMethod\":\"api_key\"}';;\nesac",
        ),
    );
    let key: rimz::ids::LoginKey = "claude@default".parse().unwrap();
    let now = jiff::Timestamp::now();
    let recorded = SpendWindow {
        usd: 99.0,
        sessions: 1,
        ..Default::default()
    };
    let tally = SpendTally {
        month: recorded.clone(),
        year: recorded,
        ..Default::default()
    };
    let mut cache = ProviderSpendingCache {
        month_cutoff_secs: utc_month_start_secs(now.as_second() as u64).unwrap(),
        month_by_login: [(
            key.clone(),
            SpendWindow {
                usd: 6.0,
                ..Default::default()
            },
        )]
        .into(),
        ..Default::default()
    };
    cache.spending.by_login.insert(key, tally.clone());
    cache
        .spending
        .by_provider
        .insert("claude".to_owned(), tally);
    write_provider_spending_cache(&env.runtime_paths().shared_provider_spending_path(), &cache);
    for args in [
        vec!["accounts", "show", "claude", "--json"],
        vec!["providers", "claude", "--json"],
    ] {
        let output = hermetic(&env, &mut env.rimz()).args(args).output().unwrap();
        let rows: Value = serde_json::from_str(&succeeded(&output)).unwrap();
        assert_eq!(rows[0]["day_budget"], Value::Null);
        assert_eq!(
            rows[0]["month_budget"],
            json!({"cap_usd":10.0,"spend_usd":6.0,"parked":false})
        );
    }
    let detail = succeeded(&accounts(&env, &["show", "claude"]));
    assert!(detail.contains("$6.00 of $10.00/month"), "{detail}");
}

#[test]
fn unsupported_login_check_does_not_claim_logout() {
    let env = Env::new();
    crate::common::write_path_shim(&provider_bin(&env), "kiro-cli", "echo 2.31.0");
    succeeded(
        &hermetic(&env, &mut env.rimz())
            .args(["hooks", "install", "kiro"])
            .output()
            .unwrap(),
    );
    let rows: Value =
        serde_json::from_str(&succeeded(&accounts(&env, &["show", "kiro", "--json"]))).unwrap();
    let row = &rows[0];
    assert_eq!(row["login"], "logged_out", "retain the raw probe record");
    assert_eq!(row["status"], "ready", "{row}");
    assert_eq!(row.get("problem"), None, "{row}");
    for args in [&["--all"][..], &["show", "kiro"][..]] {
        let text = succeeded(&accounts(&env, args));
        assert!(text.contains("no login check"), "{text}");
        assert!(
            !text.contains("logged out") && !text.contains("log in once"),
            "{text}"
        );
    }
}

#[test]
fn unavailable_login_probe_keeps_accounts_stderr_quiet() {
    let env = Env::new();
    crate::common::write_path_shim(
        &provider_bin(&env),
        "claude",
        "case \"$1\" in --version) echo 2.1.304;; auth) exit 42;; esac",
    );
    succeeded(
        &hermetic(&env, &mut env.rimz())
            .args(["hooks", "install", "claude"])
            .output()
            .unwrap(),
    );
    let plain = accounts(&env, &["--refresh"]);
    let text = succeeded(&plain);
    assert!(
        plain.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&plain.stderr)
    );
    assert!(
        text.lines()
            .any(|line| line.starts_with("Not checked, launches proceed:")
                && line.contains("claude (login read failed: exited 42)")),
        "{text}"
    );
    let alias = hermetic(&env, &mut env.rimz())
        .args(["providers", "--refresh"])
        .output()
        .unwrap();
    assert_eq!(succeeded(&alias), text);
    assert_eq!(
        String::from_utf8_lossy(&alias.stderr),
        "rimz: warning: `rimz providers` is deprecated and goes in the next release; use `rimz accounts` (`rimz accounts show <kind>` for one provider)\n"
    );
}

#[test]
fn single_markers_disappear_but_filtered_multi_account_markers_remain() {
    let env = Env::new();
    crate::common::write_path_shim(
        &provider_bin(&env),
        "claude",
        r#"case "$1" in --version) echo 2.1.304;; auth) printf '{"loggedIn":true,"subscriptionType":"pro"}\n';; esac"#,
    );
    succeeded(
        &hermetic(&env, &mut env.rimz())
            .args(["hooks", "install", "claude"])
            .output()
            .unwrap(),
    );
    let single = succeeded(&accounts(&env, &["show", "claude"]));
    assert!(!single.contains('●') && !single.contains('○'), "{single}");
    assert!(!single.contains("next launch"), "{single}");
    succeeded(&accounts(&env, &["add", "claude", "work"]));
    let default = succeeded(&accounts(&env, &["show", "claude@default"]));
    assert!(default.starts_with("● Claude"), "{default}");
    assert!(
        default.contains("● next launch from here (machine default)"),
        "{default}"
    );
    succeeded(&accounts(&env, &["use", "--global", "claude", "work"]));
    let named = succeeded(&accounts(&env, &["show", "claude@work"]));
    assert!(named.starts_with("● Claude · claude@work"), "{named}");
    assert!(
        named.contains("● next launch from here (machine default)"),
        "{named}"
    );
}

#[test]
fn bare_accounts_runs_list() {
    let env = Env::new();
    let bare = succeeded(&accounts(&env, &[]));
    assert_eq!(
        bare,
        "No provider is set up here; `rimz accounts --all` lists every supported one.\n"
    );
    assert!(!bare.contains("KIND") && !bare.contains("STATUS"), "{bare}");
    assert_eq!(bare, succeeded(&accounts(&env, &["list"])));
    let alias = hermetic(&env, &mut env.rimz())
        .arg("providers")
        .output()
        .unwrap();
    assert_eq!(bare, succeeded(&alias));
    assert_eq!(String::from_utf8_lossy(&alias.stderr).lines().count(), 1);
    assert!(String::from_utf8_lossy(&alias.stderr).contains("is deprecated"));
}

#[test]
fn global_receipt_reports_unknown_when_the_previous_selection_cannot_resolve() {
    let env = Env::new();
    env.record(&env.project_root);
    let workspace = env.resolve_workspace(&env.project_root);
    succeeded(
        &env.rimz()
            .args(["config", "set", "accounts.use.grok", "work"])
            .output()
            .unwrap(),
    );
    let used = succeeded(
        &hermetic(&env, &mut env.rimz())
            .env("RIMZ_ZELLIJ_BIN", crate::common::zellij_trace_shim())
            .env("RIMZ_TEST_ZELLIJ_LOG", env.home_root.join("use.trace"))
            .env(
                "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
                format!("{} [Created 1m ago]\n", workspace.session_name),
            )
            .args(["accounts", "use", "--global", "grok", "default"])
            .output()
            .unwrap(),
    );
    assert!(used.contains("count unknown"), "{used}");
}

#[test]
fn global_receipt_counts_only_agents_in_rooms_following_the_machine() {
    use rimz::ids::{AgentKind, AgentSessionId, PaneId};
    use rimz::store::event::{AgentLaunchPayload, AgentLaunchState, EventEnvelope};

    let env = Env::new();
    succeeded(&accounts(&env, &["add", "claude", "work"]));
    let mut sessions = String::new();
    for (index, selection) in ["machine", "pin", "project"].into_iter().enumerate() {
        let root = env.home_root.join(selection);
        env.record(&root);
        let workspace = env.resolve_workspace(&root);
        let store = env.store_for(&root);
        if selection == "pin" {
            store
                .switch_room_login(
                    &workspace,
                    &AgentKind::new_unchecked("claude"),
                    &rimz::ids::LoginName::default_login(),
                )
                .unwrap();
        } else if selection == "project" {
            env.write_config(&root, "[accounts]\nclaude = 'default'\n");
            succeeded(
                &env.rimz()
                    .current_dir(&root)
                    .args(["trust", "grant"])
                    .output()
                    .unwrap(),
            );
        }
        store
            .append_event(&EventEnvelope::agent_launched(
                workspace.workspace_id,
                &workspace.session_name,
                &AgentKind::new_unchecked("claude"),
                AgentLaunchPayload {
                    system_prompt_digest: None,
                    agent_id: AgentSessionId::from(selection),
                    launch_id: None,
                    agent_name: selection.to_owned(),
                    agent_name_explicit: true,
                    launch: Default::default(),
                    state: AgentLaunchState::Bound,
                    run_id: None,
                    pane_id: Some(PaneId::from_parts(
                        rimz::MuxName::Zellij,
                        format!("terminal_{index}"),
                    )),
                    runtime_owner: None,
                    worktree_path: Some(root.display().to_string()),
                    worktree_branch: None,
                    prompt: None,
                    description: None,
                },
            ))
            .unwrap();
        sessions.push_str(&format!("{} [Created 1m ago]\n", workspace.session_name));
    }
    let used = succeeded(
        &hermetic(&env, &mut env.rimz())
            .env("RIMZ_ZELLIJ_BIN", crate::common::zellij_trace_shim())
            .env("RIMZ_TEST_ZELLIJ_LOG", env.home_root.join("use.trace"))
            .env("RIMZ_TEST_ZELLIJ_LIST_SESSIONS", sessions)
            .args(["accounts", "use", "--global", "claude", "work"])
            .output()
            .unwrap(),
    );
    assert!(
        used.contains("new launches only; 1 running claude agent(s) stay on claude@default"),
        "{used}"
    );
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

fn room_accounts(env: &Env, args: &[&str]) -> Output {
    let workspace = env.resolve_workspace(&env.project_root);
    hermetic(env, &mut env.rimz())
        .envs(rimz::workspace::pin_env(
            &workspace.workspace_id,
            &workspace.project_root,
        ))
        .arg("accounts")
        .args(args)
        .output()
        .unwrap()
}

fn trust_codex_account(env: &Env, home: &Path) {
    crate::common::trust_codex_hooks(env);
    let native = env.agent_config_path("codex");
    let config = home.join("config.toml");
    let mut table: toml::Table = std::fs::read_to_string(&config).unwrap().parse().unwrap();
    let state = table["hooks"]["state"].as_table_mut().unwrap();
    let prefix = format!("{}:", native.display());
    let account_state = state
        .iter()
        .filter_map(|(key, value)| {
            let event = key.strip_prefix(prefix.as_str())?;
            Some((format!("{}:{event}", config.display()), value.clone()))
        })
        .collect::<Vec<_>>();
    state.extend(account_state);
    std::fs::write(&config, toml::to_string(&table).unwrap()).unwrap();
}

#[cfg(unix)]
#[test]
fn room_account_reset_unpins_and_follows_the_machine_selection() {
    let env = Env::new();
    succeeded(&accounts(&env, &["add", "codex", "work"]));
    trust_codex_account(&env, &env.rimz_home().join("accounts/codex/work"));
    succeeded(&accounts(&env, &["add", "claude", "team"]));
    succeeded(&accounts(&env, &["use", "--global", "codex", "work"]));
    env.record(&env.project_root);
    succeeded(&room_accounts(&env, &["use", "codex", "default"]));
    succeeded(&room_accounts(&env, &["use", "claude", "team"]));
    env.store()
        .begin_agent_launch_batch(
            &[rimz::store::writer::AgentLaunchRequest {
                kind: rimz::ids::AgentKind::new_unchecked("codex"),
                login: rimz::store::writer::LaunchLogin::RoomDefault,
                agent_id: rimz::ids::AgentSessionId::from("reset-runner"),
                name: rimz::store::writer::AgentLaunchName::Explicit("runner".to_owned()),
                launch: rimz::agents::LaunchParams::default(),
                run_id: None,
                prompt: None,
            }],
            rimz::store::writer::AgentLaunchScope {
                session_name: env.resolve_workspace(&env.project_root).session_name,
                cwd: env.project_root.clone(),
                branch: None,
                description: None,
            },
        )
        .unwrap();
    let config = env.rimz_home().join("config.toml");
    let before = std::fs::read(&config).unwrap();
    let reset = succeeded(&room_accounts(&env, &["use", "--reset", "codex"]));
    assert_eq!(
        reset.trim(),
        "this room now follows the machine default for codex (`codex@work`); new launches only; 1 running codex agent(s) stay on codex@default"
    );
    let record = rimz::workspace::record::read(&env.store().paths().workspace_record).unwrap();
    assert_eq!(
        serde_json::to_value(record).unwrap()["pins"],
        json!({"claude": "team"})
    );
    assert_eq!(
        succeeded(&room_accounts(&env, &["use", "--reset", "codex"])).trim(),
        "this room already follows the machine default for codex (`codex@work`)"
    );
    assert_eq!(std::fs::read(&config).unwrap(), before);
    succeeded(&accounts(&env, &["use", "--global", "codex", "default"]));
    let rows: Value =
        serde_json::from_str(&succeeded(&room_accounts(&env, &["list", "--json"]))).unwrap();
    assert!(
        rows.as_array()
            .unwrap()
            .iter()
            .any(|row| row["account"] == "codex@default" && row["active"] == true)
    );
    let workspace = env.resolve_workspace(&env.project_root);
    let output = hermetic(&env, &mut env.rimz())
        .envs(rimz::workspace::pin_env(
            &workspace.workspace_id,
            &workspace.project_root,
        ))
        .env("RIMZ_ZELLIJ_BIN", crate::common::zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", env.home_root.join("launch.trace"))
        .env("ZELLIJ_SESSION_NAME", &workspace.session_name)
        .env("ZELLIJ_PANE_ID", "1")
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
            format!("{} [Created 1m ago]\n", workspace.session_name),
        )
        .env("RIMZ_TEST_ZELLIJ_LIST_PANES", "[]")
        .args([
            "agents",
            "codex",
            "--name",
            "after-switch",
            "--new-tab",
            "--isolation",
            "host",
        ])
        .output()
        .unwrap();
    succeeded(&output);
    let snapshot = env.store().snapshot_cached().unwrap();
    let launched = snapshot
        .agents
        .iter()
        .find(|agent| agent.name.as_deref() == Some("after-switch"))
        .unwrap();
    assert_eq!(
        launched.login, None,
        "the consumer launch stamp follows the changed machine selection"
    );
    let reset = succeeded(&room_accounts(&env, &["use", "--reset", "codex"]));
    assert!(
        reset.contains("follows the provider default for codex (`codex@default`)"),
        "{reset}"
    );
}

#[test]
fn global_account_switch_refuses_failed_preflight_without_writing() {
    let env = Env::new();
    let home = env.home_root.join("missing-home");
    succeeded(&accounts(
        &env,
        &["add", "claude", "work", "--home", home.to_str().unwrap()],
    ));
    std::fs::remove_dir_all(&home).unwrap();
    let config = env.rimz_home().join("config.toml");
    let before = std::fs::read(&config).unwrap();
    let error = failed(&accounts(&env, &["use", "--global", "claude", "work"]));
    assert!(error.contains("is not a directory"), "{error}");
    assert_eq!(std::fs::read(&config).unwrap(), before);

    succeeded(&accounts(&env, &["add", "codex", "untrusted"]));
    let before = std::fs::read(&config).unwrap();
    let error = failed(&accounts(&env, &["use", "--global", "codex", "untrusted"]));
    assert!(error.contains("/hooks"), "{error}");
    assert_eq!(std::fs::read(&config).unwrap(), before);
}

#[test]
fn invalid_kind_keeps_its_error_and_default_beside_healthy_accounts() {
    invalid_kind_report(false);
}

#[test]
fn doctor_invalid_kind_keeps_its_error_and_default_beside_healthy_accounts() {
    invalid_kind_report(true);
}

fn invalid_kind_report(doctor: bool) {
    let env = Env::new();
    crate::common::write_path_shim(&provider_bin(&env), "codex", "echo 0.0.0");
    succeeded(&accounts(&env, &["add", "claude", "work"]));
    let config = env.rimz_home().join("config.toml");
    let mut machine = std::fs::read_to_string(&config).unwrap();
    machine.push_str("\n[accounts.codex.work]\nhome = '/srv/duplicate'\n[accounts.codex.dup]\nhome = '/srv/duplicate'\n");
    std::fs::write(config, machine).unwrap();
    let output = if doctor {
        hermetic(&env, &mut env.rimz())
            .args(["doctor", "--json"])
            .output()
            .unwrap()
    } else {
        accounts(&env, &["list", "--json"])
    };
    let report: Value = serde_json::from_str(&succeeded(&output)).unwrap();
    let warning = String::from_utf8_lossy(&output.stderr);
    if doctor {
        let human_output = hermetic(&env, &mut env.rimz())
            .arg("doctor")
            .output()
            .unwrap();
        let human = succeeded(&human_output);
        assert!(
            human.lines().any(|row| row.contains("codex")
                && row.contains("default")
                && row.contains("give each account its own directory")),
            "failed kind retains its unavailable default row and error: {human}"
        );
        for output in [&output, &human_output] {
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                !stderr.contains("give each account its own directory"),
                "declaration errors belong in the report, not stderr: {stderr}"
            );
        }
    } else {
        assert!(
            warning.contains("give each account its own directory"),
            "{warning}"
        );
    }
    let rows = if doctor {
        &report["accounts"]["ready"]["rows"]
    } else {
        &report
    };
    let rows = rows.as_array().unwrap();
    assert!(
        rows.iter().any(|row| if doctor {
            row["kind"] == "claude" && row["name"] == "work"
        } else {
            row["account"] == "claude@work"
        }),
        "{rows:?}"
    );
    let default = rows
        .iter()
        .find(|row| {
            if doctor {
                row["kind"] == "codex" && row["name"] == "default"
            } else {
                row["account"] == "codex@default"
            }
        })
        .expect("failed kind retains its default row");
    assert!(
        default["problem"]
            .as_str()
            .unwrap()
            .contains("give each account its own directory")
    );
    if !doctor {
        assert_eq!(default["active"], false);
        let human = succeeded(&accounts(&env, &["list", "--all"]));
        assert!(human.contains("Other providers"), "{human}");
        assert!(
            human.contains("give each account its own directory"),
            "{human}"
        );
        let default_row = human
            .lines()
            .find(|row| row.trim_start().starts_with("Codex ") && row.contains("unavailable"))
            .expect("failed kind retains its unavailable default row in Other providers");
        assert_eq!(default["default_for"], json!([]), "{human}");
        assert_eq!(default["machine_default"], false, "{human}");
        assert!(
            !default_row.contains('○') && !default_row.contains('●'),
            "{human}"
        );
    }
}

#[test]
fn room_account_reset_uses_project_trust_and_never_writes_on_refusal() {
    let env = Env::new();
    succeeded(&accounts(&env, &["add", "codex", "work"]));
    trust_codex_account(&env, &env.rimz_home().join("accounts/codex/work"));
    env.record(&env.project_root);
    succeeded(&room_accounts(&env, &["use", "codex", "default"]));
    let project_config = env.project_root.join(".rimz/config.toml");
    std::fs::create_dir_all(project_config.parent().unwrap()).unwrap();
    std::fs::write(&project_config, "[accounts]\ncodex = \"work\"\n").unwrap();
    let path = env.store().paths().workspace_record.clone();
    let before = std::fs::read(&path).unwrap();
    let error = failed(&room_accounts(&env, &["use", "--reset", "codex"]));
    assert!(
        error.contains("project account selections in .rimz/config.toml are untrusted")
            && error.contains("rimz trust grant"),
        "{error}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    succeeded(&env.rimz().args(["trust", "grant"]).output().unwrap());
    let reset = succeeded(&room_accounts(&env, &["use", "--reset", "codex"]));
    assert!(
        reset.contains("follows the project default for codex (`codex@work`)"),
        "{reset}"
    );
    let before = std::fs::read(&path).unwrap();
    std::fs::write(&project_config, "[accounts]\ncodex = \"default\"\n").unwrap();
    let error = failed(&room_accounts(&env, &["use", "--reset", "codex"]));
    assert!(
        error.contains("stale") && error.contains("rimz trust grant"),
        "{error}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn room_account_reset_refuses_a_missing_home_without_writing() {
    let env = Env::new();
    let home = env.home_root.join("missing-home");
    succeeded(&accounts(
        &env,
        &["add", "codex", "work", "--home", home.to_str().unwrap()],
    ));
    trust_codex_account(&env, &home);
    succeeded(&accounts(&env, &["use", "--global", "codex", "work"]));
    env.record(&env.project_root);
    succeeded(&room_accounts(&env, &["use", "codex", "default"]));
    std::fs::remove_dir_all(home).unwrap();
    let path = env.store().paths().workspace_record.clone();
    let before = std::fs::read(&path).unwrap();
    let error = failed(&room_accounts(&env, &["use", "--reset", "codex"]));
    assert!(
        error.contains("is not a directory; run `rimz accounts add codex work`"),
        "{error}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn room_account_reset_refuses_outside_a_room() {
    let env = Env::new();
    let error = failed(&accounts(&env, &["use", "--reset", "codex"]));
    assert!(error.contains("`rimz accounts use` changes the running room it is run inside; run it inside one, or pass --global to set the machine default for new rooms"), "{error}");
}

#[test]
fn room_account_reset_refuses_unreadable_core_without_writing() {
    let env = Env::new();
    env.record(&env.project_root);
    let path = env.store().paths().workspace_record.clone();
    let before = std::fs::read(&path).unwrap();
    std::fs::write(env.rimz_home().join("config.toml"), "[accounts.use\n").unwrap();
    let error = failed(&room_accounts(&env, &["use", "--reset", "codex"]));
    assert!(
        error.contains("cannot reset this room's account") && error.contains("rimz config get"),
        "{error}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
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
        global.contains(
            "new rooms and rooms following the machine default now use claude account `claude@work`"
        ),
        "{global}"
    );
    assert!(
        rimz::workspace::record::read(&store.paths().workspace_record)
            .unwrap()
            .pins
            .is_empty()
    );
    let before = std::fs::read(&config).unwrap();
    let switch = || switch_with(&[]);
    let output = succeeded(&switch());
    assert!(output.contains("this room now launches claude on account `claude@work` (pinned, was machine default); new launches only; 0 running claude agent(s) stay on claude@work"), "{output}");
    assert_eq!(std::fs::read(&config).unwrap(), before);
    let record = rimz::workspace::record::read(&store.paths().workspace_record).unwrap();
    assert_eq!(
        record
            .pins
            .get(&rimz::ids::AgentKind::new_unchecked("claude"))
            .unwrap()
            .as_str(),
        "work"
    );
    assert_eq!(
        succeeded(&switch()).trim(),
        "this room already launches claude on `claude@work` (pinned)"
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
            .find(|row| row["account"] == format!("claude@{name}"))
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
            .pins
            .is_empty()
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
            .any(|row| row["account"] == "claude@al")
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
        let listed = || {
            let mut rows: Value =
                serde_json::from_str(&succeeded(&accounts(&env, &["list", "--json"]))).unwrap();
            for row in rows.as_array_mut().unwrap() {
                if row["kind"] == "pi" {
                    assert!(row["probed_at"].is_string());
                    row["probed_at"] = json!("<probe time>");
                }
            }
            rows
        };
        let before = listed();
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
                    error.contains(&format!("unset `{key}` before adding or updating accounts")),
                    "{error}"
                );
                assert!(error.contains("account `work`"), "{error}");
                assert!(
                    error.contains(&format!("env -u {key} rimz accounts add {kind} {name}")),
                    "{error}"
                );
                assert!(!proposed.exists());
                assert_eq!(listed(), before);
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
fn pi_accounts_share_extensions_and_history_but_keep_logins_private() {
    let env = Env::new();
    let native = env.home_root.join(".pi/agent");
    std::fs::create_dir_all(&native).unwrap();
    let private = [
        "auth.json",
        "models-store.json",
        "mcp-auth.json",
        "oauth.json",
        "oauth.json.migrated",
        "mcp.log",
        "mcp.log.1",
        "crashes.json",
        "pi-debug.log",
        "pi-tui-debug.log",
        "pi-tui-crash.log",
        "auth.json.lock",
        "settings.json.tmp.123",
    ];
    for name in private {
        std::fs::write(native.join(name), "{}").unwrap();
    }
    let added = succeeded(
        &hermetic(&env, &mut env.rimz())
            .env_remove("RIMZ_PI_EXTENSION")
            .args(["accounts", "add", "pi", "work"])
            .output()
            .unwrap(),
    );
    let home = env.rimz_home().join("accounts/pi/work");
    for name in ["extensions", "sessions"] {
        assert!(home.join(name).is_symlink(), "{name}: {added}");
        assert!(native.join(name).is_dir(), "{name}: {added}");
        assert_eq!(
            home.join(name).canonicalize().unwrap(),
            native.join(name).canonicalize().unwrap(),
        );
    }
    assert!(native.join("extensions/rimz.ts").is_file(), "{added}");
    for name in private {
        assert!(!home.join(name).exists(), "{name} must stay private");
        assert_eq!(std::fs::read_to_string(native.join(name)).unwrap(), "{}");
    }
    assert!(
        added.contains(&format!("linked to {}:", native.display())),
        "{added}"
    );
    let extension = home.join("extensions/rimz.ts");
    let printed_extension = format!(
        "~/{}",
        extension.strip_prefix(&env.home_root).unwrap().display()
    );
    assert!(added.contains(&printed_extension), "{added}");
    assert!(
        added.contains(&format!(
            "log in once   PI_CODING_AGENT_DIR={} pi",
            home.display()
        )),
        "{added}"
    );
    let selected = succeeded(&accounts(&env, &["use", "--global", "pi", "work"]));
    assert!(selected.contains("pi@work"), "{selected}");
    succeeded(&accounts(&env, &["remove", "pi", "work"]));
    let config = std::fs::read_to_string(env.rimz_home().join("config.toml")).unwrap();
    assert!(!config.contains("[accounts.pi.work]"), "{config}");
    assert!(home.join("extensions/rimz.ts").is_file());
}

#[test]
fn pi_standalone_accounts_link_settings_and_packages_not_sessions_or_caches() {
    let env = Env::new();
    let native = env.home_root.join(".pi/agent");
    std::fs::create_dir_all(&native).unwrap();
    let files = [
        "settings.json",
        "models.json",
        "mcp.json",
        "keybindings.json",
        "trust.json",
        "AGENTS.md",
        "CLAUDE.md",
        "SYSTEM.md",
        "APPEND_SYSTEM.md",
    ];
    for name in files {
        std::fs::write(native.join(name), "{}").unwrap();
    }
    for name in ["sessions", "bin", "tmp"] {
        std::fs::create_dir(native.join(name)).unwrap();
    }
    let added = succeeded(
        &hermetic(&env, &mut env.rimz())
            .env_remove("RIMZ_PI_EXTENSION")
            .args(["accounts", "add", "pi", "solo", "--history", "standalone"])
            .output()
            .unwrap(),
    );
    let home = env.rimz_home().join("accounts/pi/solo");
    for name in files
        .into_iter()
        .chain(["extensions", "skills", "prompts", "themes", "npm", "git"])
    {
        assert!(home.join(name).is_symlink(), "{name}: {added}");
        assert_eq!(
            home.join(name).canonicalize().unwrap(),
            native.join(name).canonicalize().unwrap(),
        );
    }
    for name in ["sessions", "bin", "tmp"] {
        assert!(!home.join(name).exists(), "{name} is not shared: {added}");
    }
    assert!(native.join("extensions/rimz.ts").is_file(), "{added}");
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
        Path::new("../.claude/settings.json")
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
        Path::new("../.claude/settings.json")
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
        .find(|row| row["kind"] == "claude" && row["account"] == "claude@work")
        .expect("work row");
    assert_eq!(
        work,
        &json!({
            "kind": "claude",
            "account": "claude@work",
            "home": home_arg,
            "history": "shared",
            "label": null,
            "machine_default": false,
            "status": "login_unknown",
            "login": "unavailable",
            "entitlement": "ok",
            "active": false,
            "default_for": [],
            "agents": 0,
            "windows": [],
            "metered": null,
            "spend_step": "subscription",
            "pool": "claude@default",
            "product_name": "Claude · work",
            "probed_at": work["probed_at"],
            "probe_error": work["probe_error"],
            "plan": null,
            "plan_label": null,
            "account_id": null,
            "sub_provider": null,
            "account_scope": null,
            "version": null,
            "spending": null,
            "day_budget": null,
            "month_budget": null
        })
    );

    let detail: Value = serde_json::from_str(&succeeded(&accounts(
        &env,
        &["show", "claude@work", "--json"],
    )))
    .unwrap();
    assert_eq!(detail.as_array().unwrap().len(), 1);
    assert_eq!(detail[0]["account"], "claude@work");
    assert_eq!(detail[0]["home"], home_arg);
    let kind: Value =
        serde_json::from_str(&succeeded(&accounts(&env, &["show", "claude", "--json"]))).unwrap();
    assert_eq!(kind.as_array().unwrap().len(), 2);
    let missing = failed(&accounts(&env, &["show", "claude@missing"]));
    assert!(
        missing.contains("no claude account `missing`; configured: default, work"),
        "{missing}"
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
        Path::new("../.claude/settings.json")
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
            Path::new("../.claude").join(name)
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
        Path::new("../.claude/projects")
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
            .find(|row| row["kind"] == "claude" && row["account"] == format!("claude@{name}"))
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
fn accounts_add_inline_history_updates_config_and_links() {
    for (kind, history_dirs) in [
        ("claude", &["projects"][..]),
        ("codex", &["sessions", "archived_sessions"][..]),
    ] {
        let env = Env::new();
        succeeded(&accounts(&env, &["add", kind, "team-1"]));
        let config = env.rimz_home().join("config.toml");
        let home = env.rimz_home().join("accounts").join(kind).join("team-1");
        let seed = format!(
            "[accounts.{kind}]\nteam-2 = {{}}\n# team one\nteam-1 = {{ history = \"shared\" }}   # hand\n"
        );
        std::fs::write(&config, seed).unwrap();
        for entry in history_dirs {
            assert!(
                home.join(entry).is_symlink(),
                "missing shared {kind} {entry}"
            );
        }
        let args = ["add", kind, "team-1", "--history", "standalone"];
        let output = accounts(&env, &args);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let expected = format!(
            "[accounts.{kind}]\nteam-2 = {{}}\n\n# team one\n[accounts.{kind}.team-1]   # hand\nhistory = \"standalone\"\n"
        );
        assert_eq!(std::fs::read_to_string(&config).unwrap(), expected);
        let rows: Value =
            serde_json::from_str(&succeeded(&accounts(&env, &["list", "--json"]))).unwrap();
        let row = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["account"] == format!("{kind}@team-1"))
            .unwrap();
        assert_eq!(row["history"], "standalone");
        for entry in history_dirs {
            assert!(
                std::fs::symlink_metadata(home.join(entry)).is_err(),
                "{kind} {entry} was not unlinked"
            );
        }
        succeeded(&accounts(&env, &args));
        assert_eq!(std::fs::read_to_string(&config).unwrap(), expected);
    }
}

#[test]
fn accounts_add_inline_label_sets_and_clears_the_field() {
    let env = Env::new();
    let config = env.rimz_home().join("config.toml");
    for (name, fields, label, expected_fields) in [
        (
            "team-1",
            "history = \"shared\"",
            "work",
            "history = \"shared\"\nlabel = \"work\"\n",
        ),
        ("team-2", "label = \"work\"", "", ""),
    ] {
        std::fs::write(
            &config,
            format!("[accounts.codex]\n# account\n{name} = {{ {fields} }} # hand\n"),
        )
        .unwrap();
        let output = accounts(&env, &["add", "codex", name, "--label", label]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            format!(
                "[accounts.codex]\n\n# account\n[accounts.codex.{name}] # hand\n{expected_fields}"
            )
        );
        let rows: Value =
            serde_json::from_str(&succeeded(&accounts(&env, &["list", "--json"]))).unwrap();
        assert!(
            rows.as_array()
                .unwrap()
                .iter()
                .any(|row| row["account"] == format!("codex@{name}")),
            "{rows}"
        );
    }
}

#[test]
fn accounts_add_rejected_config_edit_keeps_history_links() {
    let env = Env::new();
    succeeded(&accounts(&env, &["add", "claude", "work"]));
    let config = env.rimz_home().join("config.toml");
    let home = env.rimz_home().join("accounts/claude/work");
    let history = home.join("projects");
    let target = std::fs::read_link(&history).unwrap();
    let seed = "[accounts.claude.work]\n[agents.commands]\n'bad/name' = 'echo invalid'\n";
    std::fs::write(&config, seed).unwrap();
    succeeded(&accounts(&env, &["list", "--json"]));
    let output = accounts(&env, &["add", "claude", "work", "--history", "standalone"]);
    assert_eq!(output.status.code(), Some(1));
    let error = failed(&output);
    assert!(error.contains("invalid command name `bad/name`"), "{error}");
    assert_eq!(std::fs::read_to_string(&config).unwrap(), seed);
    assert!(
        history.is_symlink(),
        "a rejected edit must not unlink shared history: {error}"
    );
    assert_eq!(std::fs::read_link(&history).unwrap(), target);
}

#[test]
fn accounts_add_rejected_config_edit_never_creates_a_home() {
    for label in ["work", ""] {
        let env = Env::new();
        let config = env.rimz_home().join("config.toml");
        let home = env.rimz_home().join("accounts/codex/team-1");
        let seed = "[accounts.codex.team-1]\nlabel = 'work'\n[agents.commands]\n'bad/name' = 'echo invalid'\n";
        std::fs::write(&config, seed).unwrap();
        succeeded(&accounts(&env, &["list", "--json"]));
        assert!(!home.exists());
        let output = accounts(&env, &["add", "codex", "team-1", "--label", label]);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(std::fs::read_to_string(&config).unwrap(), seed);
        assert!(
            !home.exists(),
            "a rejected label edit must not create the home"
        );
    }
}

#[test]
fn accounts_add_label_sets_rewrites_preserves_and_clears_only_the_label() {
    let env = Env::new();
    let config = env.rimz_home().join("config.toml");
    let home = env.home_root.join("work-home");
    succeeded(&accounts(
        &env,
        &[
            "add",
            "claude",
            "team-1",
            "--home",
            home.to_str().unwrap(),
            "--history",
            "standalone",
            "--label",
            "work",
        ],
    ));
    let declared = std::fs::read_to_string(&config).unwrap();
    assert!(declared.contains("label = \"work\""), "{declared}");
    let commented = declared.replace("label = \"work\"", "label = \"work\" # display only");
    std::fs::write(&config, &commented).unwrap();
    succeeded(&accounts(
        &env,
        &["add", "claude", "team-1", "--label", "personal"],
    ));
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        commented.replace("label = \"work\"", "label = \"personal\"")
    );
    let labelled = std::fs::read_to_string(&config).unwrap();
    succeeded(&accounts(&env, &["add", "claude", "team-1"]));
    assert_eq!(std::fs::read_to_string(&config).unwrap(), labelled);
    succeeded(&accounts(&env, &["add", "claude", "team-1", "--label", ""]));
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        declared.replace("label = \"work\"\n", "")
    );
}

#[test]
fn accounts_add_invalid_label_never_writes_or_creates_a_home() {
    let env = Env::new();
    let config = env.rimz_home().join("config.toml");
    std::fs::write(&config, "# machine\n").unwrap();
    let home = env.home_root.join("work-home");
    for (label, rule) in [
        (" work", "leading or trailing whitespace"),
        ("work ", "leading or trailing whitespace"),
        ("wo\nrk", "control characters"),
        ("\n", "control characters"),
        (&"界".repeat(17), "at most 32 columns wide"),
        (&"x".repeat(33), "at most 32 columns wide"),
        ("\u{200b}", "empty"),
    ] {
        let error = failed(&accounts(
            &env,
            &[
                "add",
                "codex",
                "team-1",
                "--home",
                home.to_str().unwrap(),
                "--label",
                label,
            ],
        ));
        assert!(error.contains(rule), "{error}");
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "# machine\n");
        assert!(!home.exists());
    }
}

#[test]
fn accounts_invalid_label_warns_but_keeps_the_named_account() {
    let env = Env::new();
    std::fs::write(
        env.rimz_home().join("config.toml"),
        format!(
            "[accounts.codex.team-1]\nhome = '{}'\nlabel = '{}'\n",
            env.home_root.join("team-1").display(),
            "界".repeat(17)
        ),
    )
    .unwrap();
    for args in [
        &["list", "--json"][..],
        &["show", "codex@team-1", "--json"][..],
    ] {
        let output = accounts(&env, args);
        let warning = String::from_utf8_lossy(&output.stderr);
        for text in [
            "rimz: warning:",
            "accounts.codex.team-1.label",
            "at most 32 columns wide",
            "name instead",
            "rimz accounts add codex team-1 --label",
            "--label ''",
        ] {
            assert!(warning.contains(text), "{warning}");
        }
        let rows: Value = serde_json::from_str(&succeeded(&output)).unwrap();
        let row = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["account"] == "codex@team-1")
            .unwrap();
        assert_eq!(row.get("label"), Some(&Value::Null));
        assert_eq!(row["product_name"], "Codex · team-1");
    }
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
    let refused = accounts(
        &env,
        &[
            "add",
            "claude",
            "work",
            "--history",
            "shared",
            "--label",
            "work",
        ],
    );

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
fn accounts_remove_warns_only_for_rooms_pinning_the_account() {
    let env = Env::new();
    succeeded(&accounts(&env, &["add", "claude", "work"]));
    env.record(&env.project_root);
    succeeded(&room_accounts(&env, &["use", "claude", "work"]));

    let following_root = env.home_root.join("following");
    env.record(&following_root);
    let project_config = following_root.join(".rimz/config.toml");
    std::fs::create_dir_all(project_config.parent().unwrap()).unwrap();
    std::fs::write(project_config, "[accounts]\nclaude = \"work\"\n").unwrap();
    succeeded(
        &hermetic(&env, &mut env.rimz())
            .current_dir(&following_root)
            .args(["trust", "grant"])
            .output()
            .unwrap(),
    );
    let listed: Value = serde_json::from_str(&succeeded(
        &hermetic(&env, &mut env.rimz())
            .current_dir(&following_root)
            .args(["accounts", "list", "--json"])
            .output()
            .unwrap(),
    ))
    .unwrap();
    let work = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["kind"] == "claude" && row["account"] == "claude@work")
        .unwrap();
    assert_eq!(work["active"], true);
    assert_eq!(work["default_for"], json!(["this_project"]));

    let pinned = env.resolve_workspace(&env.project_root);
    let following = env.resolve_workspace(&following_root);
    let removed = succeeded(
        &hermetic(&env, &mut env.rimz())
            .env("RIMZ_ZELLIJ_BIN", crate::common::zellij_trace_shim())
            .env("RIMZ_TEST_ZELLIJ_LOG", env.home_root.join("remove.trace"))
            .env(
                "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
                format!(
                    "{} [Created 1m ago]\n{} [Created 1m ago]\n",
                    pinned.session_name, following.session_name
                ),
            )
            .args(["accounts", "remove", "claude", "work"])
            .output()
            .unwrap(),
    );
    assert!(
        removed.contains(&format!("warning: room {} pins it", pinned.session_name)),
        "{removed}"
    );
    assert!(!removed.contains(&following.session_name), "{removed}");
}

#[test]
fn machine_account_switch_and_removal_preserve_declared_accounts() {
    let env = Env::new();
    crate::common::write_path_shim(&provider_bin(&env), "grok", "echo 0.0.0");
    succeeded(
        &hermetic(&env, &mut env.rimz())
            .args(["hooks", "install", "grok"])
            .output()
            .unwrap(),
    );
    std::fs::write(
        env.home_root.join(".grok/auth.json"),
        r#"{"test":{"auth_mode":"api_key","key":"fixture"}}"#,
    )
    .unwrap();
    let missing = failed(&accounts(&env, &["use", "--global", "codex", "missing"]));
    assert!(
        missing.contains("rimz accounts add codex missing"),
        "{missing}"
    );
    succeeded(&accounts(&env, &["add", "claude", "work"]));
    let used = succeeded(&accounts(&env, &["use", "--global", "claude", "work"]));
    assert!(
        used.contains("new launches only; 0 running claude agent(s) stay on claude@default"),
        "{used}"
    );
    assert!(
        used.contains("new rooms and rooms following the machine default")
            && used.contains("no live room follows it for claude")
            && used.contains("`rimz accounts use --reset claude`"),
        "{used}"
    );
    let listed: Value =
        serde_json::from_str(&succeeded(&accounts(&env, &["list", "--json"]))).unwrap();
    let work = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["account"] == "claude@work")
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
        .find(|row| row["kind"] == "claude" && row["account"] == "claude@default")
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
    let human = succeeded(&accounts(&env, &["show", "grok"]));
    assert!(
        human
            .lines()
            .any(|line| line.contains("grok@work") && line.contains("unavailable")),
        "undeclared selections keep the installed kind's setup state: {human}"
    );
    succeeded(&accounts(&env, &["use", "--global", "grok", "default"]));
    let listed: Value =
        serde_json::from_str(&succeeded(&accounts(&env, &["list", "--json"]))).unwrap();
    let grok: Vec<_> = listed
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["kind"] == "grok")
        .collect();
    assert_eq!(grok.len(), 1, "{listed}");
    assert_eq!(grok[0]["account"], "grok@default");
    assert!(grok[0].get("problem").is_none(), "{listed}");
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
        assert_eq!(
            stderr
                .lines()
                .filter(|line| line.contains("warning") && line.contains("config.toml"))
                .count(),
            1,
            "{args:?}: {stderr}"
        );
        assert_eq!(
            stderr.contains("is deprecated"),
            args[0] == "providers",
            "{stderr}"
        );
        assert!(stderr.contains("config.toml"), "{args:?}: {stderr}");
        let rows = rows.as_array().unwrap();
        assert!(
            rows.iter().all(|row| row["active"] == false),
            "{args:?}: no account is marked: {rows:?}"
        );
        let claude_default = rows
            .iter()
            .find(|row| {
                row["kind"] == "claude"
                    && (row["account"] == "claude@default" || row["account"] == "default")
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
            "case \"$*\" in\n  \"auth status\") echo probe >> '{log}'; if [ -f \"${{CLAUDE_CONFIG_DIR:-}}/claude-auth\" ]; then . \"$CLAUDE_CONFIG_DIR/claude-auth\"; else . '{answer}'; fi;;\n  *) echo 0.0.0;;\nesac",
            log = log.display(),
            answer = answer.display()
        ),
    );
    (answer, log)
}

#[cfg(unix)]
#[test]
fn console_login_ignores_a_cached_lapse_but_setup_token_keeps_it() {
    let env = Env::new();
    let (answer, _) = write_fake_claude(&env);
    std::fs::write(answer, "echo '{\"loggedIn\":false}'\n").unwrap();
    for name in ["console", "setup"] {
        let home = env.home_root.join(name);
        succeeded(&accounts(
            &env,
            &["add", "claude", name, "--home", home.to_str().unwrap()],
        ));
        std::fs::write(home.join(".credentials.json"), "{}").unwrap();
        std::fs::write(
            home.join("claude-auth"),
            "echo '{\"loggedIn\":true,\"authMethod\":\"oauth_token\",\"apiProvider\":\"firstParty\"}'\n",
        )
        .unwrap();
        if name == "setup" {
            let settings = home.join("settings.json");
            let mut body: Value =
                serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
            body["env"] = json!({"CLAUDE_CODE_OAUTH_TOKEN":"synthetic"});
            std::fs::remove_file(&settings).unwrap();
            std::fs::write(settings, body.to_string()).unwrap();
        }
    }
    let store = env.home_root.join("anthropic");
    std::fs::create_dir_all(store.join("configs")).unwrap();
    std::fs::create_dir_all(store.join("credentials")).unwrap();
    std::fs::write(
        store.join("configs/default.json"),
        r#"{"authentication":{"type":"user_oauth"}}"#,
    )
    .unwrap();
    std::fs::write(store.join("credentials/default.json"), "synthetic").unwrap();
    let entry = json!({
        "observed_at_ms":1,"ok":true,"entitlement":{"lapsed":{"since_ms":1}},
        "extra_credits":{"known":{"used_usd":7.0,"remaining_usd":43.0,"limit_usd":50.0}}
    });
    let runtime = env.runtime_paths();
    runtime.ensure_dirs().unwrap();
    let credits_path = runtime.shared_credits_path();
    std::fs::write(
        &credits_path,
        json!({"refreshed_at_ms":1,"logins":{
            "claude@console":entry,"claude@setup":entry
        }})
        .to_string(),
    )
    .unwrap();
    for args in [
        &["list", "--json"][..],
        &["show", "claude@console", "--json"][..],
    ] {
        let rows: Value = serde_json::from_str(&succeeded(&accounts(&env, args))).unwrap();
        let console = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["account"] == "claude@console")
            .unwrap();
        assert_eq!(console["metered"], false, "{console}");
        assert_eq!(console["entitlement"], "ok", "{console}");
        assert_eq!(console["status"], "ready", "{console}");
        assert!(console.get("problem").is_none(), "{console}");
        assert!(console["windows"].as_array().unwrap().is_empty());
        assert!(console["extra_credits"]["known"]["remaining_usd"].is_null());
        if args[0] == "list" {
            let setup = rows
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["account"] == "claude@setup")
                .unwrap();
            assert_eq!(setup["metered"], true, "{setup}");
            assert_eq!(setup["entitlement"], entry["entitlement"], "{setup}");
            assert!(
                setup["problem"].as_str().unwrap().contains("plan lapsed"),
                "{setup}"
            );
        }
    }
    let text = succeeded(&accounts(&env, &[]));
    let row = text.lines().find(|line| line.contains("console")).unwrap();
    assert!(
        !row.contains("lapsed") && !row.contains("never read"),
        "{text}"
    );
    let show = succeeded(&accounts(&env, &["show", "claude@console"]));
    assert!(
        show.contains("no limit") && !show.contains("lapsed"),
        "{show}"
    );
    let credits: Value = serde_json::from_slice(&std::fs::read(credits_path).unwrap()).unwrap();
    assert_eq!(
        credits["logins"]["claude@console"]["entitlement"],
        entry["entitlement"]
    );
}

#[cfg(unix)]
#[test]
fn live_profile_corrects_only_its_owner_and_obeys_cache_refresh_failure_and_offline() {
    let env = Env::new();
    let (answer, _) = write_fake_claude(&env);
    std::fs::write(answer, "echo '{\"loggedIn\":false}'\n").unwrap();
    for name in ["work", "other"] {
        let home = env.home_root.join(name);
        succeeded(&accounts(
            &env,
            &["add", "claude", name, "--home", home.to_str().unwrap()],
        ));
        std::fs::write(
            home.join(".credentials.json"),
            json!({"claudeAiOauth": {
                "accessToken": format!("synthetic-{name}"),
                "expiresAt": 4102444800000_u64,
                "scopes": ["user:profile"],
                "subscriptionType": "max", "rateLimitTier": "default_claude_max_5x"
            }})
            .to_string(),
        )
        .unwrap();
        let auth = json!({
            "loggedIn":true,"authMethod":"claude.ai","subscriptionType":"max",
            "email":format!("{name}@example.test"),
            "orgId":"00000000-0000-0000-0000-000000000001"
        });
        std::fs::write(
            home.join("claude-auth"),
            format!("printf '%s\\n' '{auth}'\n"),
        )
        .unwrap();
    }
    let reset = (jiff::Timestamp::now() + jiff::SignedDuration::from_hours(1)).to_string();
    let usage = json!({
        "five_hour":{"utilization":37,"resets_at":reset},
        "extra_usage":{"is_enabled":true,"used_credits":250,"monthly_limit":5000}
    })
    .to_string();
    let profile = json!({
        "account":{"email":"work@example.test"},
        "organization":{
            "uuid":"00000000-0000-0000-0000-000000000001",
            "organization_type":"claude_pro","rate_limit_tier":"default_claude_ai"
        }
    })
    .to_string();
    let query = |args: &[&str], profile_status, offline, profiles: usize| {
        let (origin, server) = super::oauth_usage::serve_http_routes(
            vec![
                ("GET /api/oauth/profile", profile_status, profile.clone()),
                ("GET /api/oauth/usage", 200, usage.clone()),
            ],
            profiles * 2,
        );
        let output = hermetic(&env, &mut env.rimz())
            .arg("accounts")
            .args(args)
            .env("RIMZ_OAUTH_USAGE_OFFLINE", if offline { "1" } else { "0" })
            .env(
                "RIMZ_CLAUDE_OAUTH_USAGE_URL",
                format!("{origin}/api/oauth/usage"),
            )
            .output()
            .unwrap();
        let text = succeeded(&output);
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let requests = server.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("GET /api/oauth/profile"))
                .count(),
            profiles
        );
        assert_eq!(requests.len(), profiles * 2);
        text
    };
    let rows: Value = serde_json::from_str(&query(&["list", "--json"], 200, false, 2)).unwrap();
    for (name, plan, label) in [
        ("work", "pro", "Claude Pro"),
        ("other", "max_5x", "Claude Max 5x"),
    ] {
        let row = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["account"] == format!("claude@{name}"))
            .unwrap();
        assert_eq!(row["plan"], plan);
        assert_eq!(row["plan_label"], label);
        assert_eq!(row["metered"], true);
        assert!(!row["windows"].as_array().unwrap().is_empty());
        assert!(!row["extra_credits"].is_null());
    }
    let text = query(&["list"], 200, false, 0);
    assert!(
        text.contains("Claude Pro") && text.contains("Claude Max 5x"),
        "{text}"
    );
    let show = query(&["show", "claude@work"], 200, false, 0);
    assert!(
        show.contains("Claude Pro") && !show.contains("Claude Max"),
        "{show}"
    );
    let refreshed: Value =
        serde_json::from_str(&query(&["list", "--refresh", "--json"], 200, false, 2)).unwrap();
    let offline: Value =
        serde_json::from_str(&query(&["list", "--refresh", "--json"], 200, true, 0)).unwrap();
    let failed: Value =
        serde_json::from_str(&query(&["list", "--refresh", "--json"], 500, false, 2)).unwrap();
    for result in [&refreshed, &offline, &failed] {
        for original in rows.as_array().unwrap() {
            let row = result
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["account"] == original["account"])
                .unwrap();
            for field in [
                "plan",
                "plan_label",
                "windows",
                "extra_credits",
                "reset_credits",
                "metered",
                "status",
            ] {
                let normalized = |value: &Value| {
                    let mut value = value.clone();
                    if field == "windows" {
                        for window in value.as_array_mut().unwrap() {
                            if let Some(observed) = window.get_mut("observed_at") {
                                *observed = json!("<observation>");
                            }
                        }
                    }
                    value
                };
                assert_eq!(
                    normalized(&row[field]),
                    normalized(&original[field]),
                    "{field}"
                );
            }
        }
    }
    let cache = std::fs::read_to_string(env.runtime_paths().shared_accounts_path()).unwrap();
    assert!(
        !cache.contains("example.test")
            && !cache.contains("00000000-0000-0000-0000-000000000001")
            && !cache.contains("synthetic-")
    );
}

#[cfg(unix)]
#[test]
fn list_probes_default_under_the_native_home_from_a_named_account_pane() {
    let env = Env::new();
    let home = env.home_root.join("work home");
    succeeded(&accounts(
        &env,
        &["add", "claude", "work", "--home", home.to_str().unwrap()],
    ));
    let native_home = env.home_root.join(".claude");
    std::fs::create_dir_all(&native_home).unwrap();
    for (home, tier) in [
        (&home, "default_claude_max_20x"),
        (&native_home, "default_claude_max_5x"),
    ] {
        std::fs::write(
            home.join(".credentials.json"),
            json!({"claudeAiOauth": {
                "subscriptionType": "max",
                "rateLimitTier": tier,
                "expiresAt": 0,
                "scopes": []
            }})
            .to_string(),
        )
        .unwrap();
    }
    let log = env.home_root.join("claude-probe-homes");
    crate::common::write_path_shim(
        &provider_bin(&env),
        "claude",
        &format!(
            "case \"$*\" in\n  \"auth status\") printf '%s\\n' \"${{CLAUDE_CONFIG_DIR:-unset}}\" >> '{}'; echo '{{\"loggedIn\": true, \"authMethod\": \"claude.ai\", \"subscriptionType\": \"max\"}}';;\n  *) echo 0.0.0;;\nesac",
            log.display()
        ),
    );
    let text = succeeded(
        &hermetic(&env, &mut env.rimz())
            .env("CLAUDE_CONFIG_DIR", &home)
            .args(["accounts", "list"])
            .output()
            .unwrap(),
    );
    assert!(text.contains("Claude Max 20x"), "{text}");
    assert!(text.contains("Claude Max 5x"), "{text}");
    let rows: Value =
        serde_json::from_str(&succeeded(&accounts(&env, &["list", "--json"]))).unwrap();
    for (name, plan, label) in [
        ("claude@work", "max_20x", "Claude Max 20x"),
        ("claude@default", "max_5x", "Claude Max 5x"),
    ] {
        let row = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["account"] == name)
            .unwrap();
        assert_eq!(row["plan"], plan, "{row}");
        assert_eq!(row["plan_label"], label, "{row}");
        assert_eq!(row["status"], "ready", "{row}");
        assert_eq!(row["metered"], true, "{row}");
    }
    let probes = std::fs::read_to_string(log).unwrap();
    let mut homes: Vec<_> = probes.lines().collect();
    homes.sort_unstable();
    assert_eq!(homes, [home.to_str().unwrap(), "unset"]);
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
            .find(|row| row["kind"] == "claude" && row["account"] == "claude@work")
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

    let legacy = |refresh| -> Value {
        let mut command = env.rimz();
        hermetic(&env, &mut command).args(["providers", "claude", "--json"]);
        if refresh {
            command.arg("--refresh");
        }
        serde_json::from_str(&succeeded(&command.output().unwrap())).unwrap()
    };
    let cached = probes();
    let first = legacy(false);
    assert_eq!(probes(), cached, "the alias reuses cached logouts");
    assert_eq!(
        legacy(false),
        first,
        "the legacy projection keeps its timestamps"
    );
    assert_eq!(probes(), cached, "a second alias read also keeps the TTL");
    legacy(true);
    assert_eq!(
        probes(),
        cached + 2,
        "explicit refresh probes each login once"
    );

    std::fs::write(&answer, LOGGED_IN).unwrap();
    let row = work();
    assert_eq!(row["status"], "ready", "a login made since shows at once");
    assert_eq!(row.get("problem"), None, "{row}");
    assert_eq!(row["metered"], true, "{row}");
    let settled = probes();
    assert_eq!(work()["status"], "ready");
    assert_eq!(probes(), settled, "a logged-in record keeps the due rule");
}

#[cfg(unix)]
#[test]
fn list_reads_the_recorded_login_and_probes_an_ambiguous_record_again() {
    let env = Env::new();
    for name in ["out", "bare", "legacy"] {
        succeeded(&accounts(&env, &["add", "claude", name]));
    }
    // Only `out` is logged out; a fresh probe of the others finds a login.
    let log = env.home_root.join("claude-probes");
    crate::common::write_path_shim(
        &provider_bin(&env),
        "claude",
        &format!(
            "case \"$*\" in\n  \"auth status\") echo \"$CLAUDE_CONFIG_DIR\" >> '{log}'\n    case \"$CLAUDE_CONFIG_DIR\" in\n      */out) /bin/echo '{{\"loggedIn\": false}}'; exit 1;;\n      *) /bin/echo '{{\"loggedIn\": true, \"authMethod\": \"claude.ai\"}}';;\n    esac;;\n  *) echo 0.0.0;;\nesac",
            log = log.display()
        ),
    );
    // What a room's sidebar publishes while a Claude agent runs: every
    // account keeps the CLI version, logged out or not.
    let now_ms = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let record = |login: Option<&str>| {
        let mut record = json!({
            "probed_at_ms": now_ms - 1_000,
            "ok": true,
            "account": {"version": "2.1.0"},
        });
        if let Some(login) = login {
            record["login"] = json!(login);
        }
        record
    };
    let cache = env.rimz_home().join("cache/providers/accounts.json");
    std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
    std::fs::write(
        &cache,
        json!({"logins": {
            "claude@default": record(Some("logged_in")),
            "claude@out": record(Some("logged_out")),
            "claude@bare": record(Some("logged_in")),
            "claude@legacy": record(None),
        }})
        .to_string(),
    )
    .unwrap();

    succeeded(
        &hermetic(&env, &mut env.rimz())
            .args(["providers", "claude", "--json"])
            .output()
            .unwrap(),
    );
    let probed = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        probed.is_empty(),
        "the alias keeps cached logout and ambiguous records within the TTL: {probed}"
    );

    let rows: Value =
        serde_json::from_str(&succeeded(&accounts(&env, &["list", "--json"]))).unwrap();
    let status = |name: &str| {
        rows.as_array()
            .unwrap()
            .iter()
            .find(|row| row["kind"] == "claude" && row["account"] == format!("claude@{name}"))
            .unwrap_or_else(|| panic!("no claude {name} row: {rows}"))["status"]
            .clone()
    };
    assert_eq!(status("out"), "logged_out", "{rows}");
    assert_eq!(status("bare"), "ready", "{rows}");
    assert_eq!(status("legacy"), "ready", "{rows}");
    let probed = std::fs::read_to_string(&log).unwrap_or_default();
    let mut probed: Vec<&str> = probed
        .lines()
        .map(|home| home.rsplit('/').next().unwrap())
        .collect();
    probed.sort_unstable();
    assert_eq!(
        probed,
        ["legacy", "out"],
        "the recorded logins keep the due rule"
    );
    let stored: Value = serde_json::from_str(&std::fs::read_to_string(&cache).unwrap()).unwrap();
    assert_eq!(stored["logins"]["claude@legacy"]["login"], "logged_in");
    assert_eq!(stored["logins"]["claude@out"]["login"], "logged_out");
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
    let run = |env: &Env, home: &std::path::Path| -> (Value, String) {
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
            .args(["budget", "--account", "claude", "--json"])
            .output()
            .unwrap();
        (serde_json::from_str(&succeeded(&budget)).unwrap(), added)
    };

    let local = Env::new();
    local.record(&local.project_root);
    let (spend, added) = run(&local, &local.home_root.join("work"));
    assert_eq!(spend["accounts"][0]["spend_usd"], 0.25, "{spend}\n{added}");

    let env = Env::new();
    env.record(&env.project_root);
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
    assert_eq!(spend["accounts"][0]["spend_usd"], 0.0, "{spend}\n{added}");
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
