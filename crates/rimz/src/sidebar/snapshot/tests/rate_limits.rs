use super::*;

#[test]
fn account_cache_missing_probeable_versions_refreshes_on_retry_cadence() {
    for kind in ["claude", "codex", "pi"] {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/provider-version"));
        let snapshot = SidebarSnapshot::build_with_agents(
            workspace.clone(),
            Vec::new(),
            vec![root_agent(kind, "active", None)],
            Timestamp::now(),
        );
        let mut accounts = BTreeMap::new();
        accounts.insert(
            kind.to_owned(),
            crate::agents::AgentAccount {
                plan: Some("Pro".to_owned()),
                metered: Some(true),
                version: None,
                sub_provider: None,
            },
        );
        let now_ms = unix_now_ms();
        let fresh_cache = AccountsCache {
            refreshed_at_ms: now_ms,
            accounts,
            ok: true,
        };
        assert!(
            !accounts_cache_version_refresh_due(&fresh_cache, &snapshot, now_ms),
            "a just-refreshed successful {kind} cache missing a display version waits for the retry window"
        );

        let stale_cache = AccountsCache {
            refreshed_at_ms: now_ms.saturating_sub(ACCOUNTS_RETRY_TTL.as_millis() as u64 + 1),
            ..fresh_cache
        };
        assert!(
            accounts_cache_version_refresh_due(&stale_cache, &snapshot, now_ms),
            "a successful {kind} account cache missing a display version re-probes after the retry window"
        );

        let empty_cache = AccountsCache {
            refreshed_at_ms: now_ms.saturating_sub(ACCOUNTS_RETRY_TTL.as_millis() as u64 + 1),
            accounts: BTreeMap::new(),
            ok: true,
        };
        assert!(
            accounts_cache_version_refresh_due(&empty_cache, &snapshot, now_ms),
            "an active {kind} session can still get a version-only cache entry"
        );

        let failed_cache = AccountsCache {
            ok: false,
            ..empty_cache
        };
        assert!(
            !accounts_cache_version_refresh_due(&failed_cache, &snapshot, now_ms),
            "a failed {kind} probe uses the failure TTL, not the missing-version bypass"
        );
    }
}

#[test]
fn idle_window_projection_ages_only_known_elapsed_windows() {
    let now = Timestamp::from_second(2_000_000_000).unwrap();
    let future = Timestamp::from_second(2_000_010_000).unwrap();
    let cached = rl_window(80, Some(future));
    let projected = project_idle_window(cached.clone(), now);
    assert_eq!(projected, cached, "before reset the cached reading stands");

    let passed = Timestamp::from_second(1_999_990_000).unwrap();
    let projected = project_idle_window(rl_window(95, Some(passed)), now);
    assert_eq!(projected.used_percentage, Some(0), "a reset window is full");
    assert_eq!(
        projected.resets_at,
        now.checked_add(SignedDuration::from_secs(300 * 60)).ok(),
        "the reset rolls one window length (300 min) forward from now"
    );

    let undated = rl_window(40, None);
    assert_eq!(project_idle_window(undated.clone(), now), undated);
    let no_duration = RateLimitWindow {
        used_percentage: Some(90),
        resets_at: Some(passed),
        duration_mins: None,
    };
    assert_eq!(project_idle_window(no_duration.clone(), now), no_duration);
}

/// The producer persists a live reading as ground truth; once the session is
/// idle (no live window), a reader projects that reading back onto the panel,
/// so the dashboard shows last-known budgets instead of an empty bar.
#[test]
fn producer_persists_live_windows_for_idle_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let future = Timestamp::from_second(4_000_000_000).unwrap();

    // A live frame reports 60% used on the 5h window; the producer writes it.
    let mut producing = snapshot_with_panels(
        workspace.clone(),
        vec![provider_panel("claude", vec![rl_window(60, Some(future))])],
    );
    apply_rate_limit_cache(&mut producing, &runtime, true);
    let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    assert_eq!(
        cache
            .windows
            .get("claude")
            .and_then(|limits| limits.windows.first())
            .and_then(|window| window.used_percentage),
        Some(60),
        "the live reading is persisted as ground truth"
    );

    // The session goes idle (no live window). A reader projects the cached
    // reading back onto the panel — the dashboard is not empty.
    let mut idle = snapshot_with_panels(workspace, vec![provider_panel("claude", Vec::new())]);
    apply_rate_limit_cache(&mut idle, &runtime, false);
    assert_eq!(
        idle.providers[0]
            .windows
            .first()
            .and_then(|window| window.used_percentage),
        Some(60),
        "an idle frame still shows the last-known budget"
    );
}

