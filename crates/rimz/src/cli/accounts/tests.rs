use super::*;
use clap::Parser;

#[derive(Debug, Parser)]
struct AccountsHarness {
    #[command(flatten)]
    args: AccountsArgs,
}

#[test]
fn use_targets_the_room_unless_global() {
    for (argv, room) in [
        (vec!["accounts", "use", "codex", "work"], true),
        (vec!["accounts", "use", "--global", "codex", "work"], false),
    ] {
        let parsed = AccountsHarness::try_parse_from(&argv).expect("parse use");
        let AccountsSubcmd::Use { global, .. } = parsed.args.command else {
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
            AccountsSubcmd::Add { history, .. } => history,
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
    )
}

fn now() -> Timestamp {
    Timestamp::from_second(1_700_000_000).unwrap()
}

fn text(
    rows: &[AccountRow],
    deciding: &[(&str, Deciding)],
    in_room: bool,
    width: Option<usize>,
) -> String {
    let deciding = deciding
        .iter()
        .map(|(kind, deciding)| (AgentKind::new_unchecked(*kind), *deciding))
        .collect();
    let mut stream = anstream::StripStream::new(Vec::new());
    write_accounts(&mut stream, rows, &deciding, in_room, now(), width).unwrap();
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
    let text = text(&rows, &deciding, false, None);
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
        status: ProviderStatus::LoggedIn,
        metered: Some(true),
        windows,
    }
}

/// The table's lines: everything above the legend, hint, and problems.
fn table(text: &str) -> Vec<&str> {
    text.lines()
        .take_while(|line| {
            line.is_empty() || line.starts_with("   ") || line.starts_with(['●', '○'])
        })
        .filter(|line| !line.starts_with("●  this") && !line.starts_with("●  new"))
        .filter(|line| !line.starts_with("○  new rooms"))
        .collect()
}

const ACCOUNTS: &str = "[claude.alpha]\nhome = \"/srv/alpha\"\n[codex.team]\nhome = \"/srv/team\"\nhistory = \"standalone\"\n[use]\ncodex = \"team\"\n";

#[test]
fn list_is_one_table_with_default_first_and_one_marker_per_kind() {
    let counts = BTreeMap::from([(key("codex", "team"), 2)]);
    let (rows, text) = listed(ACCOUNTS, Some(&counts), &BTreeMap::new());
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines.iter().filter(|line| line.contains("KIND")).count(),
        1,
        "{text}"
    );
    assert!(
        lines[0].starts_with("   KIND    NAME     STATUS        5h LEFT  7d LEFT  AGENTS  "),
        "{text}"
    );
    assert!(lines[1].starts_with("●  claude  default"), "{text}");
    assert!(lines[2].starts_with("   claude  alpha"), "{text}");
    assert_eq!(lines[3], "", "{text}");
    assert!(lines[4].starts_with("   codex   default"), "{text}");
    assert!(lines[5].starts_with("●  codex   team"), "{text}");
    assert_eq!(lines.iter().filter(|line| line.is_empty()).count(), 1);
    assert!(
        !text.contains('○'),
        "each kind's ● is its machine default: {text}"
    );
    assert_eq!(lines[6], "●  new rooms", "{text}");
    assert!(
        lines.iter().all(|line| line.trim_end() == *line),
        "{text:?}"
    );
    assert!(lines[5].contains("  2  "), "{text}");
    assert!(lines[1].contains("  -  "), "{text}");
    assert!(lines[0].ends_with("  AGENTS  HISTORY     HOME"), "{text}");
    assert!(lines[1].contains("  -           /nonexistent"), "{text}");
    assert!(lines[2].contains("  shared      /srv/alpha"), "{text}");
    assert!(lines[5].contains("  standalone  /srv/team"), "{text}");
    assert_eq!(
        serde_json::to_value(&rows[0]).unwrap()["history"],
        serde_json::Value::Null
    );
    assert_eq!(serde_json::to_value(&rows[1]).unwrap()["history"], "shared");
    let team = serde_json::to_value(&rows[3]).unwrap();
    assert_eq!(team["history"], "standalone");
    assert_eq!(team["active"], true);
    assert_eq!(team["default_for"], serde_json::json!(["new_rooms"]));
    assert_eq!(team["agents"], 2);
}

