use super::*;

#[test]
fn snapshot_returns_each_host_state_and_ready_claude_argv() {
    let snapshot = ReadinessSnapshot::from_states(HostState::Ready, HostState::Disabled);

    assert_eq!(
        snapshot.for_host(RemoteControlHost::Claude),
        &HostState::Ready
    );
    assert_eq!(
        snapshot.for_host(RemoteControlHost::Codex),
        &HostState::Disabled
    );
    assert_eq!(
        snapshot.claude_host_argv().expect("ready argv"),
        runtime_control::host_argv("claude").expect("Claude host argv")
    );
}

#[test]
fn start_gate_skips_uninstalled_hosts_and_keeps_hard_refusals() {
    let skipped = ReadinessSnapshot::from_states(
        HostState::Uninstalled(PreflightError::from_parts(
            "claude",
            "uninstalled",
            "Claude is not installed",
        )),
        HostState::Uninstalled(PreflightError::from_parts(
            "codex",
            "standalone_missing",
            "Codex standalone is missing",
        )),
    );
    assert_eq!(skipped.start_gate(), Ok(()));

    let issue = PreflightError::from_parts("claude", "blocked", "Claude is too old");
    let blocked = ReadinessSnapshot::from_states(
        HostState::Blocked(issue.clone()),
        HostState::Uninstalled(PreflightError::from_parts(
            "codex",
            "standalone_missing",
            "Codex standalone is missing",
        )),
    );
    assert_eq!(blocked.start_gate(), Err(issue));
}

#[test]
fn only_uninstalled_states_are_skippable() {
    assert!(PreflightError::from_parts("claude", "uninstalled", "missing").is_uninstalled_host());
    assert!(
        PreflightError::from_parts("codex", "standalone_missing", "missing").is_uninstalled_host()
    );
    assert!(
        !PreflightError::from_parts("claude", "blocked", "remote control disabled")
            .is_uninstalled_host()
    );
}
