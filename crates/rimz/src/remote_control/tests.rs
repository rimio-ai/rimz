use super::*;

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
        RuntimeControlReadiness::Uninstalled(RuntimeControlIssue::from_parts(
            "claude",
            "uninstalled",
            "Claude is not installed",
        )),
        RuntimeControlReadiness::Uninstalled(RuntimeControlIssue::from_parts(
            "codex",
            "standalone_missing",
            "Codex standalone is missing",
        )),
    );
    assert_eq!(skipped.start_gate(), Ok(()));

    let issue = RuntimeControlIssue::from_parts("claude", "blocked", "Claude is too old");
    let blocked = ReadinessSnapshot::from_states(
        RuntimeControlReadiness::Blocked(issue.clone()),
        RuntimeControlReadiness::Uninstalled(RuntimeControlIssue::from_parts(
            "codex",
            "standalone_missing",
            "Codex standalone is missing",
        )),
    );
    assert_eq!(blocked.start_gate(), Err(issue));
}
