use super::super::*;
use jiff::SignedDuration;
use jiff::tz::TimeZone;
use rimz::agents::{AgentAccount, SpendTally, SpendWindow};
use rimz::config::MachineConfig;
use rimz::ids::LoginName;
use rimz::store::snapshot::{RemoteControlBadge, SidebarProviderPanel};

#[test]
fn account_label_titles_use_the_theme_with_or_without_a_panel() {
    let mut config: MachineConfig = toml::from_str("[theme.providers.codex]\nproduct_name = 'Code'\n[accounts.codex.team-1]\nhome = '/accounts/team-1'\n").unwrap();
    config.accounts.codex.get_mut("team-1").unwrap().label = Some("work".to_owned());
    let catalog = LoginCatalog::from_config(&config.accounts).unwrap();
    let kind = AgentKind::new_unchecked("codex");
    let login = catalog.select(&kind, &"team-1".parse().unwrap()).unwrap();
    let mut provider = panel("codex");
    provider.account = login.name().clone();
    provider.product_name = "Code · work".to_owned();
    for panel in [None, Some(&provider)] {
        let report = build_report(
            &login,
            None,
            panel,
            &ProviderSpendingCache::default(),
            &ReportContext {
                day_cutoff_secs: None,
                config: &config,
                agents: None,
                installed: &BTreeSet::new(),
                spend_step: &|_| SpendStep::Subscription,
            },
        );
        assert_eq!(report.product_name, "Code · work");
        assert_eq!(report.account.to_string(), "codex@team-1");
    }
}

#[test]
fn account_labels_have_a_conditional_column_detail_row_and_nullable_json() {
    let mut config: MachineConfig = toml::from_str("[accounts.codex.team-1]\nhome = '/accounts/team-1'\n[accounts.codex.team-2]\nhome = '/accounts/team-2'\n[accounts.claude.personal]\nhome = '/accounts/personal'\n[accounts.use]\nclaude = 'missing'\n").unwrap();
    config.accounts.codex.get_mut("team-1").unwrap().label = Some("work".to_owned());
    config.accounts.codex.get_mut("team-2").unwrap().label = Some(" bad".to_owned());
    let catalog = LoginCatalog::from_config(&config.accounts).unwrap();
    let mut rows = account_rows(
        &config.accounts,
        &catalog,
        &AccountStanding::machine_only(&config),
        &BTreeMap::from([("HOME".to_owned(), "/home/u".to_owned())]),
        None,
        &BTreeMap::new(),
        &BTreeMap::new(),
    );
    rows.retain(|row| matches!(row.kind.as_str(), "claude" | "codex"));
    let mut out = anstream::StripStream::new(Vec::new());
    write_overview(
        &mut out,
        &rows,
        Timestamp::UNIX_EPOCH,
        &TimeZone::UTC,
        &BTreeMap::new(),
        true,
    )
    .unwrap();
    let text = String::from_utf8(out.into_inner()).unwrap();
    assert_eq!(text.matches("LABEL").count(), 1, "{text}");
    let header = text.lines().find(|line| line.contains("LABEL")).unwrap();
    assert_eq!(&cells(header)[..3], ["NAME", "LABEL", "PLAN"]);
    insta::assert_snapshot!("account_labels_overview", text);

    let row = rows
        .iter()
        .find(|row| row.account.to_string() == "codex@team-1")
        .unwrap();
    let mut out = anstream::StripStream::new(Vec::new());
    super::write_pretty(
        &mut out,
        std::slice::from_ref(row),
        Timestamp::UNIX_EPOCH,
        &TimeZone::UTC,
        &BTreeMap::new(),
        &multiple_account_kinds(&rows),
    )
    .unwrap();
    let text = String::from_utf8(out.into_inner()).unwrap();
    assert!(text.contains("Codex · codex@team-1"), "{text}");
    assert!(
        text.lines()
            .any(|line| line.trim_start().starts_with("label:") && line.ends_with("work")),
        "{text}"
    );
    insta::assert_snapshot!("account_labels_detail", text);
    for row in rows {
        let json = serde_json::to_value(&row).unwrap();
        let expected = if row.account.to_string() == "codex@team-1" {
            serde_json::json!("work")
        } else {
            serde_json::Value::Null
        };
        assert_eq!(json.get("label"), Some(&expected), "{json}");
    }
}

fn merged(reports: &[ProviderReport]) -> Vec<AccountRow> {
    reports
        .iter()
        .map(|report| AccountRow {
            kind: report.account.kind.clone(),
            account: report.account.clone(),
            home: Some(if report.account.name.is_default() {
                PathBuf::from(format!("/home/u/.{}", report.kind))
            } else {
                PathBuf::from(format!("/accounts/{}", report.account.name))
            }),
            history: None,
            label: None,
            machine_default: report.default_for.contains(Scope::NewRooms),
            problem: None,
            setup_error: None,
            login_command: None,
            status: AccountStatus::Ready,
            entitlement: report.entitlement,
            active: report.active,
            default_for: report.default_for.clone(),
            agents: report.agents,
            windows: report.windows.clone(),
            extra_credits: report.extra_credits.clone(),
            reset_credits: report.reset_credits.clone(),
            spend_step: Some(report.spend_step),
            metered: report.metered,
            login: Some(report.status),
            pool: report.pool.clone(),
            product_name: report.product_name.clone(),
            probed_at: report.probed_at,
            probe_error: report.probe_error.clone(),
            plan: report.plan.clone(),
            plan_label: report.plan_label.clone(),
            account_id: report.account_id.clone(),
            sub_provider: report.sub_provider.clone(),
            account_scope: report.account_scope.clone(),
            version: report.version.clone(),
            spending: report.spending.clone(),
            day_budget: report.day_budget,
            installed: report.installed,
            legacy_spend_step: report.spend_step,
        })
        .collect()
}

fn mark_standing(reports: &mut [ProviderReport], standing: &AccountStanding) {
    for report in reports {
        let kind = AgentKind::new_unchecked(&report.kind);
        report.active = standing.active(&kind).as_ref() == Some(&report.account.name);
        report.default_for = standing.scopes(&kind, &report.account.name);
    }
}

fn write_pretty(
    out: &mut impl std::io::Write,
    reports: &[ProviderReport],
    now: Timestamp,
    time_zone: &TimeZone,
) -> std::io::Result<()> {
    let rows = merged(reports);
    super::write_pretty(
        out,
        &rows,
        now,
        time_zone,
        &BTreeMap::new(),
        &multiple_account_kinds(&rows),
    )
}

fn assemble_reports(
    logins: &[ProviderLogin],
    accounts: &AccountsCache,
    panels: BTreeMap<LoginKey, SidebarProviderPanel>,
    spending: &ProviderSpendingCache,
    filter: Option<&str>,
) -> Vec<ProviderReport> {
    super::assemble_reports(
        logins,
        accounts,
        panels,
        spending,
        filter,
        &ReportContext {
            day_cutoff_secs: None,
            config: &MachineConfig::default(),
            agents: None,
            installed: &rimz::agents::known_kinds().map(str::to_owned).collect(),
            spend_step: &|_| SpendStep::Subscription,
        },
    )
}

fn record(probed_at_ms: u64, ok: bool, account: Option<AgentAccount>) -> ProviderRecord {
    ProviderRecord {
        inputs_stamp: None,
        profile: None,
        probe_error: None,
        login: None,
        probed_at_ms,
        ok,
        account,
    }
}

fn account(plan: &str, metered: bool) -> AgentAccount {
    AgentAccount {
        plan: Some(plan.to_owned()),
        metered: Some(metered),
        version: Some("1.2.3".to_owned()),
        ..Default::default()
    }
}

fn panel(kind: &str) -> SidebarProviderPanel {
    SidebarProviderPanel {
        account: Default::default(),
        kind: kind.to_owned(),
        account_scope: ProviderAccountScope::KindWide,
        entitlement: Default::default(),
        account_key: None,
        product_name: rimz::agents::spec_by_kind(kind)
            .unwrap()
            .display_name
            .to_owned(),
        art: Vec::new(),
        art_tints: Vec::new(),
        color: 0,
        color_rgb: None,
        color_role: None,
        version: Some("1.2.3".to_owned()),
        plan: Some("Claude Max".to_owned()),
        metered: true,
        remote_control: RemoteControlBadge::Hidden,
        active_sessions: 0,
        spending: None,
        day_budget: None,
        extra_credits: None,
        reset_credits: None,
        redeem_forecast: None,
        window_placeholders: Vec::new(),
        windows: Vec::new(),
    }
}

#[test]
fn unlimited_credits_render_in_overview_and_detail() {
    let mut provider = panel("codex");
    provider.extra_credits = Some(ExtraCredits::Unlimited);
    let reports = group("codex", vec![("default", provider)]);
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    let text = overview(&reports, now, &BTreeMap::new(), false);
    assert!(text.contains('∞'), "{text}");
    let mut out = anstream::StripStream::new(Vec::new());
    write_pretty(&mut out, &reports, now, &TimeZone::UTC).unwrap();
    let text = String::from_utf8(out.into_inner()).unwrap();
    assert!(text.contains("unlimited"), "{text}");
}

