use super::*;

use crate::agents::context::WindowSource;
use crate::ids::WorkspaceId;
use crate::sidebar::test_support::{
    provider_panel, rl_window, rl_window_mins, snapshot_with_panels,
};
use jiff::SignedDuration;

fn runtime() -> (tempfile::TempDir, WorkspaceId, RuntimePaths) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    (dir, workspace, runtime)
}

fn authoritative(mut window: RateLimitWindow) -> RateLimitWindow {
    window.source = WindowSource::Authoritative;
    window
}

fn kind_wide_cache(
    refreshed_at_ms: u64,
    windows: BTreeMap<String, AgentRateLimits>,
    mut pending: BTreeMap<String, Vec<PendingRefill>>,
) -> RateLimitsCache {
    let mut entries = BTreeMap::new();
    for (kind, limits) in windows {
        let entry = RateLimitCacheEntry {
            scope: Default::default(),
            account_key: None,
            limits,
            pending: pending.remove(&kind).unwrap_or_default(),
            unknown_since_ms: None,
        };
        entries.insert(kind, entry);
    }
    entries.extend(pending.into_iter().map(|(kind, pending)| {
        (
            kind,
            RateLimitCacheEntry {
                pending,
                ..Default::default()
            },
        )
    }));
    RateLimitsCache {
        refreshed_at_ms,
        entries,
        ..Default::default()
    }
}

fn cache_window<'a>(
    cache: &'a RateLimitsCache,
    kind: &str,
    key: RateLimitWindowKey,
) -> &'a RateLimitWindow {
    cache.entries[kind]
        .limits
        .windows
        .iter()
        .find(|window| window.key() == key)
        .unwrap()
}

fn scoped_window(id: &str, label: &str, used: u8, reset: Timestamp) -> RateLimitWindow {
    RateLimitWindow {
        scope: Some(crate::agents::RateLimitWindowScope {
            id: id.to_owned(),
            label: label.to_owned(),
        }),
        used_percentage: Some(used),
        resets_at: Some(reset),
        source: WindowSource::Authoritative,
        ..Default::default()
    }
}

fn write_claude_windows(runtime: &RuntimePaths, windows: Vec<RateLimitWindow>) {
    write_rate_limits_cache(
        &runtime.shared_rate_limits_path(),
        &kind_wide_cache(
            1,
            BTreeMap::from([("claude".to_owned(), AgentRateLimits { windows })]),
            BTreeMap::new(),
        ),
    );
}

#[test]
fn scoped_quota_windows_fuse_and_expire_independently() {
    let (_dir, workspace, runtime) = runtime();
    let past = Timestamp::from_second(1_000_000_000).unwrap();
    let future = Timestamp::from_second(4_000_000_000).unwrap();
    let mut producer = snapshot_with_panels(
        workspace.clone(),
        vec![provider_panel(
            "plugin",
            vec![
                scoped_window("build_minutes", "bld", 20, future),
                scoped_window("deployments", "dep", 70, future),
            ],
        )],
    );
    apply_rate_limit_cache(&mut producer, &runtime, true);
    let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    assert_eq!(
        cache_window(
            &cache,
            "plugin",
            RateLimitWindowKey::Scope("build_minutes".to_owned()),
        )
        .used_percentage,
        Some(20)
    );
    assert_eq!(
        cache_window(
            &cache,
            "plugin",
            RateLimitWindowKey::Scope("deployments".to_owned()),
        )
        .used_percentage,
        Some(70)
    );
    write_rate_limits_cache(
        &runtime.shared_rate_limits_path(),
        &kind_wide_cache(
            2,
            BTreeMap::from([(
                "plugin".to_owned(),
                AgentRateLimits {
                    windows: vec![
                        scoped_window("build_minutes", "bld", 20, past),
                        scoped_window("deployments", "dep", 40, future),
                    ],
                },
            )]),
            BTreeMap::new(),
        ),
    );
    let mut consumer = snapshot_with_panels(workspace, vec![provider_panel("plugin", Vec::new())]);
    apply_rate_limit_cache(&mut consumer, &runtime, false);
    let build = consumer.providers[0]
        .windows
        .iter()
        .find(|window| window.key() == RateLimitWindowKey::Scope("build_minutes".to_owned()))
        .unwrap();
    assert_eq!(build.scope.as_ref().unwrap().label, "bld");
    assert_eq!((build.used_percentage, build.resets_at), (None, None));
    let deployments = consumer.providers[0]
        .windows
        .iter()
        .find(|window| window.key() == RateLimitWindowKey::Scope("deployments".to_owned()))
        .unwrap();
    assert_eq!(deployments.scope.as_ref().unwrap().label, "dep");
    assert_eq!(deployments.used_percentage, Some(40));
    assert_eq!(deployments.resets_at, Some(future));
}
#[test]
fn producer_persisted_windows_feed_idle_consumers() {
    let (_dir, workspace, runtime) = runtime();
    let future = Timestamp::from_second(4_000_000_000).unwrap();
    let mut producer = snapshot_with_panels(
        workspace.clone(),
        vec![provider_panel("claude", vec![rl_window(60, Some(future))])],
    );
    apply_rate_limit_cache(&mut producer, &runtime, true);
    let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    assert_eq!(
        cache_window(&cache, "claude", RateLimitWindowKey::Duration(Some(300))).used_percentage,
        Some(60)
    );
    let mut consumer = snapshot_with_panels(workspace, vec![provider_panel("claude", Vec::new())]);
    apply_rate_limit_cache(&mut consumer, &runtime, false);
    assert_eq!(consumer.providers[0].windows[0].used_percentage, Some(60));
}

