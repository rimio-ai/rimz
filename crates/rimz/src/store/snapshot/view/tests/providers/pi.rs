use super::*;

#[test]
fn pi_uses_its_own_windows_without_sibling_borrowing() {
    let codex_reading = window(40, 3_600);
    let pi_reading = window(70, 3_600);
    let account = AgentAccount {
        scope: Default::default(),
        plan: Some("OpenAI OAuth".to_owned()),
        account_id: None,
        metered: Some(true),
        version: Some("0.78.0".to_owned()),
        sub_provider: Some("openai".to_owned()),
        credentials_updated_at_ms: None,
    };
    let mut probed: BTreeMap<String, AgentAccount> = BTreeMap::new();
    probed.insert("pi".to_owned(), account);

    let snapshot = room(vec![
        agent("codex", "x1", AgentStatus::Idle, 10).limits(vec![codex_reading]),
        agent("pi", "p1", AgentStatus::Idle, 20).limits(vec![pi_reading.clone()]),
    ])
    .with_provider_aggregates(&probed, &BTreeMap::new(), &BTreeMap::new());
    let pi_panel = snapshot
        .providers
        .iter()
        .find(|panel| panel.kind == "pi")
        .expect("pi panel present");
    assert!(pi_panel.metered);
    assert_eq!(pi_panel.version.as_deref(), Some("0.78.0"));
    assert_eq!(pi_panel.windows, vec![pi_reading]);

    let snapshot = room(vec![
        agent("codex", "x1", AgentStatus::Idle, 10).limits(vec![window(40, 3_600)]),
        agent("pi", "p1", AgentStatus::Idle, 20),
    ])
    .with_provider_aggregates(&probed, &BTreeMap::new(), &BTreeMap::new());
    let pi_panel = snapshot
        .providers
        .iter()
        .find(|panel| panel.kind == "pi")
        .expect("pi panel present");
    assert!(pi_panel.metered);
    assert!(pi_panel.windows.is_empty());
}