#[test]
fn detail_includes_the_account_home_without_a_pace_legend() {
    let config: MachineConfig =
        toml::from_str("[accounts.claude.work]\nhome = '/accounts/work'\n").unwrap();
    let catalog = LoginCatalog::from_config(&config.accounts).unwrap();
    let ambient = BTreeMap::from([
        ("HOME".to_owned(), "/home/u".to_owned()),
        ("CLAUDE_CONFIG_DIR".to_owned(), "/accounts/work".to_owned()),
    ]);
    let reports = account_rows(
        &config.accounts,
        &catalog,
        &AccountStanding::machine_only(&config),
        &ambient,
        None,
        &BTreeMap::new(),
        &BTreeMap::new(),
    );
    let reports: Vec<_> = reports
        .into_iter()
        .filter(|row| row.kind.as_str() == "claude")
        .collect();
    assert_eq!(
        reports[0].home.as_deref(),
        Some(std::path::Path::new("/home/u/.claude"))
    );
    assert_eq!(
        reports[1].home.as_deref(),
        Some(std::path::Path::new("/accounts/work"))
    );
    let mut out = anstream::StripStream::new(Vec::new());
    super::write_pretty(
        &mut out,
        &reports,
        Timestamp::UNIX_EPOCH,
        &TimeZone::UTC,
        &BTreeMap::new(),
        &multiple_account_kinds(&reports),
    )
    .unwrap();
    let text = String::from_utf8(out.into_inner()).unwrap();
    assert!(text.contains("home"), "{text}");
    assert!(text.contains("/accounts/work"), "{text}");
    assert!(text.contains("/home/u/.claude"), "{text}");
    assert!(!text.contains("pace:"), "{text}");
}

#[test]
fn lapsed_provider_report_does_not_restore_the_cached_plan() {
    let mut value = serde_json::to_value(panel("claude")).unwrap();
    value["plan"] = serde_json::Value::Null;
    value["entitlement"] = serde_json::json!({"lapsed":{"since_ms":1_700_000_000_000_u64}});
    let reports = group(
        "claude",
        vec![("default", serde_json::from_value(value).unwrap())],
    );
    assert_eq!(reports[0].plan_label, None);
    assert_eq!(
        serde_json::to_value(&reports[0]).unwrap()["entitlement"],
        serde_json::json!({"lapsed":{"since_ms":1_700_000_000_000_u64}})
    );
    let text = overview(
        &reports,
        Timestamp::from_second(1_700_000_000).unwrap(),
        &BTreeMap::new(),
        true,
    );
    assert!(text.contains("lapsed"));
    assert!(!text.contains("Claude Pro"));
    insta::assert_snapshot!(text);

    let mut rows = merged(&reports);
    rows[0].installed = false;
    let mut out = anstream::StripStream::new(Vec::new());
    super::write_pretty(
        &mut out,
        &rows,
        Timestamp::UNIX_EPOCH,
        &TimeZone::UTC,
        &BTreeMap::new(),
        &multiple_account_kinds(&rows),
    )
    .unwrap();
    let text = String::from_utf8(out.into_inner()).unwrap();
    assert!(
        text.lines().next().unwrap().contains("not installed"),
        "{text}"
    );
}

fn account_fixture() -> AccountsCache {
    AccountsCache {
        logins: BTreeMap::from([
            (
                rimz::ids::LoginKey::default_for(rimz::ids::AgentKind::new_unchecked("claude")),
                record(1_000, true, Some(account("max", true))),
            ),
            (
                rimz::ids::LoginKey::default_for(rimz::ids::AgentKind::new_unchecked("codex")),
                record(2_000, true, None),
            ),
            (
                rimz::ids::LoginKey::default_for(rimz::ids::AgentKind::new_unchecked("copilot")),
                record(
                    3_000,
                    false,
                    Some(AgentAccount {
                        account_id: Some("octocat".to_owned()),
                        ..Default::default()
                    }),
                ),
            ),
            (
                rimz::ids::LoginKey::default_for(rimz::ids::AgentKind::new_unchecked("pi")),
                record(4_000, true, Some(account("openai-oauth", false))),
            ),
        ]),
    }
}

fn default_logins() -> Vec<ProviderLogin> {
    rimz::agents::known_kinds()
        .map(|kind| ProviderLogin::default_for(AgentKind::new_unchecked(kind)))
        .collect()
}

fn default_panels(panels: Vec<SidebarProviderPanel>) -> BTreeMap<LoginKey, SidebarProviderPanel> {
    panels
        .into_iter()
        .map(|panel| {
            (
                LoginKey::default_for(AgentKind::new_unchecked(&panel.kind)),
                panel,
            )
        })
        .collect()
}

