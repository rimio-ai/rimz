use super::*;
use jiff::{SignedDuration, tz::TimeZone};
use rimz::agents::{AgentAccount, SpendWindow};
use rimz::{store::snapshot::RemoteControlBadge, store::snapshot::SidebarProviderPanel};

fn record(probed_at_ms: u64, ok: bool, account: Option<AgentAccount>) -> ProviderRecord {
    ProviderRecord {
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
        kind: kind.to_owned(),
        account_scope: ProviderAccountScope::KindWide,
        product_name: kind.to_owned(),
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
        window_placeholders: Vec::new(),
        windows: Vec::new(),
    }
}

fn account_fixture() -> AccountsCache {
    AccountsCache {
        providers: BTreeMap::from([
            (
                "claude".to_owned(),
                record(1_000, true, Some(account("max", true))),
            ),
            ("codex".to_owned(), record(2_000, true, None)),
            (
                "copilot".to_owned(),
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
                "pi".to_owned(),
                record(4_000, true, Some(account("openai-oauth", false))),
            ),
        ]),
    }
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
    let reports = assemble_reports(&accounts, vec![panel("claude")], &spending, None, false);

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

    let filtered = assemble_reports(&accounts, Vec::new(), &spending, Some("pi"), false);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].kind, "pi");
    assert!(assemble_reports(&accounts, Vec::new(), &spending, Some("codex"), false).is_empty());
    let logged_out = assemble_reports(&accounts, Vec::new(), &spending, Some("codex"), true);
    assert_eq!(logged_out.len(), 1);
    assert_eq!(logged_out[0].status, ProviderStatus::LoggedOut);

    let all = assemble_reports(&accounts, Vec::new(), &spending, None, true);
    assert_eq!(all.len(), rimz::agents::known_kinds().count());
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
        providers: BTreeMap::from([(
            "claude".to_owned(),
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
    let reports = assemble_reports(&accounts, vec![panel], &spending, None, false);
    let mut out = anstream::StripStream::new(Vec::new());
    let time_zone = TimeZone::get("America/New_York").unwrap();
    write_pretty(&mut out, &reports, now, &time_zone).unwrap();
    let pretty = String::from_utf8(out.into_inner()).unwrap();
    insta::assert_snapshot!("provider_report_pretty", pretty);

    let json = serde_json::to_string_pretty(&reports).unwrap();
    insta::assert_snapshot!("provider_report_json", json);
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
            },
            now,
        ),
        "  resets: 2 credits\n          - 2023-11-14 22:13:19 +00:00 · due\n          - 2023-11-14 22:13:20 +00:00 · due\n"
    );
}

#[test]
fn provider_money_style_covers_only_dollar_tokens() {
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    let (accounts, panel, spending) = protocol_fixture(now);
    let reports = assemble_reports(&accounts, vec![panel], &spending, None, false);
    let mut raw = Vec::new();
    write_pretty(&mut raw, &reports, now, &TimeZone::UTC).unwrap();
    let raw = String::from_utf8(raw).unwrap();
    let style = render::palette::money();
    let painted = |value: &str| format!("{}{value}{}", style.render(), style.render_reset());

    assert!(
        raw.contains(&format!(
            "{} used · {} limit",
            painted("$12.40"),
            painted("$50.00")
        )),
        "{raw:?}"
    );
    assert!(
        raw.contains(&format!(
            "7d {} · 30d {}",
            painted("$31.20"),
            painted("$118.75")
        )),
        "{raw:?}"
    );
    assert!(
        raw.contains(&format!(
            "{} of {}/day",
            painted("$8.10"),
            painted("$25.00")
        )),
        "{raw:?}"
    );
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
        window_value(&ready, now).as_deref(),
        Some("1% used · ready")
    );
    assert_eq!(
        window_value(
            &RateLimitWindow {
                lifted: true,
                ..Default::default()
            },
            now
        )
        .as_deref(),
        Some("∞")
    );
    assert_eq!(window_value(&RateLimitWindow::default(), now), None);
}
