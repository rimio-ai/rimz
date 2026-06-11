use super::*;

#[test]
fn provider_panel_spending_is_attached_and_panels_order_by_kind() {
    let claude = agent("claude", "c1", AgentStatus::Idle, 10);
    let codex = agent("codex", "x1", AgentStatus::Idle, 20);
    let snapshot = room(Vec::new(), vec![claude, codex]);

    let today_tally = |usd: f64| SpendTally {
        today: SpendWindow {
            usd,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut by_provider: BTreeMap<String, SpendTally> = BTreeMap::new();
    by_provider.insert("claude".to_owned(), today_tally(1.0));
    by_provider.insert("codex".to_owned(), today_tally(5.0));

    let snapshot =
        snapshot.with_provider_aggregates(&BTreeMap::new(), &BTreeMap::new(), &by_provider);

    // The panels are the dashboard's tabs, so they hold a stable kind order —
    // Codex's larger today spend (5.0) never reorders the row.
    assert_eq!(snapshot.providers[0].kind, "claude");
    assert_eq!(snapshot.providers[1].kind, "codex");
    // Each panel still carries its own spending tally.
    assert_eq!(
        snapshot.providers[0].spending.as_ref().unwrap().today.usd,
        1.0
    );
    assert_eq!(
        snapshot.providers[1].spending.as_ref().unwrap().today.usd,
        5.0
    );
}

#[test]
fn provider_cap_keeps_top_spenders_then_orders_by_kind() {
    // Three providers, room for two: today's spend decides *which* panels
    // survive the cap (claude's 1.0 is dropped), and the survivors render in
    // stable kind order regardless of who outspends whom.
    let claude = agent("claude", "c1", AgentStatus::Idle, 10);
    let codex = agent("codex", "x1", AgentStatus::Idle, 20);
    let pi = agent("pi", "p1", AgentStatus::Idle, 30);
    let mut snapshot = room(Vec::new(), vec![claude, codex, pi]);
    snapshot.sidebar.max_provider_blocks = 2;

    let today_tally = |usd: f64| SpendTally {
        today: SpendWindow {
            usd,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut by_provider: BTreeMap<String, SpendTally> = BTreeMap::new();
    by_provider.insert("claude".to_owned(), today_tally(1.0));
    by_provider.insert("codex".to_owned(), today_tally(5.0));
    by_provider.insert("pi".to_owned(), today_tally(3.0));

    let snapshot =
        snapshot.with_provider_aggregates(&BTreeMap::new(), &BTreeMap::new(), &by_provider);

    assert_eq!(provider_kinds(&snapshot), vec!["codex", "pi"]);
}

#[test]
fn provider_list_filters_and_orders_dashboard_panels() {
    for case in [
        (
            "strict allowlist orders and hides unnamed kinds",
            vec!["pi", "claude"],
            vec!["claude", "codex", "pi"],
            vec!["pi", "claude"],
        ),
        (
            "all expands remaining kinds at that position",
            vec!["codex", "all"],
            vec!["claude", "codex", "pi"],
            vec!["codex", "claude", "pi"],
        ),
        (
            "drops named kinds absent from discovery",
            vec!["opencode", "codex"],
            vec!["claude", "codex"],
            vec!["codex"],
        ),
        (
            "only absent kinds resolves to no dashboard",
            vec!["opencode"],
            vec!["claude", "codex"],
            Vec::new(),
        ),
    ] {
        let (label, provider_list, agent_kinds, expected) = case;
        let agents = agent_kinds
            .iter()
            .enumerate()
            .map(|(idx, kind)| {
                agent(
                    kind,
                    &format!("{kind}-{idx}"),
                    AgentStatus::Idle,
                    10 + idx as i64,
                )
            })
            .collect();
        let mut snapshot = room(Vec::new(), agents);
        snapshot.sidebar.max_provider_blocks = 1;
        snapshot.sidebar.provider_list = provider_list.into_iter().map(str::to_owned).collect();

        let snapshot =
            snapshot.with_provider_aggregates(&BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new());

        assert_eq!(provider_kinds(&snapshot), expected, "{label}");
    }
}

#[test]
fn recorded_spend_attaches_only_after_provider_discovery() {
    let snapshot = room(Vec::new(), Vec::new());
    let mut by_provider: BTreeMap<String, SpendTally> = BTreeMap::new();
    by_provider.insert(
        "claude".to_owned(),
        SpendTally {
            today: SpendWindow {
                usd: 2.0,
                tokens: 100,
                ..Default::default()
            },
            year: SpendWindow {
                usd: 9.0,
                tokens: 900,
                ..Default::default()
            },
            ..Default::default()
        },
    );

    let spend_only =
        snapshot
            .clone()
            .with_provider_aggregates(&BTreeMap::new(), &BTreeMap::new(), &by_provider);
    assert!(
        spend_only.providers.is_empty(),
        "historical spend alone does not create the provider section"
    );

    let mut probed = BTreeMap::new();
    probed.insert(
        "claude".to_owned(),
        AgentAccount {
            plan: Some("max".to_owned()),
            metered: Some(true),
            version: None,
            sub_provider: None,
        },
    );
    let snapshot = snapshot.with_provider_aggregates(&probed, &BTreeMap::new(), &by_provider);

    let claude = snapshot
        .providers
        .iter()
        .find(|panel| panel.kind == "claude")
        .expect("claude panel from probed account");
    assert_eq!(claude.spending.as_ref().unwrap().year.usd, 9.0);
}
