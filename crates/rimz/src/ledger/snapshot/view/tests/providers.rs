use super::*;

fn provider_kinds(snapshot: &SidebarSnapshot) -> Vec<&str> {
    snapshot
        .providers
        .iter()
        .map(|panel| panel.kind.as_str())
        .collect()
}

// ── Provider dashboard aggregation ──────────────────────────────────────────

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
fn spend_only_provider_does_not_create_a_panel() {
    // No live agents and no probed accounts — only recorded fleet spend for
    // Claude. Spend enriches a discovered provider, but it is not provider
    // presence by itself, so the dashboard stays hidden.
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

    let snapshot =
        snapshot.with_provider_aggregates(&BTreeMap::new(), &BTreeMap::new(), &by_provider);

    assert!(
        snapshot.providers.is_empty(),
        "historical spend alone does not create the provider section"
    );
}

#[test]
fn recorded_spend_attaches_to_a_probed_provider_panel() {
    let snapshot = room(Vec::new(), Vec::new());
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

    let snapshot = snapshot.with_provider_aggregates(&probed, &BTreeMap::new(), &by_provider);

    let claude = snapshot
        .providers
        .iter()
        .find(|panel| panel.kind == "claude")
        .expect("claude panel from probed account");
    assert_eq!(claude.spending.as_ref().unwrap().year.usd, 9.0);
}

#[test]
fn default_emblems_keep_every_rows_leading_spaces() {
    // The emblem literals open with a bare newline so the art sits at
    // column 0 in source; the split must keep each row's leading spaces —
    // a `\` continuation once ate the first row's indent and the art
    // drifted a cell left on screen.
    let art = |kind: &str| default_provider_style(kind).1;
    assert_eq!(art("claude"), [" ▐▛███▜▌", "▝▜█████▛▘", "  ▘▘ ▝▝"]);
    assert_eq!(art("codex"), [" ▗▛███▜▖", "▐▜▌ ▚ ▐▛▌", " ▝▀▀▀▀▀▘"]);
    assert_eq!(art("pi"), [" █▜███▛█", "▝▜▛▀▀▀▜▛▘", " ▝▘   ▝▘"]);
}

#[test]
fn provider_without_the_rate_limit_capability_drops_stray_windows() {
    // Pi declares `rate_limit_windows: false`; Claude declares it true. The
    // same stray session reading must paint a budget bar only where the
    // descriptor declares the surface.
    let reading = window(40, 3_600);
    let pi = agent("pi", "p1", AgentStatus::Idle, 10).limits(vec![reading.clone()]);
    let claude = agent("claude", "c1", AgentStatus::Idle, 10).limits(vec![reading]);

    let snapshot = room(Vec::new(), vec![pi, claude]).with_provider_aggregates(
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
        panel("pi").windows.is_empty(),
        "pi's declared absence drops the stray reading"
    );
    assert_eq!(panel("claude").windows.len(), 1);
}

#[test]
fn pi_on_a_metered_sub_borrows_the_sibling_kinds_windows() {
    // Pi reads no window surface of its own, but an OAuth subscription is the
    // sibling provider's account — `openai` maps to the codex kind — so the
    // Pi panel borrows codex's stable windows: same account, same bars.
    let reading = window(40, 3_600);
    let codex = agent("codex", "x1", AgentStatus::Idle, 10).limits(vec![reading.clone()]);
    let pi = agent("pi", "p1", AgentStatus::Idle, 20);

    let mut probed: BTreeMap<String, AgentAccount> = BTreeMap::new();
    probed.insert(
        "pi".to_owned(),
        AgentAccount {
            plan: Some("OpenAI OAuth".to_owned()),
            metered: Some(true),
            version: None,
            sub_provider: Some("openai".to_owned()),
        },
    );

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
    assert!(pi_panel.metered, "an OAuth sub is metered");
    assert_eq!(
        pi_panel.windows,
        vec![reading],
        "pi borrows the codex account's windows"
    );
}

#[test]
fn probed_pi_version_reaches_the_provider_panel() {
    let pi = agent("pi", "p1", AgentStatus::Idle, 20);
    let mut probed: BTreeMap<String, AgentAccount> = BTreeMap::new();
    probed.insert(
        "pi".to_owned(),
        AgentAccount {
            plan: Some("OpenAI OAuth".to_owned()),
            metered: Some(true),
            version: Some("0.78.0".to_owned()),
            sub_provider: Some("openai".to_owned()),
        },
    );

    let snapshot = room(Vec::new(), vec![pi]).with_provider_aggregates(
        &probed,
        &BTreeMap::new(),
        &BTreeMap::new(),
    );

    let pi_panel = snapshot
        .providers
        .iter()
        .find(|panel| panel.kind == "pi")
        .expect("pi panel present");
    assert_eq!(pi_panel.version.as_deref(), Some("0.78.0"));
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

#[test]
fn pi_sub_without_borrowable_windows_stays_bar_less() {
    // A metered Pi sub whose sibling has no readings (no codex session ever
    // reported), or whose provider maps to no kind, keeps the bar-less block.
    let pi = agent("pi", "p1", AgentStatus::Idle, 20);
    let mut probed: BTreeMap<String, AgentAccount> = BTreeMap::new();
    probed.insert(
        "pi".to_owned(),
        AgentAccount {
            plan: Some("GitHub Copilot OAuth".to_owned()),
            metered: Some(true),
            version: None,
            sub_provider: Some("github-copilot".to_owned()),
        },
    );

    let snapshot = room(Vec::new(), vec![pi]).with_provider_aggregates(
        &probed,
        &BTreeMap::new(),
        &BTreeMap::new(),
    );

    let pi_panel = snapshot
        .providers
        .iter()
        .find(|panel| panel.kind == "pi")
        .expect("pi panel present");
    assert!(pi_panel.windows.is_empty(), "no sibling kind, no bars");
}

#[test]
fn pi_api_key_sub_never_borrows_windows() {
    // An unmetered (API-key) credential has no budget to meter, so even a
    // borrowable sibling leaves the `∞` bar untouched.
    let reading = window(40, 3_600);
    let codex = agent("codex", "x1", AgentStatus::Idle, 10).limits(vec![reading]);
    let pi = agent("pi", "p1", AgentStatus::Idle, 20);
    let mut probed: BTreeMap<String, AgentAccount> = BTreeMap::new();
    probed.insert(
        "pi".to_owned(),
        AgentAccount {
            plan: Some("OpenAI API Key".to_owned()),
            metered: Some(false),
            version: None,
            sub_provider: Some("openai".to_owned()),
        },
    );

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
    assert!(!pi_panel.metered);
    assert!(pi_panel.windows.is_empty(), "an API key meters nothing");
}
