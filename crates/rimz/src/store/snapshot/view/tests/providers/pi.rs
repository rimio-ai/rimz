use super::*;

#[test]
fn legacy_or_missing_pi_inventory_keeps_the_top_level_without_alternates() {
    let legacy = AgentAccount {
        sub_provider: Some("openai-codex".to_owned()),
        plan: Some("OpenAI Codex OAuth".to_owned()),
        metered: Some(true),
        ..Default::default()
    };
    for probed in [
        BTreeMap::from([(provider_key("pi"), legacy)]),
        BTreeMap::new(),
    ] {
        let fold = |provider: Option<&str>| {
            let mut session = agent("pi", "p1", AgentStatus::Idle, 10);
            session.context = Some(crate::agents::AgentContext {
                sub_provider: provider.map(ToOwned::to_owned),
                ..Default::default()
            });
            room(vec![session]).with_provider_aggregates(
                &Default::default(),
                &probed,
                &BTreeMap::new(),
                &BTreeMap::new(),
                &shown(&["pi"]),
            )
        };
        let before = fold(None);
        let reported = fold(Some("anthropic"));
        assert!(
            reported.providers[0].alternates.is_empty(),
            "a missing credential inventory cannot classify another provider"
        );
        assert_eq!(
            serde_json::to_value(&reported.providers).unwrap(),
            serde_json::to_value(&before.providers).unwrap()
        );
    }
}