#[test]
fn list_marks_unknown_agent_counts_and_undeclared_selections() {
    let (rows, text) = listed("[use]\ncodex = \"gone\"\n", None, &BTreeMap::new());
    let gone = rows
        .iter()
        .find(|row| row.name.as_str() == "gone")
        .expect("an undeclared selection keeps a row");
    assert_eq!(gone.status, AccountStatus::Unavailable);
    assert!(!gone.active, "birth refuses an undeclared selection");
    assert_eq!(gone.default_for.label(), "new rooms");
    assert!(text.contains("unavailable"), "{text}");
    assert!(
        !text
            .lines()
            .any(|line| line.starts_with('●') && line.contains("codex")),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|line| line.starts_with("○  codex   gone     unavailable   –        –   ")),
        "the new-rooms default keeps its mark and reads no window: {text}"
    );
    assert!(
        table(&text)
            .iter()
            .filter(|line| line.contains("codex   "))
            .all(|line| line.contains("  –  ")),
        "{text}"
    );
    assert!(
        text.contains("\n●  new rooms   ○  new rooms\n"),
        "claude follows the machine layer, and codex shows its unmarked default: {text}"
    );
    let gone = serde_json::to_value(gone).unwrap();
    assert_eq!(gone["agents"], serde_json::Value::Null);
    assert_eq!(gone["status"], "unavailable");
    assert_eq!(gone["active"], false);
    assert_eq!(gone["windows"], serde_json::json!([]));
    assert_eq!(gone["metered"], serde_json::Value::Null);
}

#[test]
fn markers_and_legend_name_the_layer_that_decides() {
    let machine = machine("[claude.alpha]\nhome = \"/srv/alpha\"\n[use]\nclaude = \"alpha\"\n");
    let standing = AccountStanding::machine_only(&machine);
    let mut rows = rows_at(&machine, &standing, None, &BTreeMap::new());
    // A room at the position that launches claude on `default`.
    for row in &mut rows {
        row.active = row.name.is_default();
    }
    let room = text(
        &rows,
        &[("claude", Deciding::Room), ("codex", Deciding::Room)],
        true,
        None,
    );
    let lines = table(&room);
    assert!(lines[1].starts_with("●  claude  default"), "{room}");
    assert!(lines[2].starts_with("○  claude  alpha"), "{room}");
    assert!(lines[4].starts_with("●  codex   default"), "{room}");
    assert!(room.contains("\n●  this room   ○  new rooms\n"), "{room}");

    let project = text(
        &rows,
        &[("claude", Deciding::Project), ("codex", Deciding::Machine)],
        false,
        None,
    );
    assert!(
        project.contains("\n●  this project   ○  new rooms\n"),
        "{project}"
    );
    for row in &mut rows {
        row.active = row.machine_default;
    }
    let project = text(&rows, &[("claude", Deciding::Project)], false, None);
    assert!(
        !table(&project).iter().any(|line| line.starts_with('○')),
        "{project}"
    );
    assert!(
        project.contains("\n●  this project   ○  new rooms\n"),
        "the ○ half stays under a project: {project}"
    );

    let unread = rows_at(
        &machine,
        &AccountStanding::unread(&machine),
        None,
        &BTreeMap::new(),
    );
    let unread = text(&unread, &[], false, None);
    let lines = table(&unread);
    assert!(!unread.contains('●'), "{unread}");
    assert!(lines[1].starts_with("   claude  default"), "{unread}");
    assert!(lines[2].starts_with("○  claude  alpha"), "{unread}");
    assert!(lines[4].starts_with("○  codex   default"), "{unread}");
    assert!(unread.contains("\n○  new rooms\n"), "{unread}");
}