#[test]
fn authoritative_publication_survives_live_session_exit() {
    let (_dir, workspace, runtime) = runtime();
    let future = Timestamp::from_second(4_000_000_000).unwrap();
    let identity = AccountUsageIdentity {
        account_key: Some("claude-account".to_owned()),
        ..Default::default()
    };
    merge_account_rate_limits(
        &runtime,
        "claude",
        identity.clone(),
        AgentRateLimits {
            windows: vec![
                authoritative(rl_window_mins(0, None, 300)),
                authoritative(rl_window_mins(0, None, 10_080)),
            ],
        },
    );
    merge_account_rate_limits(
        &runtime,
        "claude",
        identity,
        AgentRateLimits {
            windows: vec![
                authoritative(rl_window_mins(36, Some(future), 300)),
                authoritative(rl_window_mins(4, Some(future), 10_080)),
            ],
        },
    );

    let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    assert_eq!(
        cache.entries["claude"].account_key.as_deref(),
        Some("claude-account")
    );
    assert_eq!(
        cache_window(&cache, "claude", RateLimitWindowKey::Duration(Some(300))).used_percentage,
        Some(36)
    );
    assert_eq!(
        cache_window(&cache, "claude", RateLimitWindowKey::Duration(Some(10_080)),).used_percentage,
        Some(4)
    );

    let mut idle = snapshot_with_panels(workspace, vec![provider_panel("claude", Vec::new())]);
    apply_rate_limit_cache(&mut idle, &runtime, false);
    assert_eq!(
        idle.providers[0]
            .windows
            .iter()
            .map(|window| (window.used_percentage, window.resets_at))
            .collect::<Vec<_>>(),
        [(Some(36), Some(future)), (Some(4), Some(future))]
    );
}

#[test]
fn account_scope_isolates_cached_windows() {
    let (_dir, workspace, runtime) = runtime();
    let international = ProviderAccountScope::sub_provider("alibaba", "international");
    let china = ProviderAccountScope::sub_provider("alibaba", "china");
    write_rate_limits_cache(
        &runtime.shared_rate_limits_path(),
        &RateLimitsCache {
            entries: BTreeMap::from([(
                "qwen".to_owned(),
                RateLimitCacheEntry {
                    scope: international.clone(),
                    account_key: None,
                    limits: AgentRateLimits {
                        windows: vec![
                            rl_window_mins(20, None, 300),
                            rl_window_mins(40, None, 10_080),
                            rl_window_mins(60, None, 43_200),
                        ],
                    },
                    pending: Vec::new(),
                    unknown_since_ms: None,
                },
            )]),
            ..Default::default()
        },
    );
    let mut matching = provider_panel("qwen", Vec::new());
    matching.account_scope = international.clone();
    let mut matching = snapshot_with_panels(workspace.clone(), vec![matching]);
    apply_rate_limit_cache(&mut matching, &runtime, false);
    assert_eq!(
        matching.providers[0]
            .windows
            .iter()
            .map(|window| window.duration_mins)
            .collect::<Vec<_>>(),
        [Some(300), Some(10_080), Some(43_200)]
    );
    let mut mismatched = provider_panel("qwen", Vec::new());
    mismatched.account_scope = china.clone();
    let mut mismatched = snapshot_with_panels(workspace.clone(), vec![mismatched]);
    apply_rate_limit_cache(&mut mismatched, &runtime, false);
    assert!(mismatched.providers[0].windows.is_empty());
    assert_eq!(
        read_rate_limits_cache(&runtime.shared_rate_limits_path()).entries["qwen"].scope,
        international
    );

    let mut switched = provider_panel("qwen", vec![rl_window_mins(55, None, 43_200)]);
    switched.account_scope = china.clone();
    let mut switched = snapshot_with_panels(workspace, vec![switched]);
    apply_rate_limit_cache(&mut switched, &runtime, true);
    let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    assert_eq!(cache.entries["qwen"].scope, china);
    assert_eq!(cache.entries["qwen"].limits.windows.len(), 1);
    assert_eq!(
        cache.entries["qwen"].limits.windows[0].duration_mins,
        Some(43_200)
    );
}
#[test]
fn reset_epoch_invalidates_oauth_usage_throttle() {
    let (_dir, workspace, runtime) = runtime();
    let old_reset = Timestamp::from_second(2_000_000_000).unwrap();
    let new_reset = old_reset + SignedDuration::from_secs(7_200);
    write_rate_limits_cache(
        &runtime.shared_rate_limits_path(),
        &kind_wide_cache(
            1,
            BTreeMap::from([(
                "codex".to_owned(),
                AgentRateLimits {
                    windows: vec![rl_window(80, Some(old_reset))],
                },
            )]),
            BTreeMap::new(),
        ),
    );
    crate::sidebar::refresh::credits::merge_provider_credits_entry(
        &runtime,
        "codex",
        crate::sidebar::refresh::credits::ProviderCreditsEntry {
            scope: Default::default(),
            observed_at_ms: 1,
            oauth_read_at_ms: 1234,
            auth_settled: false,
            credentials_stamp: None,
            account_key: None,
            plan: None,
            ok: true,
            extra_credits: Some(crate::agents::ExtraCredits::known(None, Some(4.0), None)),
            reset_credits: None,
            direct_query_claim: Some(crate::sidebar::refresh::credits::DirectQueryClaim {
                nonce: uuid::Uuid::nil(),
                claimed_at_ms: 1,
                requested_scope: Default::default(),
                credentials_stamp: None,
                preflight_account_key: None,
            }),
        },
    );
    let mut frame = snapshot_with_panels(
        workspace,
        vec![provider_panel("codex", vec![rl_window(1, Some(new_reset))])],
    );
    apply_rate_limit_cache(&mut frame, &runtime, true);

    let credits =
        crate::sidebar::refresh::credits::read_credits_cache(&runtime.shared_credits_path());
    assert_eq!(credits.entries["codex"].oauth_read_at_ms, 0);
    assert_eq!(credits.entries["codex"].direct_query_claim, None);
}