#[test]
fn pi_credentials_partition_live_windows_without_partitioning_stats() {
    let credential = |provider: &str, owner: &str| AgentAccount {
        sub_provider: Some(provider.to_owned()),
        scope: crate::agents::ProviderAccountScope::sub_provider(provider, "oauth"),
        account_key: Some(owner.to_owned()),
        plan: Some(
            if provider == "anthropic" {
                "Anthropic OAuth"
            } else {
                "OpenAI Codex OAuth"
            }
            .to_owned(),
        ),
        metered: Some(true),
        ..Default::default()
    };
    let anthropic = credential("anthropic", "a");
    let codex = credential("openai-codex", "b");
    let account = AgentAccount {
        credentials: BTreeMap::from([
            ("anthropic".to_owned(), anthropic.clone()),
            ("openai-codex".to_owned(), codex),
        ]),
        ..anthropic
    };
    let session = |id: &str, provider: Option<&str>, owner: Option<&str>, used, rank| {
        let mut session =
            agent("pi", id, AgentStatus::Idle, rank).limits(vec![window(used, 3_600)]);
        session.account_key = owner.map(ToOwned::to_owned);
        session.context.as_mut().unwrap().sub_provider = provider.map(ToOwned::to_owned);
        session
    };
    let snapshot = room_with_agent_panes(vec![
        session("a", Some("anthropic"), Some("a"), 20, 10),
        session("b", Some("openai-codex"), Some("b"), 70, 20),
        session("old-b", Some("openai-codex"), Some("old-b"), 99, 30),
        session("legacy", None, None, 45, 40),
        session("unlisted", Some("custom"), None, 90, 50),
    ])
    .with_provider_aggregates(
        &Default::default(),
        &BTreeMap::from([(provider_key("pi"), account)]),
        &BTreeMap::new(),
        &BTreeMap::from([(
            provider_key("pi"),
            SpendTally {
                month: SpendWindow {
                    usd: 12.0,
                    sessions: 8,
                    ..Default::default()
                },
                ..Default::default()
            },
        )]),
        &shown(&["pi"]),
    );
    let top = &snapshot.providers[0];
    let panels: Vec<_> = std::iter::once(top).chain(&top.alternates).collect();
    let actual: Vec<_> = panels
        .iter()
        .map(|panel| {
            (
                panel.sub_provider.as_deref(),
                panel.account_key.as_deref(),
                panel.metered,
                panel.plan.as_deref(),
                panel
                    .windows
                    .iter()
                    .filter_map(|w| w.used_percentage)
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    assert_eq!(
        actual,
        [
            (
                Some("anthropic"),
                Some("a"),
                true,
                Some("Anthropic OAuth"),
                vec![45]
            ),
            (Some("custom"), None, false, None, vec![]),
            (
                Some("openai-codex"),
                Some("b"),
                true,
                Some("OpenAI Codex OAuth"),
                vec![70]
            ),
        ]
    );
    assert_eq!(snapshot.providers.len(), 1);
    for panel in panels {
        assert_eq!(panel.active_sessions, 5);
        assert_eq!(panel.spending, top.spending);
    }
    assert!(
        top.alternates
            .iter()
            .all(|panel| panel.alternates.is_empty())
    );
    assert!(top.alternates[0].account_scope.is_kind_wide());
    assert_eq!(
        top.alternates[1].account_scope,
        crate::agents::ProviderAccountScope::sub_provider("openai-codex", "oauth")
    );
}

#[test]
fn an_undecided_pi_login_still_exposes_each_reported_credential() {
    let mut api = agent("pi", "api", AgentStatus::Idle, 10).limits(Vec::new());
    api.context.as_mut().unwrap().sub_provider = Some("anthropic".to_owned());
    let probed = AgentAccount {
        provisional: true,
        credentials: BTreeMap::from([(
            "anthropic".to_owned(),
            AgentAccount {
                sub_provider: Some("anthropic".to_owned()),
                metered: Some(false),
                plan: Some("Anthropic API Key".to_owned()),
                ..Default::default()
            },
        )]),
        ..Default::default()
    };
    let snapshot = room(vec![api]).with_provider_aggregates(
        &Default::default(),
        &BTreeMap::from([(provider_key("pi"), probed)]),
        &BTreeMap::new(),
        &BTreeMap::new(),
        &shown(&["pi"]),
    );
    let panel = &snapshot.providers[0];
    assert_eq!(
        panel
            .alternates
            .iter()
            .map(|p| (
                p.sub_provider.as_deref(),
                p.undecided,
                p.metered,
                p.plan.as_deref()
            ))
            .collect::<Vec<_>>(),
        [(Some("anthropic"), false, false, Some("Anthropic API Key"))]
    );
    assert!(panel.undecided);
    assert!(!panel.metered);
    assert!(panel.windows.is_empty());
}

#[test]
fn undecided_account_cannot_infer_metering_or_windows_from_a_session() {
    let probed = BTreeMap::from([(
        provider_key("pi"),
        AgentAccount {
            provisional: true,
            ..Default::default()
        },
    )]);
    let snapshot = room(vec![
        agent("pi", "p1", AgentStatus::Idle, 20).limits(vec![window(70, 3_600)]),
    ])
    .with_provider_aggregates(
        &Default::default(),
        &probed,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &shown(&["pi"]),
    );
    let panel = &snapshot.providers[0];
    assert_eq!(
        (
            panel.undecided,
            panel.metered,
            panel.plan.as_deref(),
            panel.windows.len()
        ),
        (true, false, None, 0)
    );
}

#[test]
fn pi_uses_its_own_windows_without_sibling_borrowing() {
    let codex_reading = window(40, 3_600);
    let pi_reading = window(70, 3_600);
    let account = AgentAccount {
        profile_fingerprint: None,
        scope: Default::default(),
        plan: Some("OpenAI OAuth".to_owned()),
        account_id: None,
        metered: Some(true),
        version: Some("0.78.0".to_owned()),
        sub_provider: Some("openai".to_owned()),
        credentials_updated_at_ms: None,
        ..Default::default()
    };
    let mut probed: BTreeMap<crate::ids::LoginKey, AgentAccount> = BTreeMap::new();
    probed.insert(provider_key("pi"), account);

    let snapshot = room(vec![
        agent("codex", "x1", AgentStatus::Idle, 10).limits(vec![codex_reading]),
        agent("pi", "p1", AgentStatus::Idle, 20).limits(vec![pi_reading.clone()]),
    ])
    .with_provider_aggregates(
        &Default::default(),
        &probed,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &shown(&["codex", "pi"]),
    );
    let pi_panel = snapshot
        .providers
        .iter()
        .find(|panel| panel.kind == "pi")
        .expect("pi panel present");
    assert!(pi_panel.metered);
    assert_eq!(pi_panel.sub_provider.as_deref(), Some("openai"));
    assert_eq!(pi_panel.version.as_deref(), Some("0.78.0"));
    assert_eq!(pi_panel.windows, vec![pi_reading]);

    let snapshot = room(vec![
        agent("codex", "x1", AgentStatus::Idle, 10).limits(vec![window(40, 3_600)]),
        agent("pi", "p1", AgentStatus::Idle, 20),
    ])
    .with_provider_aggregates(
        &Default::default(),
        &probed,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &shown(&["codex", "pi"]),
    );
    let pi_panel = snapshot
        .providers
        .iter()
        .find(|panel| panel.kind == "pi")
        .expect("pi panel present");
    assert!(pi_panel.metered);
    assert!(pi_panel.windows.is_empty());
}