#[test]
fn window_cells_read_what_is_left_and_when_it_resets() {
    let accounts = "[claude.alpha]\nhome = \"/srv/alpha\"\n[claude.beta]\nhome = \"/srv/beta\"\n[claude.gamma]\nhome = \"/srv/gamma\"\n[codex.team]\nhome = \"/srv/team\"\n";
    let lifted = RateLimitWindow {
        lifted: true,
        ..window(FIVE_HOURS, Some(40), None)
    };
    let readings = BTreeMap::from([
        (
            key("claude", "default"),
            logged_in(vec![
                window(FIVE_HOURS, Some(2), Some(3 * 3_600 + 32 * 60)),
                window(SEVEN_DAYS, Some(69), None),
            ]),
        ),
        (
            key("claude", "alpha"),
            LoginReading {
                status: ProviderStatus::LoggedOut,
                metered: Some(true),
                windows: vec![window(FIVE_HOURS, Some(10), Some(60))],
            },
        ),
        (
            key("claude", "beta"),
            logged_in(vec![window(FIVE_HOURS, None, Some(3_600))]),
        ),
        (
            key("claude", "gamma"),
            LoginReading {
                status: ProviderStatus::Unavailable,
                metered: None,
                windows: vec![window(SEVEN_DAYS, Some(50), Some(3_600))],
            },
        ),
        (
            key("codex", "default"),
            LoginReading {
                status: ProviderStatus::LoggedIn,
                metered: Some(false),
                windows: vec![window(FIVE_HOURS, Some(10), Some(60))],
            },
        ),
        (
            key("codex", "team"),
            logged_in(vec![lifted, window(SEVEN_DAYS, Some(0), Some(7 * 86_400))]),
        ),
    ]);
    let (rows, text) = listed(accounts, None, &readings);
    let cells = |name: &str, kind: &str| -> String {
        let line = table(&text)
            .into_iter()
            .find(|line| line.contains(&format!("{kind}  ")) && line.contains(name))
            .unwrap_or_else(|| panic!("no {kind} {name} row in {text}"));
        let from = text.lines().next().unwrap().find("5h LEFT").unwrap();
        let to = text.lines().next().unwrap().find("AGENTS").unwrap();
        line.chars()
            .skip(from)
            .take(to - from)
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    // A reset-less window with a known span is the pre-start placeholder,
    // which reads as untouched here as in `rimz providers`.
    assert_eq!(cells("default", "claude"), "98% · 3h32m 100%", "{text}");
    assert_eq!(cells("alpha", "claude"), "– –", "logged out: {text}");
    assert_eq!(cells("beta", "claude"), "– –", "no percentage, no window");
    assert_eq!(cells("gamma", "claude"), "– 50% · 1h00m", "failed probe");
    assert_eq!(cells("default", "codex"), "∞ ∞", "unmetered: {text}");
    assert_eq!(cells("team", "codex"), "∞ 100% · ready", "lifted: {text}");

    let default = serde_json::to_value(&rows[0]).unwrap();
    assert_eq!(default["metered"], true);
    assert_eq!(default["windows"][0]["used_percentage"], 2);
    assert!(default["windows"][0]["resets_at"].is_string(), "{default}");
    assert_eq!(default["windows"][1]["used_percentage"], 69);
    assert!(default.get("login").is_none(), "{default}");
}

#[test]
fn the_list_keeps_only_the_unscoped_5h_and_7d_windows() {
    let scoped = RateLimitWindow {
        scope: Some(rimz::agents::RateLimitWindowScope {
            id: "model:fable".to_owned(),
            label: "Fable".to_owned(),
        }),
        ..window(SEVEN_DAYS, Some(95), None)
    };
    let kept = list_windows(&[
        scoped,
        window(SEVEN_DAYS, Some(30), None),
        window(60, Some(99), None),
        window(FIVE_HOURS, Some(10), None),
    ]);
    assert_eq!(
        kept,
        [
            window(FIVE_HOURS, Some(10), None),
            window(SEVEN_DAYS, Some(30), None)
        ]
    );
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
        assert_eq!(row.status, AccountStatus::Ready, "{status:?}");
        assert_eq!(row.problem, None);
        assert_eq!(row.windows.len(), 1);
    }
    let mut row = rows().remove(1);
    (row.status, row.problem) = (AccountStatus::Ready, None);
    row.read(None, || unreachable!("no record"));
    assert_eq!(row.status, AccountStatus::Ready, "a missing record");
    assert_eq!(row.login, Some(ProviderStatus::Unavailable));
}

fn at_in(text: &str, needle: &str) -> usize {
    text.lines()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no `{needle}` in {text}"))
}

