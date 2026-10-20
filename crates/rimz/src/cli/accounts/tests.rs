use super::super::*;
use super::*;
use clap::Parser;

#[path = "report_tests.rs"]
mod reports;

#[derive(Debug, Parser)]
struct AccountsHarness {
    #[command(flatten)]
    args: AccountsArgs,
}

#[test]
fn bare_accounts_lists_and_help_orders_the_workflow() {
    assert!(AccountsHarness::try_parse_from(["accounts"]).is_ok());
    use clap::CommandFactory;
    let help = AccountsHarness::command().render_help().to_string();
    let positions = ["list", "use", "add", "redeem", "remove"]
        .map(|verb| help.find(&format!("  {verb} ")).unwrap());
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{help}");
}

#[test]
fn unified_account_surface_accepts_overview_and_detail_options() {
    for argv in [
        vec!["accounts", "--json", "--refresh", "--all"],
        vec!["accounts", "list", "--json", "--refresh", "--all"],
        vec!["accounts", "show", "claude@work", "--json", "--refresh"],
        vec!["accounts", "show", "grok"],
    ] {
        assert!(AccountsHarness::try_parse_from(&argv).is_ok(), "{argv:?}");
    }
    use clap::CommandFactory;
    let mut command = AccountsHarness::command();
    assert!(
        command
            .render_long_help()
            .to_string()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .contains(PACE_HELP)
    );
    assert!(
        command
            .find_subcommand_mut("list")
            .unwrap()
            .render_long_help()
            .to_string()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .contains(PACE_HELP)
    );
}

#[test]
fn unified_overview_uses_provider_tables_and_no_pace_or_home_columns() {
    let (_, output) = listed(ACCOUNTS, None, &BTreeMap::new());
    assert!(output.contains("Claude\n"), "{output}");
    assert!(output.contains("Codex\n"), "{output}");
    for removed in ["KIND", "STATUS", "HISTORY", "HOME", "pace:"] {
        assert!(!output.contains(removed), "{output}");
    }
    assert!(
        output.contains("● next launch from here (machine default)"),
        "{output}"
    );
    assert!(!output.contains("○ new rooms"), "{output}");
}

#[test]
fn undeclared_selection_keeps_its_own_pool_and_no_spend() {
    let (mut rows, _) = listed("[use]\ncodex = 'gone'\n", None, &BTreeMap::new());
    let default = rows
        .iter_mut()
        .find(|row| row.account == key("codex", "default"))
        .unwrap();
    default.spending = Some(ProviderSpend {
        today: None,
        headline: Default::default(),
        window: MachineConfig::default().sidebar.spend_window,
        week: SpendWindow {
            usd: 12.0,
            ..Default::default()
        },
        month: Default::default(),
        year: Default::default(),
    });
    let gone = rows
        .iter()
        .find(|row| row.account == key("codex", "gone"))
        .unwrap();
    assert_eq!(serde_json::to_value(gone).unwrap()["pool"], "codex@gone");
    let output = text(&rows, &[], None);
    assert!(
        output.contains("shared pool $12.00 spent in 7d"),
        "{output}"
    );
    assert!(!output.contains("shared by"), "{output}");
    assert!(output.contains("SPEND 7d"), "{output}");
    let line = output
        .lines()
        .find(|line| line.contains("gone") && !line.contains("account `"))
        .unwrap();
    let spend_column = output
        .lines()
        .find_map(|line| line.find("SPEND 7d"))
        .unwrap();
    assert_eq!(
        line.chars()
            .skip(spend_column)
            .take(8)
            .collect::<String>()
            .trim(),
        "-",
        "{output}"
    );
}