/// A settled OAuth read inside its one-hour ceiling — the state that holds the
/// account probe off until the unknown display forces it.
fn seed_settled_credits(runtime: &RuntimePaths, kind: &str, oauth_read_at_ms: u64) {
    crate::sidebar::refresh::credits::merge_provider_credits_entry(
        runtime,
        kind,
        crate::sidebar::refresh::credits::ProviderCreditsEntry {
            scope: Default::default(),
            observed_at_ms: oauth_read_at_ms,
            oauth_read_at_ms,
            auth_settled: true,
            credentials_stamp: None,
            account_key: None,
            plan: None,
            ok: true,
            extra_credits: None,
            reset_credits: None,
            direct_query_claim: None,
        },
    );
}

fn oauth_read_at_ms(runtime: &RuntimePaths, kind: &str) -> u64 {
    crate::sidebar::refresh::credits::read_credits_cache(&runtime.shared_credits_path()).entries
        [kind]
        .oauth_read_at_ms
}

fn unknown_since_ms(runtime: &RuntimePaths, kind: &str) -> Option<u64> {
    read_rate_limits_cache(&runtime.shared_rate_limits_path())
        .entries
        .get(kind)
        .and_then(|entry| entry.unknown_since_ms)
}

#[test]
fn unknown_display_forces_immediate_account_refresh() {
    let (_dir, workspace, runtime) = runtime();
    let past = Timestamp::from_second(1_000_000_000).unwrap();
    write_claude_windows(&runtime, vec![rl_window_mins(90, Some(past), 300)]);
    seed_settled_credits(&runtime, "claude", 1_700_000_000_000);

    let mut frame = snapshot_with_panels(workspace, vec![provider_panel("claude", Vec::new())]);
    apply_rate_limit_cache(&mut frame, &runtime, true);

    // The display went unknown, so the settled read is dropped and the next
    // claim is due immediately rather than an hour from now.
    assert!(
        frame.providers[0]
            .windows
            .iter()
            .all(|window| window.used_percentage.is_none())
    );
    assert_eq!(oauth_read_at_ms(&runtime, "claude"), 0);
    assert!(unknown_since_ms(&runtime, "claude").is_some());
}

#[test]
fn unknown_display_forces_refresh_once_per_episode() {
    let (_dir, workspace, runtime) = runtime();
    let past = Timestamp::from_second(1_000_000_000).unwrap();
    write_claude_windows(&runtime, vec![rl_window_mins(90, Some(past), 300)]);
    seed_settled_credits(&runtime, "claude", 1_700_000_000_000);

    let mut first = snapshot_with_panels(
        workspace.clone(),
        vec![provider_panel("claude", Vec::new())],
    );
    apply_rate_limit_cache(&mut first, &runtime, true);
    assert_eq!(oauth_read_at_ms(&runtime, "claude"), 0);
    let forced_at = unknown_since_ms(&runtime, "claude").expect("episode marker");

    // Stand in for the forced probe completing: it restamps the read whatever
    // the outcome. A second unknown frame leaves that stamp alone, so a provider
    // with nothing to report costs one fetch rather than one per frame.
    seed_settled_credits(&runtime, "claude", 1_700_000_500_000);
    let mut second = snapshot_with_panels(workspace, vec![provider_panel("claude", Vec::new())]);
    apply_rate_limit_cache(&mut second, &runtime, true);

    assert_eq!(oauth_read_at_ms(&runtime, "claude"), 1_700_000_500_000);
    assert_eq!(unknown_since_ms(&runtime, "claude"), Some(forced_at));
}

