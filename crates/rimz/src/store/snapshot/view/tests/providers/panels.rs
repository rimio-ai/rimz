use super::*;

#[test]
fn live_provider_outranks_heavier_history_and_cap_keeps_usage_leaders() {
    let claude = agent("claude", "c1", AgentStatus::Idle, 10);
    let accounts = provider_accounts([("codex", 10), ("pi", 20)]);
    let by_provider = provider_sessions([
        ("claude", 0, 0, 0),
        ("codex", 100, 100, 100),
        ("pi", 50, 50, 50),
    ]);

    let mut snapshot = room(vec![claude.clone()]);
    // The cap only trims the stacked dashboard; pin `never` so it bites here.
    snapshot.theme.display.provider_tabs = crate::config::ProviderTabsMode::Never;
    snapshot.theme.display.max_provider_blocks = 3;
    let snapshot = snapshot.with_provider_aggregates(&accounts, &BTreeMap::new(), &by_provider);

    assert_eq!(provider_kinds(&snapshot), vec!["claude", "codex", "pi"]);

    let mut snapshot = room(vec![claude]);
    snapshot.theme.display.provider_tabs = crate::config::ProviderTabsMode::Never;
    snapshot.theme.display.max_provider_blocks = 2;
    let snapshot = snapshot.with_provider_aggregates(&accounts, &BTreeMap::new(), &by_provider);

    assert_eq!(provider_kinds(&snapshot), vec!["claude", "codex"]);
}

#[test]
fn session_windows_rank_lexicographically() {
    let accounts = provider_accounts([("claude", 1), ("codex", 2), ("pi", 3)]);
    let spending = provider_sessions([
        ("claude", 1, 1, 1),
        ("codex", 0, 100, 100),
        ("pi", 0, 0, 1_000),
    ]);
    let snapshot =
        room(Vec::new()).with_provider_aggregates(&accounts, &BTreeMap::new(), &spending);
    assert_eq!(provider_kinds(&snapshot), vec!["claude", "codex", "pi"]);
}

#[test]
fn login_and_credential_recency_break_unused_live_ties() {
    let agents = vec![
        agent("claude", "c1", AgentStatus::Idle, 10),
        agent("codex", "x1", AgentStatus::Idle, 20),
        agent("pi", "p1", AgentStatus::Idle, 30),
    ];
    let accounts = provider_accounts([("codex", 10), ("pi", 20)]);
    let snapshot =
        room(agents).with_provider_aggregates(&accounts, &BTreeMap::new(), &BTreeMap::new());
    assert_eq!(provider_kinds(&snapshot), vec!["pi", "codex", "claude"]);
}

#[test]
fn full_usage_tie_falls_back_to_registry_order() {
    let snapshot = room(vec![
        agent("pi", "p1", AgentStatus::Idle, 10),
        agent("codex", "x1", AgentStatus::Idle, 20),
        agent("claude", "c1", AgentStatus::Idle, 30),
    ])
    .with_provider_aggregates(&BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new());
    assert_eq!(provider_kinds(&snapshot), vec!["claude", "codex", "pi"]);
}

#[test]
fn tabbed_dashboard_shows_every_provider_past_the_cap() {
    // Three or more providers auto-tab, and a tabbed dashboard is bounded by its
    // single active block — so every logged-in provider keeps its tab even under
    // a cap that would trim the stacked layout to two. This is what keeps Pi on
    // the dashboard once OpenCode joins as a fourth account.
    let agents = vec![
        agent("claude", "c1", AgentStatus::Idle, 10),
        agent("codex", "x1", AgentStatus::Idle, 20),
        agent("amp", "a1", AgentStatus::Idle, 30),
        agent("pi", "p1", AgentStatus::Idle, 40),
        agent("opencode", "o1", AgentStatus::Idle, 50),
    ];
    let mut snapshot = room(agents);
    // Default `auto` tabs at five providers; a tight cap must not trim them.
    snapshot.theme.display.max_provider_blocks = 2;
    let snapshot =
        snapshot.with_provider_aggregates(&BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new());

    // Painted in the registry's display order, not alphabetically.
    assert_eq!(
        provider_kinds(&snapshot),
        vec!["claude", "codex", "amp", "pi", "opencode"]
    );
}