/// An idle short window whose reset has passed projects to full while the
/// longer cached window is still active, but the
/// producer keeps persisting the real last reading — the synthesized full
/// window is a read-time projection, never written back.
#[test]
fn idle_short_window_past_reset_shows_full_without_persisting_the_synthetic_window() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let passed = Timestamp::from_second(1_000_000_000).unwrap(); // 2001 — always past
    let future = Timestamp::from_second(4_000_000_000).unwrap();

    // Seed a drained 5h reading whose reset has long since passed, plus a 7d
    // reading whose reset is still ahead. The short window refills, but the
    // long window keeps the cache inside a known provider budget shape.
    let path = runtime.shared_rate_limits_path();
    write_rate_limits_cache(
        &path,
        &RateLimitsCache {
            refreshed_at_ms: 0,
            windows: BTreeMap::from([(
                "claude".to_owned(),
                AgentRateLimits {
                    windows: vec![
                        rl_window_mins(90, Some(passed), 300),
                        rl_window_mins(70, Some(future), 7 * 24 * 60),
                    ],
                },
            )]),
        },
    );

    // An idle producer frame with no live window: the display projects to
    // full, while the persisted ground truth stays the real 90% reading.
    let mut idle = snapshot_with_panels(workspace, vec![provider_panel("claude", Vec::new())]);
    apply_rate_limit_cache(&mut idle, &runtime, true);
    let shown = idle.providers[0].windows.first().expect("a full window");
    assert_eq!(shown.used_percentage, Some(0), "a reset window shows full");
    assert!(shown.resets_at.is_some(), "with a rolled-forward countdown");
    assert_eq!(
        idle.providers[0]
            .windows
            .get(1)
            .and_then(|window| window.used_percentage),
        Some(70),
        "the unexpired long window keeps its cached reading"
    );

    let persisted = read_rate_limits_cache(&path);
    assert_eq!(
        persisted
            .windows
            .get("claude")
            .and_then(|limits| limits.windows.first())
            .and_then(|window| window.used_percentage),
        Some(90),
        "the cache retains ground truth, not the synthesized full window"
    );
}

/// Once the longest cached window has reset, every cached bar reads as unknown
/// until a provider refresh supplies real data. The cache still stores the last
/// ground-truth readings so a future projection never writes synthetic values.
#[test]
fn idle_longest_window_past_reset_shows_unknown_without_persisting_it() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let passed = Timestamp::from_second(1_000_000_000).unwrap();
    let path = runtime.shared_rate_limits_path();
    write_rate_limits_cache(
        &path,
        &RateLimitsCache {
            refreshed_at_ms: 0,
            windows: BTreeMap::from([(
                "claude".to_owned(),
                AgentRateLimits {
                    windows: vec![
                        rl_window_mins(90, Some(passed), 300),
                        rl_window_mins(80, Some(passed), 7 * 24 * 60),
                    ],
                },
            )]),
        },
    );

    let mut idle = snapshot_with_panels(workspace, vec![provider_panel("claude", Vec::new())]);
    apply_rate_limit_cache(&mut idle, &runtime, true);

    assert_eq!(idle.providers[0].windows.len(), 2);
    assert!(
        idle.providers[0]
            .windows
            .iter()
            .all(|window| window.used_percentage.is_none() && window.resets_at.is_none()),
        "expired long-window cache displays unknown bars"
    );
    assert_eq!(
        idle.providers[0]
            .windows
            .iter()
            .map(|window| window.duration_mins)
            .collect::<Vec<_>>(),
        vec![Some(300), Some(7 * 24 * 60)],
        "unknown bars keep their window labels"
    );

    let persisted = read_rate_limits_cache(&path);
    let persisted_windows = &persisted.windows["claude"].windows;
    assert_eq!(persisted_windows[0].used_percentage, Some(90));
    assert_eq!(persisted_windows[1].used_percentage, Some(80));
}