#[test]
fn usable_window_rearms_the_unknown_refresh() {
    let (_dir, workspace, runtime) = runtime();
    let past = Timestamp::from_second(1_000_000_000).unwrap();
    let future = Timestamp::from_second(4_000_000_000).unwrap();
    write_claude_windows(&runtime, vec![rl_window_mins(90, Some(past), 300)]);
    seed_settled_credits(&runtime, "claude", 1_700_000_000_000);

    let mut unknown = snapshot_with_panels(
        workspace.clone(),
        vec![provider_panel("claude", Vec::new())],
    );
    apply_rate_limit_cache(&mut unknown, &runtime, true);
    assert!(unknown_since_ms(&runtime, "claude").is_some());

    // A live reading paints a real value again, closing the episode so the next
    // one forces its own fetch. This frame also advances the reset epoch, which
    // invalidates the read on its own account — the marker is what this asserts.
    seed_settled_credits(&runtime, "claude", 1_700_000_500_000);
    let mut known = snapshot_with_panels(
        workspace,
        vec![provider_panel(
            "claude",
            vec![rl_window_mins(35, Some(future), 300)],
        )],
    );
    apply_rate_limit_cache(&mut known, &runtime, true);

    assert_eq!(known.providers[0].windows[0].used_percentage, Some(35));
    assert_eq!(unknown_since_ms(&runtime, "claude"), None);
}

#[test]
fn cold_start_without_cached_windows_forces_refresh() {
    let (_dir, workspace, runtime) = runtime();
    seed_settled_credits(&runtime, "claude", 1_700_000_000_000);

    // No rate-limit cache at all: nothing expires, so the aged-out path never
    // trips, yet the dashboard is just as blank.
    let mut frame = snapshot_with_panels(workspace, vec![provider_panel("claude", Vec::new())]);
    apply_rate_limit_cache(&mut frame, &runtime, true);

    assert!(frame.providers[0].windows.is_empty());
    assert_eq!(oauth_read_at_ms(&runtime, "claude"), 0);
    assert!(unknown_since_ms(&runtime, "claude").is_some());
}

#[test]
fn elapsed_short_idle_window_shows_full_without_persisting_projection() {
    let (_dir, workspace, runtime) = runtime();
    let past = Timestamp::from_second(1_000_000_000).unwrap();
    let future = Timestamp::from_second(4_000_000_000).unwrap();
    write_claude_windows(
        &runtime,
        vec![
            rl_window_mins(90, Some(past), 300),
            rl_window_mins(70, Some(future), 10_080),
        ],
    );
    let mut idle = snapshot_with_panels(workspace, vec![provider_panel("claude", Vec::new())]);
    apply_rate_limit_cache(&mut idle, &runtime, true);
    assert_eq!(idle.providers[0].windows[0].used_percentage, Some(0));
    assert_eq!(idle.providers[0].windows[1].used_percentage, Some(70));

    let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    let raw = cache_window(&cache, "claude", RateLimitWindowKey::Duration(Some(300)));
    assert_eq!((raw.used_percentage, raw.resets_at), (Some(90), Some(past)));
}
#[test]
fn elapsed_long_live_window_rolls_forward_without_persisting_projection() {
    let (_dir, workspace, runtime) = runtime();
    let past = Timestamp::from_second(1_000_000_000).unwrap();
    let future = Timestamp::from_second(4_000_000_000).unwrap();
    let mut frame = snapshot_with_panels(
        workspace,
        vec![provider_panel(
            "claude",
            vec![
                rl_window_mins(40, Some(future), 300),
                rl_window_mins(80, Some(past), 10_080),
            ],
        )],
    );
    apply_rate_limit_cache(&mut frame, &runtime, true);
    assert_eq!(frame.providers[0].windows[0].used_percentage, Some(40));
    assert_eq!(frame.providers[0].windows[1].used_percentage, Some(0));
    assert!(
        frame.providers[0].windows[1]
            .resets_at
            .is_some_and(|reset| reset > frame.now)
    );

    let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    let raw = cache_window(&cache, "claude", RateLimitWindowKey::Duration(Some(10_080)));
    assert_eq!((raw.used_percentage, raw.resets_at), (Some(80), Some(past)));
}
#[test]
fn elapsed_longest_idle_cache_shows_unknown_without_persisting_projection() {
    let (_dir, workspace, runtime) = runtime();
    let past = Timestamp::from_second(1_000_000_000).unwrap();
    write_claude_windows(
        &runtime,
        vec![
            rl_window_mins(90, Some(past), 300),
            rl_window_mins(80, Some(past), 10_080),
        ],
    );
    let mut idle = snapshot_with_panels(workspace, vec![provider_panel("claude", Vec::new())]);
    apply_rate_limit_cache(&mut idle, &runtime, true);
    let shown = &idle.providers[0].windows;
    assert!(
        shown
            .iter()
            .all(|window| window.used_percentage.is_none() && window.resets_at.is_none())
    );
    assert_eq!(
        shown
            .iter()
            .map(|window| window.duration_mins)
            .collect::<Vec<_>>(),
        [Some(300), Some(10_080)]
    );

    let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    assert_eq!(
        cache_window(&cache, "claude", RateLimitWindowKey::Duration(Some(300))).used_percentage,
        Some(90)
    );
    assert_eq!(
        cache_window(&cache, "claude", RateLimitWindowKey::Duration(Some(10_080)),).used_percentage,
        Some(80)
    );
}