#[test]
fn merged_json_retains_launch_health_and_adds_provider_facts() {
    let (rows, _) = listed(ACCOUNTS, None, &BTreeMap::new());
    let json = serde_json::to_value([&rows[0], &rows[2]]).unwrap();
    for row in json.as_array().unwrap() {
        for key in [
            "pool",
            "product_name",
            "probed_at",
            "plan",
            "plan_label",
            "account_id",
            "sub_provider",
            "account_scope",
            "version",
            "spending",
            "day_budget",
        ] {
            assert!(row.get(key).is_some(), "missing {key}: {row}");
        }
        assert_eq!(row["status"], "home_missing");
    }
    insta::assert_json_snapshot!(json);
}

#[test]
fn plan_prefers_setup_problems_to_lapsed_entitlement_and_problems_are_last() {
    let (mut rows, _) = listed(ACCOUNTS, None, &BTreeMap::new());
    rows[1].status = AccountStatus::HooksUntrusted;
    rows[1].entitlement = Entitlement::Lapsed { since_ms: 100 };
    rows[1].problem = Some("trust the hooks first".to_owned());
    let text = text(
        &rows,
        &[("claude", Deciding::Room), ("codex", Deciding::Machine)],
        None,
    );
    let alpha = text
        .lines()
        .find(|line| line.contains("alpha") && !line.contains("account `"))
        .unwrap();
    assert!(alpha.contains("hooks untrusted"), "{text}");
    assert!(!alpha.contains("lapsed"), "{text}");
    assert!(
        text.lines()
            .rev()
            .take_while(|line| !line.is_empty())
            .any(|line| line == "trust the hooks first"),
        "{text}"
    );
}

#[test]
fn healthy_setup_requires_a_confident_login_to_say_ready() {
    for (reading, login, status) in [
        (None, serde_json::Value::Null, "login_unknown"),
        (
            Some(ProviderStatus::Unavailable),
            serde_json::json!("unavailable"),
            "login_unknown",
        ),
        (
            Some(ProviderStatus::LoggedOut),
            serde_json::json!("logged_out"),
            "logged_out",
        ),
        (
            Some(ProviderStatus::LoggedIn),
            serde_json::json!("logged_in"),
            "ready",
        ),
    ] {
        let mut row = listed(ACCOUNTS, None, &BTreeMap::new()).0.remove(1);
        row.status = AccountStatus::Ready;
        row.problem = None;
        let reading = reading.map(|status| LoginReading {
            status,
            ..logged_in(Vec::new())
        });
        row.read(reading.as_ref(), String::new);
        let json = serde_json::to_value(&row).unwrap();
        assert_eq!(json["login"], login);
        assert_eq!(json["status"], status);
        assert_eq!(json["account"], "claude@alpha");
        assert!(json.get("name").is_none());
        if status == "login_unknown" {
            assert!(text(&[row], &[], None).contains("not probed"));
        }
    }
}

#[test]
fn overview_never_prints_the_pace_definition() {
    for windows in [Vec::new(), vec![window(FIVE_HOURS, Some(10), Some(7200))]] {
        let (_, output) = listed(
            ACCOUNTS,
            None,
            &BTreeMap::from([(key("claude", "alpha"), logged_in(windows))]),
        );
        assert!(!output.contains("pace:"), "{output}");
    }
}
#[test]
fn use_reset_takes_only_a_kind() {
    let parsed = AccountsHarness::try_parse_from(["accounts", "use", "--reset", "codex"]);
    assert!(parsed.is_ok(), "{parsed:?}");
    for argv in [
        vec!["accounts", "use", "--reset", "codex", "work"],
        vec!["accounts", "use", "--reset", "--global", "codex"],
        vec!["accounts", "use", "codex"],
        vec!["accounts", "use", "--reset"],
    ] {
        assert!(AccountsHarness::try_parse_from(&argv).is_err(), "{argv:?}");
    }
}

#[test]
fn redeem_accepts_optional_name_dry_run_and_yes() {
    for argv in [
        vec!["accounts", "redeem", "codex", "--dry-run"],
        vec!["accounts", "redeem", "codex", "spare", "--yes"],
    ] {
        assert!(AccountsHarness::try_parse_from(&argv).is_ok(), "{argv:?}");
    }
}

