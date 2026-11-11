//! Judge budget refusals: an account over its day cap, parked, spent, or short of surplus.

use crate::config::MachineConfig;
use crate::disk::paths::StatePaths;

#[test]
fn a_judges_account_over_its_day_cap_or_parked_is_refused_before_it_spawns() {
    use crate::agents::spending::{
        ProviderSpendingCache, SpendWindow, write_provider_spending_cache,
    };
    use crate::harness::budget::{BudgetParkStamp, DailyBudgetLedger, DailyBudgetScope};
    let now: jiff::Timestamp = "2026-06-02T12:00:00Z".parse().unwrap();
    for parked in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let state = StatePaths::for_project_root(root.path()).unwrap();
        let runtime = crate::disk::paths::RuntimePaths::for_state(&state).unwrap();
        runtime.ensure_dirs().unwrap();
        let config = MachineConfig {
            timezone: Some("UTC".into()),
            accounts: toml::from_str("[budget]\ncodex = '10/day'\n").unwrap(),
            ..Default::default()
        };
        let profile = crate::harness::headless::HeadlessProfile::resolve(
            &config,
            "codex",
            root.path(),
            &state,
            &runtime,
            now,
        )
        .unwrap();
        let key = crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("codex"));
        write_provider_spending_cache(
            &runtime.shared_provider_spending_path(),
            &ProviderSpendingCache {
                day_cutoff_secs: jiff::civil::date(2026, 6, 2)
                    .to_zoned(jiff::tz::TimeZone::UTC)
                    .unwrap()
                    .timestamp()
                    .as_second() as u64,
                day_by_login: std::collections::BTreeMap::from([(
                    key.clone(),
                    SpendWindow {
                        usd: if parked { 0.0 } else { 12.0 },
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            },
        );
        if parked {
            DailyBudgetScope::Account(key)
                .write_ledger(
                    &runtime,
                    &state,
                    &DailyBudgetLedger {
                        parked: Some(BudgetParkStamp {
                            at_cost: 12.0,
                            at: now,
                        }),
                        ..Default::default()
                    },
                )
                .unwrap();
        }
        let refusal = super::super::script::budget_refusal(
            &config,
            &profile,
            &crate::agents::ManagedLaunchState::Unsupported,
            None,
            None,
            &state,
            &runtime,
            now,
        );
        assert!(
            refusal
                .as_deref()
                .is_some_and(|reason| reason.contains("codex")),
            "parked={parked}: {refusal:?}"
        );
    }
}

#[test]
fn a_judge_on_a_spent_quota_binding_or_short_of_its_tasks_surplus_is_refused() {
    use crate::agents::account::AccountUsageKey;
    use crate::agents::{
        AgentRateLimits, ManagedLaunchState, ProviderAccountBinding, ProviderAccountScope,
        RateLimitCacheEntry, RateLimitWindow, RateLimitsCache,
    };
    let now = jiff::Timestamp::now();
    let root = tempfile::tempdir().unwrap();
    let state = StatePaths::for_project_root(root.path()).unwrap();
    let runtime = crate::disk::paths::RuntimePaths::for_state(&state).unwrap();
    runtime.ensure_dirs().unwrap();
    let config = MachineConfig::default();
    let profile = crate::harness::headless::HeadlessProfile::resolve(
        &config,
        "codex",
        root.path(),
        &state,
        &runtime,
        now,
    )
    .unwrap();
    let key = crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("codex"));
    let window = |used, resets_in: i64, mins| RateLimitWindow {
        used_percentage: Some(used),
        resets_at: Some(now + jiff::SignedDuration::from_secs(resets_in)),
        duration_mins: Some(mins),
        ..RateLimitWindow::default()
    };
    let scope = ProviderAccountScope::sub_provider("openai", "oauth");
    let binding = ProviderAccountBinding::decode(
        r#"{"scope":{"kind":"sub_provider","provider":"openai","variant":"oauth"},"account_key":"owner"}"#,
    )
    .unwrap();
    let week = 7 * 24 * 60;
    let entries = std::collections::BTreeMap::from([
        (
            AccountUsageKey::new(key.clone(), scope.clone()),
            RateLimitCacheEntry {
                scope,
                account_key: Some("owner".to_owned()),
                limits: AgentRateLimits {
                    windows: vec![window(100, 3_600, 300)],
                },
                ..Default::default()
            },
        ),
        (
            key.clone().into(),
            RateLimitCacheEntry {
                limits: AgentRateLimits {
                    windows: vec![window(90, i64::from(week) * 60 / 2, week)],
                },
                ..Default::default()
            },
        ),
    ]);
    crate::disk::atomic::write_temp_then_rename_cache(
        &RateLimitsCache::shared_path(&runtime),
        &RateLimitsCache {
            entries,
            ..Default::default()
        },
    )
    .unwrap();
    let refuse = |managed: &ManagedLaunchState, surplus| {
        super::super::script::budget_refusal(
            &config, &profile, managed, surplus, None, &state, &runtime, now,
        )
    };
    let quota = refuse(&ManagedLaunchState::Bound(binding), None);
    assert!(
        quota
            .as_deref()
            .is_some_and(|reason| reason.contains("exhausted")),
        "{quota:?}"
    );
    let surplus = refuse(&ManagedLaunchState::Unsupported, Some("1.5x"));
    assert!(
        surplus
            .as_deref()
            .is_some_and(|reason| reason.contains("window surplus")),
        "{surplus:?}"
    );
    assert_eq!(
        refuse(&ManagedLaunchState::Unsupported, None),
        None,
        "a kind-wide account has no bound quota, and surplus is read only for a run that keeps it"
    );
}