const TEAMS: &str = "[codex.team-0]\nhome = \"/srv/t0\"\n[codex.team-1]\nhome = \"/srv/t1\"\n[codex.team-2]\nhome = \"/srv/t2\"\n";

/// The list where codex `default` is active with `used` on its 7d window
/// and each sibling reads its own, every account's setup healthy.
fn hinted(
    used: u8,
    siblings: &[(&str, ProviderStatus, u8)],
    deciding: Deciding,
    in_room: bool,
) -> String {
    hinted_from(
        vec![
            window(FIVE_HOURS, Some(30), Some(3_600)),
            window(SEVEN_DAYS, Some(used), Some(86_400)),
        ],
        siblings,
        &[],
        deciding,
        in_room,
    )
}

/// `hinted` with the active account's windows given, and the `broken`
/// siblings left with the setup problem their missing fixture home gives.
fn hinted_from(
    active: Vec<RateLimitWindow>,
    siblings: &[(&str, ProviderStatus, u8)],
    broken: &[&str],
    deciding: Deciding,
    in_room: bool,
) -> String {
    let machine = machine(TEAMS);
    let mut readings = BTreeMap::from([(key("codex", "default"), logged_in(active))]);
    for (name, status, used) in siblings {
        readings.insert(
            key("codex", name),
            LoginReading {
                status: *status,
                metered: Some(true),
                windows: vec![window(SEVEN_DAYS, Some(*used), Some(86_400))],
            },
        );
    }
    let standing = AccountStanding::machine_only(&machine);
    let mut rows = rows_at(&machine, &standing, None, &readings);
    for row in &mut rows {
        if !broken.contains(&row.name.as_str()) {
            (row.status, row.problem) = (AccountStatus::Ready, None);
        }
    }
    text(&rows, &[("codex", deciding)], in_room, None)
}

#[test]
fn hint_names_the_sibling_with_the_most_left_once_the_marked_account_runs_low() {
    use ProviderStatus::LoggedIn;
    let room = hinted(
        88,
        &[
            ("team-2", LoggedIn, 31),
            ("team-1", LoggedIn, 5),
            ("team-0", LoggedIn, 5),
        ],
        Deciding::Room,
        true,
    );
    assert!(
        room.contains(
            "\n●  this room   ○  new rooms\n  codex default has 12% of its 7d window left; team-0 has the most room:\n    rimz accounts use codex team-0\n"
        ),
        "{room}"
    );
    let lines: Vec<&str> = room.lines().collect();
    let at = |needle: &str| {
        lines
            .iter()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no `{needle}` in {room}"))
    };
    assert!(at("codex   team-2") < at("this room"), "{room}");

    // `rimz accounts use` refuses an account with a setup problem, so the
    // hint passes over one however much it has left.
    let active = || {
        vec![
            window(FIVE_HOURS, Some(30), Some(3_600)),
            window(SEVEN_DAYS, Some(88), Some(86_400)),
        ]
    };
    let siblings = [("team-1", LoggedIn, 5), ("team-2", LoggedIn, 31)];
    let passed_over = hinted_from(active(), &siblings, &["team-1"], Deciding::Room, true);
    assert!(
        passed_over.contains("; team-2 has the most room:\n    rimz accounts use codex team-2\n"),
        "{passed_over}"
    );
    assert_eq!(
        at_in(&passed_over, "rimz accounts use") + 1,
        at_in(&passed_over, "is not a directory"),
        "{passed_over}"
    );
    let none_usable = hinted_from(
        active(),
        &siblings,
        &["team-1", "team-2"],
        Deciding::Room,
        true,
    );
    assert!(!none_usable.contains("has the most room"), "{none_usable}");

    let global = hinted(80, &[("team-1", LoggedIn, 5)], Deciding::Machine, false);
    assert!(
        global.contains(
            "\n●  new rooms\n  codex default has 20% of its 7d window left; team-1 has the most room:\n    rimz accounts use --global codex team-1\n"
        ),
        "{global}"
    );
    for (text, why) in [
        (
            hinted(79, &[("team-1", LoggedIn, 5)], Deciding::Room, true),
            "more than 20% left",
        ),
        (
            hinted(92, &[("team-1", LoggedIn, 5)], Deciding::Project, false),
            "project decides",
        ),
        (
            hinted(92, &[("team-1", LoggedIn, 5)], Deciding::Room, false),
            "outside the room",
        ),
        (
            hinted(
                92,
                &[
                    ("team-1", ProviderStatus::Unavailable, 5),
                    ("team-2", ProviderStatus::LoggedOut, 5),
                ],
                Deciding::Room,
                true,
            ),
            "no sibling is logged in",
        ),
        (
            hinted(
                92,
                &[("team-1", LoggedIn, 92), ("team-2", LoggedIn, 95)],
                Deciding::Room,
                true,
            ),
            "no sibling has more left",
        ),
    ] {
        assert!(!text.contains("has the most room"), "{why}: {text}");
    }
}