#[test]
fn redeem_outcome_exit_codes_cover_every_provider_code() {
    use rimz::agents::account::RedemptionCode;
    for (outcome, code) in [
        (RedemptionCode::Reset, 0),
        (RedemptionCode::NoCredit, 3),
        (RedemptionCode::NothingToReset, 4),
        (RedemptionCode::AlreadyRedeemed, 5),
        (RedemptionCode::Unknown, 6),
        (RedemptionCode::Cooldown, 7),
    ] {
        assert_eq!(redeem_exit_code(outcome), code, "{outcome:?}");
    }
}

#[test]
fn redeem_preview_shows_a_lifted_window_without_a_redeem_line() {
    let lifted = RateLimitWindow {
        lifted: true,
        ..window(FIVE_HOURS, None, None)
    };
    let credits = ResetCredits {
        count: 1,
        soonest_expiry: None,
        expiries: Vec::new(),
        effect: rimz::agents::RedeemEffect::RestartsWindow,
    };
    let mut stream = anstream::StripStream::new(Vec::new());
    write_redeem_preview(
        &mut stream,
        &key("codex", "rimio"),
        &credits,
        &[lifted, window(SEVEN_DAYS, Some(9), Some(6 * 86_400))],
        Some(RedeemForecast::Armed),
        Duration::from_secs(3600),
        now(),
    )
    .unwrap();
    let text = String::from_utf8(stream.into_inner()).unwrap();
    let row = |label: &str| {
        text.lines()
            .find(|line| line.trim_start().starts_with(label))
    };
    assert!(row("5h:").unwrap().contains("∞ not enforced now"), "{text}");
    assert!(row("redeem 5h").is_none(), "{text}");
    assert!(row("redeem 7d").unwrap().contains("refills now"), "{text}");
}

#[test]
fn list_credits_show_banked_zero_and_unknown_and_serialize_the_provider_shape() {
    let credits = ResetCredits {
        count: 2,
        soonest_expiry: Some(now() + Duration::from_secs(3600)),
        expiries: vec![],
        effect: RedeemEffect::RestartsWindow,
    };
    let (rows, text) = listed(
        ACCOUNTS,
        None,
        &BTreeMap::from([
            (
                key("codex", "team"),
                LoginReading {
                    reset_credits: Some(credits.clone()),
                    ..logged_in(Vec::new())
                },
            ),
            (
                key("codex", "default"),
                LoginReading {
                    reset_credits: Some(ResetCredits {
                        count: 0,
                        ..credits.clone()
                    }),
                    ..logged_in(Vec::new())
                },
            ),
        ]),
    );
    assert!(text.contains("RESET CREDITS"), "{text}");
    assert!(text.contains("2 · exp 1h00m"), "{text}");
    assert_eq!(
        serde_json::to_value(&rows[3]).unwrap()["reset_credits"],
        serde_json::to_value(credits).unwrap()
    );
    assert!(
        serde_json::to_value(&rows[0])
            .unwrap()
            .get("reset_credits")
            .is_none()
    );
}
#[test]
fn use_targets_the_room_unless_global() {
    for (argv, room) in [
        (vec!["accounts", "use", "codex", "work"], true),
        (vec!["accounts", "use", "--global", "codex", "work"], false),
    ] {
        let parsed = AccountsHarness::try_parse_from(&argv).expect("parse use");
        let Some(AccountsSubcmd::Use { global, .. }) = parsed.args.command else {
            panic!("{argv:?} parsed as another subcommand");
        };
        assert_eq!(!global, room, "{argv:?}");
    }
    AccountsHarness::try_parse_from(["accounts", "use", "--room", "codex", "work"])
        .expect_err("--room is gone");
}

