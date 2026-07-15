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
            scope: Default::default(),
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

#[test]
fn idle_provider_presence_requires_account_substance() {
    let panel_count = |account: AgentAccount, spending: BTreeMap<String, SpendTally>| {
        let accounts = BTreeMap::from([("kimi".to_owned(), account)]);
        room(Vec::new())
            .with_provider_aggregates(&accounts, &BTreeMap::new(), &spending)
            .providers
            .len()
    };
    let credentials_only = AgentAccount {
        plan: Some("API Key".to_owned()),
        metered: Some(false),
        ..Default::default()
    };

    assert_eq!(panel_count(credentials_only.clone(), BTreeMap::new()), 0);
    assert_eq!(
        panel_count(
            credentials_only.clone(),
            provider_sessions([("kimi", 0, 0, 1)])
        ),
        1
    );
    assert_eq!(
        panel_count(
            AgentAccount {
                metered: Some(true),
                ..Default::default()
            },
            BTreeMap::new()
        ),
        1
    );
    assert_eq!(
        panel_count(
            AgentAccount {
                account_id: Some("octocat".to_owned()),
                ..Default::default()
            },
            BTreeMap::new()
        ),
        1
    );

    let accounts = BTreeMap::from([("kimi".to_owned(), credentials_only)]);
    let live = room(vec![agent("kimi", "k1", AgentStatus::Idle, 10)]).with_provider_aggregates(
        &accounts,
        &BTreeMap::new(),
        &BTreeMap::new(),
    );
    assert_eq!(provider_kinds(&live), vec!["kimi"]);
}

#[test]
fn antigravity_account_stays_metered_before_and_after_quota_arrives() {
    let mut account = AgentAccount {
        plan: Some("Google AI Ultra".to_owned()),
        account_id: Some("user@example.com".to_owned()),
        metered: Some(true),
        ..Default::default()
    };
    let mut accounts = BTreeMap::from([("antigravity".to_owned(), account.clone())]);
    let initial =
        room(Vec::new()).with_provider_aggregates(&accounts, &BTreeMap::new(), &BTreeMap::new());
    let panel = initial.providers.first().expect("account-only panel");
    assert_eq!(panel.kind, "antigravity");
    assert!(panel.metered);
    assert!(panel.windows.is_empty());

    let now = initial.now;
    let agent = agent("antigravity", "agy-1", AgentStatus::Idle, 10).limits(vec![
        RateLimitWindow {
            used_percentage: Some(70),
            resets_at: now.checked_add(jiff::SignedDuration::from_hours(4)).ok(),
            duration_mins: Some(300),
            source: crate::agents::context::WindowSource::Authoritative,
            ..Default::default()
        },
        RateLimitWindow {
            used_percentage: Some(60),
            resets_at: now.checked_add(jiff::SignedDuration::from_hours(120)).ok(),
            duration_mins: Some(10_080),
            source: crate::agents::context::WindowSource::Authoritative,
            ..Default::default()
        },
    ]);
    account.plan = None;
    accounts.insert("antigravity".to_owned(), account);
    let with_quota =
        room(vec![agent]).with_provider_aggregates(&accounts, &BTreeMap::new(), &BTreeMap::new());
    let panel = with_quota.providers.first().expect("live panel");
    assert!(panel.metered);
    assert_eq!(
        panel
            .windows
            .iter()
            .map(|window| (window.duration_mins, window.used_percentage))
            .collect::<Vec<_>>(),
        vec![(Some(300), Some(70)), (Some(10_080), Some(60))]
    );
}

#[test]
fn provider_active_sessions_count_bound_identity_panes_not_durable_rows() {
    let live = pane("%1", "agy", "/repo/main");
    let mut older = agent("antigravity", "conversation-old", AgentStatus::Success, 10)
        .worktree("/repo/main")
        .in_pane("%1");
    older.registered_at = Some(ago(120));
    let mut newer = agent("antigravity", "conversation-new", AgentStatus::Running, 20)
        .worktree("/repo/main")
        .in_pane("%1");
    newer.registered_at = Some(ago(60));
    let snapshot = room(vec![older, newer])
        .with_live_panes(vec![live], None)
        .with_provider_aggregates(&BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new());
    let panel = snapshot
        .providers
        .iter()
        .find(|panel| panel.kind == "antigravity")
        .unwrap();
    assert_eq!(panel.active_sessions, 1);
    assert!(panel.spending.is_none());

    let accounts = BTreeMap::from([(
        "antigravity".to_owned(),
        AgentAccount {
            metered: Some(true),
            ..Default::default()
        },
    )]);
    let mut identityless = room(Vec::new());
    identityless.wired_kinds = vec!["antigravity".to_owned()];
    let identityless = identityless
        .with_live_panes(vec![pane("%2", "agy", "/repo/main")], None)
        .with_provider_aggregates(&accounts, &BTreeMap::new(), &BTreeMap::new());
    assert_eq!(identityless.providers[0].active_sessions, 0);
}

#[test]
fn active_session_count_does_not_replace_real_provider_history() {
    let session = agent("claude", "c1", AgentStatus::Running, 10)
        .worktree("/repo/main")
        .in_pane("%1");
    let spending = BTreeMap::from([(
        "claude".to_owned(),
        SpendTally {
            headline: SpendWindow {
                sessions: 12,
                tokens: 42_000,
                usd: 3.5,
                ..Default::default()
            },
            ..Default::default()
        },
    )]);
    let snapshot = room(vec![session])
        .with_live_panes(vec![pane("%1", "claude", "/repo/main")], None)
        .with_provider_aggregates(&BTreeMap::new(), &BTreeMap::new(), &spending);
    let panel = &snapshot.providers[0];
    assert_eq!(panel.active_sessions, 1);
    assert_eq!(panel.spending.as_ref().unwrap().headline.sessions, 12);
    assert_eq!(panel.spending.as_ref().unwrap().headline.tokens, 42_000);
    assert_eq!(panel.spending.as_ref().unwrap().headline.usd, 3.5);
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