#[test]
fn elapsed_undated_cache_shows_unknown_and_opens_refresh_episode() {
    let (_dir, workspace, runtime) = runtime();
    let past = Timestamp::from_second(1_000_000_000).unwrap();
    let observed = |window| RateLimitWindow {
        observed_at: Some(past),
        ..authoritative(window)
    };
    write_claude_windows(
        &runtime,
        vec![
            observed(rl_window_mins(0, None, 300)),
            observed(rl_window_mins(0, None, 10_080)),
        ],
    );

    let mut idle = snapshot_with_panels(workspace, vec![provider_panel("claude", Vec::new())]);
    apply_rate_limit_cache(&mut idle, &runtime, true);

    assert!(
        idle.providers[0]
            .windows
            .iter()
            .all(|window| window.used_percentage.is_none() && window.resets_at.is_none())
    );
    assert!(unknown_since_ms(&runtime, "claude").is_some());
}

#[test]
fn dated_long_window_keeps_undated_lifted_window_cache_fresh() {
    let (_dir, workspace, runtime) = runtime();
    let past = Timestamp::from_second(1_000_000_000).unwrap();
    let future = Timestamp::from_second(4_000_000_000).unwrap();
    write_rate_limits_cache(
        &runtime.shared_rate_limits_path(),
        &kind_wide_cache(
            1,
            BTreeMap::from([(
                "codex".to_owned(),
                AgentRateLimits {
                    windows: vec![
                        RateLimitWindow {
                            used_percentage: None,
                            observed_at: Some(past),
                            source: WindowSource::Authoritative,
                            lifted: true,
                            ..rl_window_mins(0, None, 300)
                        },
                        RateLimitWindow {
                            observed_at: Some(past),
                            source: WindowSource::Authoritative,
                            ..rl_window_mins(4, Some(future), 10_080)
                        },
                    ],
                },
            )]),
            BTreeMap::new(),
        ),
    );

    let mut idle = snapshot_with_panels(workspace, vec![provider_panel("codex", Vec::new())]);
    apply_rate_limit_cache(&mut idle, &runtime, true);

    assert!(idle.providers[0].windows[0].lifted);
    assert_eq!(
        (
            idle.providers[0].windows[1].used_percentage,
            idle.providers[0].windows[1].resets_at,
        ),
        (Some(4), Some(future))
    );
    assert_eq!(unknown_since_ms(&runtime, "codex"), None);
}

#[test]
fn producer_cache_tracks_logged_in_panels() {
    let (_dir, workspace, runtime) = runtime();
    let future = Timestamp::from_second(4_000_000_000).unwrap();
    let mut seeded = snapshot_with_panels(
        workspace.clone(),
        vec![
            provider_panel("claude", vec![rl_window(40, Some(future))]),
            provider_panel("codex", vec![rl_window(30, Some(future))]),
        ],
    );
    apply_rate_limit_cache(&mut seeded, &runtime, true);

    let mut partial = snapshot_with_panels(
        workspace.clone(),
        vec![provider_panel("claude", vec![rl_window(40, Some(future))])],
    );
    apply_rate_limit_cache(&mut partial, &runtime, true);
    let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    assert!(cache.entries.contains_key("claude"));
    assert!(!cache.entries.contains_key("codex"));

    let mut consumer = snapshot_with_panels(workspace.clone(), Vec::new());
    apply_rate_limit_cache(&mut consumer, &runtime, false);
    let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    assert!(cache.entries.contains_key("claude"));

    let mut producer = snapshot_with_panels(workspace, Vec::new());
    apply_rate_limit_cache(&mut producer, &runtime, true);
    assert!(
        read_rate_limits_cache(&runtime.shared_rate_limits_path())
            .entries
            .is_empty()
    );
}
#[test]
fn account_merge_preserves_other_kinds() {
    let (_dir, _workspace, runtime) = runtime();
    write_claude_windows(&runtime, vec![rl_window(20, None)]);
    super::merge_account_rate_limits(
        &runtime,
        "codex",
        Default::default(),
        AgentRateLimits {
            windows: vec![rl_window(55, None)],
        },
    );

    let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    assert_eq!(
        cache_window(&cache, "codex", RateLimitWindowKey::Duration(Some(300))).used_percentage,
        Some(55)
    );
    assert!(cache.entries.contains_key("claude"));
}
#[test]
fn authoritative_omissions_track_lifted_duration_windows() {
    let (_dir, _workspace, runtime) = runtime();
    let five_hours = 300;
    let seven_days = 10_080;
    write_rate_limits_cache(
        &runtime.shared_rate_limits_path(),
        &kind_wide_cache(
            1,
            BTreeMap::from([(
                "codex".to_owned(),
                AgentRateLimits {
                    windows: vec![
                        authoritative(rl_window_mins(20, None, five_hours)),
                        authoritative(rl_window_mins(40, None, seven_days)),
                    ],
                },
            )]),
            BTreeMap::new(),
        ),
    );

    let only_week = AgentRateLimits {
        windows: vec![authoritative(rl_window_mins(41, None, seven_days))],
    };
    for _ in 0..2 {
        super::merge_account_rate_limits(&runtime, "codex", Default::default(), only_week.clone());
    }
    let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    assert_eq!(cache.entries["codex"].limits.windows.len(), 2);
    let lifted = cache_window(
        &cache,
        "codex",
        RateLimitWindowKey::Duration(Some(five_hours)),
    );
    assert!(lifted.lifted);
    assert_eq!((lifted.used_percentage, lifted.resets_at), (None, None));
    let reported = cache_window(
        &cache,
        "codex",
        RateLimitWindowKey::Duration(Some(seven_days)),
    );
    assert_eq!(reported.used_percentage, Some(41));
    assert!(!reported.lifted);

    super::merge_account_rate_limits(
        &runtime,
        "codex",
        Default::default(),
        AgentRateLimits {
            windows: vec![
                authoritative(rl_window_mins(2, None, five_hours)),
                authoritative(rl_window_mins(42, None, seven_days)),
            ],
        },
    );
    assert!(
        read_rate_limits_cache(&runtime.shared_rate_limits_path()).entries["codex"]
            .limits
            .windows
            .iter()
            .all(|window| !window.lifted)
    );

    super::merge_account_rate_limits(
        &runtime,
        "codex",
        Default::default(),
        AgentRateLimits::default(),
    );
    assert!(
        read_rate_limits_cache(&runtime.shared_rate_limits_path()).entries["codex"]
            .limits
            .windows
            .is_empty()
    );
}
#[test]
fn authoritative_account_identity_survives_publication_and_rotates_by_key() {
    let (_dir, workspace, runtime) = runtime();
    let scope = ProviderAccountScope::sub_provider("alibaba", "international");

    super::merge_account_rate_limits(
        &runtime,
        "qwen",
        AccountUsageIdentity {
            scope: scope.clone(),
            account_key: Some("first".to_owned()),
            credentials_stamp: Some(1),
        },
        AgentRateLimits {
            windows: vec![rl_window_mins(20, None, 300)],
        },
    );
    let first = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    assert_eq!(first.entries["qwen"].account_key.as_deref(), Some("first"));

    super::merge_account_rate_limits(
        &runtime,
        "qwen",
        AccountUsageIdentity {
            scope: scope.clone(),
            account_key: Some("second".to_owned()),
            credentials_stamp: Some(2),
        },
        AgentRateLimits {
            windows: vec![rl_window_mins(70, None, 300)],
        },
    );
    let rotated = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    assert_eq!(
        rotated.entries["qwen"].account_key.as_deref(),
        Some("second")
    );
    assert_eq!(rotated.entries["qwen"].limits.windows.len(), 1);

    let mut snapshot = snapshot_with_panels(
        workspace,
        vec![provider_panel("qwen", vec![rl_window_mins(10, None, 300)])],
    );
    snapshot.providers[0].account_scope = scope;
    apply_rate_limit_cache(&mut snapshot, &runtime, true);
    let after_display_fusion = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    assert_eq!(
        after_display_fusion.entries["qwen"].account_key.as_deref(),
        Some("second")
    );
    assert_eq!(
        after_display_fusion.entries["qwen"].limits.windows[0].used_percentage,
        Some(70),
        "a scope-only panel cannot rewrite exact-account control truth"
    );
}

