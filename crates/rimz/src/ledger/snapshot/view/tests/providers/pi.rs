use super::*;

#[test]
fn pi_borrows_windows_only_for_metered_mapped_subscriptions() {
    let reading = window(40, 3_600);
    for (label, account, expect_metered, expect_windows, expect_version) in [
        (
            "mapped OAuth subscription borrows codex bars",
            AgentAccount {
                plan: Some("OpenAI OAuth".to_owned()),
                metered: Some(true),
                version: Some("0.78.0".to_owned()),
                sub_provider: Some("openai".to_owned()),
            },
            true,
            1,
            Some("0.78.0"),
        ),
        (
            "unmapped metered subscription stays bar-less",
            AgentAccount {
                plan: Some("GitHub Copilot OAuth".to_owned()),
                metered: Some(true),
                version: None,
                sub_provider: Some("github-copilot".to_owned()),
            },
            true,
            0,
            None,
        ),
        (
            "unmetered API key never borrows sibling bars",
            AgentAccount {
                plan: Some("OpenAI API Key".to_owned()),
                metered: Some(false),
                version: None,
                sub_provider: Some("openai".to_owned()),
            },
            false,
            0,
            None,
        ),
    ] {
        let codex = agent("codex", "x1", AgentStatus::Idle, 10).limits(vec![reading.clone()]);
        let pi = agent("pi", "p1", AgentStatus::Idle, 20);
        let mut probed: BTreeMap<String, AgentAccount> = BTreeMap::new();
        probed.insert("pi".to_owned(), account);

        let snapshot = room(Vec::new(), vec![codex, pi]).with_provider_aggregates(
            &probed,
            &BTreeMap::new(),
            &BTreeMap::new(),
        );

        let pi_panel = snapshot
            .providers
            .iter()
            .find(|panel| panel.kind == "pi")
            .expect("pi panel present");
        assert_eq!(pi_panel.metered, expect_metered, "{label}");
        assert_eq!(pi_panel.windows.len(), expect_windows, "{label}");
        assert_eq!(pi_panel.version.as_deref(), expect_version, "{label}");
        if expect_windows == 1 {
            assert_eq!(pi_panel.windows, vec![reading.clone()], "{label}");
        }
    }
}

#[test]
fn version_only_pi_probe_enriches_active_panel_without_creating_idle_one() {
    let mut probed: BTreeMap<String, AgentAccount> = BTreeMap::new();
    probed.insert(
        "pi".to_owned(),
        AgentAccount {
            version: Some("0.78.1".to_owned()),
            ..Default::default()
        },
    );

    let active = room(Vec::new(), vec![agent("pi", "p1", AgentStatus::Idle, 20)])
        .with_provider_aggregates(&probed, &BTreeMap::new(), &BTreeMap::new());
    let pi_panel = active
        .providers
        .iter()
        .find(|panel| panel.kind == "pi")
        .expect("active pi panel present");
    assert_eq!(pi_panel.version.as_deref(), Some("0.78.1"));

    let idle = room(Vec::new(), Vec::new()).with_provider_aggregates(
        &probed,
        &BTreeMap::new(),
        &BTreeMap::new(),
    );
    assert!(
        idle.providers.is_empty(),
        "a binary version alone is not a logged-in account"
    );
}