#[test]
fn add_takes_a_history_mode_and_refuses_an_unknown_one() {
    let history = |argv: &[&str]| {
        AccountsHarness::try_parse_from(argv).map(|parsed| match parsed.args.command {
            Some(AccountsSubcmd::Add { history, .. }) => history,
            other => panic!("{other:?} parsed as another subcommand"),
        })
    };
    let add = ["accounts", "add", "claude", "work"];
    assert_eq!(history(&add).unwrap(), None);
    for (word, mode) in [
        ("shared", AccountHistory::Shared),
        ("standalone", AccountHistory::Standalone),
    ] {
        let argv = [&add[..], &["--history", word]].concat();
        assert_eq!(history(&argv).unwrap(), Some(mode));
    }
    let error = history(&[&add[..], &["--history", "mine"]].concat())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("shared") && error.contains("standalone"),
        "{error}"
    );
}

fn machine(accounts: &str) -> MachineConfig {
    MachineConfig {
        accounts: toml::from_str(accounts).unwrap(),
        ..Default::default()
    }
}

fn rows_at(
    machine: &MachineConfig,
    standing: &AccountStanding,
    agents: Option<&BTreeMap<LoginKey, usize>>,
    readings: &BTreeMap<LoginKey, LoginReading>,
) -> Vec<AccountRow> {
    let catalog = LoginCatalog::from_config(&machine.accounts).unwrap();
    let ambient = BTreeMap::from([("HOME".to_owned(), "/nonexistent/u".to_owned())]);
    account_rows(
        &machine.accounts,
        &catalog,
        standing,
        &ambient,
        agents,
        readings,
        &BTreeMap::new(),
    )
    .into_iter()
    .filter(|row| matches!(row.kind.as_str(), "claude" | "codex"))
    .map(|mut row| {
        row.installed = true;
        row.expanded = true;
        row
    })
    .collect()
}

fn now() -> Timestamp {
    Timestamp::from_second(1_700_000_000).unwrap()
}

fn text(rows: &[AccountRow], deciding: &[(&str, Deciding)], _width: Option<usize>) -> String {
    let deciding = deciding
        .iter()
        .map(|(kind, source)| (AgentKind::new_unchecked(*kind), *source))
        .collect();
    let mut stream = anstream::StripStream::new(Vec::new());
    write_overview(
        &mut stream,
        rows,
        jiff::Timestamp::from_second(1_700_000_000).unwrap(),
        &jiff::tz::TimeZone::UTC,
        &deciding,
    )
    .unwrap();
    String::from_utf8(stream.into_inner()).unwrap()
}
/// The list at a position with no room and no project: the machine layer
/// decides every kind.
fn listed(
    accounts: &str,
    agents: Option<&BTreeMap<LoginKey, usize>>,
    readings: &BTreeMap<LoginKey, LoginReading>,
) -> (Vec<AccountRow>, String) {
    let machine = machine(accounts);
    let standing = AccountStanding::machine_only(&machine);
    let rows = rows_at(&machine, &standing, agents, readings);
    let deciding: Vec<(&str, Deciding)> = ["claude", "codex"]
        .into_iter()
        .filter_map(|kind| Some((kind, standing.deciding(&AgentKind::new_unchecked(kind))?)))
        .collect();
    let text = text(&rows, &deciding, None);
    (rows, text)
}

fn key(kind: &str, name: &str) -> LoginKey {
    LoginKey {
        kind: AgentKind::new_unchecked(kind),
        name: name.parse().unwrap(),
    }
}

fn window(mins: u32, used: Option<u8>, resets_in_secs: Option<i64>) -> RateLimitWindow {
    RateLimitWindow {
        used_percentage: used,
        resets_at: resets_in_secs.map(|secs| now() + jiff::SignedDuration::from_secs(secs)),
        duration_mins: Some(mins),
        ..Default::default()
    }
}

const FIVE_HOURS: u32 = 300;
const SEVEN_DAYS: u32 = 10_080;

fn logged_in(windows: Vec<RateLimitWindow>) -> LoginReading {
    LoginReading {
        entitlement: Default::default(),
        extra_credits: None,
        reset_credits: None,
        spend_step: SpendStep::Subscription,
        status: ProviderStatus::LoggedIn,
        metered: Some(true),
        windows,
    }
}

