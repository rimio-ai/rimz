use jiff::{SignedDuration, Timestamp};

use crate::agents::context::WindowAmount;
use crate::agents::{Entitlement, ExtraCredits, RateLimitWindow, RateLimitWindowScope};
use crate::store::snapshot::{
    CreditDisplay, ProviderBar, ProviderBudget, SidebarProviderPanel, WindowReading,
};

fn now() -> Timestamp {
    Timestamp::from_second(1_800_000_000).unwrap()
}

fn panel(metered: bool) -> SidebarProviderPanel {
    serde_json::from_value(serde_json::json!({
        "kind": "claude",
        "product_name": "Claude",
        "art": [],
        "color": 1,
        "metered": metered,
    }))
    .unwrap()
}

fn window(mins: u32, used: Option<u8>) -> RateLimitWindow {
    RateLimitWindow {
        used_percentage: used,
        resets_at: Some(now() + SignedDuration::from_hours(1)),
        duration_mins: Some(mins),
        ..Default::default()
    }
}

fn amount(mins: u32, used: u64, limit: u64) -> RateLimitWindow {
    let amount = WindowAmount { used, limit };
    RateLimitWindow {
        amount: Some(amount),
        ..window(mins, Some(amount.used_percentage()))
    }
}

fn scoped(id: &str, mins: Option<u32>, used: Option<u8>) -> RateLimitWindow {
    RateLimitWindow {
        scope: Some(RateLimitWindowScope {
            id: id.to_owned(),
            label: id.to_owned(),
        }),
        duration_mins: mins,
        ..window(0, used)
    }
}

const FIVE_HOURS: u32 = 5 * 60;
const WEEK: u32 = 7 * 24 * 60;
const MONTH: u32 = 30 * 24 * 60;

fn bars(panel: &SidebarProviderPanel) -> Vec<ProviderBar<'_>> {
    match panel.budget(now()) {
        ProviderBudget::Bars(bars) => bars,
        other => panic!("expected bars, got {other:?}"),
    }
}

/// Each bar as its window's duration (0 for the `ex` row) and its reading.
fn rows(panel: &SidebarProviderPanel) -> Vec<(u32, Option<WindowReading>)> {
    bars(panel)
        .into_iter()
        .map(|bar| match bar {
            ProviderBar::Window { window, reading } => {
                (window.duration_mins.unwrap_or(0), Some(reading))
            }
            ProviderBar::Extra(_) => (0, None),
        })
        .collect()
}

fn durations(panel: &SidebarProviderPanel) -> Vec<u32> {
    rows(panel).into_iter().map(|(mins, _)| mins).collect()
}

fn reading(panel: &SidebarProviderPanel, mins: u32) -> WindowReading {
    rows(panel)
        .into_iter()
        .find_map(|(row_mins, reading)| (row_mins == mins).then_some(reading).flatten())
        .unwrap_or_else(|| panic!("no {mins}-minute bar"))
}

fn draining(reading: WindowReading) -> (u8, Option<Timestamp>, Vec<u8>, Option<u8>) {
    match reading {
        WindowReading::Draining {
            remaining,
            resets_at,
            sub_caps,
            ceiling,
            ..
        } => (remaining, resets_at, sub_caps, ceiling),
        other => panic!("expected a draining bar, got {other:?}"),
    }
}

fn available() -> Option<ExtraCredits> {
    Some(ExtraCredits::known(Some(7.0), None, Some(50.0)))
}

#[test]
fn budget_precedence_runs_undecided_lapsed_api_pending_then_bars() {
    let mut panel = panel(false);
    panel.undecided = true;
    panel.entitlement = Entitlement::Lapsed {
        since_ms: 1_700_000_000_000,
    };
    panel.windows = vec![window(FIVE_HOURS, Some(10))];
    assert_eq!(panel.budget(now()), ProviderBudget::Undecided);

    panel.undecided = false;
    assert_eq!(
        panel.budget(now()),
        ProviderBudget::Lapsed {
            since: Some(Timestamp::from_millisecond(1_700_000_000_000).unwrap())
        }
    );
    panel.entitlement = Entitlement::Lapsed { since_ms: u64::MAX };
    assert_eq!(panel.budget(now()), ProviderBudget::Lapsed { since: None });

    panel.entitlement = Entitlement::Ok;
    assert_eq!(
        panel.budget(now()),
        ProviderBudget::Api(CreditDisplay::Missing),
        "an unmetered panel reads credit even beside windows"
    );

    panel.metered = true;
    panel.windows.clear();
    assert_eq!(panel.budget(now()), ProviderBudget::Pending(&[]));
    panel.window_placeholders = vec!["5h".to_owned(), "7d".to_owned()];
    assert_eq!(
        panel.budget(now()),
        ProviderBudget::Pending(&["5h".to_owned(), "7d".to_owned()])
    );

    panel.windows = vec![scoped("model:fable", Some(WEEK), Some(10))];
    assert_eq!(
        panel.budget(now()),
        ProviderBudget::Bars(Vec::new()),
        "a panel of sub-caps alone draws no bar"
    );
}

