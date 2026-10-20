use super::*;
use crate::ids::RoomLogins;

fn named_pane_child(test: &str) -> bool {
    if std::env::var_os("RIMZ_TEST_NATIVE_LOGIN").is_some() {
        return false;
    }
    let root = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("remote_control::tests::{test}"),
            "--nocapture",
        ])
        .env_clear()
        .env("HOME", root.path())
        .env("CLAUDE_CONFIG_DIR", "/srv/claude-work")
        .env("CODEX_HOME", "/srv/codex-work")
        .env("RIMZ_TEST_NATIVE_LOGIN", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

#[test]
fn default_host_envs_ignore_exported_declared_homes() {
    if named_pane_child("default_host_envs_ignore_exported_declared_homes") {
        return;
    }
    let accounts = toml::from_str(
        "[claude.work]\nhome = '/srv/claude-work'\n[codex.work]\nhome = '/srv/codex-work'\n",
    )
    .unwrap();
    let envs = HostLoginEnvs::from_logins(&accounts, &crate::agents::RoomAccounts::default());
    for (host, key) in [
        (RemoteControlHost::Claude, "CLAUDE_CONFIG_DIR"),
        (RemoteControlHost::Codex, "CODEX_HOME"),
    ] {
        assert_eq!(envs.for_host(host).unwrap().get(key), None);
    }
}

#[test]
fn default_codex_daemon_seed_ignores_an_exported_declared_home() {
    if named_pane_child("default_codex_daemon_seed_ignores_an_exported_declared_home") {
        return;
    }
    let machine = crate::config::MachineConfig {
        accounts: toml::from_str("[codex.work]\nhome = '/srv/codex-work'\n").unwrap(),
        ..Default::default()
    };
    for enabled in [true, false] {
        let envs = codex_daemon_envs(&machine, &[], enabled);
        assert_eq!(envs[0].get("CODEX_HOME"), None);
        assert_eq!(envs.len(), if enabled { 1 } else { 2 });
    }
}

#[test]
fn host_login_envs_select_each_provider_account() {
    let accounts: AccountsConfig = toml::from_str("[claude.work]\nhome = \"/srv/work\"\n").unwrap();
    let logins = RoomLogins::from([(AgentKind::new_unchecked("claude"), "work".parse().unwrap())]);
    let envs = HostLoginEnvs::from_logins(&accounts, &logins.into());
    let ambient = crate::agents::ambient_env();
    let mut claude = ambient.clone();
    claude.insert("CLAUDE_CONFIG_DIR".to_owned(), "/srv/work".to_owned());
    assert_eq!(envs.for_host(RemoteControlHost::Claude).unwrap(), &claude);
    assert_eq!(envs.for_host(RemoteControlHost::Codex).unwrap(), &ambient);
}

#[test]
fn codex_toggle_off_reaches_every_declared_codex_account() {
    let machine = crate::config::MachineConfig {
        accounts: toml::from_str("[codex.work]\nhome = \"/srv/codex-work\"\n").unwrap(),
        ..Default::default()
    };
    let ambient = crate::agents::ambient_env();
    // A shared account's daemon also takes the default home's databases.
    let work = crate::agents::LoginCatalog::from_config(&machine.accounts)
        .unwrap()
        .select(&AgentKind::new_unchecked("codex"), &"work".parse().unwrap())
        .unwrap()
        .env(&ambient);
    assert_eq!(work["CODEX_HOME"], "/srv/codex-work");
    assert_eq!(
        codex_daemon_envs(&machine, &[], true),
        vec![ambient.clone()]
    );
    assert_eq!(codex_daemon_envs(&machine, &[], false), vec![ambient, work]);
}

#[test]
fn host_login_envs_reject_unknown_accounts() {
    let logins = RoomLogins::from([(
        AgentKind::new_unchecked("claude"),
        "missing".parse().unwrap(),
    )]);
    let envs = HostLoginEnvs::from_logins(&AccountsConfig::default(), &logins.into());
    assert!(envs.for_host(RemoteControlHost::Claude).is_err());
    assert!(envs.for_host(RemoteControlHost::Codex).is_ok());
}

#[test]
fn snapshot_returns_each_host_state_and_ready_claude_argv() {
    let host_argv = vec![
        "claude".to_owned(),
        "remote-control".to_owned(),
        "--spawn".to_owned(),
        "worktree".to_owned(),
    ];
    let snapshot = ReadinessSnapshot::from_states(
        RuntimeControlReadiness::Ready {
            host_argv: Some(host_argv.clone()),
        },
        RuntimeControlReadiness::Disabled,
    );

    assert_eq!(
        snapshot.for_host(RemoteControlHost::Claude),
        &RuntimeControlReadiness::Ready {
            host_argv: Some(host_argv.clone()),
        }
    );
    assert_eq!(
        snapshot.for_host(RemoteControlHost::Codex),
        &RuntimeControlReadiness::Disabled
    );
    assert_eq!(snapshot.claude_host_argv().expect("ready argv"), host_argv);
}

#[test]
fn readiness_names_blocked_installed_hosts_and_their_fixes() {
    let skipped = ReadinessSnapshot::from_states(
        RuntimeControlReadiness::Uninstalled(RuntimeControlIssue::new(
            "claude",
            "uninstalled",
            crate::blocker::Blocker::new("Claude is not installed"),
        )),
        RuntimeControlReadiness::Uninstalled(RuntimeControlIssue::new(
            "codex",
            "standalone_missing",
            crate::blocker::Blocker::new("Codex standalone is missing"),
        )),
    );
    assert_eq!(skipped.start_gate(), Ok(()));

    let issue = RuntimeControlIssue::new(
        "claude",
        "blocked",
        crate::blocker::Blocker::new("Claude is too old"),
    );
    let blocked = ReadinessSnapshot::from_states(
        RuntimeControlReadiness::Blocked(issue.clone()),
        RuntimeControlReadiness::Uninstalled(RuntimeControlIssue::new(
            "codex",
            "standalone_missing",
            crate::blocker::Blocker::new("Codex standalone is missing"),
        )),
    );
    let block = blocked.start_gate().unwrap_err();
    assert_eq!(block.host, RemoteControlHost::Claude);
    assert_eq!(block.blocker.problem, issue.to_string());
    assert_eq!(
        block
            .blocker
            .fixes
            .last()
            .and_then(|fix| fix.command.as_deref()),
        Some("rimz config set remote_control.claude false")
    );

    let both = ReadinessSnapshot::from_states(
        RuntimeControlReadiness::Blocked(issue),
        RuntimeControlReadiness::Blocked(RuntimeControlIssue::new(
            "codex",
            "account_unavailable",
            crate::blocker::Blocker::new("Account unavailable"),
        )),
    );
    assert_eq!(
        both.start_gate().unwrap_err().host,
        RemoteControlHost::Codex
    );
    assert!(
        !both
            .blocked_host()
            .unwrap_err()
            .1
            .to_string()
            .contains("rimz config set")
    );
}
