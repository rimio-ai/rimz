//! CLI coverage for room-fleet and provider-account daily dollar caps.

use assert_cmd::assert::OutputAssertExt;
use predicates::str::contains;

use crate::common::Env;
use rimz::ids::{AgentKind, LoginKey};

#[test]
fn budget_reports_configured_pools_when_the_catalog_is_invalid() {
    let env = Env::new();
    env.record(&env.project_root);
    std::fs::write(
        env.rimz_home().join("config.toml"),
        "[accounts.budget]\nclaude = '50/day'\n[accounts.codex.work]\nhome = 'relative-home'\n",
    )
    .unwrap();
    let json = env.rimz().args(["budget", "--json"]).assert().success();
    let report: serde_json::Value = serde_json::from_slice(&json.get_output().stdout).unwrap();
    assert_eq!(report["accounts"][0]["scope"], "claude@default");
    assert_eq!(report["accounts"][0]["cap_usd"], 50.0);
}

#[test]
fn budget_reports_plain_facts_and_runtime_pools() {
    use rimz::harness::budget::{BudgetParkStamp, DailyBudgetScope};
    let env = Env::new();
    env.record(&env.project_root);
    let text = env.rimz().arg("budget").assert().success();
    insta::assert_snapshot!(String::from_utf8_lossy(&text.get_output().stdout), @"no daily cap · set one: rimz budget 50/day");
    let json = env.rimz().args(["budget", "--json"]).assert().success();
    insta::assert_json_snapshot!(serde_json::from_slice::<serde_json::Value>(&json.get_output().stdout).unwrap(), @r#"
    {
      "accounts": [],
      "fleet": {
        "cap_usd": null,
        "parked": false,
        "scope": "fleet",
        "source": "none",
        "spend_usd": 0.0
      },
      "turn_cap_usd": null
    }
    "#);
    let text = env.rimz().args(["budget", "5/day"]).assert().success();
    insta::assert_snapshot!(String::from_utf8_lossy(&text.get_output().stdout), @"$0.00 of $5.00 today · not parked · source override · raise: rimz budget +10 · lift: rimz budget off");
    let parked = BudgetParkStamp {
        at_cost: 5.0,
        at: jiff::Timestamp::now(),
    };
    // Fleet choices and the producer's park are separate records.
    std::fs::write(
        env.runtime_paths().lane_path("budget.fleet-park.json"),
        serde_json::to_vec(&parked).unwrap(),
    )
    .unwrap();
    let text = env.rimz().arg("budget").assert().success();
    insta::assert_snapshot!(String::from_utf8_lossy(&text.get_output().stdout), @"$0.00 of $5.00 today · parked · source override · raise: rimz budget +10 · lift: rimz budget off");
    let json = env.rimz().args(["budget", "--json"]).assert().success();
    insta::assert_json_snapshot!(serde_json::from_slice::<serde_json::Value>(&json.get_output().stdout).unwrap(), @r#"
    {
      "accounts": [],
      "fleet": {
        "cap_usd": 5.0,
        "parked": true,
        "scope": "fleet",
        "source": "override",
        "spend_usd": 0.0
      },
      "turn_cap_usd": null
    }
    "#);
    env.rimz().args(["config", "init"]).assert().success();
    env.rimz()
        .args([
            "config",
            "set",
            "accounts.claude.work.home",
            "/srv/budget-work",
        ])
        .assert()
        .success();
    let text = env
        .rimz()
        .args(["budget", "--account", "claude@work", "5/day"])
        .assert()
        .success();
    insta::assert_snapshot!(String::from_utf8_lossy(&text.get_output().stdout), @"
    claude@work (shares the claude@default pool with default)
    $0.00 of $5.00 today · not parked · source override · raise: rimz budget --account claude@work +10 · lift: rimz budget --account claude@work off
    ");
    let scope = DailyBudgetScope::Account("claude@default".parse().unwrap());
    let mut ledger = scope.read_ledger(&env.runtime_paths(), Some(env.store().paths()));
    ledger.parked = Some(BudgetParkStamp {
        at_cost: 5.0,
        at: jiff::Timestamp::now(),
    });
    scope
        .write_ledger(&env.runtime_paths(), env.store().paths(), &ledger)
        .unwrap();
    let text = env
        .rimz()
        .args(["budget", "--account", "claude"])
        .assert()
        .success();
    insta::assert_snapshot!(String::from_utf8_lossy(&text.get_output().stdout), @"
    claude resolves through this room's account: claude@default (shares the claude@default pool with work)
    $0.00 of $5.00 today · parked · source override · raise: rimz budget --account claude@default +10 · lift: rimz budget --account claude@default off
    ");
    let json = env
        .rimz()
        .args(["budget", "--account", "claude@work", "--json"])
        .assert()
        .success();
    insta::assert_json_snapshot!(serde_json::from_slice::<serde_json::Value>(&json.get_output().stdout).unwrap(), @r#"
    {
      "accounts": [
        {
          "cap_usd": 5.0,
          "members": [
            "claude@default",
            "claude@work"
          ],
          "parked": true,
          "requested": "claude@work",
          "scope": "claude@default",
          "source": "override",
          "spend_usd": 0.0
        }
      ],
      "fleet": null,
      "turn_cap_usd": null
    }
    "#);
    let json = env.rimz().args(["budget", "--json"]).assert().success();
    let report: serde_json::Value = serde_json::from_slice(&json.get_output().stdout).unwrap();
    assert_eq!(report["accounts"][0]["scope"], "claude@default");
    assert_eq!(report["accounts"][0]["source"], "override");
    assert!(report["accounts"][0].get("requested").is_none());
    let text = env.rimz().arg("budget").assert().success();
    assert!(String::from_utf8_lossy(&text.get_output().stdout).contains("claude@default"));
}

#[test]
fn budget_set_raise_clear_and_config_routes() {
    let env = Env::new();
    env.record(&env.project_root);
    env.rimz().args(["config", "init"]).assert().success();
    env.rimz()
        .args(["config", "set", "harness.budget", "50/day"])
        .assert()
        .success();
    env.rimz()
        .args(["config", "set", "accounts.budget.claude", "100/day"])
        .assert()
        .success();
    env.rimz()
        .args([
            "config",
            "set",
            "accounts.claude.work.home",
            "/srv/budget-test-work",
        ])
        .assert()
        .success();
    env.rimz()
        .args([
            "config",
            "set",
            "accounts.claude.work.history",
            "standalone",
        ])
        .assert()
        .success();
    // A shared account spends against `default`'s cap and earns no row.
    env.rimz()
        .args([
            "config",
            "set",
            "accounts.claude.pooled.home",
            "/srv/budget-test-pooled",
        ])
        .assert()
        .success();
    env.rimz()
        .args(["budget", "--account", "claude@pooled"])
        .assert()
        .success()
        .stdout(contains("claude@default pool"));
    let inspected = env.rimz().arg("budget").assert().success();
    let output = String::from_utf8_lossy(&inspected.get_output().stdout);
    let table_rows = output
        .lines()
        .filter(|line| line.starts_with("claude@"))
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .collect::<Vec<_>>();
    assert_eq!(
        table_rows,
        vec![
            vec!["claude@default", "$100.00/day", "config", "$0.00", "no"],
            vec!["claude@work", "$100.00/day", "config", "$0.00", "no"],
        ]
    );

    env.rimz()
        .args(["budget", "20/day", "--no-continue"])
        .assert()
        .success()
        .stdout(contains("of $20.00 today"))
        .stdout(contains("source override"));
    env.rimz()
        .args(["budget", "+10", "--no-continue"])
        .assert()
        .success()
        .stdout(contains("of $30.00 today"))
        .stdout(contains("source raised"));
    env.rimz()
        .args(["budget", "--account", "claude", "80/day", "--no-continue"])
        .assert()
        .success()
        .stdout(contains("claude@default pool"))
        .stdout(contains("of $80.00 today"));
    env.rimz()
        .args(["budget", "--account", "claude", "clear", "--no-continue"])
        .assert()
        .success()
        .stdout(contains("no daily cap"));

    env.rimz()
        .args([
            "budget",
            "--account",
            "claude@work",
            "60/day",
            "--no-continue",
        ])
        .assert()
        .success()
        .stdout(contains("claude@work (pool claude@work)"))
        .stdout(contains("of $60.00 today"));
    env.rimz()
        .args(["budget", "--account", "claude"])
        .assert()
        .success()
        .stdout(contains("no daily cap"));
    env.rimz()
        .args(["budget", "--account", "claude@missing"])
        .assert()
        .failure()
        .stderr(contains("rimz accounts add"));

    env.rimz()
        .args(["budget", "5", "--no-continue"])
        .assert()
        .failure()
        .stderr(contains("must end in `/day`"))
        .stderr(contains("`off` to disable"));
    env.rimz()
        .args(["budget", "off", "--no-continue"])
        .assert()
        .success()
        .stdout(contains("no daily cap"));
}

#[test]
fn budget_ignores_ineligible_siblings_and_warns_only_for_fleet_reports() {
    let env = Env::new();
    env.record(&env.project_root);
    std::fs::create_dir_all(env.rimz_home()).expect("config dir");
    std::fs::write(
        env.rimz_home().join("config.toml"),
        "[harness]\nbudget = '50/day'\n[accounts.budget]\nclaude = '100/day'\nantigravity = '50/day'\nunknown = '10/day'\n",
    )
    .expect("config");

    for args in [
        vec!["budget", "--account", "claude"],
        vec!["budget", "--account", "claude", "80/day", "--no-continue"],
    ] {
        let inspected = env.rimz().args(args).assert().success().stderr("");
        let output = String::from_utf8_lossy(&inspected.get_output().stdout);
        assert!(output.contains("claude@default (pool claude@default)"));
        assert!(!output.contains("antigravity"));
        assert!(!output.contains("unknown"));
    }

    for args in [vec!["budget"], vec!["budget", "20/day", "--no-continue"]] {
        let inspected = env.rimz().args(&args).assert().success();
        let output = String::from_utf8_lossy(&inspected.get_output().stdout);
        let rows = output
            .lines()
            .skip_while(|line| !line.starts_with("ACCOUNT"))
            .skip(1)
            .map(|line| line.split_whitespace().collect::<Vec<_>>())
            .collect::<Vec<_>>();
        assert_eq!(
            rows,
            vec![vec![
                "claude@default",
                "$80.00/day",
                "override",
                "$0.00",
                "no"
            ]]
        );
        if args.len() > 1 {
            assert!(output.contains("of $20.00 today"));
            assert!(output.contains("source override"));
        }
        assert_eq!(
            String::from_utf8_lossy(&inspected.get_output().stderr),
            "rimz: warning: unsupported `accounts.budget.antigravity`; remove it because antigravity has no durable account-spend source with authoritative account-level dollars\nrimz: warning: unknown agent kind in `accounts.budget.unknown`; remove it because no adapter can publish authoritative account-level dollars\n"
        );
    }
}

#[test]
fn budget_arms_unconfigured_daily_caps_parks_and_lifts() {
    use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
    use rimz::harness::budget::DailyBudgetScope;
    use rimz::ids::AgentSessionId;
    use rimz::store::event::EventEnvelope;

    for (scope, args) in [
        (DailyBudgetScope::Fleet, vec!["budget"]),
        (
            DailyBudgetScope::Account(LoginKey::default_for(AgentKind::new_unchecked("claude"))),
            vec!["budget", "--account", "claude"],
        ),
    ] {
        let env = Env::new();
        env.record(&env.project_root);
        let workspace = env.resolve_workspace(&env.project_root);
        let observation = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("budget-session")),
            LifecycleSignal::Registered,
        );
        env.store()
            .append_event(&EventEnvelope::agent_lifecycle(
                workspace.workspace_id.clone(),
                &workspace.session_name,
                "claude",
                "SessionStart",
                &observation,
            ))
            .unwrap();
        let panes = env.write_pane_fixture(&[]);
        let refresh = || {
            env.rimz()
                .args([
                    "sidebar",
                    "snapshot",
                    "--json",
                    "--workspace-id",
                    env.workspace_id.as_str(),
                    "--session-name",
                    &workspace.session_name,
                ])
                .env("RIMZ_TEST_PANE_LIST", &panes)
                .assert()
                .success();
        };
        env.rimz()
            .args(&args)
            .args(["+10", "--no-continue"])
            .assert()
            .failure()
            .stderr(contains("set an absolute `/day` cap first"));
        // At zero dollars, a zero cap meets the boundary exactly, without a clock-sensitive transcript fixture.
        env.rimz()
            .args(&args)
            .args(["0/day", "--no-continue"])
            .assert()
            .success()
            .stdout(contains("source override"));
        refresh();
        let runtime = env.runtime_paths();
        let store = env.store();
        let ledger = scope.read_ledger(&runtime, Some(store.paths()));
        assert_eq!(ledger.override_spec.unwrap().cap_usd, 0.0);
        assert!(
            ledger.parked.is_some(),
            "{scope:?} must park when spend equals the cap"
        );
        env.rimz()
            .args(&args)
            .args(["off", "--no-continue"])
            .assert()
            .success()
            .stdout(contains("no daily cap"));
        refresh();
        let ledger = scope.read_ledger(&runtime, Some(store.paths()));
        assert!(ledger.disabled);
        assert!(ledger.parked.is_none(), "{scope:?} must stay lifted");
    }
}

#[test]
fn unsupported_account_caps_leave_config_and_ledger_untouched() {
    let env = Env::new();
    env.rimz().args(["config", "init"]).assert().success();
    let config_path = env.rimz_home().join("config.toml");
    let before = std::fs::read(&config_path).expect("read generated config");

    env.rimz()
        .args(["config", "set", "accounts.budget.cursor", "100/day"])
        .assert()
        .failure()
        .stderr(contains("no durable account-spend source"));
    assert_eq!(
        std::fs::read(&config_path).expect("read rejected config"),
        before
    );

    let cursor = AgentKind::new_unchecked("cursor");
    let ledger = rimz::harness::budget::DailyBudgetScope::Account(LoginKey::default_for(cursor))
        .ledger_path(&env.runtime_paths(), env.store().paths());
    env.rimz()
        .args(["budget", "--account", "cursor", "100/day", "--no-continue"])
        .assert()
        .failure()
        .stderr(contains("no durable account-spend source"));
    assert!(
        !ledger.exists(),
        "unsupported account must not create a ledger"
    );
}

#[test]
fn config_set_rejects_unsupported_account_budget_without_writing() {
    let env = Env::new();
    env.rimz().args(["config", "init"]).assert().success();
    let path = env.rimz_home().join("config.toml");
    let before = std::fs::read_to_string(&path).expect("config");

    env.rimz()
        .args(["config", "set", "accounts.budget.antigravity", "50/day"])
        .assert()
        .failure()
        .stderr(contains("accounts.budget.antigravity"))
        .stderr(contains("authoritative account-level dollars"));

    assert_eq!(std::fs::read_to_string(path).expect("config"), before);
}

/// A session resumed under a second shared account answers to that account:
/// the pool's budget stop reaches its pane, and a parked message checks hooks
/// in the second account's home.
#[cfg(unix)]
#[test]
fn a_cross_account_resume_is_stopped_and_hook_checked_under_its_new_account() {
    use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
    use rimz::harness::launch::{ExecAction, ExecRequest};
    use rimz::ids::{AgentSessionId, MuxName, PaneId};
    use rimz::store::event::{EventEnvelope, EventKind};

    use crate::common::{exec_args, path_with_front, write_long_lived_agent_shim};

    const SESSION: &str = "sess-pool";
    const PANE: &str = "terminal_3";
    let env = Env::new();
    env.record(&env.project_root);
    let workspace = env.resolve_workspace(&env.project_root);
    let rimz = || {
        let mut command = env.rimz();
        command.env_remove("CLAUDE_CONFIG_DIR");
        command
    };
    env.install_agent_hooks("claude");
    let native = env.home_root.join(".claude");
    std::fs::create_dir_all(native.join("projects")).unwrap();
    let one = env.home_root.join("one");
    let two = env.home_root.join("two");
    for (name, home) in [("one", &one), ("two", &two)] {
        rimz()
            .args(["accounts", "add", "claude", name, "--home"])
            .arg(home)
            .assert()
            .success();
    }
    rimz()
        .args(["config", "set", "accounts.budget.claude", "1/day"])
        .assert()
        .success();

    let lifecycle = |name: &str, signal: LifecycleSignal| {
        let mut observation =
            AgentLifecycleObservation::new(Some(AgentSessionId::from(SESSION)), signal);
        observation.pane_id = Some(PaneId::from_parts(MuxName::Zellij, PANE));
        env.store()
            .append_event(&EventEnvelope::agent_lifecycle(
                workspace.workspace_id.clone(),
                &workspace.session_name,
                "claude",
                name,
                &observation,
            ))
            .unwrap();
    };
    lifecycle("SessionStart", LifecycleSignal::Registered);
    lifecycle("SessionEnd", LifecycleSignal::Ended);
    // The resumed provider stays alive, so the row keeps a live runtime owner.
    let shim_dir = write_long_lived_agent_shim(&env, "claude");
    let resume_under = |login_name: &str| -> std::process::Child {
        let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("claude"), Vec::new());
        request.action = ExecAction::Resume {
            session_id: SESSION.to_owned(),
            extra_args: Vec::new(),
        };
        request.identity.params.login = Some(login_name.parse().unwrap());
        let resumed_stamps = || {
            env.store()
                .read_events()
                .unwrap()
                .iter()
                .filter(|event| {
                    matches!(event.kind(), EventKind::AgentLifecycle(payload)
                        if payload.event_name.as_deref() == Some("rimz.agent-resumed")
                            && payload.observation.agent_id.as_ref()
                                .is_some_and(|session| session.as_str() == SESSION))
                })
                .count()
        };
        let stamps_before = resumed_stamps();
        let child = rimz()
            .args(exec_args(&env, &request))
            .arg("--root")
            .arg(&env.project_root)
            .env("SHELL", "/definitely/not/a/shell")
            .env("PATH", path_with_front(&shim_dir))
            .env("ZELLIJ", "0")
            .env("ZELLIJ_PANE_ID", "3")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        // The row is ready for a turn only once the wrapper's resume stamp
        // has revived it: the stamp rests the row at Idle and is the last
        // status write before the provider runs, so a turn opened ahead of
        // it is undone. The attach carries the login the stamp does not.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !(env.store().read_events().unwrap().iter().any(|event| {
            matches!(event.kind(), EventKind::AgentAttach(attach)
                if attach.runtime_owner.pid == child.id()
                    && attach.login.as_ref().is_some_and(|login| login.as_str() == login_name))
        }) && resumed_stamps() > stamps_before)
        {
            assert!(
                std::time::Instant::now() < deadline,
                "attach or resume stamp missing under {login_name}"
            );
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        lifecycle(
            "UserPromptSubmit",
            LifecycleSignal::TurnStarted { turn_id: None },
        );
        child
    };
    let message = || {
        rimz()
            .args(["message", "@claude", "--", "new direction"])
            .output()
            .unwrap()
    };

    // Under the first account, whose home has lost its hooks, a parked
    // message is refused: the control for the check below.
    let mut first = resume_under("one");
    std::fs::remove_file(one.join("settings.json")).unwrap();
    let refused = message();
    assert!(!refused.status.success(), "{refused:?}");
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("requires claude hooks"),
        "{refused:?}"
    );

    first.kill().unwrap();
    first.wait().unwrap();
    let mut second = resume_under("two");
    let sent = message();
    assert!(
        sent.status.success(),
        "{}",
        String::from_utf8_lossy(&sent.stderr)
    );
    assert!(
        String::from_utf8_lossy(&sent.stdout).contains("queued for @claude"),
        "{sent:?}"
    );

    // Spend over the cap, written through the second account's history link.
    assert_eq!(
        std::fs::read_link(two.join("projects")).unwrap(),
        native.join("projects")
    );
    let now = rimz::agents::spending::unix_secs_now();
    let tod = now % 86_400;
    std::fs::create_dir_all(two.join("projects/repo")).unwrap();
    std::fs::write(
        two.join("projects/repo/turn.jsonl"),
        format!(
            r#"{{"timestamp":"{}T{:02}:{:02}:{:02}.000Z","costUSD":2.0,"requestId":"req-1","message":{{"id":"msg-1","usage":{{"input_tokens":1200,"output_tokens":80}}}}}}"#,
            rimz::agents::spending::utc_date(now),
            tod / 3_600,
            (tod % 3_600) / 60,
            tod % 60
        ) + "\n",
    )
    .unwrap();

    let trace = env.project_root.join("budget-trace.log");
    let panes = env.write_pane_fixture(&[rimz::pane::PaneRef {
        pane_id: PaneId::from_parts(MuxName::Zellij, PANE),
        session_name: workspace.session_name.clone(),
        view_id: Some("tab_1".to_owned()),
        view_kind: Some(rimz::ids::ViewKind::Tab),
        view_name: Some("project".to_owned()),
        title: None,
        is_floating: false,
        command: Some("claude".to_owned()),
        foreground_cmdline: None,
        spawn_command: None,
        cwd: Some(env.project_root.display().to_string()),
        pane_pid: None,
        pane_process_start: None,
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
        hosted_agent_lineage: Vec::new(),
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    }]);
    let snapshot = rimz()
        .args([
            "sidebar",
            "snapshot",
            "--json",
            "--workspace-id",
            env.workspace_id.as_str(),
            "--session-name",
            &workspace.session_name,
        ])
        .env("RIMZ_TEST_PANE_LIST", &panes)
        .env("RIMZ_ZELLIJ_BIN", crate::common::zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace)
        .output()
        .unwrap();
    assert!(
        snapshot.status.success(),
        "{}",
        String::from_utf8_lossy(&snapshot.stderr)
    );
    let escape = format!("\taction\twrite\t--pane-id\t{PANE}\t27");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !std::fs::read_to_string(&trace)
        .is_ok_and(|log| log.lines().any(|line| line.ends_with(&escape)))
    {
        assert!(
            std::time::Instant::now() < deadline,
            "no interrupt reached the pane: {:?}",
            std::fs::read_to_string(&trace)
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let snapshot: serde_json::Value = serde_json::from_slice(&snapshot.stdout).unwrap();
    let card = snapshot["agents"]
        .as_array()
        .and_then(|agents| agents.iter().find(|agent| agent["agent_id"] == SESSION))
        .unwrap_or_else(|| panic!("no card for {SESSION}: {snapshot}"));
    assert_eq!(card["login"], "two", "{card}");
    assert_eq!(card["budget_park"]["scope"], "account", "{card}");
    assert_eq!(card["budget_park"]["account_kind"], "claude", "{card}");
    rimz()
        .args(["budget", "--account", "claude@two"])
        .assert()
        .success()
        .stdout(contains("claude@default pool"))
        .stdout(contains("$2.00 of"))
        .stdout(contains("· parked ·"));
    second.kill().unwrap();
    second.wait().unwrap();
}