#[test]
fn omission_completion_requires_matching_authoritative_duration_truth() {
    let now = Timestamp::from_second(2_000_000_000).unwrap();
    let future = Timestamp::from_second(4_000_000_000).unwrap();
    let stamped = |window| RateLimitWindow {
        observed_at: Some(now),
        ..window
    };
    let scoped = |id, label, used| stamped(scoped_window(id, label, used, future));
    let cases = vec![
        (
            "same-scope authoritative prior",
            vec![stamped(authoritative(rl_window_mins(20, None, 300)))],
            AgentRateLimits {
                windows: vec![stamped(authoritative(rl_window_mins(41, None, 10_080)))],
            },
            true,
        ),
        (
            "best-effort current",
            vec![stamped(authoritative(rl_window_mins(20, None, 300)))],
            AgentRateLimits {
                windows: vec![stamped(rl_window_mins(41, None, 10_080))],
            },
            false,
        ),
        (
            "best-effort prior",
            vec![stamped(rl_window_mins(20, None, 300))],
            AgentRateLimits {
                windows: vec![stamped(authoritative(rl_window_mins(41, None, 10_080)))],
            },
            false,
        ),
        (
            "named quotas",
            vec![scoped("build_minutes", "bld", 20)],
            AgentRateLimits {
                windows: vec![scoped("deployments", "dep", 40)],
            },
            false,
        ),
        (
            "cold declared durations",
            Vec::new(),
            AgentRateLimits {
                windows: vec![stamped(authoritative(rl_window_mins(41, None, 10_080)))],
            },
            false,
        ),
    ];
    for (name, prior, mut current, expected_lift) in cases {
        complete_omitted_duration_windows(&prior, &mut current);
        assert_eq!(
            current.windows.iter().any(|window| window.lifted),
            expected_lift,
            "{name}"
        );
    }

    let (_dir, workspace, runtime) = runtime();
    let openai = ProviderAccountScope::sub_provider("openai", "oauth");
    write_rate_limits_cache(
        &runtime.shared_rate_limits_path(),
        &RateLimitsCache {
            entries: BTreeMap::from([(
                "pi".to_owned(),
                RateLimitCacheEntry {
                    scope: openai.clone(),
                    account_key: None,
                    limits: AgentRateLimits {
                        windows: vec![
                            stamped(authoritative(rl_window_mins(20, None, 300))),
                            stamped(authoritative(rl_window_mins(40, None, 10_080))),
                        ],
                    },
                    pending: Vec::new(),
                    unknown_since_ms: None,
                },
            )]),
            ..Default::default()
        },
    );
    let weekly = stamped(authoritative(rl_window_mins(41, None, 10_080)));
    let mut matching = provider_panel("pi", vec![weekly.clone()]);
    matching.account_scope = openai;
    let mut matching = snapshot_with_panels(workspace.clone(), vec![matching]);
    apply_rate_limit_cache(&mut matching, &runtime, false);
    assert!(
        matching.providers[0]
            .windows
            .iter()
            .any(|window| window.lifted)
    );

    let mut mismatched = provider_panel("pi", vec![weekly]);
    mismatched.account_scope = ProviderAccountScope::sub_provider("anthropic", "oauth");
    let mut mismatched = snapshot_with_panels(workspace, vec![mismatched]);
    apply_rate_limit_cache(&mut mismatched, &runtime, false);
    assert_eq!(mismatched.providers[0].windows.len(), 1);
    assert!(
        mismatched.providers[0]
            .windows
            .iter()
            .all(|window| !window.lifted)
    );
}