#[test]
fn lapsed_entitlement_preserves_setup_repairs() {
    let mut reading = logged_in(Vec::new());
    reading.entitlement = Entitlement::Lapsed { since_ms: 100 };
    for status in [AccountStatus::HomeMissing, AccountStatus::HooksUntrusted] {
        let mut row = listed(ACCOUNTS, None, &BTreeMap::new()).0.remove(1);
        row.status = status;
        row.problem = Some("fix the account setup first".to_owned());
        row.read(Some(&reading), || unreachable!("not a logout"));
        assert_eq!(row.status, status);
        assert_eq!(row.problem.as_deref(), Some("fix the account setup first"));
        let json = serde_json::to_value(&row).unwrap();
        assert_eq!(
            json["entitlement"],
            serde_json::json!({"lapsed": {"since_ms": 100}})
        );
        let output = text(&[row], &[], None);
        assert!(output.contains(status.as_str()), "{output}");
        assert!(
            output.trim_end().ends_with("fix the account setup first"),
            "{output}"
        );
    }
    let mut unread = reading;
    unread.status = ProviderStatus::Unavailable;
    let mut row = listed(ACCOUNTS, None, &BTreeMap::new()).0.remove(1);
    (row.status, row.problem) = (AccountStatus::Ready, None);
    row.read(Some(&unread), || unreachable!("not a logout"));
    assert_eq!(row.status, AccountStatus::LoginUnknown);
    assert!(
        row.problem
            .as_deref()
            .is_some_and(|problem| problem.starts_with("plan lapsed: no ")),
        "{:?}",
        row.problem
    );
}

#[test]
fn lapsed_account_reports_subscription_access_instead_of_login_or_setup() {
    let mut reading = logged_in(Vec::new());
    reading.entitlement = rimz::agents::Entitlement::Lapsed { since_ms: 100 };
    let mut rows = listed(ACCOUNTS, None, &BTreeMap::new()).0;
    let row = rows
        .iter_mut()
        .find(|row| row.kind.as_str() == "claude" && row.account.name.as_str() == "alpha")
        .unwrap();
    (row.status, row.problem) = (AccountStatus::Ready, None);
    row.read(Some(&reading), String::new);
    assert_eq!(
        row.problem.as_deref(),
        Some("plan lapsed: no Claude access")
    );
    assert_eq!(
        serde_json::to_value(row).unwrap()["entitlement"],
        serde_json::json!({"lapsed":{"since_ms":100}})
    );
    let text = text(
        &rows,
        &[("claude", Deciding::Machine), ("codex", Deciding::Machine)],
        None,
    );
    let line = text
        .lines()
        .find(|line| line.contains("alpha") && !line.contains("account `"))
        .unwrap();
    assert!(line.contains("lapsed"), "{text}");
    insta::assert_snapshot!(text);
}

#[test]
fn lapsed_account_problem_names_the_delegated_product() {
    let mut reading = logged_in(Vec::new());
    reading.entitlement = Entitlement::Lapsed { since_ms: 100 };
    for (kind, expected) in [
        ("pi", "plan lapsed: no Pi access"),
        ("opencode", "plan lapsed: no Open Code access"),
    ] {
        let mut row = listed(ACCOUNTS, None, &BTreeMap::new()).0.remove(1);
        row.kind = AgentKind::new_unchecked(kind);
        (row.status, row.problem) = (AccountStatus::Ready, None);
        row.read(Some(&reading), String::new);
        assert_eq!(row.problem.as_deref(), Some(expected));
    }
}

const ACCOUNTS: &str = "[claude.alpha]\nhome = \"/srv/alpha\"\n[codex.team]\nhome = \"/srv/team\"\nhistory = \"standalone\"\n[use]\ncodex = \"team\"\n";