#[test]
fn provider_brand_color_carries_rgb_and_indexed_fallback() {
    let panel_for = |mut snapshot: SidebarSnapshot| {
        snapshot =
            snapshot.with_provider_aggregates(&BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new());
        snapshot
            .providers
            .into_iter()
            .find(|panel| panel.kind == "claude")
            .expect("claude panel")
    };

    let panel = panel_for(room(vec![agent("claude", "c1", AgentStatus::Idle, 10)]));
    assert_eq!(panel.color, 173);
    assert_eq!(panel.color_rgb, Some((0xd9, 0x77, 0x57)));

    let mut snapshot = room(vec![agent("claude", "c1", AgentStatus::Idle, 10)]);
    snapshot.theme.providers.insert(
        "claude".to_owned(),
        crate::config::ThemeProviderStyle {
            color: Some(crate::config::ThemeColor::Indexed(208)),
            ..Default::default()
        },
    );
    let panel = panel_for(snapshot);
    assert_eq!(panel.color, 208);
    assert_eq!(
        panel.color_rgb, None,
        "indexed override stays compatible with indexed-only renderers"
    );

    let mut snapshot = room(vec![agent("claude", "c1", AgentStatus::Idle, 10)]);
    snapshot.theme.providers.insert(
        "claude".to_owned(),
        crate::config::ThemeProviderStyle {
            color: Some(crate::config::ThemeColor::Rgb(0xa3, 0xbe, 0x8c)),
            ..Default::default()
        },
    );
    let panel = panel_for(snapshot);
    assert_eq!(
        panel.color,
        crate::config::nearest_xterm_index(0xa3, 0xbe, 0x8c)
    );
    assert_eq!(panel.color_rgb, Some((0xa3, 0xbe, 0x8c)));
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
        let mut snapshot = room(agents);
        snapshot.theme.display.max_provider_blocks = 1;
        snapshot.theme.display.provider_list =
            provider_list.into_iter().map(str::to_owned).collect();

        let snapshot =
            snapshot.with_provider_aggregates(&BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new());

        assert_eq!(provider_kinds(&snapshot), expected, "{label}");
    }
}

#[test]
fn provider_list_all_expands_remaining_in_usage_order() {
    let agents = vec![
        agent("claude", "c1", AgentStatus::Idle, 10),
        agent("codex", "x1", AgentStatus::Idle, 20),
        agent("pi", "p1", AgentStatus::Idle, 30),
    ];
    let spending = provider_sessions([("claude", 0, 0, 0), ("codex", 1, 1, 1), ("pi", 2, 2, 2)]);
    let mut snapshot = room(agents);
    snapshot.theme.display.provider_list = vec!["claude".to_owned(), "all".to_owned()];
    let snapshot = snapshot.with_provider_aggregates(&BTreeMap::new(), &BTreeMap::new(), &spending);
    assert_eq!(provider_kinds(&snapshot), vec!["claude", "pi", "codex"]);
}

#[test]
fn recorded_spend_attaches_only_after_provider_discovery() {
    let snapshot = room(Vec::new());
    let mut by_provider: BTreeMap<String, SpendTally> = BTreeMap::new();
    by_provider.insert(
        "claude".to_owned(),
        SpendTally {
            headline: SpendWindow {
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
            account_id: None,
            metered: Some(true),
            version: None,
            sub_provider: None,
            credentials_updated_at_ms: None,
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

fn provider_sessions(
    entries: impl IntoIterator<Item = (&'static str, u32, u32, u32)>,
) -> BTreeMap<String, SpendTally> {
    entries
        .into_iter()
        .map(|(kind, week, month, year)| {
            (
                kind.to_owned(),
                SpendTally {
                    week: SpendWindow {
                        sessions: week,
                        ..Default::default()
                    },
                    month: SpendWindow {
                        sessions: month,
                        ..Default::default()
                    },
                    year: SpendWindow {
                        sessions: year,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
        })
        .collect()
}

fn provider_accounts(
    entries: impl IntoIterator<Item = (&'static str, u64)>,
) -> BTreeMap<String, AgentAccount> {
    entries
        .into_iter()
        .map(|(kind, credentials_updated_at_ms)| {
            (
                kind.to_owned(),
                AgentAccount {
                    plan: Some("pro".to_owned()),
                    metered: Some(true),
                    credentials_updated_at_ms: Some(credentials_updated_at_ms),
                    ..Default::default()
                },
            )
        })
        .collect()
}
