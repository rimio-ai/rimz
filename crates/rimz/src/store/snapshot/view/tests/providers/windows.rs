use super::*;
use crate::store::snapshot::SidebarProviderPanel;

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

fn claude_panel(agents: Vec<AgentState>) -> SidebarProviderPanel {
    room(agents)
        .with_provider_aggregates(&BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new())
        .providers
        .into_iter()
        .find(|panel| panel.kind == "claude")
        .expect("claude panel")
}

#[test]
fn newest_keyed_session_partitions_live_windows() {
    let mut old = agent("claude", "old", AgentStatus::Idle, 10).limits(vec![window(88, 3_600)]);
    old.account_key = Some("old-key".to_owned());
    old.registered_at = Some(ago(60));
    let mut new = agent("claude", "new", AgentStatus::Idle, 20).limits(vec![window(4, 3_600)]);
    new.account_key = Some("new-key".to_owned());
    new.registered_at = Some(ago(30));

    let panel = claude_panel(vec![old, new]);
    assert_eq!(panel.account_key.as_deref(), Some("new-key"));
    assert_eq!(panel.windows[0].used_percentage, Some(4));
}

#[test]
fn keyed_panel_stays_metered_when_foreign_windows_are_excluded() {
    let mut old = agent("claude", "old", AgentStatus::Idle, 10).limits(vec![window(88, 3_600)]);
    old.account_key = Some("old-key".to_owned());
    old.registered_at = Some(ago(60));
    let mut new = agent("claude", "new", AgentStatus::Idle, 20);
    new.account_key = Some("new-key".to_owned());
    new.registered_at = Some(ago(30));

    let panel = claude_panel(vec![old, new]);
    assert_eq!(panel.account_key.as_deref(), Some("new-key"));
    assert!(panel.windows.is_empty());
    assert!(panel.metered);
}

#[test]
fn same_key_parallel_sessions_keep_the_most_drained_window() {
    let mut first = agent("claude", "first", AgentStatus::Idle, 10).limits(vec![window(20, 3_600)]);
    first.account_key = Some("same-key".to_owned());
    let mut second =
        agent("claude", "second", AgentStatus::Idle, 20).limits(vec![window(70, 3_600)]);
    second.account_key = Some("same-key".to_owned());

    let panel = claude_panel(vec![first, second]);
    assert_eq!(panel.account_key.as_deref(), Some("same-key"));
    assert_eq!(panel.windows[0].used_percentage, Some(70));
}

#[test]
fn keyless_session_remains_compatible_beside_a_keyed_session() {
    let keyless = agent("claude", "legacy", AgentStatus::Idle, 10).limits(vec![window(65, 3_600)]);
    let mut keyed = agent("claude", "keyed", AgentStatus::Idle, 20).limits(vec![window(15, 3_600)]);
    keyed.account_key = Some("account-key".to_owned());

    let panel = claude_panel(vec![keyless, keyed]);
    assert_eq!(panel.account_key.as_deref(), Some("account-key"));
    assert_eq!(panel.windows[0].used_percentage, Some(65));
}

#[test]
fn all_keyless_sessions_keep_a_keyless_panel() {
    let first = agent("claude", "first", AgentStatus::Idle, 10).limits(vec![window(20, 3_600)]);
    let second = agent("claude", "second", AgentStatus::Idle, 20).limits(vec![window(70, 3_600)]);

    let panel = claude_panel(vec![first, second]);
    assert_eq!(panel.account_key, None);
    assert_eq!(panel.windows[0].used_percentage, Some(70));
}
