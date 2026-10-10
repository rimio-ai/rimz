use super::*;
use jiff::SignedDuration;
use jiff::tz::TimeZone;
use rimz::agents::{AgentAccount, SpendWindow};
use rimz::config::MachineConfig;
use rimz::store::snapshot::{RemoteControlBadge, SidebarProviderPanel};

fn record(probed_at_ms: u64, ok: bool, account: Option<AgentAccount>) -> ProviderRecord {
    ProviderRecord {
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
        false,
    );
    assert!(text.contains("lapsed"));
    assert!(!text.contains("Claude Pro"));
    insta::assert_snapshot!(text);
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
fn report_assembly_covers_auth_states_raw_accounts_filters_and_all() {
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
        false,
    );

    assert_eq!(
        reports
            .iter()
            .map(|report| report.kind.as_str())
            .collect::<Vec<_>>(),
        ["claude", "copilot", "pi", "qwen"]
    );
    assert_eq!(reports[0].status, ProviderStatus::LoggedIn);
    assert!(reports[0].metered.is_some_and(|metered| metered));
    assert_eq!(reports[1].status, ProviderStatus::Unavailable);
    assert_eq!(reports[1].account_id.as_deref(), Some("octocat"));
    assert_eq!(reports[2].status, ProviderStatus::LoggedIn);
    assert_eq!(reports[2].plan.as_deref(), Some("openai-oauth"));
    assert_eq!(reports[2].plan_label.as_deref(), Some("Openai Oauth"));
    assert_eq!(reports[2].metered, Some(false));

    let filtered = assemble_reports(
        &default_logins(),
        &accounts,
        BTreeMap::new(),
        &spending,
        Some("pi"),
        false,
    );
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].kind, "pi");
    assert!(
        assemble_reports(
            &default_logins(),
            &accounts,
            BTreeMap::new(),
            &spending,
            Some("codex"),
            false
        )
        .is_empty()
    );
    let logged_out = assemble_reports(
        &default_logins(),
        &accounts,
        BTreeMap::new(),
        &spending,
        Some("codex"),
        true,
    );
    assert_eq!(logged_out.len(), 1);
    assert_eq!(logged_out[0].status, ProviderStatus::LoggedOut);

    let all = assemble_reports(
        &default_logins(),
        &accounts,
        BTreeMap::new(),
        &spending,
        None,
        true,
    );
    assert_eq!(all.len(), rimz::agents::known_kinds().count());
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
            true,
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
    let mut reports =
        assemble_reports(&logins, &accounts, panels, &spending, Some("claude"), false);
    assert_eq!(
        reports
            .iter()
            .map(|report| (report.kind.as_str(), report.account.as_str()))
            .collect::<Vec<_>>(),
        [("claude", "default"), ("claude", "work")]
    );
    assert_eq!(reports[0].spending, None, "default never reads by_provider");
    assert_eq!(reports[1].spending, Some(week(12.0)));
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
    write_pretty(
        &mut out,
        &reports,
        Timestamp::from_second(1).unwrap(),
        &TimeZone::UTC,
    )
    .unwrap();
    let pretty = String::from_utf8(out.into_inner()).unwrap();
    assert_eq!(
        pretty,
        "  Claude · default — Claude Max · logged in\n  version: v1.2.3\n  usage:   –\n  spend:   –\n\n● Claude · work — Claude Max · logged in · new rooms\n  version: v1.2.3\n  usage:   –\n  spend:   7d $12.00 · 30d $0.00\n  budget:  $3.00 of $20.00/day\n"
    );
    let unprobed = assemble_reports(
        &logins,
        &AccountsCache::default(),
        BTreeMap::new(),
        &ProviderSpendingCache::default(),
        None,
        false,
    );
    assert_eq!(
        unprobed
            .iter()
            .map(|report| report.account.as_str())
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
    in_room: bool,
) -> String {
    let mut out = anstream::StripStream::new(Vec::new());
    write_overview(&mut out, reports, now, deciding, in_room).unwrap();
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
        false,
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
fn overview_renders_every_kind_as_one_table() {
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
    let mut reports = assemble_reports(&logins, &accounts, panels, &spending, None, false);
    mark_standing(
        &mut reports,
        &AccountStanding::machine_only(&machine_using("claude", "work")),
    );
    let text = overview(&reports, now, &BTreeMap::new(), false);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "Claude · v2.1.274", "{text}");
    assert_eq!(
        cells(lines[1]),
        [
            "ACCOUNT",
            "PLAN",
            "5h LEFT",
            "7d LEFT",
            "Fable LEFT",
            "EXTRA",
            "RESETS",
            "SPEND 7d"
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
    assert_eq!(
        lines[6..],
        [
            "Pi · v1.2.3",
            "   ACCOUNT  PLAN          SPEND 7d",
            "●  default  Openai Oauth  -"
        ],
        "an all-default kind is a table too: {text}"
    );
    insta::assert_snapshot!("provider_overview", text);
}

fn hinted_window(
    active: RateLimitWindow,
    siblings: &[(&str, u8)],
    deciding: Option<Deciding>,
    in_room: bool,
) -> String {
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    let with_window = |window| SidebarProviderPanel {
        windows: vec![window],
        ..panel("codex")
    };
    let mut rows = vec![("default", with_window(active))];
    rows.extend(siblings.iter().map(|(name, used)| {
        let week = window(7 * 24 * 60, *used, Some(86_400), now);
        (*name, with_window(week))
    }));
    let mut reports = group("codex", rows);
    reports[0].active = true;
    let deciding = deciding
        .map(|deciding| BTreeMap::from([("codex".to_owned(), deciding)]))
        .unwrap_or_default();
    overview(&reports, now, &deciding, in_room)
}

fn hinted(
    active_used: u8,
    siblings: &[(&str, u8)],
    deciding: Option<Deciding>,
    in_room: bool,
) -> String {
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    hinted_window(
        window(7 * 24 * 60, active_used, Some(86_400), now),
        siblings,
        deciding,
        in_room,
    )
}

#[test]
fn hint_names_the_roomiest_sibling_once_the_active_account_runs_low() {
    let text = hinted(
        92,
        &[("team-2", 31), ("team-1", 5), ("team-0", 5)],
        Some(Deciding::Room),
        true,
    );
    assert!(
        text.ends_with(
            "\n  codex default has 8% left of its 7d window; team-0 has the most room:\n    rimz accounts use codex team-0\n"
        ),
        "{text}"
    );
    let global = hinted(80, &[("team-1", 5)], Some(Deciding::Machine), false);
    assert!(
        global.contains("    rimz accounts use --global codex team-1\n"),
        "{global}"
    );
    for (text, why) in [
        (
            hinted(79, &[("team-1", 5)], Some(Deciding::Room), true),
            "21% left",
        ),
        (
            hinted_window(
                RateLimitWindow {
                    resets_at: None,
                    ..window(
                        7 * 24 * 60,
                        99,
                        None,
                        Timestamp::from_second(1_700_000_000).unwrap(),
                    )
                },
                &[("team-1", 5)],
                Some(Deciding::Room),
                true,
            ),
            "a placeholder reading before the window starts",
        ),
        (
            hinted(
                92,
                &[("team-1", 92), ("team-2", 95)],
                Some(Deciding::Room),
                true,
            ),
            "no sibling with more left",
        ),
        (
            hinted(92, &[("team-1", 5)], Some(Deciding::Project), true),
            "project decides",
        ),
        (
            hinted(92, &[("team-1", 5)], Some(Deciding::Room), false),
            "room decides but the caller is outside it",
        ),
        (hinted(92, &[("team-1", 5)], None, true), "nothing decides"),
    ] {
        assert!(text.contains("team-1"), "{why}: table rendered: {text}");
        assert!(!text.contains("most room"), "{why}: {text}");
    }
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
    assert_eq!(
        cells(lines[2]),
        [
            "●",
            "default",
            "Claude Max",
            "55% · 1h10m · 0.6x",
            "100% · ready",
            "100%",
            "–",
            "-"
        ],
        "{text}"
    );
    assert_eq!(
        cells(lines[3]),
        ["team", "Claude Max", "–", "∞", "–", "30%", "-"],
        "{text}"
    );

    let mut raw = Vec::new();
    write_overview(&mut raw, &reports, now, &BTreeMap::new(), false).unwrap();
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
    let mut reports = group(
        "codex",
        vec![("default", panel("codex")), ("team", panel("codex"))],
    );
    let marks = |reports: &[ProviderReport]| -> Vec<String> {
        overview(reports, now, &BTreeMap::new(), false)
            .lines()
            .skip(2)
            .map(|line| line.chars().take(1).collect())
            .collect()
    };

    mark_standing(
        &mut reports,
        &AccountStanding::unread(&machine_using("codex", "team")),
    );
    assert_eq!(marks(&reports), [" ", "○"], "unread layers mark no launch");
    reports[0].active = true;
    assert_eq!(marks(&reports), ["●", "○"]);
    mark_standing(
        &mut reports,
        &AccountStanding::machine_only(&machine_using("codex", "team")),
    );
    assert_eq!(marks(&reports), [" ", "●"], "one row never takes both");
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
            .map(|line| cells(line)[2])
            .collect::<Vec<_>>(),
        ["RESETS", "2 · exp 3d00h", "4 · due", "1", "-", "–"],
        "{text}"
    );
}

#[test]
fn hint_and_block_read_a_reset_less_sub_cap_at_its_own_remainder() {
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

    let with_window = |window| SidebarProviderPanel {
        windows: vec![window],
        ..panel("claude")
    };
    let mut reports = group(
        "claude",
        vec![
            ("default", with_window(sub_cap(85))),
            ("work", with_window(sub_cap(40))),
        ],
    );
    reports[0].active = true;
    let deciding = BTreeMap::from([("claude".to_owned(), Deciding::Machine)]);
    let text = overview(&reports, now, &deciding, false);
    assert!(
        text.ends_with(
            "\n  claude default has 15% left of its Fable window; work has the most room:\n    rimz accounts use --global claude work\n"
        ),
        "{text}"
    );
}

#[test]
fn unknown_kind_error_lists_registered_providers() {
    let error = validate_kind(Some("wat")).unwrap_err().to_string();
    assert!(error.contains("unknown provider kind `wat`"));
    assert!(error.contains("claude, codex"));
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
    provider.spending = Some(SpendTally {
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
    });
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
    spending
        .spending
        .by_provider
        .insert("claude".to_owned(), provider.spending.clone().unwrap());
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
        None,
        false,
    );
    let mut out = anstream::StripStream::new(Vec::new());
    let time_zone = TimeZone::get("America/New_York").unwrap();
    write_pretty(&mut out, &reports, now, &time_zone).unwrap();
    let pretty = String::from_utf8(out.into_inner()).unwrap();
    insta::assert_snapshot!("provider_report_pretty", pretty);

    let json = serde_json::to_string_pretty(&reports).unwrap();
    assert!(!json.contains("account_key"));
    assert!(!json.contains("must-not-leak"));
    insta::assert_snapshot!("provider_report_json", json);
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
        None,
        false,
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
        None,
        false,
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
        rows[2],
        format!(
            "  {}       {} left · resets in 1h23m · pace {}",
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
    ];
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
        Some("∞")
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