#[test]
fn drop_kind_removes_only_target_entry() {
    let (_dir, _workspace, runtime) = runtime();
    let first_seen_at = Timestamp::from_second(2_000_000_000).unwrap();
    let pending = |used_percentage| {
        vec![PendingRefill {
            scope_id: None,
            duration_mins: Some(300),
            used_percentage,
            first_seen_at,
        }]
    };
    write_rate_limits_cache(
        &runtime.shared_rate_limits_path(),
        &kind_wide_cache(
            1,
            BTreeMap::from([
                (
                    "codex".to_owned(),
                    AgentRateLimits {
                        windows: vec![rl_window(55, None)],
                    },
                ),
                (
                    "claude".to_owned(),
                    AgentRateLimits {
                        windows: vec![rl_window(20, None)],
                    },
                ),
            ]),
            BTreeMap::from([
                ("codex".to_owned(), pending(1)),
                ("claude".to_owned(), pending(2)),
            ]),
        ),
    );

    drop_kind_rate_limits(&runtime, "codex");
    let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    assert!(!cache.entries.contains_key("codex"));
    assert_eq!(cache.entries["claude"].pending[0].used_percentage, 2);
    assert_eq!(
        cache.entries["claude"].limits.windows[0].used_percentage,
        Some(20)
    );

    let refreshed_at_ms = cache.refreshed_at_ms;
    drop_kind_rate_limits(&runtime, "missing");
    assert_eq!(
        read_rate_limits_cache(&runtime.shared_rate_limits_path()).refreshed_at_ms,
        refreshed_at_ms
    );
}

#[test]
fn producer_lock_contention_degrades_to_read_only() {
    let (_dir, workspace, runtime) = runtime();
    write_claude_windows(&runtime, vec![rl_window(20, None)]);
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(runtime.shared_rate_limits_lock())
        .unwrap();
    lock_file.try_lock().unwrap();

    let mut contending = snapshot_with_panels(
        workspace,
        vec![provider_panel("codex", vec![rl_window(55, None)])],
    );
    apply_rate_limit_cache(&mut contending, &runtime, true);
    let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    assert_eq!(
        cache_window(&cache, "claude", RateLimitWindowKey::Duration(Some(300))).used_percentage,
        Some(20)
    );
    assert!(!cache.entries.contains_key("codex"));
    lock_file.unlock().unwrap();
}

#[test]
fn detached_merge_waits_for_lock_and_preserves_other_kinds() {
    let (_dir, _workspace, runtime) = runtime();
    write_claude_windows(&runtime, vec![rl_window(20, None)]);
    let held =
        crate::store::lock::WorkspaceLock::acquire(&runtime.shared_rate_limits_lock()).unwrap();
    let worker_runtime = runtime.clone();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        super::merge_account_rate_limits(
            &worker_runtime,
            "codex",
            Default::default(),
            AgentRateLimits {
                windows: vec![authoritative(rl_window(55, None))],
            },
        );
        finished_tx.send(()).unwrap();
    });
    assert!(
        finished_rx
            .recv_timeout(std::time::Duration::from_millis(50))
            .is_err()
    );

    drop(held);
    finished_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .unwrap();
    worker.join().unwrap();
    let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    assert!(cache.entries.contains_key("claude"));
    assert!(cache.entries.contains_key("codex"));
}

fn fuse_now() -> Timestamp {
    Timestamp::from_second(2_000_000_000).unwrap()
}

fn be(used: u8, resets_at: Timestamp, observed_at: Timestamp) -> RateLimitWindow {
    RateLimitWindow {
        used_percentage: Some(used),
        resets_at: Some(resets_at),
        duration_mins: Some(300),
        observed_at: Some(observed_at),
        source: WindowSource::BestEffort,
        ..Default::default()
    }
}

fn auth(used: u8, resets_at: Timestamp, observed_at: Timestamp) -> RateLimitWindow {
    RateLimitWindow {
        source: WindowSource::Authoritative,
        ..be(used, resets_at, observed_at)
    }
}