#[test]
fn list_has_provider_tables_with_bare_names_and_agent_counts() {
    let (rows, text) = listed(
        ACCOUNTS,
        Some(&BTreeMap::from([(key("codex", "team"), 2)])),
        &BTreeMap::new(),
    );
    assert_eq!(
        text.lines()
            .filter(|line| line.contains("NAME")
                && line.contains("PLAN")
                && line.contains("AGENTS"))
            .count(),
        2,
        "{text}"
    );
    assert!(!text.contains('○'), "{text}");
    assert!(
        text.contains("● next launch from here (machine default)"),
        "{text}"
    );
    assert_eq!(serde_json::to_value(&rows[1]).unwrap()["history"], "shared");
    let team = serde_json::to_value(&rows[3]).unwrap();
    assert_eq!(team["history"], "standalone");
    assert_eq!(team["active"], true);
    assert_eq!(team["agents"], 2);
    assert_eq!(team["default_for"], serde_json::json!(["new_rooms"]));
}
#[test]
fn extra_column_marks_paid_credit_and_does_not_invent_unknown_credit() {
    let (rows, text) = listed(
        ACCOUNTS,
        None,
        &BTreeMap::from([
            (
                key("codex", "default"),
                LoginReading {
                    extra_credits: Some(ExtraCredits::known(None, Some(2413.92), None)),
                    spend_step: SpendStep::PaidCredit,
                    ..logged_in(Vec::new())
                },
            ),
            (
                key("codex", "team"),
                LoginReading {
                    extra_credits: Some(ExtraCredits::Disabled),
                    ..logged_in(Vec::new())
                },
            ),
        ]),
    );
    assert!(text.contains("EXTRA LEFT"), "{text}");
    assert!(text.contains("$2,413.92 · paying"), "{text}");
    assert!(text.contains("off"), "{text}");
    assert!(
        serde_json::to_value(&rows[0])
            .unwrap()
            .get("extra_credits")
            .is_none()
    );
}
#[test]
fn list_marks_unknown_agent_counts_and_undeclared_selections() {
    let (rows, text) = listed("[use]\ncodex = 'gone'\n", None, &BTreeMap::new());
    let gone = rows
        .iter()
        .find(|row| row.account.name.as_str() == "gone")
        .unwrap();
    assert_eq!(gone.status, AccountStatus::Unavailable);
    assert!(!gone.active);
    assert!(gone.default_for.contains(Scope::NewRooms));
    assert!(text.contains("○  gone"), "{text}");
    assert!(text.contains("○ new rooms"), "{text}");
    let json = serde_json::to_value(gone).unwrap();
    assert_eq!(json["agents"], serde_json::Value::Null);
    assert_eq!(json["windows"], serde_json::json!([]));
}
#[test]
fn markers_and_legend_name_the_layer_that_decides() {
    let machine = machine("[claude.alpha]\nhome = '/srv/alpha'\n[use]\nclaude = 'alpha'\n");
    let standing = AccountStanding::machine_only(&machine);
    let mut rows = rows_at(&machine, &standing, None, &BTreeMap::new());
    for row in &mut rows {
        row.active = row.account.name.is_default();
    }
    let room = text(
        &rows,
        &[("claude", Deciding::Room), ("codex", Deciding::Room)],
        None,
    );
    assert!(
        room.contains("● next launch from here (this room)   ○ new rooms"),
        "{room}"
    );
    let project = text(
        &rows,
        &[("claude", Deciding::Project), ("codex", Deciding::Machine)],
        None,
    );
    assert!(
        project.contains("claude pinned by this project, codex by machine default   ○ new rooms"),
        "{project}"
    );
    for row in &mut rows {
        row.active = row.machine_default;
    }
    let project = text(
        &rows,
        &[("claude", Deciding::Project), ("codex", Deciding::Machine)],
        None,
    );
    assert!(!project.contains('○'), "{project}");
    let unread = rows_at(
        &machine,
        &AccountStanding::unread(&machine),
        None,
        &BTreeMap::new(),
    );
    let unread = text(&unread, &[], None);
    assert!(!unread.contains('●'), "{unread}");
    assert!(unread.contains("○ new rooms"), "{unread}");
}
#[test]
fn window_cells_hide_logged_out_and_lapsed_quotas() {
    let readings = BTreeMap::from([
        (
            key("claude", "default"),
            logged_in(vec![window(FIVE_HOURS, Some(2), Some(3780))]),
        ),
        (
            key("claude", "alpha"),
            LoginReading {
                status: ProviderStatus::LoggedOut,
                ..logged_in(vec![window(FIVE_HOURS, Some(96), Some(3780))])
            },
        ),
        (
            key("codex", "team"),
            LoginReading {
                entitlement: Entitlement::Lapsed { since_ms: 1 },
                ..logged_in(vec![window(FIVE_HOURS, Some(96), Some(3780))])
            },
        ),
    ]);
    let (rows, text) = listed(ACCOUNTS, None, &readings);
    assert!(text.contains("98% · 1h03m · 0.0x"), "{text}");
    assert!(!text.contains("4%"), "{text}");
    assert_eq!(
        serde_json::to_value(&rows[0]).unwrap()["windows"][0]["used_percentage"],
        2
    );
}
#[test]
fn list_keeps_every_reported_window() {
    let scoped = RateLimitWindow {
        scope: Some(rimz::agents::RateLimitWindowScope {
            id: "model:fable".to_owned(),
            label: "Fable".to_owned(),
        }),
        ..window(SEVEN_DAYS, Some(95), None)
    };
    let windows = vec![
        scoped,
        window(60, Some(99), None),
        window(FIVE_HOURS, Some(10), None),
    ];
    let (rows, text) = listed(
        ACCOUNTS,
        None,
        &BTreeMap::from([(key("claude", "alpha"), logged_in(windows.clone()))]),
    );
    assert_eq!(rows[1].windows, windows);
    assert!(text.contains("Fable LEFT"), "{text}");
}
#[test]
fn a_logged_out_account_reads_logged_out_unless_its_setup_is_broken() {
    let machine = machine("[claude.alpha]\nhome = \"/srv/my alpha\"\n");
    let catalog = LoginCatalog::from_config(&machine.accounts).unwrap();
    let ambient = BTreeMap::from([("HOME".to_owned(), "/nonexistent/u".to_owned())]);
    let login = |name: &str| {
        catalog
            .select(&AgentKind::new_unchecked("claude"), &name.parse().unwrap())
            .unwrap()
    };
    assert_eq!(
        login_command(&login("alpha"), &ambient),
        "CLAUDE_CONFIG_DIR='/srv/my alpha' claude"
    );
    assert_eq!(login_command(&login("default"), &ambient), "claude");

    let reading = |status| LoginReading {
        status,
        entitlement: Default::default(),
        extra_credits: None,
        reset_credits: None,
        spend_step: SpendStep::Subscription,
        metered: None,
        windows: vec![window(FIVE_HOURS, Some(10), None)],
    };
    let standing = AccountStanding::machine_only(&machine);
    let rows = || rows_at(&machine, &standing, None, &BTreeMap::new());
    // Neither fixture home exists, so every row starts with a setup problem.
    let mut broken = rows().remove(1);
    let setup = broken.problem.clone();
    broken.read(Some(&reading(ProviderStatus::LoggedOut)), || {
        "the command".to_owned()
    });
    assert_eq!(broken.status, AccountStatus::HomeMissing);
    assert_eq!(broken.problem, setup, "the setup problem is fixed first");
    assert_eq!(broken.login, Some(ProviderStatus::LoggedOut));

    for (index, kind_name, command) in [
        (1, "alpha", "CLAUDE_CONFIG_DIR=/srv/alpha claude"),
        (0, "default", "claude"),
    ] {
        let mut row = rows().remove(index);
        (row.status, row.problem) = (AccountStatus::Ready, None);
        row.read(Some(&reading(ProviderStatus::LoggedOut)), || {
            command.to_owned()
        });
        assert_eq!(row.status.as_str(), "logged out");
        let json = serde_json::to_value(&row).unwrap();
        assert_eq!(json["status"], "logged_out");
        assert_eq!(
            json["problem"],
            format!("claude account `{kind_name}` is logged out; log in once: {command}")
        );
    }
    for status in [ProviderStatus::Unavailable, ProviderStatus::LoggedIn] {
        let mut row = rows().remove(1);
        (row.status, row.problem) = (AccountStatus::Ready, None);
        row.read(Some(&reading(status)), || unreachable!("{status:?}"));
        assert_eq!(
            row.status,
            if status == ProviderStatus::LoggedIn {
                AccountStatus::Ready
            } else {
                AccountStatus::LoginUnknown
            },
            "{status:?}"
        );
        assert_eq!(row.problem, None);
        assert_eq!(row.windows.len(), 1);
    }
    let mut row = rows().remove(1);
    (row.status, row.problem) = (AccountStatus::Ready, None);
    row.read(None, || unreachable!("no record"));
    assert_eq!(row.status, AccountStatus::LoginUnknown, "a missing record");
    assert_eq!(row.login, None);
}

