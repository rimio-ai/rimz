use super::*;

#[test]
fn every_provider_paints_its_own_session_window() {
    // Each registered provider meters the rate-limit reading its own session
    // reports — OpenCode included, now that its OAuth usage probe gives it a
    // window source. A reading stays confined to the kind that reported it
    // (sibling isolation is pinned in `pi.rs`).
    let reading = window(40, 3_600);
    let opencode = agent("opencode", "o1", AgentStatus::Idle, 10).limits(vec![reading.clone()]);
    let claude = agent("claude", "c1", AgentStatus::Idle, 10);

    let snapshot = room(vec![opencode, claude]).with_provider_aggregates(
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
    );

    let panel = |kind: &str| {
        snapshot
            .providers
            .iter()
            .find(|panel| panel.kind == kind)
            .unwrap_or_else(|| panic!("{kind} panel present"))
    };
    assert_eq!(
        panel("opencode").windows,
        vec![reading],
        "opencode paints its own session reading"
    );
    assert!(
        panel("claude").windows.is_empty(),
        "a provider that reported nothing grows no stray bar"
    );
}
