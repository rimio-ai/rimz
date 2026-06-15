use super::*;

#[test]
fn provider_without_the_rate_limit_capability_drops_stray_windows() {
    // OpenCode declares `rate_limit_windows: false`; Claude declares it true. The
    // same stray session reading must paint a budget bar only where the
    // descriptor declares the surface.
    let reading = window(40, 3_600);
    let opencode = agent("opencode", "o1", AgentStatus::Idle, 10).limits(vec![reading.clone()]);
    let claude = agent("claude", "c1", AgentStatus::Idle, 10).limits(vec![reading]);

    let snapshot = room(Vec::new(), vec![opencode, claude]).with_provider_aggregates(
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
    assert!(
        panel("opencode").windows.is_empty(),
        "opencode's declared absence drops the stray reading"
    );
    assert_eq!(panel("claude").windows.len(), 1);
}