#[test]
fn sub_caps_partition_their_parent_and_tick_its_remaining() {
    let mut panel = panel(true);
    panel.windows = vec![
        window(FIVE_HOURS, Some(25)),
        window(WEEK, Some(40)),
        scoped("model:fable", Some(WEEK), Some(58)),
        scoped("model:sonnet", Some(WEEK), Some(130)),
        scoped("model:opus", Some(WEEK), None),
    ];
    assert_eq!(durations(&panel), [FIVE_HOURS, WEEK]);
    assert_eq!(draining(reading(&panel, FIVE_HOURS)).2, Vec::<u8>::new());
    assert_eq!(
        draining(reading(&panel, WEEK)).2,
        [42, 0],
        "panel order, saturating, unread caps dropped"
    );
}

#[test]
fn lifted_windows_yield_to_an_enforced_candidate_and_read_lifted_alone() {
    let mut panel = panel(true);
    panel.windows = vec![
        RateLimitWindow {
            lifted: true,
            ..window(FIVE_HOURS, None)
        },
        window(WEEK, Some(30)),
    ];
    assert_eq!(durations(&panel), [WEEK]);

    panel.windows.pop();
    panel
        .windows
        .push(scoped("model:fable", Some(FIVE_HOURS), Some(50)));
    assert_eq!(
        rows(&panel),
        [(FIVE_HOURS, Some(WindowReading::Lifted))],
        "a lifted bar takes no sub-cap tick"
    );
}

#[test]
fn a_scoped_candidate_lists_every_window_without_substitution() {
    let mut panel = panel(true);
    panel.extra_credits = available();
    panel.windows = vec![
        window(FIVE_HOURS, Some(100)),
        window(WEEK, Some(10)),
        window(MONTH, Some(10)),
        scoped("premium", None, Some(100)),
    ];
    let rows = rows(&panel);
    assert_eq!(rows.len(), 4, "every candidate keeps a row, no ex");
    assert!(rows.iter().all(|(_, reading)| reading.is_some()));
}

#[test]
fn the_longest_spent_window_binds_and_leads_only_available_credit() {
    let mut panel = panel(true);
    panel.windows = vec![
        window(FIVE_HOURS, Some(5)),
        window(WEEK, Some(100)),
        window(MONTH, Some(10)),
    ];
    for credits in [None, Some(ExtraCredits::Disabled)] {
        panel.extra_credits = credits;
        assert_eq!(durations(&panel), [FIVE_HOURS, WEEK]);
    }
    panel.extra_credits = available();
    let bars = bars(&panel);
    assert!(matches!(
        bars.as_slice(),
        [
            ProviderBar::Window { window, .. },
            ProviderBar::Extra(CreditDisplay::Drain { .. }),
        ] if window.duration_mins == Some(WEEK)
    ));

    panel.extra_credits = None;
    panel.windows[0].used_percentage = Some(100);
    panel.windows[1].used_percentage = Some(10);
    assert_eq!(
        durations(&panel),
        [FIVE_HOURS, WEEK],
        "a spent first window keeps the pair"
    );
    panel.windows.truncate(1);
    assert_eq!(durations(&panel), [FIVE_HOURS], "one window, one row");
}

#[test]
fn ceilings_take_the_tightest_absolute_remainder_of_longer_windows() {
    let mut panel = panel(true);
    panel.windows = vec![
        amount(FIVE_HOURS, 20, 100),
        amount(WEEK, 150, 200),
        amount(MONTH, 170, 200),
    ];
    assert_eq!(draining(reading(&panel, FIVE_HOURS)).3, Some(30));
    assert_eq!(
        draining(reading(&panel, WEEK)).3,
        Some(15),
        "the month caps the week against the week's own limit"
    );

    panel.windows[2] = amount(MONTH, 199_999_999_999_999, 200_000_000_000_000);
    panel.windows[0] = amount(FIVE_HOURS, 0, u64::MAX / 2);
    assert_eq!(
        draining(reading(&panel, FIVE_HOURS)).3,
        Some(1),
        "u128 comparisons, floored at one percent"
    );

    panel.windows[0] = amount(FIVE_HOURS, 20, 100);
    panel.windows.truncate(2);
    panel.windows[1].lifted = true;
    assert_eq!(
        draining(reading(&panel, FIVE_HOURS)).3,
        None,
        "lifted longer"
    );
    panel.windows[1] = window(WEEK, Some(75));
    assert_eq!(draining(reading(&panel, FIVE_HOURS)).3, None, "no amount");
}

