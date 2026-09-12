use super::*;

#[test]
fn host_login_envs_select_each_provider_account() {
    let accounts: AccountsConfig = toml::from_str("[claude.work]\nhome = \"/srv/work\"\n").unwrap();
    let logins = RoomLogins::from([(AgentKind::new_unchecked("claude"), "work".parse().unwrap())]);
    let envs = HostLoginEnvs::from_logins(&accounts, &logins).unwrap();
    let ambient = crate::agents::ambient_env();
    let mut claude = ambient.clone();
    claude.insert("CLAUDE_CONFIG_DIR".to_owned(), "/srv/work".to_owned());
    assert_eq!(envs.for_host(RemoteControlHost::Claude), &claude);
    assert_eq!(envs.for_host(RemoteControlHost::Codex), &ambient);
}

#[test]
fn host_login_envs_reject_unknown_accounts() {
    let logins = RoomLogins::from([(
        AgentKind::new_unchecked("claude"),
        "missing".parse().unwrap(),
    )]);
    assert!(HostLoginEnvs::from_logins(&AccountsConfig::default(), &logins).is_err());
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
fn start_gate_skips_uninstalled_hosts_and_keeps_hard_refusals() {
    let skipped = ReadinessSnapshot::from_states(
        RuntimeControlReadiness::Uninstalled(RuntimeControlIssue::new(
            "claude",
            "uninstalled",
            &"Claude is not installed",
        )),
        RuntimeControlReadiness::Uninstalled(RuntimeControlIssue::new(
            "codex",
            "standalone_missing",
            &"Codex standalone is missing",
        )),
    );
    assert_eq!(skipped.start_gate(), Ok(()));

    let issue = RuntimeControlIssue::new("claude", "blocked", &"Claude is too old");
    let blocked = ReadinessSnapshot::from_states(
        RuntimeControlReadiness::Blocked(issue.clone()),
        RuntimeControlReadiness::Uninstalled(RuntimeControlIssue::new(
            "codex",
            "standalone_missing",
            &"Codex standalone is missing",
        )),
    );
    assert_eq!(blocked.start_gate(), Err(issue));
}