#[test]
fn fuse_window_selects_truth_by_source_freshness_and_epoch() {
    let now = fuse_now();
    let reset = now + SignedDuration::from_secs(3_600);
    let later_reset = reset + SignedDuration::from_secs(7_200);
    let older = now - SignedDuration::from_secs(60);
    let stale = now - SignedDuration::from_secs(LIVE_HORIZON_SECS + 1);
    let parked = PendingRefill {
        scope_id: None,
        duration_mins: Some(300),
        used_percentage: 2,
        first_seen_at: now,
    };
    let lifted = RateLimitWindow {
        duration_mins: Some(300),
        observed_at: Some(now),
        source: WindowSource::Authoritative,
        lifted: true,
        ..Default::default()
    };
    let cases = vec![
        (
            "no live input carries truth and pending",
            Some(be(50, reset, now)),
            None,
            Some(parked.clone()),
            true,
            Some(be(50, reset, now)),
            Some(parked.clone()),
        ),
        (
            "cold live input becomes truth",
            None,
            Some(be(30, reset, now)),
            None,
            true,
            Some(be(30, reset, now)),
            None,
        ),
        (
            "climb clears pending",
            Some(be(20, reset, now)),
            Some(be(35, reset, now)),
            Some(parked.clone()),
            true,
            Some(be(35, reset, now)),
            None,
        ),
        (
            "fresh authoritative drop wins",
            Some(be(80, reset, now)),
            Some(auth(2, reset, now)),
            None,
            true,
            Some(auth(2, reset, now)),
            None,
        ),
        (
            "authoritative drop supersedes unprovenanced truth",
            Some(RateLimitWindow {
                observed_at: None,
                ..be(80, reset, now)
            }),
            Some(auth(2, reset, now)),
            None,
            true,
            Some(auth(2, reset, now)),
            None,
        ),
        (
            "stale authoritative drop holds newer truth",
            Some(auth(80, reset, now)),
            Some(auth(2, reset, older)),
            None,
            true,
            Some(auth(80, reset, now)),
            None,
        ),
        (
            "reset advance proves a new epoch",
            Some(be(80, reset, now)),
            Some(be(1, later_reset, now)),
            None,
            true,
            Some(be(1, later_reset, now)),
            None,
        ),
        (
            "stale live sample is ignored",
            Some(be(50, reset, now)),
            Some(be(2, reset, stale)),
            None,
            true,
            Some(be(50, reset, now)),
            None,
        ),
        (
            "consumer cannot lower truth",
            Some(be(75, reset, now)),
            Some(be(1, reset, now)),
            None,
            false,
            Some(be(75, reset, now)),
            None,
        ),
        (
            "mid-range jitter never confirms",
            Some(be(80, reset, now)),
            Some(be(70, reset, now)),
            None,
            true,
            Some(be(80, reset, now)),
            None,
        ),
        (
            "lifted row carries without live input",
            Some(lifted.clone()),
            None,
            None,
            true,
            Some(lifted.clone()),
            None,
        ),
        (
            "real reading replaces lifted row",
            Some(lifted),
            Some(auth(1, reset, now)),
            None,
            true,
            Some(auth(1, reset, now)),
            None,
        ),
    ];

    for (name, prior, live, pending, producer, expected_truth, expected_pending) in cases {
        let actual = fuse_window(
            prior.as_ref(),
            live.as_ref(),
            pending.as_ref(),
            now,
            producer,
        );
        assert_eq!(actual, (expected_truth, expected_pending), "{name}");
    }
}

#[test]
fn best_effort_refill_requires_sustained_low_reading() {
    let now = fuse_now();
    let reset = now + SignedDuration::from_secs(3_600);
    let (truth, pending) = fuse_window(
        Some(&be(75, reset, now)),
        Some(&be(1, reset, now)),
        None,
        now,
        true,
    );
    assert_eq!(truth.unwrap().used_percentage, Some(75));
    let parked = pending.unwrap();
    assert_eq!(parked.used_percentage, 1);

    let before = now + SignedDuration::from_secs(REFILL_CONFIRM_SECS - 1);
    let (truth, pending) = fuse_window(
        Some(&be(75, reset, before)),
        Some(&be(2, reset, before)),
        Some(&parked),
        before,
        true,
    );
    assert_eq!(truth.unwrap().used_percentage, Some(75));
    assert!(pending.is_some());

    let after = now + SignedDuration::from_secs(REFILL_CONFIRM_SECS + 1);
    let (truth, pending) = fuse_window(
        Some(&be(75, reset, after)),
        Some(&be(3, reset, after)),
        Some(&parked),
        after,
        true,
    );
    assert_eq!(truth.unwrap().used_percentage, Some(3));
    assert!(pending.is_none());

    let (_, pending) = fuse_window(
        Some(&be(40, reset, now)),
        Some(&be(2, reset, now)),
        None,
        now,
        true,
    );
    let next = now + SignedDuration::from_secs(5);
    let (truth, pending) = fuse_window(
        Some(&be(40, reset, next)),
        Some(&be(41, reset, next)),
        pending.as_ref(),
        next,
        true,
    );
    assert_eq!(truth.unwrap().used_percentage, Some(41));
    assert!(pending.is_none());
}