#[test]
fn report_assembly_covers_auth_states_raw_accounts_and_filters() {
    let accounts = account_fixture();
    let mut spending = ProviderSpendingCache::default();
    spending.spending.by_provider.insert(
        "qwen".to_owned(),
        SpendTally {
            year: SpendWindow {
                usd: 1.0,
                sessions: 1,
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let reports = assemble_reports(
        &default_logins(),
        &accounts,
        default_panels(vec![panel("claude")]),
        &spending,
        None,
    );

    assert_eq!(
        reports
            .iter()
            .map(|report| report.kind.as_str())
            .collect::<Vec<_>>(),
        rimz::agents::known_kinds().collect::<Vec<_>>()
    );
    assert_eq!(reports[0].status, ProviderStatus::LoggedIn);
    assert!(reports[0].metered.is_some_and(|metered| metered));
    let copilot = reports
        .iter()
        .find(|report| report.kind == "copilot")
        .unwrap();
    assert_eq!(copilot.status, ProviderStatus::Unavailable);
    assert_eq!(copilot.account_id.as_deref(), Some("octocat"));
    let pi = reports.iter().find(|report| report.kind == "pi").unwrap();
    assert_eq!(pi.status, ProviderStatus::LoggedIn);
    assert_eq!(pi.plan.as_deref(), Some("openai-oauth"));
    assert_eq!(pi.plan_label.as_deref(), Some("Openai Oauth"));
    assert_eq!(pi.metered, Some(false));

    let filtered = assemble_reports(
        &default_logins(),
        &accounts,
        BTreeMap::new(),
        &spending,
        Some("pi"),
    );
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].kind, "pi");
    let logged_out = assemble_reports(
        &default_logins(),
        &accounts,
        BTreeMap::new(),
        &spending,
        Some("codex"),
    );
    assert_eq!(logged_out.len(), 1);
    assert_eq!(logged_out[0].status, ProviderStatus::LoggedOut);
}

fn week(usd: f64) -> SpendTally {
    SpendTally {
        week: SpendWindow {
            usd,
            sessions: 1,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn machine_using(kind: &str, name: &str) -> MachineConfig {
    MachineConfig {
        accounts: toml::from_str(&format!(
            "[{kind}.{name}]\nhome = \"/accounts/{name}\"\n[use]\n{kind} = \"{name}\"\n"
        ))
        .unwrap(),
        ..Default::default()
    }
}

#[test]
fn undeclared_or_unread_selections_mark_no_report_active() {
    let logins = vec![ProviderLogin::default_for(AgentKind::new_unchecked(
        "claude",
    ))];
    let mut machine = MachineConfig::default();
    machine
        .accounts
        .use_accounts
        .insert(AgentKind::new_unchecked("claude"), "gone".parse().unwrap());
    for standing in [
        AccountStanding::machine_only(&machine),
        AccountStanding::unread(&MachineConfig::default()),
    ] {
        let mut reports = assemble_reports(
            &logins,
            &AccountsCache::default(),
            BTreeMap::new(),
            &ProviderSpendingCache::default(),
            None,
        );
        mark_standing(&mut reports, &standing);
        assert!(reports.iter().all(|report| !report.active), "{standing:?}");
    }
}

#[test]
fn named_accounts_carry_their_own_spend_and_default_never_reads_kind_wide() {
    let kind = AgentKind::new_unchecked("claude");
    let logins = vec![
        ProviderLogin::default_for(kind.clone()),
        ProviderLogin::named(kind, "work".parse().unwrap(), "/accounts/work".into()).unwrap(),
    ];
    let accounts = AccountsCache {
        logins: logins
            .iter()
            .map(|login| (login.key(), record(1_000, true, Some(account("max", true)))))
            .collect(),
    };
    let mut named_panel = panel("claude");
    named_panel.product_name = "Claude · work".to_owned();
    named_panel.day_budget = Some(DailyBudgetView {
        cap_usd: 20.0,
        spend_usd: 3.0,
        parked: false,
    });
    let panels = BTreeMap::from([
        (logins[0].key(), panel("claude")),
        (logins[1].key(), named_panel.clone()),
    ]);
    let mut spending = ProviderSpendingCache::default();
    spending
        .spending
        .by_provider
        .insert("claude".to_owned(), week(99.0));
    spending
        .spending
        .by_login
        .insert(logins[1].key(), week(12.0));
    let mut reports = assemble_reports(&logins, &accounts, panels, &spending, Some("claude"));
    assert_eq!(
        reports
            .iter()
            .map(|report| (report.kind.as_str(), report.account.name.as_str()))
            .collect::<Vec<_>>(),
        [("claude", "default"), ("claude", "work")]
    );
    assert_eq!(reports[0].spending, None, "default never reads by_provider");
    assert_eq!(
        reports[1].spending.as_ref().map(|spend| &spend.week),
        Some(&week(12.0).week)
    );
    assert_eq!(reports[1].day_budget, named_panel.day_budget);

    mark_standing(
        &mut reports,
        &AccountStanding::machine_only(&machine_using("claude", "work")),
    );
    assert!(!reports[0].active && reports[1].active);
    let json = serde_json::to_value(&reports).unwrap();
    assert_eq!(json[0]["default_for"], serde_json::json!([]));
    assert_eq!(json[1]["default_for"], serde_json::json!(["new_rooms"]));
    assert_eq!(json[1]["active"], true);

    let mut out = anstream::StripStream::new(Vec::new());
    super::write_pretty(
        &mut out,
        &merged(&reports),
        Timestamp::from_second(1).unwrap(),
        &TimeZone::UTC,
        &BTreeMap::from([(AgentKind::new_unchecked("claude"), Deciding::Machine)]),
        &multiple_account_kinds(&merged(&reports)),
    )
    .unwrap();
    let pretty = String::from_utf8(out.into_inner()).unwrap();
    assert!(
        pretty.contains("● next launch from here (machine default)"),
        "{pretty}"
    );
    insta::assert_snapshot!("provider_named_detail", pretty);
    let unprobed = assemble_reports(
        &logins,
        &AccountsCache::default(),
        BTreeMap::new(),
        &ProviderSpendingCache::default(),
        None,
    );
    assert_eq!(
        unprobed
            .iter()
            .map(|report| report.account.name.as_str())
            .collect::<Vec<_>>(),
        ["default", "work"],
        "a kind with named accounts always reports its default"
    );
}

fn window(minutes: u32, used: u8, resets_in: Option<i64>, now: Timestamp) -> RateLimitWindow {
    RateLimitWindow {
        used_percentage: Some(used),
        resets_at: resets_in.map(|secs| now + SignedDuration::from_secs(secs)),
        duration_mins: Some(minutes),
        observed_at: Some(now),
        ..Default::default()
    }
}

fn overview(
    reports: &[ProviderReport],
    now: Timestamp,
    deciding: &BTreeMap<String, Deciding>,
    all: bool,
) -> String {
    let mut out = anstream::StripStream::new(Vec::new());
    write_overview(
        &mut out,
        &merged(reports),
        now,
        &TimeZone::UTC,
        &deciding
            .iter()
            .map(|(kind, source)| (AgentKind::new_unchecked(kind), *source))
            .collect(),
        all,
    )
    .unwrap();
    String::from_utf8(out.into_inner()).unwrap()
}

/// One kind's logged-in accounts, `default` first, each with its panel.
fn group(kind: &str, rows: Vec<(&str, SidebarProviderPanel)>) -> Vec<ProviderReport> {
    let kind = AgentKind::new_unchecked(kind);
    let (logins, panels): (Vec<_>, Vec<_>) = rows
        .into_iter()
        .map(|(name, panel)| {
            let name: LoginName = name.parse().unwrap();
            let login = if name.is_default() {
                ProviderLogin::default_for(kind.clone())
            } else {
                let home = format!("/accounts/{name}").into();
                ProviderLogin::named(kind.clone(), name, home).unwrap()
            };
            (login, panel)
        })
        .unzip();
    let accounts = AccountsCache {
        logins: logins
            .iter()
            .map(|login| (login.key(), record(1_000, true, Some(account("pro", true)))))
            .collect(),
    };
    let panels = logins.iter().map(ProviderLogin::key).zip(panels).collect();
    assemble_reports(
        &logins,
        &accounts,
        panels,
        &ProviderSpendingCache::default(),
        None,
    )
}

/// A table line's cells: columns are separated by two or more spaces.
fn cells(line: &str) -> Vec<&str> {
    line.split("  ")
        .map(str::trim)
        .filter(|cell| !cell.is_empty())
        .collect()
}

#[test]
fn overview_keeps_account_choices_and_collects_single_logins() {
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    let claude = AgentKind::new_unchecked("claude");
    let named = |name: &str| {
        ProviderLogin::named(
            claude.clone(),
            name.parse().unwrap(),
            format!("/accounts/{name}").into(),
        )
        .unwrap()
    };
    let logins = vec![
        ProviderLogin::default_for(claude.clone()),
        named("spare"),
        named("work"),
        ProviderLogin::default_for(AgentKind::new_unchecked("pi")),
    ];
    let accounts = AccountsCache {
        logins: BTreeMap::from([
            (
                logins[0].key(),
                record(1_000, true, Some(account("max", true))),
            ),
            (logins[1].key(), record(1_000, true, None)),
            (
                logins[2].key(),
                record(1_000, true, Some(account("max", true))),
            ),
            (
                logins[3].key(),
                record(1_000, true, Some(account("openai-oauth", false))),
            ),
        ]),
    };
    let mut default_panel = panel("claude");
    default_panel.version = Some("2.1.274".to_owned());
    default_panel.windows = vec![
        window(5 * 60, 62, Some(83 * 60), now),
        window(7 * 24 * 60, 14, Some(4 * 86_400 + 2 * 3_600), now),
    ];
    default_panel.extra_credits = Some(ExtraCredits::known(Some(12.4), Some(37.6), Some(50.0)));
    default_panel.reset_credits = Some(ResetCredits {
        count: 2,
        soonest_expiry: Some(now + SignedDuration::from_secs(3 * 86_400)),
        expiries: Vec::new(),
        effect: rimz::agents::RedeemEffect::KeepsSchedule,
    });
    let mut work_panel = panel("claude");
    work_panel.version = None;
    work_panel.windows = vec![
        window(7 * 24 * 60, 30, Some(86_400), now),
        RateLimitWindow {
            scope: Some(rimz::agents::RateLimitWindowScope {
                id: "fable".to_owned(),
                label: "Fable".to_owned(),
            }),
            ..window(7 * 24 * 60, 31, None, now)
        },
    ];
    work_panel.extra_credits = Some(ExtraCredits::Disabled);
    let panels = BTreeMap::from([
        (logins[0].key(), default_panel),
        (logins[2].key(), work_panel),
    ]);
    let mut spending = ProviderSpendingCache::default();
    spending
        .spending
        .by_login
        .insert(logins[0].key(), week(2_742.9));
    spending
        .spending
        .by_login
        .insert(logins[2].key(), week(12.0));
    let mut reports = assemble_reports(&logins, &accounts, panels, &spending, None);
    mark_standing(
        &mut reports,
        &AccountStanding::machine_only(&machine_using("claude", "work")),
    );
    let text = overview(&reports, now, &BTreeMap::new(), false);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[0], "Claude · v1.2.3 · shared pool $2,742.90 spent in 7d",
        "{text}"
    );
    assert_eq!(
        cells(lines[1]),
        [
            "NAME",
            "PLAN",
            "5h LEFT",
            "7d LEFT",
            "Fable LEFT",
            "EXTRA LEFT",
            "RESET CREDITS",
            "AGENTS",
            "SPEND 7d",
            "READING"
        ],
        "{text}"
    );
    assert!(lines[2].starts_with("   default"), "{text}");
    assert!(
        lines[3].contains("spare") && lines[3].contains("logged out"),
        "{text}"
    );
    assert!(lines[4].starts_with("●  work"), "{text}");
    assert_eq!(
        cells(lines[4])[5],
        "69%",
        "a sub-cap with no reset reads its own remainder: {text}"
    );
    assert!(
        lines.iter().all(|line| line.trim_end() == *line),
        "{text:?}"
    );
    assert!(text.contains("Other providers\n"), "{text}");
    assert!(
        text.lines().any(|line| line.trim_start().starts_with("Pi ")
            && line.contains("Openai Oauth")
            && line.ends_with("v1.2.3")),
        "{text}"
    );
    assert!(text.contains("default"), "{text}");
    assert_eq!(
        text.matches("● next launch from here (this room)").count(),
        1,
        "{text}"
    );
    assert!(!text.contains('○'), "{text}");
    insta::assert_snapshot!("provider_overview", text);
}

#[test]
fn overview_separates_readings_healthy_logins_and_unchecked_providers() {
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    let mut reading = panel("copilot");
    reading.windows = vec![window(60, 30, Some(1_800), now)];
    let mut reports = group("copilot", vec![("default", reading)]);
    reports[0].active = true;
    let mut other_panel = panel("opencode");
    other_panel.spending = Some(std::sync::Arc::new(week(1.25)));
    let mut other = group("opencode", vec![("default", other_panel)]);
    other[0].sub_provider = Some("openai".to_owned());
    other[0].agents = Some(2);
    reports.extend(other);
    for kind in [
        "grok",
        "qwen",
        "antigravity",
        "pi",
        "claude",
        "muse",
        "kiro",
        "droid",
    ] {
        let mut rows = group(kind, vec![("default", panel(kind))]);
        match kind {
            "grok" => {
                rows[0].plan_label = None;
                rows[0].version = None;
            }
            "qwen" | "antigravity" => {
                rows[0].status = ProviderStatus::Unavailable;
                rows[0].probe_error = Some("account source unavailable".to_owned());
            }
            "pi" => rows[0].status = ProviderStatus::Unavailable,
            "claude" => {
                rows[0].status = ProviderStatus::Unavailable;
                rows[0].probed_at = None;
            }
            "droid" => rows[0].installed = false,
            _ => {}
        }
        reports.extend(rows);
    }
    let text = overview(&reports, now, &BTreeMap::new(), false);
    assert!(text.contains("Other providers"), "{text}");
    assert!(
        text.lines().any(|line| line.starts_with("   NAME")),
        "{text}"
    );
    let header = text.lines().find(|line| line.contains("PROVIDER")).unwrap();
    assert_eq!(
        cells(header),
        [
            "PROVIDER", "LOGIN", "READING", "AGENTS", "SPEND 7d", "VERSION"
        ]
    );
    assert!(text.contains("never read"), "{text}");
    assert!(text.contains("$1.25"), "{text}");
    assert!(
        text.contains("antigravity, qwen (login read failed: account source unavailable)"),
        "{text}"
    );
    let unchecked = text
        .lines()
        .find(|line| line.starts_with("Not checked,"))
        .unwrap();
    assert!(
        unchecked.ends_with(
            "pi (login read failed) · claude (not probed yet) · kiro, muse (no login check)"
        ),
        "{text}"
    );
    assert!(!text.contains("droid"), "{text}");
    assert!(!text.contains('●') && !text.contains('○'), "{text}");
    assert!(
        !text.contains("\n\n\n") && !text.ends_with("\n\n"),
        "{text:?}"
    );
    let other_section = text.split("Other providers\n").nth(1).unwrap();
    assert!(!other_section.contains("\n\nNot checked"), "{text}");
    let grok = other_section
        .lines()
        .find(|line| line.trim_start().starts_with("Grok "))
        .unwrap();
    assert!(grok.contains("logged in") && grok.ends_with('–'), "{text}");
    let all = overview(&reports, now, &BTreeMap::new(), true);
    let other = all.split("Other providers\n").nth(1).expect(&all);
    for state in [
        "probe failed",
        "not probed",
        "no login check",
        "not installed",
    ] {
        assert!(other.contains(state), "{all}");
    }
    assert!(other.contains("Droid"), "{all}");
    assert!(!all.contains("Not checked"), "{all}");
}

#[test]
fn only_multiple_accounts_contribute_markers_and_the_legend() {
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    let mut reports = group(
        "codex",
        vec![("default", panel("codex")), ("work", panel("codex"))],
    );
    reports[0].active = true;
    let mut single = group("copilot", vec![("default", panel("copilot"))]);
    single[0].active = true;
    single[0].windows = vec![window(60, 30, Some(1_800), now)];
    reports.extend(single.clone());
    let deciding = BTreeMap::from([
        ("codex".to_owned(), Deciding::Room),
        ("copilot".to_owned(), Deciding::Machine),
    ]);
    let text = overview(&reports, now, &deciding, true);
    assert!(
        text.contains("● next launch from here (this room)"),
        "{text}"
    );
    assert_eq!(text.matches('●').count(), 2, "{text}");
    let mut out = anstream::StripStream::new(Vec::new());
    write_pretty(&mut out, &single, now, &TimeZone::UTC).unwrap();
    let text = String::from_utf8(out.into_inner()).unwrap();
    assert!(!text.contains('●') && !text.contains('○'), "{text}");
    assert!(!text.contains("next launch"), "{text}");
}

#[test]
fn a_single_login_earns_a_table_for_credits_but_not_for_logged_out_cache() {
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    for reset in [false, true] {
        let mut reports = group("cursor", vec![("default", panel("cursor"))]);
        if reset {
            reports[0].reset_credits = Some(ResetCredits {
                count: 1,
                soonest_expiry: None,
                expiries: Vec::new(),
                effect: rimz::agents::RedeemEffect::KeepsSchedule,
            });
        } else {
            reports[0].extra_credits = Some(ExtraCredits::Disabled);
        }
        let text = overview(&reports, now, &BTreeMap::new(), false);
        assert!(
            text.lines().any(|line| line.starts_with("   NAME")),
            "{text}"
        );
        assert!(!text.contains("Other providers"), "{text}");
        reports[0].status = ProviderStatus::LoggedOut;
        let text = overview(&reports, now, &BTreeMap::new(), false);
        assert!(
            !text.contains("NAME") && !text.contains("Other providers"),
            "{text}"
        );
    }
}

#[test]
fn rounded_zero_heading_spend_has_no_tally_or_pool_members() {
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    for usd in [0.0, 0.001] {
        let mut default = panel("claude");
        default.spending = Some(std::sync::Arc::new(week(usd)));
        let mut reports = group(
            "claude",
            vec![("default", default), ("work", panel("claude"))],
        );
        reports[1].pool = reports[0].pool.clone();
        for standalone in [false, true] {
            if standalone {
                reports[1].pool = reports[1].account.clone();
            }
            let text = overview(&reports, now, &BTreeMap::new(), false);
            assert!(
                !text.contains("spent in 7d") && !text.contains("shared by"),
                "{text}"
            );
        }
    }
}

#[test]
fn needs_you_orders_structured_repairs_and_keeps_detail_problems() {
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    let row = |kind: &str, name: &str| merged(&group(kind, vec![(name, panel(kind))])).remove(0);
    let mut kimi = row("kimi", "default");
    kimi.read(
        Some(&LoginReading {
            status: ProviderStatus::LoggedOut,
            ..super::logged_in(Vec::new())
        }),
        || "kimi".to_owned(),
    );
    let mut pi = row("pi", "default");
    pi.read(
        Some(&LoginReading {
            entitlement: Entitlement::Lapsed { since_ms: 100 },
            ..super::logged_in(Vec::new())
        }),
        String::new,
    );
    let mut rows = vec![kimi, pi];
    for (kind, name, error) in [
        (
            "hermes",
            "default",
            BirthLoginErr::HooksUntrusted {
                kind: AgentKind::new_unchecked("hermes"),
                name: LoginName::default_login(),
                home: Box::new("/home/u/.hermes".into()),
                env_key: None,
                hooks: "rimz-start, rimz-stop".into(),
                fix: "launch hermes in a terminal and answer y to each RimZ hook prompt".into(),
                choice: None,
            },
        ),
        (
            "codex",
            "gone",
            BirthLoginErr::MachineUnknown {
                kind: AgentKind::new_unchecked("codex"),
                name: "gone".parse().unwrap(),
                configured: Vec::new(),
                path: "/home/u/.rimz/config.toml".into(),
            },
        ),
        (
            "codex",
            "work",
            BirthLoginErr::MissingHome {
                kind: AgentKind::new_unchecked("codex"),
                name: "work".parse().unwrap(),
                home: "/accounts/work".into(),
            },
        ),
        (
            "claude",
            "default",
            BirthLoginErr::HooksMissing {
                kind: AgentKind::new_unchecked("claude"),
                name: LoginName::default_login(),
                home: "/home/u/.claude".into(),
            },
        ),
        (
            "claude",
            "off",
            BirthLoginErr::HooksDisabled {
                kind: AgentKind::new_unchecked("claude"),
                name: "off".parse().unwrap(),
                reason: "all hooks disabled".into(),
                fix: "enable hooks".into(),
            },
        ),
        (
            "claude",
            "trust",
            BirthLoginErr::HooksUntrusted {
                kind: AgentKind::new_unchecked("claude"),
                name: "trust".parse().unwrap(),
                home: Box::new("/accounts/trust".into()),
                env_key: Some("CLAUDE_CONFIG_DIR"),
                hooks: "start-hook".into(),
                fix: "trust hooks".into(),
                choice: None,
            },
        ),
    ] {
        let mut row = row(kind, name);
        row.status = AccountStatus::of(Some(&error));
        row.problem = Some(error.to_string());
        row.setup_error = Some(error);
        rows.push(row);
    }
    let mut absent = row("omp", "default");
    absent.read(
        Some(&LoginReading {
            status: ProviderStatus::LoggedOut,
            ..super::logged_in(Vec::new())
        }),
        || "omp".to_owned(),
    );
    absent.installed = false;
    rows.push(absent);
    let mut out = anstream::StripStream::new(Vec::new());
    write_overview(
        &mut out,
        &rows,
        now,
        &TimeZone::UTC,
        &BTreeMap::new(),
        false,
    )
    .unwrap();
    let text = String::from_utf8(out.into_inner()).unwrap();
    assert!(text.contains("Needs you\n"), "{text}");
    let needs = text.split("Needs you\n").nth(1).expect(&text);
    let lines = needs.lines().collect::<Vec<_>>();
    let position = |kind| {
        lines
            .iter()
            .position(|line| line.trim_start().starts_with(kind))
            .unwrap()
    };
    assert!(
        position("hermes") < position("pi") && position("pi") < position("kimi"),
        "{text}"
    );
    let hermes = lines[position("hermes")];
    assert_eq!(
        cells(hermes),
        [
            "hermes",
            "hooks untrusted",
            "launch hermes in a terminal and answer y to each RimZ hook prompt"
        ]
    );
    assert!(!hermes.contains("rimz-start"), "{text}");
    assert!(needs.contains("run rimz accounts add codex work"), "{text}");
    assert!(needs.contains("codex@work"), "{text}");
    assert!(needs.contains("run rimz hooks install claude"), "{text}");
    assert!(needs.contains("enable hooks"), "{text}");
    assert!(
        needs.contains("trust hooks, started as `CLAUDE_CONFIG_DIR=/accounts/trust claude`"),
        "{text}"
    );
    assert!(needs.contains("no Pi access"), "{text}");
    assert!(needs.contains("log in once: kimi"), "{text}");
    let unavailable = rows
        .iter()
        .find(|row| row.account.to_string() == "codex@gone")
        .unwrap();
    assert!(
        needs.contains(unavailable.problem.as_deref().unwrap()),
        "{text}"
    );
    assert!(
        !needs
            .lines()
            .any(|line| line.trim_start().starts_with("omp ")),
        "{text}"
    );
    assert!(!text.ends_with("\n\n"), "{text:?}");
    let hermes = rows
        .iter()
        .find(|row| row.kind.as_str() == "hermes")
        .unwrap();
    let mut out = anstream::StripStream::new(Vec::new());
    super::write_pretty(
        &mut out,
        std::slice::from_ref(hermes),
        now,
        &TimeZone::UTC,
        &BTreeMap::new(),
        &BTreeSet::new(),
    )
    .unwrap();
    let text = String::from_utf8(out.into_inner()).unwrap();
    assert!(text.contains("rimz-start, rimz-stop"), "{text}");
}

#[test]
fn table_window_cells_show_what_is_left() {
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    let mut default_panel = panel("codex");
    default_panel.windows = vec![
        window(5 * 60, 45, Some(70 * 60), now),
        window(7 * 24 * 60, 1, Some(7 * 86_400), now),
        window(30 * 24 * 60, 99, None, now),
    ];
    let mut team_panel = panel("codex");
    team_panel.windows = vec![
        RateLimitWindow {
            used_percentage: None,
            ..window(5 * 60, 0, None, now)
        },
        RateLimitWindow {
            lifted: true,
            ..window(7 * 24 * 60, 0, None, now)
        },
        RateLimitWindow {
            duration_mins: None,
            scope: Some(rimz::agents::RateLimitWindowScope {
                id: "spark".to_owned(),
                label: "Spark".to_owned(),
            }),
            ..window(0, 70, None, now)
        },
    ];
    let mut reports = group(
        "codex",
        vec![("default", default_panel), ("team", team_panel)],
    );
    reports[0].active = true;

    let text = overview(&reports, now, &BTreeMap::new(), false);
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines[2].contains("default") && lines[2].contains("55% · 1h10m · 0.6x"),
        "{text}"
    );
    assert!(lines[2].contains("100% · ready"), "{text}");
    assert!(
        lines[3].contains("team") && lines[3].contains("no limit"),
        "{text}"
    );
    assert!(text.contains("AGENTS") && text.contains("shared"), "{text}");

    let mut raw = Vec::new();
    write_overview(
        &mut raw,
        &merged(&reports),
        now,
        &TimeZone::UTC,
        &BTreeMap::new(),
        false,
    )
    .unwrap();
    let raw = String::from_utf8(raw).unwrap();
    let painted = |style: anstyle::Style, text: &str| {
        format!("{}{text}{}", style.render(), style.render_reset())
    };
    assert!(
        raw.contains(&format!(
            "{} {}",
            painted(render::palette::budget(55), "55%"),
            painted(render::palette::body(), "· 1h10m")
        )),
        "only the percent takes the budget tone: {raw:?}"
    );
    assert_ne!(
        render::palette::budget(55),
        render::palette::budget(5),
        "the tone follows what is left"
    );
}

#[test]
fn new_rooms_default_is_marked_unless_the_row_is_the_active_one() {
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    let pretty = |reports: &[ProviderReport], now| {
        let mut out = anstream::StripStream::new(Vec::new());
        write_pretty(&mut out, reports, now, &TimeZone::UTC).unwrap();
        String::from_utf8(out.into_inner()).unwrap()
    };
    let mut reports = group(
        "codex",
        vec![("default", panel("codex")), ("team", panel("codex"))],
    );
    let marks = |reports: &[ProviderReport]| -> Vec<String> {
        overview(reports, now, &BTreeMap::new(), false)
            .lines()
            .skip(2)
            .take(reports.len())
            .map(|line| line.chars().take(1).collect())
            .collect()
    };

    mark_standing(
        &mut reports,
        &AccountStanding::unread(&machine_using("codex", "team")),
    );
    assert_eq!(marks(&reports), [" ", "○"], "unread layers mark no launch");
    for text in [
        overview(&reports, now, &BTreeMap::new(), false),
        pretty(&reports, now),
    ] {
        assert!(text.contains("○ new rooms"), "{text}");
        assert!(
            !text.contains("● next launch from here (this room)"),
            "{text}"
        );
    }
    reports[0].active = true;
    assert_eq!(marks(&reports), ["●", "○"]);
    for text in [
        overview(&reports, now, &BTreeMap::new(), false),
        pretty(&reports, now),
    ] {
        assert!(
            text.contains("● next launch from here (this room)   ○ new rooms"),
            "{text}"
        );
    }
    mark_standing(
        &mut reports,
        &AccountStanding::machine_only(&machine_using("codex", "team")),
    );
    assert_eq!(marks(&reports), [" ", "●"], "one row never takes both");
    for text in [
        overview(&reports, now, &BTreeMap::new(), false),
        pretty(&reports, now),
    ] {
        assert!(
            text.contains("● next launch from here (this room)"),
            "{text}"
        );
        assert!(!text.contains('○'), "{text}");
    }
}

#[test]
fn reset_cell_marks_its_countdown_as_an_expiry() {
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    let banked = |count, soonest_expiry: Option<Timestamp>| SidebarProviderPanel {
        reset_credits: Some(ResetCredits {
            count,
            soonest_expiry,
            expiries: Vec::new(),
            effect: rimz::agents::RedeemEffect::KeepsSchedule,
        }),
        ..panel("codex")
    };
    let reports = group(
        "codex",
        vec![
            (
                "default",
                banked(2, Some(now + SignedDuration::from_secs(3 * 86_400))),
            ),
            ("due", banked(4, Some(now))),
            ("bare", banked(1, None)),
            ("none", banked(0, None)),
            ("unknown", panel("codex")),
        ],
    );
    let text = overview(&reports, now, &BTreeMap::new(), false);
    assert_eq!(
        text.lines()
            .skip(1)
            .take(reports.len() + 1)
            .map(|line| cells(line)[2])
            .collect::<Vec<_>>(),
        ["RESET CREDITS", "2 · exp 3d00h", "4 · due", "1", "-", "–"],
        "{text}"
    );
}

#[test]
fn block_reads_a_reset_less_sub_cap_at_its_own_remainder() {
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    let sub_cap = |used| RateLimitWindow {
        scope: Some(rimz::agents::RateLimitWindowScope {
            id: "fable".to_owned(),
            label: "Fable".to_owned(),
        }),
        ..window(7 * 24 * 60, used, None, now)
    };
    assert_eq!(
        block_window(&sub_cap(31), now).as_deref(),
        Some("69% left · resets –")
    );
}

#[test]
fn unknown_kind_error_lists_registered_providers() {
    let error = validate_kind(Some("wat")).unwrap_err().to_string();
    assert!(error.contains("unknown provider kind `wat`"));
    assert!(error.contains("claude, codex"));
}

#[test]
fn provider_contract_names_pool_and_separates_today_from_headline() {
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    let (accounts, panel, spending) = protocol_fixture(now);
    let reports = assemble_reports(
        &default_logins(),
        &accounts,
        default_panels(vec![panel]),
        &spending,
        Some("claude"),
    );
    let json = serde_json::to_value(&reports).unwrap();
    assert_eq!(json[0]["account"], "claude@default");
    assert_eq!(json[0]["pool"], "claude@default");
    assert_eq!(json[0]["spending"]["today"], serde_json::Value::Null);
    assert!(json[0]["spending"]["headline"].is_object());
    assert_eq!(json[0]["spending"]["window"], "session");
    assert!(json[0].get("active_sessions").is_none());
    assert_eq!(json[0]["agents"], serde_json::Value::Null);
}

#[test]
fn unprobed_kinds_are_grouped_without_earning_own_tables() {
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    let reports = assemble_reports(
        &default_logins(),
        &AccountsCache::default(),
        BTreeMap::new(),
        &ProviderSpendingCache::default(),
        Some("kiro"),
    );
    assert_eq!(reports.len(), 1);
    let reports = assemble_reports(
        &default_logins(),
        &AccountsCache::default(),
        BTreeMap::new(),
        &ProviderSpendingCache::default(),
        None,
    );
    let text = overview(&reports, now, &BTreeMap::new(), false);
    assert!(text.starts_with("Not checked, launches proceed:"), "{text}");
    assert!(text.contains("kiro"), "{text}");
    assert!(!text.contains("NAME"), "{text}");
}

#[test]
fn logged_out_kind_with_spend_history_still_shows_its_login_fix() {
    let login = ProviderLogin::default_for(AgentKind::new_unchecked("claude"));
    let accounts = AccountsCache {
        logins: BTreeMap::from([(login.key(), record(1_000, true, None))]),
    };
    let mut spending = ProviderSpendingCache::default();
    spending.spending.by_login.insert(login.key(), week(123.0));
    let reports = assemble_reports(&[login], &accounts, BTreeMap::new(), &spending, None);
    assert_eq!(reports[0].spending.as_ref().unwrap().week.usd, 123.0);
    let mut rows = merged(&reports);
    rows[0].read(
        Some(&LoginReading {
            status: ProviderStatus::LoggedOut,
            ..super::logged_in(Vec::new())
        }),
        || "claude".to_owned(),
    );
    let text = super::text(&rows, &[], None);
    assert!(text.contains("Needs you\n"), "{text}");
    assert!(text.contains("log in once: claude"), "{text}");
    assert!(!text.contains("NAME"), "{text}");
    assert!(!text.contains("spent in 7d"), "{text}");
}

#[test]
fn low_account_prints_facts_without_advice() {
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    let with_usage = |used| SidebarProviderPanel {
        windows: vec![window(7 * 24 * 60, used, Some(86_400), now)],
        ..panel("codex")
    };
    let mut reports = group(
        "codex",
        vec![("default", with_usage(92)), ("team", with_usage(5))],
    );
    reports[0].active = true;
    let text = overview(
        &reports,
        now,
        &BTreeMap::from([("codex".to_owned(), Deciding::Room)]),
        true,
    );
    assert!(!text.contains("rimz accounts use"), "{text}");
    assert!(!text.contains("pace:"), "{text}");
    assert!(
        text.contains("● next launch from here (this room)"),
        "{text}"
    );
}

#[test]
fn shared_pool_spend_appears_once_with_standalone_spend_in_its_row() {
    let config = MachineConfig {
        accounts: toml::from_str("[claude.a]\nhome = '/accounts/a'\n[claude.b]\nhome = '/accounts/b'\n[claude.solo]\nhome = '/accounts/solo'\nhistory = 'standalone'\n").unwrap(),
        ..Default::default()
    };
    let catalog = LoginCatalog::from_config(&config.accounts).unwrap();
    let mut logins: Vec<_> = catalog
        .all()
        .filter(|login| login.kind().as_str() == "claude")
        .cloned()
        .collect();
    logins.sort_by_key(|login| (!login.name().is_default(), login.name().clone()));
    let accounts = AccountsCache {
        logins: logins
            .iter()
            .map(|login| (login.key(), record(1_000, true, Some(account("max", true)))))
            .collect(),
    };
    let mut spending = ProviderSpendingCache::default();
    spending
        .spending
        .by_login
        .insert("claude@default".parse().unwrap(), week(123.0));
    spending
        .spending
        .by_login
        .insert("claude@solo".parse().unwrap(), week(17.0));
    let reports = assemble_reports(&logins, &accounts, BTreeMap::new(), &spending, None);
    let text = overview(
        &reports,
        Timestamp::from_second(1_700_000_000).unwrap(),
        &BTreeMap::new(),
        false,
    );
    assert_eq!(text.matches("$123.00").count(), 1, "{text}");
    assert!(text.contains("shared pool $123.00 spent in 7d"), "{text}");
    assert!(
        text.lines()
            .any(|line| line.contains("solo") && line.contains("$17.00")),
        "{text}"
    );
    insta::assert_snapshot!("provider_shared_pool", text);
}

#[test]
fn missing_readings_follow_the_accounts_quota_surface() {
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    for (kind, provider, metered, expected) in [
        ("opencode", Some("openai"), true, true),
        ("copilot", None, true, true),
        ("pi", Some("openai"), true, false),
        ("pi", Some("openai-codex"), false, false),
        ("grok", None, true, false),
        ("cursor", None, true, false),
    ] {
        let mut reports = group(kind, vec![("default", panel(kind))]);
        reports[0].sub_provider = provider.map(str::to_owned);
        reports[0].metered = Some(metered);
        let text = overview(&reports, now, &BTreeMap::new(), false);
        assert_eq!(text.contains("READING"), expected, "{kind}: {text}");
        assert_eq!(text.contains("never read"), expected, "{kind}: {text}");
        let mut out = anstream::StripStream::new(Vec::new());
        write_pretty(&mut out, &reports, now, &TimeZone::UTC).unwrap();
        let text = String::from_utf8(out.into_inner()).unwrap();
        if metered {
            assert_eq!(text.contains("usage:"), expected, "{kind}: {text}");
        } else {
            assert!(text.contains("no limit"), "{text}");
            assert!(!text.contains("never read"), "{text}");
        }

        reports[0].windows.push(RateLimitWindow {
            observed_at: Some(now),
            ..Default::default()
        });
        let text = overview(&reports, now, &BTreeMap::new(), false);
        assert_eq!(
            text.contains("no reading since 2023-11-14"),
            metered,
            "{text}"
        );
    }
}

#[test]
fn failed_probe_and_missing_readings_have_reasons() {
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    let mut accounts = AccountsCache::default();
    let mut failed = record(1_700_000_000_000, false, None);
    failed.probe_error = Some(rimz::agents::account::AccountProbeError::TimedOut);
    accounts
        .logins
        .insert("claude@default".parse().unwrap(), failed);
    let reports = assemble_reports(
        &default_logins(),
        &accounts,
        BTreeMap::new(),
        &ProviderSpendingCache::default(),
        Some("claude"),
    );
    let text = overview(&reports, now, &BTreeMap::new(), true);
    assert!(text.contains("probe failed"), "{text}");
    assert!(text.contains("never read"), "{text}");
    assert_eq!(
        serde_json::to_value(&reports).unwrap()[0]["probe_error"],
        "timed out"
    );
}

#[test]
fn local_day_spend_uses_the_configured_timezone_and_agents_use_login_not_pool() {
    let now: Timestamp = "2026-10-15T02:00:00Z".parse().unwrap();
    let config: MachineConfig =
        toml::from_str("timezone = 'America/New_York'\n[sidebar]\nspend_window = '24h'\n[accounts.claude.work]\nhome = '/accounts/work'\n").unwrap();
    let key: LoginKey = "claude@default".parse().unwrap();
    let catalog = LoginCatalog::from_config(&config.accounts).unwrap();
    let logins: Vec<_> = catalog
        .all()
        .filter(|login| login.kind() == &key.kind)
        .cloned()
        .collect();
    let mut spending = ProviderSpendingCache::default();
    spending.spending.by_login.insert(
        key.clone(),
        SpendTally {
            headline: SpendWindow {
                usd: 90.0,
                ..Default::default()
            },
            ..week(123.0)
        },
    );
    spending.day_by_login.insert(
        key.clone(),
        SpendWindow {
            usd: 12.0,
            ..Default::default()
        },
    );
    let cutoff: Timestamp = "2026-10-14T04:00:00Z".parse().unwrap();
    spending.day_cutoff_secs = cutoff.as_second() as u64;
    let agents = BTreeMap::from([(key, 3), ("claude@work".parse().unwrap(), 7)]);
    let context = ReportContext {
        day_cutoff_secs: now
            .to_zoned(config.time_zone())
            .start_of_day()
            .ok()
            .map(|day| day.timestamp().as_second().max(0) as u64),
        config: &config,
        agents: Some(&agents),
        installed: &rimz::agents::known_kinds().map(str::to_owned).collect(),
        spend_step: &|_| SpendStep::Subscription,
    };
    let render = |spending: &ProviderSpendingCache| {
        serde_json::to_value(super::assemble_reports(
            &logins,
            &AccountsCache::default(),
            BTreeMap::new(),
            spending,
            None,
            &context,
        ))
        .unwrap()
    };
    let json = render(&spending);
    assert_eq!(json[0]["spending"]["today"]["usd"], 12.0);
    assert_eq!(json[0]["spending"]["headline"]["usd"], 90.0);
    assert_eq!(json[0]["spending"]["window"], "24h");
    assert_eq!(json[0]["agents"], 3);
    assert_eq!(json[1]["pool"], "claude@default");
    assert_eq!(json[1]["spending"]["today"]["usd"], 12.0);
    assert_eq!(json[1]["agents"], 7);
    spending.day_by_login.clear();
    let idle = render(&spending);
    assert_eq!(idle[0]["spending"]["today"]["usd"], 0.0);
    assert_eq!(idle[1]["spending"]["today"]["usd"], 0.0);
    assert_eq!(idle[0]["spending"]["headline"]["usd"], 90.0);
    spending.day_cutoff_secs -= 86_400;
    assert_eq!(
        render(&spending)[0]["spending"]["today"],
        serde_json::Value::Null
    );
}

#[test]
fn absent_binary_is_hidden_unless_all_lists_its_login_state() {
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    let logins = [ProviderLogin::default_for(AgentKind::new_unchecked("kiro"))];
    let accounts = AccountsCache {
        logins: BTreeMap::from([(logins[0].key(), record(1_000, true, None))]),
    };
    let installed = BTreeSet::new();
    let config = MachineConfig::default();
    let context = ReportContext {
        day_cutoff_secs: None,
        config: &config,
        agents: None,
        installed: &installed,
        spend_step: &|_| SpendStep::Subscription,
    };
    let reports = super::assemble_reports(
        &logins,
        &accounts,
        BTreeMap::new(),
        &ProviderSpendingCache::default(),
        None,
        &context,
    );
    let text = overview(&reports, now, &BTreeMap::new(), false);
    assert!(text.contains("No provider is set up here"), "{text}");
    assert!(!text.contains("kiro"), "{text}");
    let text = overview(&reports, now, &BTreeMap::new(), true);
    assert!(
        text.contains("Other providers")
            && text.contains("not installed")
            && !text.contains("NAME")
            && !text.contains("Not checked"),
        "{text}"
    );
    let reports = assemble_reports(
        &logins,
        &accounts,
        BTreeMap::new(),
        &ProviderSpendingCache::default(),
        None,
    );
    let text = overview(&reports, now, &BTreeMap::new(), false);
    assert!(
        text.contains("no login check") && !text.contains("logged out"),
        "{text}"
    );
    let reports = assemble_reports(
        &logins,
        &AccountsCache::default(),
        BTreeMap::new(),
        &ProviderSpendingCache::default(),
        None,
    );
    let text = overview(&reports, now, &BTreeMap::new(), false);
    assert!(text.contains("no login check"), "{text}");
}

#[test]
fn heading_prefers_active_version_and_detail_dates_missing_readings() {
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    let mut active = panel("claude");
    active.version = Some("9.9.9".to_owned());
    active.windows = vec![RateLimitWindow {
        observed_at: Some(now - SignedDuration::from_secs(60)),
        ..Default::default()
    }];
    let mut reports = group(
        "claude",
        vec![("default", panel("claude")), ("work", active)],
    );
    reports[1].active = true;
    let text = overview(&reports, now, &BTreeMap::new(), true);
    assert!(text.starts_with("Claude · v9.9.9"), "{text}");
    assert!(text.contains("no reading since 2023-11-14"), "{text}");
    let mut out = anstream::StripStream::new(Vec::new());
    write_pretty(&mut out, &reports, now, &TimeZone::UTC).unwrap();
    let text = String::from_utf8(out.into_inner()).unwrap();
    assert!(text.contains("probed:"), "{text}");
    assert!(text.contains("read 1m ago"), "{text}");
}

fn protocol_fixture(
    now: Timestamp,
) -> (AccountsCache, SidebarProviderPanel, ProviderSpendingCache) {
    let mut provider = panel("claude");
    provider.account_key = Some("must-not-leak".to_owned());
    provider.active_sessions = 2;
    provider.windows = vec![
        RateLimitWindow {
            used_percentage: Some(62),
            resets_at: Some(now + SignedDuration::from_secs(83 * 60)),
            duration_mins: Some(5 * 60),
            observed_at: Some(now),
            ..Default::default()
        },
        RateLimitWindow {
            used_percentage: Some(14),
            resets_at: Some(now + SignedDuration::from_secs(4 * 86_400 + 2 * 3_600)),
            duration_mins: Some(7 * 24 * 60),
            observed_at: Some(now),
            ..Default::default()
        },
    ];
    provider.extra_credits = Some(ExtraCredits::known(Some(12.4), None, Some(50.0)));
    provider.reset_credits = Some(ResetCredits {
        count: 2,
        soonest_expiry: Some(now + SignedDuration::from_secs(3 * 86_400)),
        expiries: vec![
            now + SignedDuration::from_secs(3 * 86_400),
            now + SignedDuration::from_secs(5 * 86_400),
        ],
        effect: rimz::agents::RedeemEffect::RestartsWindow,
    });
    provider.spending = Some(
        SpendTally {
            week: SpendWindow {
                usd: 31.2,
                tokens: 12_000,
                sessions: 3,
                ..Default::default()
            },
            month: SpendWindow {
                usd: 118.75,
                tokens: 48_000,
                sessions: 9,
                ..Default::default()
            },
            year: SpendWindow {
                usd: 400.0,
                tokens: 160_000,
                sessions: 20,
                ..Default::default()
            },
            ..Default::default()
        }
        .into(),
    );
    provider.day_budget = Some(DailyBudgetView {
        cap_usd: 25.0,
        spend_usd: 8.1,
        parked: false,
    });
    let accounts = AccountsCache {
        logins: BTreeMap::from([(
            rimz::ids::LoginKey::default_for(rimz::ids::AgentKind::new_unchecked("claude")),
            record(
                u64::try_from(now.as_millisecond()).unwrap(),
                true,
                Some(AgentAccount {
                    plan: Some("max".to_owned()),
                    account_id: Some("acct_123".to_owned()),
                    metered: Some(true),
                    version: Some("1.2.3".to_owned()),
                    ..Default::default()
                }),
            ),
        )]),
    };
    let mut spending = ProviderSpendingCache::default();
    spending.spending.by_provider.insert(
        "claude".to_owned(),
        provider.spending.as_deref().cloned().unwrap(),
    );
    (accounts, provider, spending)
}

#[test]
fn pretty_and_json_reports_are_stable() {
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    let (accounts, panel, spending) = protocol_fixture(now);
    let reports = assemble_reports(
        &default_logins(),
        &accounts,
        default_panels(vec![panel]),
        &spending,
        Some("claude"),
    );
    let mut out = anstream::StripStream::new(Vec::new());
    let time_zone = TimeZone::get("America/New_York").unwrap();
    write_pretty(&mut out, &reports, now, &time_zone).unwrap();
    let pretty = String::from_utf8(out.into_inner()).unwrap();
    insta::assert_snapshot!("provider_report_pretty", pretty);

    let standing = AccountStanding::unread(&machine_using("claude", "other"));
    let projected: Vec<_> = merged(&reports)
        .iter()
        .map(|row| row.legacy(&standing, false))
        .collect();
    let json = serde_json::to_string_pretty(&projected).unwrap();
    assert!(!json.contains("account_key"));
    assert!(!json.contains("must-not-leak"));
    insta::with_settings!({prepend_module_to_snapshot => false}, {
        insta::assert_snapshot!("rimz__cli__providers__tests__provider_report_json", json);
    });
}

#[test]
fn paid_credit_step_marks_the_extra_cell_and_line_as_paying() {
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    let (accounts, panel, spending) = protocol_fixture(now);
    let mut reports = assemble_reports(
        &default_logins(),
        &accounts,
        default_panels(vec![panel]),
        &spending,
        Some("claude"),
    );
    let render = |reports: &[ProviderReport]| {
        let mut out = anstream::StripStream::new(Vec::new());
        write_pretty(&mut out, reports, now, &TimeZone::UTC).unwrap();
        let pretty = String::from_utf8(out.into_inner()).unwrap();
        (overview(reports, now, &BTreeMap::new(), false), pretty)
    };
    let (table, pretty) = render(&reports);
    assert!(!table.contains("paying") && !pretty.contains("paying"));
    assert_eq!(
        serde_json::to_value(&reports[0]).unwrap()["spend_step"],
        "subscription"
    );

    reports[0].spend_step = SpendStep::PaidCredit;
    let (table, pretty) = render(&reports);
    assert!(table.contains("  $12.40 used · paying"), "{table}");
    let extra = pretty
        .lines()
        .find(|line| line.trim_start().starts_with("extra"))
        .unwrap_or_else(|| panic!("no extra line: {pretty}"));
    assert!(extra.ends_with(" · paying"), "{pretty}");
    assert_eq!(
        serde_json::to_value(&reports[0]).unwrap()["spend_step"],
        "paid_credit"
    );
}

#[test]
fn pretty_report_hides_zero_reset_credits() {
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    let (accounts, mut panel, spending) = protocol_fixture(now);
    panel.reset_credits = Some(ResetCredits {
        count: 0,
        soonest_expiry: None,
        expiries: Vec::new(),
        effect: rimz::agents::RedeemEffect::RestartsWindow,
    });
    let reports = assemble_reports(
        &default_logins(),
        &accounts,
        default_panels(vec![panel]),
        &spending,
        Some("claude"),
    );
    let mut out = anstream::StripStream::new(Vec::new());
    write_pretty(&mut out, &reports, now, &TimeZone::UTC).unwrap();
    assert!(
        !String::from_utf8(out.into_inner())
            .unwrap()
            .contains("resets:")
    );
}

fn rendered_resets(reset: &ResetCredits, now: Timestamp) -> String {
    let mut rows = KeyVals::new().indent(2);
    rows.push_lines("resets", reset_credit_lines(reset, now, &TimeZone::UTC));
    let mut out = anstream::StripStream::new(Vec::new());
    rows.render(&mut out).unwrap();
    String::from_utf8(out.into_inner()).unwrap()
}

#[test]
fn reset_rendering_sorts_preserves_duplicates_and_caps_detail() {
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    let day = |days: i64| now + SignedDuration::from_secs(days * 86_400);
    let reset = ResetCredits {
        count: 5,
        soonest_expiry: Some(day(1)),
        expiries: vec![day(4), day(1), day(3), day(1), day(2)],
        effect: rimz::agents::RedeemEffect::RestartsWindow,
    };

    assert_eq!(
        rendered_resets(&reset, now),
        "  resets: 5 credits\n          - 2023-11-15 22:13:20 +00:00 · in 1d00h\n          - 2023-11-15 22:13:20 +00:00 · in 1d00h\n          - 2023-11-16 22:13:20 +00:00 · in 2d00h\n"
    );
}

#[test]
fn reset_rendering_respects_count_falls_back_to_summary_and_marks_due() {
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    let hour = |hours: i64| now + SignedDuration::from_hours(hours);
    assert_eq!(
        rendered_resets(
            &ResetCredits {
                count: 1,
                soonest_expiry: Some(hour(1)),
                expiries: vec![hour(2), hour(1)],
                effect: rimz::agents::RedeemEffect::RestartsWindow,
            },
            now,
        ),
        "  resets: 1 credit\n          - 2023-11-14 23:13:20 +00:00 · in 1h00m\n"
    );
    assert_eq!(
        rendered_resets(
            &ResetCredits {
                count: 4,
                soonest_expiry: Some(hour(6)),
                expiries: Vec::new(),
                effect: rimz::agents::RedeemEffect::RestartsWindow,
            },
            now,
        ),
        "  resets: 4 credits\n          - 2023-11-15 04:13:20 +00:00 · in 6h00m\n"
    );
    assert_eq!(
        rendered_resets(
            &ResetCredits {
                count: 2,
                soonest_expiry: Some(now),
                expiries: vec![now - SignedDuration::from_secs(1), now],
                effect: rimz::agents::RedeemEffect::RestartsWindow,
            },
            now,
        ),
        "  resets: 2 credits\n          - 2023-11-14 22:13:19 +00:00 · due\n          - 2023-11-14 22:13:20 +00:00 · due\n"
    );
}

#[test]
fn provider_block_colours_dollar_percent_and_pace_tokens() {
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    let (accounts, panel, spending) = protocol_fixture(now);
    let reports = assemble_reports(
        &default_logins(),
        &accounts,
        default_panels(vec![panel]),
        &spending,
        Some("claude"),
    );
    let mut raw = Vec::new();
    write_pretty(&mut raw, &reports, now, &TimeZone::UTC).unwrap();
    let raw = String::from_utf8(raw).unwrap();
    let painted = |style: anstyle::Style, value: &str| {
        format!("{}{value}{}", style.render(), style.render_reset())
    };
    let money = |value: &str| painted(render::palette::money(), value);

    assert!(
        raw.contains(&format!(
            "{} used · {} limit",
            money("$12.40"),
            money("$50.00")
        )),
        "{raw:?}"
    );
    assert!(
        raw.contains(&format!(
            "7d {} · 30d {}",
            money("$31.20"),
            money("$118.75")
        )),
        "{raw:?}"
    );
    assert!(
        raw.contains(&format!("{} of {}/day", money("$8.10"), money("$25.00"))),
        "{raw:?}"
    );
    let rows: Vec<&str> = raw.lines().skip(1).collect();
    assert_eq!(
        rows[4],
        format!(
            "  {}            {} left · resets in 1h23m · pace {} · read 0s ago",
            painted(render::palette::muted(), "5h:"),
            painted(render::palette::budget(38), "38%"),
            painted(
                render::palette::pace(reports[0].windows[0].pace(now).unwrap()),
                "0.9x"
            )
        ),
        "{raw:?}"
    );
    let coloured = [
        "$12.40", "$50.00", "$31.20", "$118.75", "$8.10", "$25.00", "38%", "86%", "0.9x", "0.3x",
        "–",
    ];
    assert!(raw.contains(&painted(render::palette::faint(), "–")));
    let labels: usize = rows
        .iter()
        .map(|row| {
            row.matches(&render::palette::muted().render().to_string())
                .count()
        })
        .sum();
    assert_eq!(
        rows.iter()
            .map(|row| row.matches("\u{1b}[").count())
            .sum::<usize>(),
        (labels + coloured.len()) * 2,
        "nothing else in the body is coloured: {raw:?}"
    );
}

fn block_window(window: &RateLimitWindow, now: Timestamp) -> Option<String> {
    let mut rows = KeyVals::new();
    rows.push_spans("w", window_spans(window, now)?);
    let mut out = anstream::StripStream::new(Vec::new());
    rows.render(&mut out).unwrap();
    let text = String::from_utf8(out.into_inner()).unwrap();
    Some(text.trim_end().strip_prefix("w: ").unwrap().to_owned())
}

#[test]
fn window_rendering_marks_ready_lifted_and_unknown_states() {
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    let ready = RateLimitWindow {
        used_percentage: Some(1),
        resets_at: Some(now + SignedDuration::from_secs(5 * 3_600)),
        duration_mins: Some(5 * 60),
        ..Default::default()
    };
    assert_eq!(
        block_window(&ready, now).as_deref(),
        Some("100% left · ready")
    );
    let month = RateLimitWindow {
        fixed_period: true,
        ..window(43_200, 0, Some(31 * 86_400), now)
    };
    assert_eq!(
        block_window(&month, now).as_deref(),
        Some("100% left · resets in 31d00h · pace –")
    );
    assert_eq!(
        block_window(&window(5 * 60, 45, Some(70 * 60), now), now).as_deref(),
        Some("55% left · resets in 1h10m · pace 0.6x")
    );
    assert_eq!(
        block_window(
            &RateLimitWindow {
                used_percentage: Some(30),
                ..Default::default()
            },
            now
        )
        .as_deref(),
        Some("70% left · resets –")
    );
    assert_eq!(
        block_window(
            &RateLimitWindow {
                lifted: true,
                ..Default::default()
            },
            now
        )
        .as_deref(),
        Some("no limit")
    );
    assert_eq!(block_window(&RateLimitWindow::default(), now), None);
}

#[test]
fn detail_pace_gates_the_floor_and_omits_blank_states() {
    let now = Timestamp::UNIX_EPOCH;
    for (secs, expected) in [
        (17_101, "80% left · resets in 4h45m · pace –"),
        (17_100, "80% left · resets in 4h45m · pace 4.0x"),
        (17_099, "80% left · resets in 4h44m · pace 4.0x"),
    ] {
        assert_eq!(
            block_window(&window(300, 20, Some(secs), now), now).as_deref(),
            Some(expected)
        );
    }
    for (blank, expected) in [
        (
            window(300, 100, Some(17_100), now),
            "0% left · resets in 4h45m",
        ),
        (
            RateLimitWindow {
                duration_mins: None,
                ..window(300, 20, Some(17_100), now)
            },
            "80% left · resets in 4h45m",
        ),
        (
            window(300, 20, Some(18_001), now),
            "80% left · resets in 5h00m",
        ),
    ] {
        assert_eq!(block_window(&blank, now).as_deref(), Some(expected));
    }
}