/// A hot model-scoped window never reaches the hint, even when a sibling has
/// room on the same one: each reading passes the list's window filter first.
#[test]
fn a_hot_model_scoped_window_alone_gives_no_hint() {
    let scoped = |used| RateLimitWindow {
        scope: Some(rimz::agents::RateLimitWindowScope {
            id: "model:fable".to_owned(),
            label: "Fable".to_owned(),
        }),
        ..window(SEVEN_DAYS, Some(used), None)
    };
    let hint = |keep: fn(&[RateLimitWindow]) -> Vec<RateLimitWindow>| {
        let machine = machine(TEAMS);
        let readings = BTreeMap::from([
            (
                key("codex", "default"),
                logged_in(keep(&[scoped(95), window(SEVEN_DAYS, Some(30), None)])),
            ),
            (
                key("codex", "team-1"),
                logged_in(keep(&[scoped(5), window(SEVEN_DAYS, Some(5), None)])),
            ),
        ]);
        let standing = AccountStanding::machine_only(&machine);
        let mut rows = rows_at(&machine, &standing, None, &readings);
        for row in &mut rows {
            (row.status, row.problem) = (AccountStatus::Ready, None);
        }
        text(&rows, &[("codex", Deciding::Room)], true, None)
    };
    let unfiltered = hint(<[RateLimitWindow]>::to_vec);
    assert!(
        unfiltered.contains("has the most room"),
        "the scoped window hints once it reaches a row: {unfiltered}"
    );
    let filtered = hint(list_windows);
    assert!(!filtered.contains("has the most room"), "{filtered}");
}

#[test]
fn only_home_clips_at_a_terminal_bound() {
    let machine = machine("[claude.alpha]\nhome = \"/srv/a/rather/long/account/home/alpha\"\n");
    let standing = AccountStanding::machine_only(&machine);
    let rows = rows_at(&machine, &standing, None, &BTreeMap::new());
    let deciding = [("claude", Deciding::Machine), ("codex", Deciding::Machine)];
    let full = text(&rows, &deciding, false, None);
    let alpha = |text: &str| -> String {
        table(text)
            .into_iter()
            .find(|line| line.contains("alpha"))
            .unwrap()
            .to_owned()
    };
    assert!(
        alpha(&full).ends_with("  /srv/a/rather/long/account/home/alpha"),
        "{full}"
    );
    let bound = alpha(&full).chars().count() - 10;
    let clipped = text(&rows, &deciding, false, Some(bound));
    assert_eq!(alpha(&clipped).chars().count(), bound, "{clipped}");
    let home = alpha(&full).find("/srv").unwrap();
    assert_eq!(alpha(&clipped)[..home], alpha(&full)[..home], "{clipped}");
    assert!(alpha(&clipped).ends_with('…'), "{clipped}");
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
fn removed_notice_warns_about_live_room_defaults() {
    for (live, warning) in [
        (
            vec!["rimz-one".to_owned()],
            "warning: room rimz-one selects it as the default for new claude launches; add the account back or run `rimz accounts use claude default` inside each room",
        ),
        (
            vec!["rimz-one".to_owned(), "rimz-two".to_owned()],
            "warning: rooms rimz-one, rimz-two select it as the default for new claude launches; add the account back or run `rimz accounts use claude default` inside each room",
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
    let work = parse_account_flag("claude=work").expect("claude=work");
    let default = parse_account_flag("codex=default").expect("codex=default");
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