#[test]
fn a_spent_longer_window_forces_a_shorter_one_spent() {
    let mut panel = panel(true);
    panel.windows = vec![
        amount(FIVE_HOURS, 20, 100),
        window(WEEK, Some(100)),
        amount(MONTH, 150, 200),
    ];
    let forced = reading(&panel, FIVE_HOURS);
    assert!(matches!(forced, WindowReading::Draining { pace: None, .. }));
    assert_eq!(draining(forced), (0, None, Vec::new(), None));

    panel.windows[0] = window(FIVE_HOURS, None);
    assert_eq!(
        draining(reading(&panel, FIVE_HOURS)).0,
        0,
        "unknown reads spent"
    );
    panel.windows[1].used_percentage = Some(40);
    assert_eq!(reading(&panel, FIVE_HOURS), WindowReading::Unknown);

    // Any longer unscoped window counts, a lifted one included.
    panel.windows[1] = RateLimitWindow {
        lifted: true,
        ..window(WEEK, Some(100))
    };
    panel.windows[0] = window(FIVE_HOURS, Some(10));
    assert_eq!(draining(reading(&panel, FIVE_HOURS)).0, 0);
}

#[test]
fn countdown_and_pace_show_only_on_a_started_window() {
    let mut panel = panel(true);
    let fresh = RateLimitWindow {
        resets_at: Some(now() + SignedDuration::from_mins(i64::from(FIVE_HOURS))),
        ..window(FIVE_HOURS, Some(1))
    };
    assert!(fresh.not_started(now()));
    panel.windows = vec![fresh];
    assert!(matches!(
        reading(&panel, FIVE_HOURS),
        WindowReading::Draining {
            remaining: 100,
            resets_at: None,
            pace: None,
            ..
        }
    ));

    let started = window(FIVE_HOURS, Some(60));
    let pace = started.pace(now());
    assert!(pace.is_some());
    panel.windows = vec![started];
    assert_eq!(
        reading(&panel, FIVE_HOURS),
        WindowReading::Draining {
            remaining: 40,
            resets_at: Some(now() + SignedDuration::from_hours(1)),
            pace,
            sub_caps: Vec::new(),
            ceiling: None,
        }
    );

    panel.windows[0].resets_at = None;
    assert!(matches!(
        reading(&panel, FIVE_HOURS),
        WindowReading::Draining { pace: None, .. }
    ));
}

#[test]
fn credit_display_classifies_each_state_and_its_availability() {
    let cases = [
        (None, CreditDisplay::Missing, false),
        (Some(ExtraCredits::Disabled), CreditDisplay::Off, false),
        (
            Some(ExtraCredits::Unlimited),
            CreditDisplay::Unlimited,
            true,
        ),
        (
            Some(ExtraCredits::known(None, None, None)),
            CreditDisplay::Unlimited,
            true,
        ),
        (
            Some(ExtraCredits::known(None, None, Some(50.0))),
            CreditDisplay::Unknown,
            false,
        ),
        (
            Some(ExtraCredits::known(None, Some(12.0), None)),
            CreditDisplay::Balance(12.0),
            true,
        ),
        (
            Some(ExtraCredits::known(None, Some(0.0), None)),
            CreditDisplay::Balance(0.0),
            false,
        ),
        (
            Some(ExtraCredits::known(Some(45.0), None, Some(50.0))),
            CreditDisplay::Drain {
                left: 5.0,
                percentage: 10,
            },
            true,
        ),
        (
            Some(ExtraCredits::known(None, Some(0.2), Some(50.0))),
            CreditDisplay::Drain {
                left: 0.2,
                percentage: 1,
            },
            true,
        ),
        (
            Some(ExtraCredits::known(Some(50.0), None, Some(50.0))),
            CreditDisplay::Drain {
                left: 0.0,
                percentage: 0,
            },
            false,
        ),
    ];
    for (credits, display, available) in cases {
        let classified = CreditDisplay::classify(credits.as_ref());
        assert_eq!(classified, display, "{credits:?}");
        assert_eq!(classified.is_available(), available, "{credits:?}");
    }
}

#[test]
fn an_alternate_credential_reads_its_own_windows() {
    let mut top = panel(true);
    top.sub_provider = Some("anthropic".to_owned());
    top.windows = vec![window(FIVE_HOURS, Some(10))];
    let mut alternate = panel(true);
    alternate.sub_provider = Some("openai".to_owned());
    alternate.windows = vec![window(WEEK, Some(100))];
    top.alternates = vec![alternate];

    let shown = top.for_sub_provider("openai").unwrap();
    assert_eq!(durations(shown), [WEEK]);
    assert_eq!(draining(reading(shown, WEEK)).0, 0);
    assert_eq!(durations(&top), [FIVE_HOURS]);
}