/// When one provider logs out while another stays, the logged-out kind loses
/// its panel, so the producer's next write — rebuilt from the panels alone —
/// drops its cached windows while the surviving kind's are kept. Cache
/// presence tracks login, so no stale budget can flash on a later re-login.
#[test]
fn producer_drops_windows_for_a_logged_out_provider() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let future = Timestamp::from_second(4_000_000_000).unwrap();
    let path = runtime.shared_rate_limits_path();
    let windows = |used| vec![rl_window(used, Some(future))];

    // Seed windows for both providers through a live frame.
    let mut seeded = snapshot_with_panels(
        workspace.clone(),
        vec![
            provider_panel("claude", windows(40)),
            provider_panel("codex", windows(30)),
        ],
    );
    apply_rate_limit_cache(&mut seeded, &runtime, true);
    let seeded_cache = read_rate_limits_cache(&path);
    assert!(seeded_cache.windows.contains_key("claude"));
    assert!(seeded_cache.windows.contains_key("codex"));

    // Codex logs out: only claude has a panel now. The next producer write
    // rebuilds the cache from the surviving panels, so codex drops out while
    // claude's windows are kept.
    let mut codex_gone =
        snapshot_with_panels(workspace, vec![provider_panel("claude", windows(40))]);
    apply_rate_limit_cache(&mut codex_gone, &runtime, true);
    let after = read_rate_limits_cache(&path);
    assert!(
        after.windows.contains_key("claude"),
        "a still-logged-in provider keeps its windows"
    );
    assert!(
        !after.windows.contains_key("codex"),
        "a logged-out provider's windows drop on the next write"
    );
}

/// The out-of-band helper seeds one kind's windows into the shared cache
/// without disturbing another kind's, so an idle provider's bars paint from
/// the next producer frame.
#[test]
fn merge_account_rate_limits_seeds_a_kind_without_clobbering_others() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let path = runtime.shared_rate_limits_path();

    // Claude already has cached windows from a live session this run.
    write_rate_limits_cache(
        &path,
        &RateLimitsCache {
            refreshed_at_ms: 1,
            windows: BTreeMap::from([(
                "claude".to_owned(),
                AgentRateLimits {
                    windows: vec![rl_window(20, None)],
                },
            )]),
        },
    );

    merge_account_rate_limits(
        &runtime,
        "codex",
        AgentRateLimits {
            windows: vec![rl_window(55, None)],
        },
    );

    let cache = read_rate_limits_cache(&path);
    assert_eq!(
        cache
            .windows
            .get("codex")
            .and_then(|limits| limits.windows.first())
            .and_then(|w| w.used_percentage),
        Some(55),
        "the idle provider's windows are seeded"
    );
    assert!(
        cache.windows.contains_key("claude"),
        "an existing kind's windows are preserved"
    );
}

#[test]
fn held_rate_limit_lock_makes_producer_read_only_instead_of_dropping_other_kinds() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let path = runtime.shared_rate_limits_path();
    write_rate_limits_cache(
        &path,
        &RateLimitsCache {
            refreshed_at_ms: 1,
            windows: BTreeMap::from([(
                "claude".to_owned(),
                AgentRateLimits {
                    windows: vec![rl_window(20, None)],
                },
            )]),
        },
    );

    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(runtime.shared_rate_limits_lock())
        .unwrap();
    <std::fs::File as fs4::FileExt>::try_lock(&lock_file).unwrap();

    let mut contending = snapshot_with_panels(
        workspace,
        vec![provider_panel("codex", vec![rl_window(55, None)])],
    );
    apply_rate_limit_cache(&mut contending, &runtime, true);

    let cache = read_rate_limits_cache(&path);
    assert_eq!(
        cache
            .windows
            .get("claude")
            .and_then(|limits| limits.windows.first())
            .and_then(|window| window.used_percentage),
        Some(20),
        "a producer that cannot get the RMW lock leaves existing kinds intact"
    );
    assert!(
        !cache.windows.contains_key("codex"),
        "the contending producer does not publish its partial provider set"
    );
    <std::fs::File as fs4::FileExt>::unlock(&lock_file).unwrap();
}
