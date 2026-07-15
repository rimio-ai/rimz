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
        claude::host_argv()
    );
}

#[test]
fn start_gate_skips_uninstalled_hosts_and_keeps_hard_refusals() {
    let skipped = ReadinessSnapshot::from_states(
        HostState::Uninstalled(PreflightError::Claude(claude::Issue::Uninstalled)),
        HostState::Uninstalled(PreflightError::Codex(codex::Issue::StandaloneMissing)),
    );
    assert_eq!(skipped.start_gate(), Ok(()));

    let issue = claude::Issue::TooOld {
        found: crate::agents::version::CliVersion::new(2, 1, 50),
    };
    let blocked = ReadinessSnapshot::from_states(
        HostState::Blocked(PreflightError::Claude(issue.clone())),
        HostState::Uninstalled(PreflightError::Codex(codex::Issue::StandaloneMissing)),
    );
    assert_eq!(blocked.start_gate(), Err(PreflightError::Claude(issue)));
}

#[test]
fn only_uninstalled_states_are_skippable() {
    assert!(PreflightError::Claude(claude::Issue::Uninstalled).is_uninstalled_host());
    assert!(PreflightError::Codex(codex::Issue::StandaloneMissing).is_uninstalled_host());
    assert!(
        !PreflightError::Claude(claude::Issue::RemoteControlDisabled {
            settings_path: "/home/u/.claude/settings.json".into(),
        })
        .is_uninstalled_host()
    );
}