#[test]
fn overview_has_no_home_column_or_terminal_clipping() {
    let (rows, full) = listed(ACCOUNTS, None, &BTreeMap::new());
    assert!(!full.contains("HOME"), "{full}");
    assert_eq!(
        text(
            &rows,
            &[("claude", Deciding::Machine), ("codex", Deciding::Machine)],
            Some(20)
        ),
        full
    );
}
#[test]
fn removed_notice_without_live_rooms_matches_reference() {
    assert_eq!(
        removed_notice(
            &AgentKind::new_unchecked("claude"),
            &"work".parse().unwrap(),
            "~/.rimz/accounts/claude/work",
            &[]
        ),
        "removed claude account `work`; its home ~/.rimz/accounts/claude/work and the provider files in it stay on disk; add it back to resume its sessions, or use `rimz accounts use claude default` for future launches"
    );
}

#[test]
fn removed_notice_warns_about_live_room_pins() {
    for (live, warning) in [
        (
            vec!["rimz-one".to_owned()],
            "warning: room rimz-one pins it for new claude launches; add the account back, run `rimz accounts use --reset claude` to follow the defaults, or run `rimz accounts use claude default` inside each room",
        ),
        (
            vec!["rimz-one".to_owned(), "rimz-two".to_owned()],
            "warning: rooms rimz-one, rimz-two pin it for new claude launches; add the account back, run `rimz accounts use --reset claude` to follow the defaults, or run `rimz accounts use claude default` inside each room",
        ),
    ] {
        let notice = removed_notice(
            &AgentKind::new_unchecked("claude"),
            &"work".parse().unwrap(),
            "~/.rimz/accounts/claude/work",
            &live,
        );
        assert_eq!(notice.lines().nth(1), Some(warning));
        assert_eq!(notice.lines().count(), 2);
    }
}

#[test]
fn account_flags_parse_kind_and_name_and_refuse_a_repeated_kind() {
    let work = parse_account_flag("claude@work").expect("claude@work");
    assert_eq!(parse_account_flag("claude=work").unwrap(), work);
    let default = parse_account_flag("codex@default").expect("codex@default");
    assert_eq!(
        requested_logins(&[work.clone(), default]).expect("one per kind"),
        RoomLogins::from([
            (AgentKind::new_unchecked("claude"), "work".parse().unwrap()),
            (
                AgentKind::new_unchecked("codex"),
                LoginName::default_login()
            ),
        ])
    );
    assert!(parse_account_flag("claude").is_err());
    assert!(parse_account_flag("nope=work").is_err());
    assert!(parse_account_flag("claude=Work").is_err());

    let personal = parse_account_flag("claude=personal").expect("claude=personal");
    let err = requested_logins(&[work, personal]).unwrap_err();
    assert!(err.to_string().contains("names claude twice"), "{err}");
}
